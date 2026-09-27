//! Backups as the server runs them: settings in the database, a daily run at a chosen hour, a run
//! on request, and the status of the last one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, watch};
use uwumail_store::Store;

use crate::mailbox::{MailboxProgress, MailboxRestoreReport, SnapshotPerson};
use crate::{BackupReport, Error, Manifest, RepoKey, Repository, Retention, Storage, Target};

const SETTINGS_KEY: &str = "backup.settings";
const STATUS_KEY: &str = "backup.status";
/// The file that tells the next start a snapshot is waiting to take over, and what it is.
pub const READY_FILE: &str = "restore.ready";
/// The directory a snapshot is put together in before it does.
pub const STAGING_DIR: &str = "restore";
/// After a failed run, the next attempt waits this long.
const RETRY_SECS: i64 = 3600;
/// A run that has not finished after this long was cut short by a restart, not still going. Without
/// the limit a single kill would keep `blocks` saying "a backup is running" forever.
const RUN_MAX_SECS: i64 = 6 * 3600;
/// Where the database of a snapshot is put together to take one mailbox out of it.
const MAILBOX_DIR: &str = "backup-tmp/restore-mailbox";

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
    /// The minute of that hour. Older settings have none and start on the hour.
    pub minute: u8,
}

impl Default for BackupSettings {
    fn default() -> Self {
        BackupSettings { enabled: false, target: None, key: None, retention: Retention::default(), hour: 1, minute: 0 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupStatus {
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    /// When the run that is going on, or the last one, began and ended. Both are missing for
    /// backups made before the server kept track, which then simply never block an update.
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_report: Option<BackupReport>,
}

/// What the portal writes down when it has fetched a snapshot, for the next start to read.
///
/// The fetching half cannot put the files where they belong: the database it would replace is the
/// one it is running on. So it leaves this beside the files and asks the server to stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Staged {
    pub snapshot: String,
    /// The server the snapshot was made on.
    pub hostname: String,
    pub created_at: i64,
    /// Keep the gateway this machine is paired with, rather than the one in the snapshot. That is
    /// what you want when this machine is the replacement for one that died.
    pub keep_gateway: bool,
    pub asked_at: i64,
    /// Who asked, for the log.
    pub by: String,
}

impl Default for Staged {
    fn default() -> Self {
        Staged {
            snapshot: String::new(),
            hostname: String::new(),
            created_at: 0,
            keep_gateway: true,
            asked_at: 0,
            by: String::new(),
        }
    }
}

/// A look at a backup server, before anything is decided.
#[derive(Debug, Clone)]
pub struct Look {
    /// The fingerprint of the backup server's host key, to check against what you expect. Only
    /// SFTP servers have one.
    pub host_key: Option<String>,
    pub encrypted: bool,
    /// Newest first. Empty for an encrypted repository nobody gave the key for.
    pub snapshots: Vec<(String, Manifest)>,
}

/// Where a restore has got to, in this process. It lives in memory on purpose: the whole point of
/// the exercise is that the process ends, and after that the file beside the data speaks for it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Fetching {
    /// `idle`, `fetching`, `ready` or `failed`.
    pub state: String,
    pub snapshot: String,
    pub error: String,
    pub started_at: i64,
    /// How much of the snapshot is here, in bytes, and how much there is.
    pub done_bytes: u64,
    pub total_bytes: u64,
}

// Not derived: the portal hides the restore card while the state is `idle`, and an empty state would
// show it with nothing in it.
impl Default for Fetching {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            snapshot: String::new(),
            error: String::new(),
            started_at: 0,
            done_bytes: 0,
            total_bytes: 0,
        }
    }
}

