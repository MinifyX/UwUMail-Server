//! Backups as the server runs them: settings in the database, a daily run at a chosen hour, a run
//! on request, and the status of the last one.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, watch};
use uwumail_store::Store;

use crate::sftp::Sftp;
use crate::{BackupReport, Error, Manifest, RepoKey, Repository, Retention, Storage, Target};

const SETTINGS_KEY: &str = "backup.settings";
const STATUS_KEY: &str = "backup.status";
/// After a failed run, the next attempt waits this long.
const RETRY_SECS: i64 = 3600;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupSettings {
    pub enabled: bool,
    pub target: Option<Target>,
    /// The recovery key of an encrypted repository; `None` backs up without encryption.
    pub key: Option<String>,
    pub retention: Retention,
    /// The hour (UTC) from which the daily backup runs.
    pub hour: u8,
}

impl Default for BackupSettings {
    fn default() -> Self {
        BackupSettings { enabled: false, target: None, key: None, retention: Retention::default(), hour: 1 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupStatus {
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_report: Option<BackupReport>,
}

struct Inner {
    store: Store,
    hostname: String,
    version: String,
    wakeup: Notify,
    running: Mutex<()>,
}

#[derive(Clone)]
pub struct Backups {
    inner: Arc<Inner>,
}

impl Backups {
    pub fn new(store: Store, hostname: &str, version: &str) -> Backups {
        Backups {
            inner: Arc::new(Inner {
                store,
                hostname: hostname.to_owned(),
                version: version.to_owned(),
                wakeup: Notify::new(),
                running: Mutex::new(()),
            }),
        }
    }

    pub async fn settings(&self) -> Result<BackupSettings, Error> {
        Ok(match self.inner.store.setting(SETTINGS_KEY).await? {
            Some(raw) => {
                serde_json::from_str(&raw).map_err(|_| Error::Config("the backup settings are damaged".into()))?
            }
            None => BackupSettings::default(),
        })
    }

    pub async fn save_settings(&self, settings: &BackupSettings) -> Result<(), Error> {
        if settings.hour > 23 {
            return Err(Error::Config("the hour must be between 0 and 23".into()));
        }
        if let Some(key) = &settings.key {
            RepoKey::from_recovery_text(key)?;
        }
        let raw = serde_json::to_string(settings).expect("settings serialize");
        self.inner.store.set_setting(SETTINGS_KEY, &raw).await?;
        Ok(())
    }

    pub async fn status(&self) -> BackupStatus {
        match self.inner.store.setting(STATUS_KEY).await {
            Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
            _ => BackupStatus::default(),
        }
    }

    async fn save_status(&self, status: &BackupStatus) {
        let raw = serde_json::to_string(status).expect("status serializes");
        if let Err(err) = self.inner.store.set_setting(STATUS_KEY, &raw).await {
            tracing::warn!(%err, "saving the backup status failed");
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.try_lock().is_err()
    }

    /// Asks the scheduler to back up right away.
    pub fn run_soon(&self) {
        self.inner.wakeup.notify_one();
    }

    /// Opens the repository, remembering the host key the first time.
    async fn open(&self, settings: &mut BackupSettings) -> Result<Repository, Error> {
        let target = settings.target.as_mut().ok_or_else(|| Error::Config("no backup server is set up".into()))?;
        let sftp = Sftp::connect(target).await?;
        if target.host_key.is_none() {
            target.host_key = Some(sftp.host_key.clone());
            self.save_settings(settings).await?;
        }
        let key = settings.key.as_deref().map(RepoKey::from_recovery_text).transpose()?;
        Repository::open(Storage::Sftp(sftp), key, now()).await
    }

    /// Backs up now, unless one is running already.
    pub async fn run_now(&self) -> Result<BackupReport, Error> {
        let _running =
            self.inner.running.try_lock().map_err(|_| Error::Config("a backup is running already".into()))?;
        let mut status = self.status().await;
        status.last_attempt_at = Some(now());
        self.save_status(&status).await;

        let mut settings = self.settings().await?;
        let result = async {
            let repo = self.open(&mut settings).await?;
            let report = crate::backup(
                &self.inner.store,
                &repo,
                &self.inner.hostname,
                &self.inner.version,
                settings.retention,
                now(),
            )
            .await;
            repo.storage.close().await;
            report
        }
        .await;
        match &result {
            Ok(report) => {
                status.last_success_at = Some(now());
                status.last_error = None;
                status.last_report = Some(report.clone());
                tracing::info!(snapshot = %report.snapshot, uploaded = report.uploaded, "backup done");
            }
            Err(err) => {
                status.last_error = Some(err.to_string());
                tracing::warn!(%err, "backup failed");
            }
        }
        self.save_status(&status).await;
        result
    }

    /// The snapshots in the repository, newest first.
    pub async fn snapshots(&self) -> Result<Vec<(String, Manifest)>, Error> {
        let mut settings = self.settings().await?;
        let repo = self.open(&mut settings).await?;
        let mut found = Vec::new();
        let result = async {
            for name in repo.snapshots().await?.into_iter().rev() {
                let manifest = repo.manifest(&name).await?;
                found.push((name, manifest));
            }
            Ok(found)
        }
        .await;
        repo.storage.close().await;
        result
    }

    /// Whether the daily run is due.
    fn due(settings: &BackupSettings, status: &BackupStatus, now: i64) -> bool {
        if !settings.enabled || settings.target.is_none() {
            return false;
        }
        let today = now.div_euclid(86_400);
        let hour = now.rem_euclid(86_400) / 3600;
        if hour < i64::from(settings.hour) {
            return false;
        }
        let succeeded_today = status.last_success_at.is_some_and(|at| at.div_euclid(86_400) == today);
        let tried_recently = status.last_attempt_at.is_some_and(|at| now - at < RETRY_SECS);
        !succeeded_today && !tried_recently
    }

    /// Runs backups when they are due or asked for, until shutdown.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        loop {
            let asked = tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => false,
                _ = self.inner.wakeup.notified() => true,
                _ = shutdown.changed() => return,
            };
            let settings = match self.settings().await {
                Ok(settings) => settings,
                Err(err) => {
                    tracing::warn!(%err, "reading the backup settings failed");
                    continue;
                }
            };
            if asked || Self::due(&settings, &self.status().await, now()) {
                let _ = self.run_now().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Login;

    #[test]
    fn the_daily_backup_waits_for_its_hour_and_retries_hourly() {
        let target = Target {
            host: "nas.example.de".into(),
            port: 22,
            user: "backup".into(),
            path: "uwumail".into(),
            login: Login::Password { password: "geheim".into() },
            host_key: None,
        };
        let settings = BackupSettings { enabled: true, target: Some(target), hour: 3, ..Default::default() };
        let day = 20_000 * 86_400;
        let fresh = BackupStatus::default();
        assert!(!Backups::due(&settings, &fresh, day + 2 * 3600), "before three");
        assert!(Backups::due(&settings, &fresh, day + 3 * 3600));
        let done = BackupStatus {
            last_success_at: Some(day + 3 * 3600),
            last_attempt_at: Some(day + 3 * 3600),
            ..fresh.clone()
        };
        assert!(!Backups::due(&settings, &done, day + 20 * 3600), "once a day");
        assert!(Backups::due(&settings, &done, day + 86_400 + 5 * 3600), "the next day");
        let failed = BackupStatus { last_attempt_at: Some(day + 3 * 3600), ..fresh.clone() };
        assert!(!Backups::due(&settings, &failed, day + 3 * 3600 + 600));
        assert!(Backups::due(&settings, &failed, day + 4 * 3600 + 1), "again an hour later");
        let off = BackupSettings { enabled: false, ..settings };
        assert!(!Backups::due(&off, &fresh, day + 5 * 3600));
    }
}