/// Taking one person's mail out of a snapshot: first the snapshot is opened (its database fetched),
/// then the admin picks a person and folders, then their mail comes back. Lives in memory, like
/// [`Fetching`]; the snapshot's database waits in the data directory until it is closed.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MailboxRestore {
    /// `idle`, `opening`, `open`, `restoring` or `failed`.
    pub state: String,
    pub snapshot: String,
    /// When the snapshot was made.
    pub created_at: i64,
    pub error: String,
    /// How much of the snapshot's database is here while it is opened, and how much there is.
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Everybody in the snapshot, once it is open.
    pub people: Vec<SnapshotPerson>,
    /// Whose mail is coming back right now, and how far it got.
    pub account: String,
    pub total: u64,
    pub done: u64,
    pub restored: u64,
    pub skipped: u64,
    /// How the last restore of a mailbox from this snapshot went.
    pub last: Option<MailboxRestoreDone>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MailboxRestoreDone {
    /// The person in the snapshot, and the account here it went into.
    pub account: String,
    pub into: String,
    pub folder: String,
    pub restored: u64,
    pub skipped: u64,
    /// Empty when it went well.
    pub error: String,
    pub finished_at: i64,
}

struct Inner {
    store: Store,
    hostname: String,
    version: String,
    wakeup: Notify,
    /// Held while anything reads or writes the backup server at length: a backup, or taking a
    /// mailbox out of a snapshot. A backup prunes what no snapshot needs, and must not do that
    /// under a restore that is reading it.
    running: Arc<Mutex<()>>,
    /// Whether the one holding `running` is a backup.
    backing_up: AtomicBool,
    mailbox: std::sync::Mutex<MailboxRestore>,
    /// Where the server keeps its data. Only set when a restore is possible at all -- the command
    /// line and the tests have no use for it.
    data_dir: std::sync::OnceLock<PathBuf>,
    /// Stops the server, so the next start can put the snapshot in place. Set by the server.
    stop: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    fetching: std::sync::Mutex<Fetching>,
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
                running: Arc::new(Mutex::new(())),
                backing_up: AtomicBool::new(false),
                mailbox: std::sync::Mutex::default(),
                data_dir: std::sync::OnceLock::new(),
                stop: std::sync::OnceLock::new(),
                fetching: std::sync::Mutex::default(),
            }),
        }
    }

    /// What is on a backup server this one does not call its own — for the setup assistant, where
    /// there is nothing saved yet and the whole point is to look before deciding.
    ///
    /// Saves nothing and changes nothing. Answers whether the repository is encrypted and, when it
    /// could be opened, the snapshots on it, newest first. An encrypted repository without the
    /// recovery key answers `(true, [])` rather than an error: "there is something here, and you
    /// need the key" is a more useful thing to show than a failure.
    pub async fn look_at(target: &Target, key: Option<&str>) -> Result<Look, Error> {
        let storage = Storage::open(target).await?;
        let mut look =
            Look { host_key: storage.host_key().map(str::to_owned), encrypted: false, snapshots: Vec::new() };
        let encrypted = match Repository::is_encrypted(&storage).await {
            Ok(encrypted) => encrypted,
            Err(err) => {
                storage.close().await;
                return Err(err);
            }
        };
        look.encrypted = encrypted;
        // A key that was given is used whatever the backup server says: a repository that claims
        // to be unencrypted to someone who knows it is not is refused, not read as plain text.
        let key = match (encrypted, key) {
            (true, None) => {
                storage.close().await;
                return Ok(look);
            }
            (_, Some(text)) => match RepoKey::from_recovery_text(text) {
                Ok(key) => Some(key),
                Err(err) => {
                    storage.close().await;
                    return Err(err);
                }
            },
            (false, None) => None,
        };
        let repo = Repository::open_existing(storage, key).await?;
        let found = async {
            let mut found = Vec::new();
            for name in repo.snapshots().await?.into_iter().rev() {
                found.push((name.clone(), repo.manifest(&name).await?));
            }
            Ok::<_, Error>(found)
        }
        .await;
        repo.storage.close().await;
        look.snapshots = found?;
        Ok(look)
    }

    /// Lets this server be restored into. Without it the portal can still show snapshots, but the
    /// button that puts one back is absent -- there would be nowhere to put it.
    pub fn restores_into(&self, data_dir: &Path, stop: Box<dyn Fn() + Send + Sync>) {
        let _ = self.inner.data_dir.set(data_dir.to_owned());
        let _ = self.inner.stop.set(stop);
    }

    pub fn can_restore(&self) -> bool {
        self.inner.data_dir.get().is_some()
    }

    /// Where a restore has got to in this process.
    pub fn fetching(&self) -> Fetching {
        self.inner.fetching.lock().expect("restore progress poisoned").clone()
    }

    /// A snapshot that has already been fetched and is waiting for the next start.
    pub fn staged(&self) -> Option<Staged> {
        let dir = self.inner.data_dir.get()?;
        serde_json::from_str(&std::fs::read_to_string(dir.join(READY_FILE)).ok()?).ok()
    }

    /// Fetches a snapshot into the data directory and asks the server to stop, so the next start
    /// can put it in place. Returns as soon as the fetching has begun.
    ///
    /// `latest` takes the newest snapshot there is.
    pub async fn start_restore(&self, snapshot: &str, keep_gateway: bool, by: &str) -> Result<(), Error> {
        let dir = self
            .inner
            .data_dir
            .get()
            .ok_or_else(|| Error::Config("this server cannot restore into itself".into()))?
            .clone();
        if self.staged().is_some() {
            return Err(Error::Config("a restore is already waiting; restart the server to put it in place".into()));
        }
        if self.is_running() {
            return Err(Error::Busy("a backup is running right now".into()));
        }
        if self.mailbox_busy() {
            return Err(Error::Busy("a mailbox is being restored right now".into()));
        }
        // Claimed here, under the lock and before anything is awaited. Checking first and setting
        // after would let two clicks a moment apart both get through, and the second one empties the
        // staging directory the first is still filling.
        {
            let mut progress = self.inner.fetching.lock().expect("restore progress poisoned");
            if progress.state == "fetching" {
                return Err(Error::Busy("a restore is already being fetched".into()));
            }
            *progress = Fetching {
                state: "fetching".into(),
                snapshot: snapshot.to_owned(),
                started_at: now(),
                ..Fetching::default()
            };
        }
        match self.settings().await {
            Ok(ref settings) if settings.target.is_some() => {}
            other => {
                // The slot goes back: nothing was started.
                *self.inner.fetching.lock().expect("restore progress poisoned") = Fetching::default();
                other?;
                return Err(Error::Config("no backup server is set up".into()));
            }
        }
        // An empty staging directory: `restore` refuses to write into one that already holds a
        // database, and a leftover from an attempt that failed halfway would be exactly that.
        let staging = dir.join(STAGING_DIR);
        let _ = tokio::fs::remove_dir_all(&staging).await;

        let this = self.clone();
        let (snapshot, by) = (snapshot.to_owned(), by.to_owned());
        tokio::spawn(async move { this.fetch_restore(dir, snapshot, keep_gateway, by).await });
        Ok(())
    }

    async fn fetch_restore(self, dir: PathBuf, snapshot: String, keep_gateway: bool, by: String) {
        let staging = dir.join(STAGING_DIR);
        let result = async {
            let mut settings = self.settings().await?;
            let repo = self.open(&mut settings).await?;
            let fetched = async {
                let name = match snapshot.as_str() {
                    "latest" => repo
                        .snapshots()
                        .await?
                        .pop()
                        .ok_or_else(|| Error::Config("there are no snapshots on the backup server".into()))?,
                    name => name.to_owned(),
                };
                let manifest = repo.manifest(&name).await?;
                {
                    let mut progress = self.inner.fetching.lock().expect("restore progress poisoned");
                    progress.snapshot = name.clone();
                    progress.total_bytes = manifest.database_size + manifest.blobs_size;
                }
                let manifest = crate::restore(&repo, &name, &staging).await?;
                Ok::<_, Error>((name, manifest))
            }
            .await;
            repo.storage.close().await;
            fetched
        }
        .await;

        let (name, manifest) = match result {
            Ok(fetched) => fetched,
            Err(err) => {
                // Half a snapshot is worse than none: the next start must not find something it
                // would take for a whole one.
                let _ = tokio::fs::remove_dir_all(&staging).await;
                let mut progress = self.inner.fetching.lock().expect("restore progress poisoned");
                progress.state = "failed".into();
                progress.error = err.to_string();
                tracing::warn!(%err, "fetching the snapshot to restore failed");
                return;
            }
        };

        let staged = Staged {
            snapshot: name.clone(),
            hostname: manifest.hostname.clone(),
            created_at: manifest.created_at,
            keep_gateway,
            asked_at: now(),
            by,
        };
        let raw = serde_json::to_string(&staged).expect("the note serializes");
        if let Err(err) = tokio::fs::write(dir.join(READY_FILE), raw).await {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            let mut progress = self.inner.fetching.lock().expect("restore progress poisoned");
            progress.state = "failed".into();
            progress.error = format!("the snapshot is here but could not be handed over: {err}");
            return;
        }
        {
            let mut progress = self.inner.fetching.lock().expect("restore progress poisoned");
            progress.state = "ready".into();
            progress.done_bytes = progress.total_bytes;
        }
        tracing::warn!(snapshot = %name, from = %manifest.hostname, "the snapshot is here; stopping so it can take over");
        // A moment for the answer to reach the browser before the server goes away under it.
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Some(stop) = self.inner.stop.get() {
            stop();
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
        if settings.minute > 59 {
            return Err(Error::Config("the minute must be between 0 and 59".into()));
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
        self.inner.backing_up.load(Ordering::SeqCst)
    }

    /// Asks the scheduler to back up right away.
    pub fn run_soon(&self) {
        self.inner.wakeup.notify_one();
    }

    /// Opens the repository, remembering an SFTP server's host key the first time.
    async fn open(&self, settings: &mut BackupSettings) -> Result<Repository, Error> {
        let target = settings.target.as_mut().ok_or_else(|| Error::Config("no backup server is set up".into()))?;
        let storage = Storage::open(target).await?;
        if let (Some(sftp), Some(seen)) = (target.as_sftp_mut(), storage.host_key())
            && sftp.host_key.is_none()
        {
            sftp.host_key = Some(seen.to_owned());
            self.save_settings(settings).await?;
        }
        let key = settings.key.as_deref().map(RepoKey::from_recovery_text).transpose()?;
        Repository::open(storage, key, now()).await
    }

    /// The repository the settings name, for the command line. Close its storage when done.
    pub async fn repository(&self) -> Result<Repository, Error> {
        let mut settings = self.settings().await?;
        self.open(&mut settings).await
    }

    /// Backs up now, unless one is running already.
    pub async fn run_now(&self) -> Result<BackupReport, Error> {
        let _running = self.inner.running.try_lock().map_err(|_| {
            Error::Busy(if self.mailbox_busy() {
                "a mailbox is being restored from a snapshot right now".into()
            } else {
                "a backup is running already".into()
            })
        })?;
        let _flag = RunningFlag::raise(&self.inner.backing_up);
        let mut status = self.status().await;
        let started = now();
        status.last_attempt_at = Some(started);
        status.started_at = Some(started);
        status.finished_at = None;
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
        status.finished_at = Some(now());
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

    /// Where taking a mailbox out of a snapshot stands.
    pub fn mailbox_restore(&self) -> MailboxRestore {
        self.inner.mailbox.lock().expect("mailbox restore poisoned").clone()
    }

    /// Whether a snapshot is being opened or a mailbox restored right now.
    pub fn mailbox_busy(&self) -> bool {
        matches!(self.mailbox_restore().state.as_str(), "opening" | "restoring")
    }

    fn set_mailbox(&self, change: impl FnOnce(&mut MailboxRestore)) {
        change(&mut self.inner.mailbox.lock().expect("mailbox restore poisoned"));
    }

    fn mailbox_database(&self) -> PathBuf {
        self.inner.store.data_dir().join(MAILBOX_DIR).join("uwumail.db")
    }

    /// Opens a snapshot to take mailboxes out of it: fetches its database and reads who is in it.
    /// Returns as soon as the fetching has begun; `latest` takes the newest snapshot.
    pub async fn open_snapshot(&self, snapshot: &str) -> Result<(), Error> {
        let running = self.claim_mailbox("opening", |job| {
            *job = MailboxRestore { state: "opening".into(), snapshot: snapshot.to_owned(), ..Default::default() };
        })?;
        let this = self.clone();
        let snapshot = snapshot.to_owned();
        tokio::spawn(async move {
            let opened = this.fetch_snapshot_database(&snapshot).await;
            drop(running);
            match opened {
                Ok((name, manifest, people)) => this.set_mailbox(|job| {
                    job.state = "open".into();
                    job.snapshot = name;
                    job.created_at = manifest.created_at;
                    job.people = people;
                    job.done_bytes = job.total_bytes;
                }),
                Err(err) => {
                    tracing::warn!(%err, "opening a snapshot to restore a mailbox failed");
                    let _ = tokio::fs::remove_dir_all(this.inner.store.data_dir().join(MAILBOX_DIR)).await;
                    this.set_mailbox(|job| {
                        job.state = "failed".into();
                        job.error = err.to_string();
                        job.people.clear();
                    });
                }
            }
        });
        Ok(())
    }

    /// Takes the slot for a job on a snapshot: the backup server to itself, and the state moved on.
    fn claim_mailbox(
        &self,
        state: &str,
        start: impl FnOnce(&mut MailboxRestore),
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, Error> {
        if self.staged().is_some() || self.fetching().state == "fetching" {
            return Err(Error::Busy("the whole server is being restored".into()));
        }
        let mut job = self.inner.mailbox.lock().expect("mailbox restore poisoned");
        if matches!(job.state.as_str(), "opening" | "restoring") {
            return Err(Error::Busy("a mailbox is being restored already".into()));
        }
        if state == "restoring" && job.state != "open" {
            return Err(Error::Config("open a snapshot first".into()));
        }
        let running = self
            .inner
            .running
            .clone()
            .try_lock_owned()
            .map_err(|_| Error::Busy("a backup is running right now".into()))?;
        start(&mut job);
        Ok(running)
    }

    async fn fetch_snapshot_database(&self, snapshot: &str) -> Result<(String, Manifest, Vec<SnapshotPerson>), Error> {
        let mut settings = self.settings().await?;
        let repo = self.open(&mut settings).await?;
        let path = self.mailbox_database();
        let fetched = async {
            let name = match snapshot {
                "latest" => repo
                    .snapshots()
                    .await?
                    .pop()
                    .ok_or_else(|| Error::Config("there are no snapshots on the backup server".into()))?,
                name => name.to_owned(),
            };
            let manifest = repo.manifest(&name).await?;
            self.set_mailbox(|job| {
                job.snapshot = name.clone();
                job.created_at = manifest.created_at;
                job.total_bytes = manifest.database_size;
            });
            let this = self.clone();
            let manifest = crate::mailbox::fetch_database(&repo, &name, &path, &mut move |bytes| {
                this.set_mailbox(|job| job.done_bytes = bytes)
            })
            .await?;
            Ok::<_, Error>((name, manifest))
        }
        .await;
        repo.storage.close().await;
        let (name, manifest) = fetched?;
        let people = crate::mailbox::people(&path).await?;
        Ok((name, manifest, people))
    }

    /// Puts one person's mail from the open snapshot back, into the account with the same login
    /// here -- or into `into`, for a person who has another address by now. Only the chosen folders
    /// of the snapshot when `folders` names them. Returns as soon as it has begun.
    pub async fn start_mailbox_restore(
        &self,
        login: &str,
        into: Option<&str>,
        folders: Option<Vec<i64>>,
        by: &str,
    ) -> Result<(), Error> {
        let target = into.map(str::trim).filter(|into| !into.is_empty()).unwrap_or(login);
        let account = self
            .inner
            .store
            .account(target)
            .await?
            .filter(|account| account.deleted_at.is_none() && account.has_mailbox())
            .ok_or_else(|| {
                Error::Config(format!("there is no mailbox {target} on this server to put the mail into"))
            })?;
        {
            let job = self.mailbox_restore();
            if job.state == "open" && !job.people.iter().any(|person| person.login == login) {
                return Err(Error::Config(format!("{login} is not in this snapshot")));
            }
        }
        let running = self.claim_mailbox("restoring", |job| {
            job.state = "restoring".into();
            job.account = login.to_owned();
            job.error.clear();
            (job.total, job.done, job.restored, job.skipped) = (0, 0, 0, 0);
        })?;
        let this = self.clone();
        let (login, into, by) = (login.to_owned(), account.login.clone(), by.to_owned());
        tokio::spawn(async move {
            let created_at = this.mailbox_restore().created_at;
            let result = this.restore_into(&login, account.id, created_at, folders).await;
            drop(running);
            let done = match result {
                Ok(report) => {
                    tracing::info!(%login, %into, restored = report.restored, skipped = report.skipped, %by, "a mailbox was restored from a snapshot");
                    MailboxRestoreDone {
                        account: login,
                        into,
                        folder: report.folder,
                        restored: report.restored,
                        skipped: report.skipped,
                        error: String::new(),
                        finished_at: now(),
                    }
                }
                Err(err) => {
                    tracing::warn!(%login, %err, "restoring a mailbox from a snapshot failed");
                    let job = this.mailbox_restore();
                    MailboxRestoreDone {
                        account: login,
                        into,
                        folder: crate::mailbox::restored_folder_name(created_at),
                        restored: job.restored,
                        skipped: job.skipped,
                        error: err.to_string(),
                        finished_at: now(),
                    }
                }
            };
            this.set_mailbox(|job| {
                job.state = "open".into();
                job.last = Some(done);
            });
        });
        Ok(())
    }

    async fn restore_into(
        &self,
        login: &str,
        account_id: i64,
        created_at: i64,
        folders: Option<Vec<i64>>,
    ) -> Result<MailboxRestoreReport, Error> {
        let mut settings = self.settings().await?;
        let repo = self.open(&mut settings).await?;
        let this = self.clone();
        let mut progress = move |state: MailboxProgress| {
            this.set_mailbox(|job| {
                (job.total, job.done, job.restored, job.skipped) =
                    (state.total, state.done, state.restored, state.skipped);
            })
        };
        let result = crate::mailbox::restore_mailbox(
            &self.inner.store,
            &repo,
            &self.mailbox_database(),
            created_at,
            login,
            account_id,
            folders,
            &mut progress,
        )
        .await;
        repo.storage.close().await;
        result
    }

    /// Closes the open snapshot and removes its database.
    pub async fn close_snapshot(&self) -> Result<(), Error> {
        {
            let mut job = self.inner.mailbox.lock().expect("mailbox restore poisoned");
            if matches!(job.state.as_str(), "opening" | "restoring") {
                return Err(Error::Busy("wait until the mailbox is back".into()));
            }
            *job = MailboxRestore::default();
        }
        let _ = tokio::fs::remove_dir_all(self.inner.store.data_dir().join(MAILBOX_DIR)).await;
        Ok(())
    }

    /// Puts one person's mail from a snapshot back, from start to end, for the command line.
    /// `folders` are paths like `Inbox` or `Projects/2025`, each with the folders inside it.
    pub async fn restore_mailbox_now(
        &self,
        snapshot: &str,
        login: &str,
        into: Option<&str>,
        folders: &[String],
        progress: &mut (dyn FnMut(&str, MailboxProgress) + Send),
    ) -> Result<MailboxRestoreReport, Error> {
        let target = into.unwrap_or(login);
        let account = self
            .inner
            .store
            .account(target)
            .await?
            .filter(|account| account.deleted_at.is_none() && account.has_mailbox())
            .ok_or_else(|| {
                Error::Config(format!("there is no mailbox {target} on this server to put the mail into"))
            })?;
        let _running = self
            .inner
            .running
            .clone()
            .try_lock_owned()
            .map_err(|_| Error::Busy("a backup is running right now".into()))?;
        let dir = self.inner.store.data_dir().join(crate::TEMP_DIR).join("restore-mailbox-cli");
        let path = dir.join("uwumail.db");
        let result = async {
            let mut settings = self.settings().await?;
            let repo = self.open(&mut settings).await?;
            let restored = async {
                let name = match snapshot {
                    "latest" => repo
                        .snapshots()
                        .await?
                        .pop()
                        .ok_or_else(|| Error::Config("there are no snapshots on the backup server".into()))?,
                    name => name.to_owned(),
                };
                progress(&format!("fetching the database of snapshot {name}"), MailboxProgress::default());
                let manifest = crate::mailbox::fetch_database(&repo, &name, &path, &mut |_| {}).await?;
                let people = crate::mailbox::people(&path).await?;
                let person = people
                    .iter()
                    .find(|person| person.login.eq_ignore_ascii_case(login))
                    .ok_or_else(|| Error::Config(format!("{login} is not in snapshot {name}")))?;
                let chosen =
                    if folders.is_empty() { None } else { Some(crate::mailbox::folders_named(person, folders)?) };
                let person_login = person.login.clone();
                crate::mailbox::restore_mailbox(
                    &self.inner.store,
                    &repo,
                    &path,
                    manifest.created_at,
                    &person_login,
                    account.id,
                    chosen,
                    &mut |state| progress("", state),
                )
                .await
            }
            .await;
            repo.storage.close().await;
            restored
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result
    }

    /// Whether the daily run is due.
    fn due(settings: &BackupSettings, status: &BackupStatus, now: i64) -> bool {
        if !settings.enabled || settings.target.is_none() {
            return false;
        }
        if now.rem_euclid(86_400) / 60 < minute_of_day(settings) {
            return false;
        }
        let tried_recently = status.last_attempt_at.is_some_and(|at| now - at < RETRY_SECS);
        !succeeded_today(status, now) && !tried_recently
    }

    /// Whether a backup is too close for an update to start: while one runs, and `margin_secs`
    /// before and after it.
    ///
    /// Two things this does not know, so whoever updates has to ask them separately: a backup can
    /// always be started by hand a second later, so check [`Backups::is_running`] right before
    /// starting, and an update that makes its own backup first has to ask again afterwards,
    /// because by then the window has moved.
    pub fn blocks(settings: &BackupSettings, status: &BackupStatus, now: i64, margin_secs: i64) -> bool {
        if running_at(status, now) {
            return true;
        }
        if status.finished_at.is_some_and(|at| (0..margin_secs).contains(&(now - at))) {
            return true;
        }
        if !settings.enabled || settings.target.is_none() {
            return false;
        }
        if (0..=margin_secs).contains(&(next_run_at(settings, now) - now)) {
            return true;
        }
        // A run that failed tries again every hour until one works, and the hour it does is as bad
        // a moment as the planned one. While they keep failing there is no quiet gap left, so an
        // update waits until a backup works or the backups are switched off — the portal says so
        // rather than leaving it a mystery.
        Self::due(settings, status, now) || Self::due(settings, status, now + margin_secs)
    }

    /// Runs backups when they are due or asked for, until shutdown.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        // A snapshot opened before a restart is not open any more; its database goes.
        let _ = tokio::fs::remove_dir_all(self.inner.store.data_dir().join(MAILBOX_DIR)).await;
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

/// Says "a backup is running" for as long as it lives.
struct RunningFlag<'a>(&'a AtomicBool);

impl<'a> RunningFlag<'a> {
    fn raise(flag: &'a AtomicBool) -> RunningFlag<'a> {
        flag.store(true, Ordering::SeqCst);
        RunningFlag(flag)
    }
}

impl Drop for RunningFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The minute of the day the daily backup starts at.
fn minute_of_day(settings: &BackupSettings) -> i64 {
    i64::from(settings.hour) * 60 + i64::from(settings.minute)
}

/// When the daily backup starts next: today's time while it is still to come, else tomorrow's.
fn next_run_at(settings: &BackupSettings, now: i64) -> i64 {
    let at = now.div_euclid(86_400) * 86_400 + minute_of_day(settings) * 60;
    if at >= now { at } else { at + 86_400 }
}

/// Whether a run began and has not finished since. See [`RUN_MAX_SECS`] for the one that was killed.
fn running_at(status: &BackupStatus, now: i64) -> bool {
    match (status.started_at, status.finished_at) {
        (Some(started), finished) if finished.is_none_or(|at| at < started) => now - started < RUN_MAX_SECS,
        _ => false,
    }
}

fn succeeded_today(status: &BackupStatus, now: i64) -> bool {
    status.last_success_at.is_some_and(|at| at.div_euclid(86_400) == now.div_euclid(86_400))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Login, SftpTarget};

    fn a_backup_server() -> Target {
        Target::Sftp(SftpTarget {
            host: "nas.example.org".into(),
            port: 22,
            user: "backup".into(),
            path: "uwumail".into(),
            login: Login::Password { password: "geheim".into() },
            host_key: None,
        })
    }

    #[test]
    fn no_restore_under_way_reads_as_idle() {
        // The portal shows its restore card for anything but `idle`.
        let json = serde_json::to_value(Fetching::default()).unwrap();
        assert_eq!(json["state"], "idle");
    }

    #[test]
    fn the_daily_backup_waits_for_its_hour_and_retries_hourly() {
        let target = a_backup_server();
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

    #[test]
    fn the_daily_backup_waits_for_the_minute_too() {
        let settings = BackupSettings {
            enabled: true,
            target: Some(a_backup_server()),
            hour: 3,
            minute: 45,
            ..Default::default()
        };
        let day = 20_000 * 86_400;
        let fresh = BackupStatus::default();
        assert!(!Backups::due(&settings, &fresh, day + 3 * 3600 + 44 * 60), "a minute early");
        assert!(Backups::due(&settings, &fresh, day + 3 * 3600 + 45 * 60));
    }

    #[test]
    fn an_update_keeps_away_from_the_backup_window() {
        let half_an_hour = 1800;
        let settings =
            BackupSettings { enabled: true, target: Some(a_backup_server()), hour: 3, minute: 0, ..Default::default() };
        let day = 20_000 * 86_400;
        let fresh = BackupStatus::default();

        assert!(!Backups::blocks(&settings, &fresh, day + 3600, half_an_hour), "two hours before");
        assert!(Backups::blocks(&settings, &fresh, day + 2 * 3600 + 40 * 60, half_an_hour), "twenty minutes before");
        assert!(Backups::blocks(&settings, &fresh, day + 3 * 3600, half_an_hour), "on the dot");

        let running =
            BackupStatus { started_at: Some(day + 3 * 3600), last_attempt_at: Some(day + 3 * 3600), ..fresh.clone() };
        assert!(Backups::blocks(&settings, &running, day + 3 * 3600 + 600, half_an_hour), "while it runs");

        let done = BackupStatus {
            started_at: Some(day + 3 * 3600),
            finished_at: Some(day + 3 * 3600 + 300),
            last_success_at: Some(day + 3 * 3600 + 300),
            last_attempt_at: Some(day + 3 * 3600),
            ..fresh.clone()
        };
        assert!(Backups::blocks(&settings, &done, day + 3 * 3600 + 900, half_an_hour), "ten minutes after");
        assert!(!Backups::blocks(&settings, &done, day + 5 * 3600, half_an_hour), "two hours after");
        assert!(!Backups::blocks(&settings, &done, day + 86_400 + 3600, half_an_hour), "and after midnight");

        // A run that failed leaves no quiet gap: the half hour after it runs into the half hour
        // before its hourly retry.
        let failed = BackupStatus {
            started_at: Some(day + 3 * 3600),
            finished_at: Some(day + 3 * 3600 + 60),
            last_attempt_at: Some(day + 3 * 3600),
            last_error: Some("the backup server said no".into()),
            ..fresh.clone()
        };
        assert!(Backups::blocks(&settings, &failed, day + 3 * 3600 + 20 * 60, half_an_hour), "just after it failed");
        assert!(Backups::blocks(&settings, &failed, day + 3 * 3600 + 40 * 60, half_an_hour), "before the retry");
        assert!(Backups::blocks(&settings, &failed, day + 4 * 3600, half_an_hour), "as the retry is due");

        let off = BackupSettings { enabled: false, ..settings.clone() };
        assert!(!Backups::blocks(&off, &fresh, day + 3 * 3600, half_an_hour), "nothing is planned");
        assert!(Backups::blocks(&off, &running, day + 3 * 3600 + 600, half_an_hour), "but a run by hand still counts");
        let killed = BackupStatus { started_at: Some(day + 3 * 3600), ..fresh.clone() };
        assert!(!Backups::blocks(&off, &killed, day + 12 * 3600, half_an_hour), "a run a restart cut short lets go");
    }
}
