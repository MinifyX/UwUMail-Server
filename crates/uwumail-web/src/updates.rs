//! Knowing when there is something newer: once a day the server asks GitHub for the releases of its
//! channel (stable or beta), or, for an `edge` build of `main`, which commits came since. Updating
//! stays a command the admin runs; the portal shows it with the changes.

use std::cmp::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::Web;
use crate::health::unix_now;

const SETTINGS_KEY: &str = "updates.settings";
const INFO_KEY: &str = "updates.info";
const STATUS_KEY: &str = "updates.status";
const CHECK_INTERVAL: i64 = 24 * 3600;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
/// How far a backup has to be from an update that starts by itself: half an hour before it, all the
/// while it runs, and half an hour after.
pub const BACKUP_MARGIN_SECS: i64 = 30 * 60;
/// A scheduled update that could not start at its minute -- because a backup was too close -- keeps
/// trying for this long, then lets the day go and waits for the next one.
const CATCH_UP_SECS: i64 = 6 * 3600;
/// An update this old with no word from the helper did not happen. Pulling an image and waiting for
/// the health check takes a minute or two; this leaves room for a slow line and then gives up.
const NO_ANSWER_SECS: i64 = 45 * 60;

/// What this binary is: its version, and for CI builds the commit and whether it is a release.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Build {
    pub version: &'static str,
    pub commit: Option<&'static str>,
    /// Built from a `v…` tag; otherwise an `edge` build of `main`, or one made locally.
    pub release: bool,
}

pub fn build() -> Build {
    Build {
        version: env!("CARGO_PKG_VERSION"),
        commit: option_env!("UWUMAIL_GIT_SHA").filter(|sha| !sha.is_empty()),
        release: option_env!("UWUMAIL_RELEASE").is_some_and(|tag| !tag.is_empty()),
    }
}

/// `owner/name` of the GitHub repository this server comes from.
fn repository() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/").trim_end_matches('/')
}

pub fn image() -> String {
    format!("ghcr.io/{}", repository().to_lowercase())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateSettings {
    pub check: bool,
    pub channel: Channel,
    /// Install new stable releases by itself, at the time below.
    pub auto: bool,
    /// The weekday (0 = Monday) the scheduled update runs on; `None` means every day.
    pub weekday: Option<u8>,
    /// The hour and minute, in UTC, like the backup settings. The browser shows local time.
    pub hour: u8,
    pub minute: u8,
    /// Back up before updating. Switching this off is a deliberate act, for a server that has no
    /// backup target -- the portal makes you say so.
    pub backup_first: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        UpdateSettings {
            check: true,
            channel: Channel::Stable,
            auto: false,
            weekday: None,
            // Four in the morning UTC: clear of the backup, which starts at one by default.
            hour: 4,
            minute: 0,
            backup_first: true,
        }
    }
}

/// Where an update is. It lives in the database, because the update replaces the container that
/// started it: the process that comes back is a different one and has to be able to find out how
/// the thing it never saw the end of turned out.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdateState {
    #[default]
    Idle,
    /// Making the backup that comes first.
    Backup,
    /// The helper is pulling and recreating.
    Running,
    Done,
    Failed,
    /// The new version did not answer, so the helper put the old tag back.
    RolledBack,
}

/// How the backup before the update went.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BackupBefore {
    /// Asked for without one, on purpose.
    Skipped,
    Done,
    Failed,
}

/// The update going on, or the last one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateStatus {
    pub state: UpdateState,
    /// `hand` or `schedule`.
    pub by: String,
    /// The version that was running when this started.
    pub from: String,
    /// The version it is going to; `None` for an edge build, which only follows its tag.
    pub to: Option<String>,
    /// The helper's job, to find out across a restart how it ended.
    pub job: Option<String>,
    pub backup: Option<BackupBefore>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
    /// The planned minute that was last acted on, so one failure is not tried again every minute.
    pub handled: Option<i64>,
}

impl UpdateStatus {
    /// Whether something is going on right now.
    pub fn busy(&self) -> bool {
        matches!(self.state, UpdateState::Backup | UpdateState::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub name: String,
    pub notes: String,
    pub published_at: String,
    pub url: String,
    pub prerelease: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub sha: String,
    pub message: String,
}

/// The result of the last check.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateInfo {
    pub checked_at: Option<i64>,
    pub error: Option<String>,
    /// Newer releases of the channel, newest first.
    pub releases: Vec<Release>,
    /// For an edge build: how many commits `main` is ahead, and the newest of them.
    pub behind: Option<u32>,
    pub commits: Vec<Commit>,
    /// The newest stable release there is, whatever this server follows. Only the gateway needs
    /// this: it is built for releases and nothing else, so even an `edge` server updates its
    /// gateway to a release.
    pub newest_release: Option<String>,
}

/// `1.2.3-beta.4` as comparable parts; a pre-release sorts before its release.
fn version_key(version: &str) -> Option<(Vec<u64>, Option<Vec<String>>)> {
    let version = version.trim().trim_start_matches('v');
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre.split('.').map(str::to_owned).collect())),
        None => (version, None),
    };
    let numbers = core.split('.').map(|part| part.parse().ok()).collect::<Option<Vec<u64>>>()?;
    Some((numbers, pre))
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    let (Some((a_core, a_pre)), Some((b_core, b_pre))) = (version_key(a), version_key(b)) else {
        return Ordering::Equal;
    };
    a_core.cmp(&b_core).then_with(|| match (a_pre, b_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => {
            for (x, y) in a.iter().zip(&b) {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    _ => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            a.len().cmp(&b.len())
        }
    })
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
    html_url: String,
}

/// Releases newer than `current` on the channel, newest first.
fn newer_releases(list: Vec<GitHubRelease>, current: &str, channel: Channel) -> Vec<Release> {
    let mut newer: Vec<Release> = list
        .into_iter()
        .filter(|release| !release.draft && (channel == Channel::Beta || !release.prerelease))
        .filter(|release| compare_versions(&release.tag_name, current) == Ordering::Greater)
        .map(|release| Release {
            version: release.tag_name.trim_start_matches('v').to_owned(),
            name: release.name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| release.tag_name.clone()),
            notes: release.body.unwrap_or_default(),
            published_at: release.published_at.unwrap_or_default(),
            url: release.html_url,
            prerelease: release.prerelease,
        })
        .collect();
    newer.sort_by(|a, b| compare_versions(&b.version, &a.version));
    newer
}

/// The releases GitHub lists, newest first as it sends them.
async fn releases(https: &uwumail_smtp::https::Https, timeout: Duration) -> Result<Vec<GitHubRelease>, String> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page=30", repository());
    let fetched = https.get(&url, MAX_RESPONSE, timeout).await?;
    serde_json::from_str(&fetched.body).map_err(|_| "GitHub answered something unexpected".to_owned())
}

/// The newest release that is neither a draft nor a beta.
fn newest_stable(list: &[GitHubRelease]) -> Option<String> {
    let mut stable: Vec<&str> = list
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .map(|release| release.tag_name.trim_start_matches('v'))
        .collect();
    stable.sort_by(|a, b| compare_versions(b, a));
    stable.first().map(|version| (*version).to_owned())
}

#[derive(Deserialize)]
struct Comparison {
    ahead_by: u32,
    commits: Vec<ComparedCommit>,
}

#[derive(Deserialize)]
struct ComparedCommit {
    sha: String,
    commit: CommitMessage,
}

#[derive(Deserialize)]
struct CommitMessage {
    message: String,
}

/// 0 = Monday. Unix day 0 was a Thursday, which is where the three comes from.
fn weekday_of(at: i64) -> u8 {
    (at.div_euclid(86_400) + 3).rem_euclid(7) as u8
}

/// The last moment the schedule came round at or before `now`, or `None` when nothing is planned.
///
/// Times are UTC, like the backup's, so a clock that changes twice a year never moves an update
/// into a backup it was carefully placed away from.
fn planned_before(settings: &UpdateSettings, now: i64) -> Option<i64> {
    if !settings.auto || settings.hour > 23 || settings.minute > 59 {
        return None;
    }
    let minute_of_day = i64::from(settings.hour) * 60 + i64::from(settings.minute);
    let mut at = now.div_euclid(86_400) * 86_400 + minute_of_day * 60;
    if at > now {
        at -= 86_400;
    }
    match settings.weekday {
        None => Some(at),
        Some(day) if day <= 6 => {
            // At most a week back: one of the seven is the right one.
            for _ in 0..7 {
                if weekday_of(at) == day {
                    return Some(at);
                }
                at -= 86_400;
            }
            None
        }
        Some(_) => None,
    }
}

/// Whether a scheduled update is waiting to be carried out.
fn scheduled_due(settings: &UpdateSettings, status: &UpdateStatus, now: i64) -> Option<i64> {
    let planned = planned_before(settings, now)?;
    if status.handled.is_some_and(|at| at >= planned) {
        return None;
    }
    // A server that was off over its time does not update hours later out of the blue; it waits for
    // the next one. Inside the window it keeps trying, which is what lets it wait out a backup.
    (now - planned <= CATCH_UP_SECS).then_some(planned)
}

impl Web {
    pub async fn update_settings(&self) -> UpdateSettings {
        match self.store().setting(SETTINGS_KEY).await {
            Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
            _ => UpdateSettings::default(),
        }
    }

    pub(crate) async fn save_update_settings(&self, settings: &UpdateSettings) -> uwumail_store::Result<()> {
        self.store().set_setting(SETTINGS_KEY, &serde_json::to_string(settings).expect("settings serialize")).await
    }

    pub async fn update_info(&self) -> UpdateInfo {
        match self.store().setting(INFO_KEY).await {
            Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
            _ => UpdateInfo::default(),
        }
    }

    pub async fn update_status(&self) -> UpdateStatus {
        match self.store().setting(STATUS_KEY).await {
            Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
            _ => UpdateStatus::default(),
        }
    }

    async fn save_update_status(&self, status: &UpdateStatus) {
        let raw = serde_json::to_string(status).expect("status serializes");
        if let Err(err) = self.store().set_setting(STATUS_KEY, &raw).await {
            tracing::warn!(%err, "saving the update status failed");
        }
    }

    /// Which version an update goes to. `None` is an edge build, which has no version to name and
    /// simply follows its tag.
    ///
    /// Anything else has to be a release the last check actually saw. The helper checks the shape of
    /// what it is handed as well, but a version the server never heard of should not get that far:
    /// the shortest way to make a mailserver install something else is to let it pass a string on.
    async fn update_target(&self, wanted: Option<&str>, stable_only: bool) -> Result<Option<String>, String> {
        if !build().release {
            return Ok(None);
        }
        let info = self.update_info().await;
        let mut releases = info.releases.iter().filter(|release| !stable_only || !release.prerelease);
        match wanted {
            Some(version) => releases
                .find(|release| release.version == version)
                .map(|release| Some(release.version.clone()))
                .ok_or_else(|| format!("{version} is not one of the versions the last check found")),
            None => releases
                .next()
                .map(|release| Some(release.version.clone()))
                .ok_or_else(|| "there is nothing newer to install".to_owned()),
        }
    }

    /// Whether a backup is too close for an update to start by itself. Told apart from "a backup is
    /// running", which stops an update by hand too.
    pub(crate) async fn update_near_backup(&self, now: i64) -> bool {
        let Some(backups) = self.backups() else { return false };
        let Ok(settings) = backups.settings().await else { return false };
        uwumail_backup::Backups::blocks(&settings, &backups.status().await, now, BACKUP_MARGIN_SECS)
    }

    /// Starts an update and returns right away: the pulling and recreating happens in the
    /// background, because this very request is about to be cut off by it.
    ///
    /// `backup` false is the deliberate way out for a server with nowhere to back up to.
    pub(crate) async fn start_update(
        &self,
        wanted: Option<&str>,
        backup: bool,
        by: &str,
    ) -> Result<UpdateStatus, String> {
        let host = self
            .host()
            .cloned()
            .ok_or_else(|| "this machine has no helper installed, so the portal cannot update it".to_owned())?;
        let before = self.update_status().await;
        if before.busy() {
            return Err("an update is going on already".into());
        }
        let to = self.update_target(wanted, by == "schedule").await?;
        if backup {
            let backups = self.backups().ok_or_else(|| "this server has no backups set up".to_owned())?;
            let settings = backups.settings().await.map_err(|err| err.to_string())?;
            if settings.target.is_none() {
                return Err("no backup server is set up, so there is nothing to back up to".into());
            }
            if backups.is_running() {
                return Err("a backup is running right now".into());
            }
        }
        let status = UpdateStatus {
            state: if backup { UpdateState::Backup } else { UpdateState::Running },
            by: by.to_owned(),
            from: build().version.to_owned(),
            to: to.clone(),
            job: None,
            backup: (!backup).then_some(BackupBefore::Skipped),
            started_at: Some(unix_now()),
            finished_at: None,
            error: None,
            handled: before.handled,
        };
        self.save_update_status(&status).await;
        self.inner.updating.store(true, std::sync::atomic::Ordering::SeqCst);
        let web = self.clone();
        tokio::spawn(async move { web.carry_out_update(host, to, backup).await });
        Ok(status)
    }

    /// Puts the last result away. The minute the schedule last acted on stays: forgetting a result
    /// is not a reason to try that minute again.
    pub(crate) async fn forget_update(&self) -> Result<(), String> {
        let status = self.update_status().await;
        if status.busy() {
            return Err("this update is still going on".into());
        }
        self.save_update_status(&UpdateStatus { handled: status.handled, ..UpdateStatus::default() }).await;
        Ok(())
    }

    /// The backup, then the word to the helper. After that this process is on borrowed time.
    async fn carry_out_update(
        self,
        host: std::sync::Arc<dyn crate::host::HostBackend>,
        to: Option<String>,
        backup: bool,
    ) {
        if backup {
            let made = match self.backups() {
                Some(backups) => backups.run_now().await.map(|_| ()).map_err(|err| err.to_string()),
                None => Err("this server has no backups set up".to_owned()),
            };
            if let Err(err) = made {
                // Nothing was touched, which is the whole point of backing up first.
                let error = format!("the backup failed, so nothing was updated: {err}");
                self.finish_update(UpdateState::Failed, Some(error), Some(BackupBefore::Failed)).await;
                return;
            }
            let mut status = self.update_status().await;
            status.backup = Some(BackupBefore::Done);
            status.state = UpdateState::Running;
            self.save_update_status(&status).await;
        }
        match host.ask("server-update", to.as_deref()).await {
            Ok(id) => {
                let mut status = self.update_status().await;
                status.job = Some(id);
                status.state = UpdateState::Running;
                self.save_update_status(&status).await;
                tracing::info!(to = ?to, "the machine's helper is updating this server");
            }
            Err(err) => self.finish_update(UpdateState::Failed, Some(err), None).await,
        }
    }

    async fn finish_update(&self, state: UpdateState, error: Option<String>, backup: Option<BackupBefore>) {
        let mut status = self.update_status().await;
        status.state = state;
        status.finished_at = Some(unix_now());
        if error.is_some() {
            status.error = error;
        }
        if backup.is_some() {
            status.backup = backup;
        }
        self.inner.updating.store(false, std::sync::atomic::Ordering::SeqCst);
        self.save_update_status(&status).await;
    }

    /// Finds out how an update ended that this process did not see the end of.
    ///
    /// This is the piece the whole arrangement is for: the update replaces the container, so the
    /// process that comes back has never heard of the request. It reads the helper's answer out of
    /// the shared directory, and failing that compares the version it is with the one that asked.
    pub(crate) async fn reconcile_update(&self) {
        let status = self.update_status().await;
        if !status.busy() {
            return;
        }
        let ours = self.inner.updating.load(std::sync::atomic::Ordering::SeqCst);
        let now = unix_now();
        let started = status.started_at.unwrap_or(0);
        if status.state == UpdateState::Backup {
            if !ours {
                let error = "the server restarted while the backup was running, so nothing was updated";
                self.finish_update(UpdateState::Failed, Some(error.into()), Some(BackupBefore::Failed)).await;
            }
            return;
        }
        let job = status.job.as_deref().and_then(|id| self.host().and_then(|host| host.job(id)));
        let ended = match job.as_ref().map(|job| job.state.as_str()) {
            Some("done") => Some((UpdateState::Done, None)),
            Some("failed") => Some((UpdateState::Failed, Some(job_error(job.as_ref())))),
            Some("rolledBack") => Some((UpdateState::RolledBack, Some(job_error(job.as_ref())))),
            _ => None,
        };
        if let Some((state, error)) = ended {
            tracing::info!(?state, "the update the helper carried out has an answer");
            self.finish_update(state, error, None).await;
            return;
        }
        // No answer in the shared directory, but a different binary is reading this than the one
        // that asked: it plainly worked.
        if !ours && !status.from.is_empty() && status.from != build().version {
            self.finish_update(UpdateState::Done, None, None).await;
            return;
        }
        if now - started > NO_ANSWER_SECS {
            let error = "the helper never said how the update went";
            self.finish_update(UpdateState::Failed, Some(error.into()), None).await;
        }
    }

    /// Asks GitHub what is newer than this build and remembers the answer.
    pub(crate) async fn check_updates(&self) -> UpdateInfo {
        let settings = self.update_settings().await;
        let build = build();
        let https = uwumail_smtp::https::Https::new();
        let mut info = UpdateInfo { checked_at: Some(unix_now()), ..Default::default() };
        let timeout = Duration::from_secs(20);
        let result: Result<(), String> = async {
            match (build.release, build.commit) {
                (false, Some(commit)) => {
                    let url = format!("https://api.github.com/repos/{}/compare/{commit}...main", repository());
                    let fetched = https.get(&url, MAX_RESPONSE, timeout).await?;
                    let comparison: Comparison = serde_json::from_str(&fetched.body)
                        .map_err(|_| "GitHub answered something unexpected".to_owned())?;
                    info.behind = Some(comparison.ahead_by);
                    info.commits = comparison
                        .commits
                        .into_iter()
                        .rev()
                        .take(30)
                        .map(|commit| Commit {
                            sha: commit.sha.chars().take(7).collect(),
                            message: commit.commit.message.lines().next().unwrap_or_default().to_owned(),
                        })
                        .collect();
                    // One more request, for the gateway alone: it is built for releases and
                    // nothing else, so even a server following main updates its gateway to one.
                    if let Ok(list) = releases(&https, timeout).await {
                        info.newest_release = newest_stable(&list);
                    }
                }
                _ => {
                    let list = releases(&https, timeout).await?;
                    info.newest_release = newest_stable(&list);
                    info.releases = newer_releases(list, build.version, settings.channel);
                }
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            tracing::info!(%err, "checking for updates failed");
            info.error = Some(err);
        }
        let raw = serde_json::to_string(&info).expect("info serializes");
        if let Err(err) = self.store().set_setting(INFO_KEY, &raw).await {
            tracing::warn!(%err, "saving the update check failed");
        }
        info
    }

    /// Looks for updates once a day, carries out a scheduled one when its minute comes, and keeps
    /// an eye on one that is under way.
    pub async fn run_updates(self, mut shutdown: watch::Receiver<bool>) {
        // Before anything else: an update may have replaced the container this is starting in, and
        // the status in the database is still waiting to be told how that went.
        self.reconcile_update().await;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = shutdown.changed() => return,
            }
            self.reconcile_update().await;
            let settings = self.update_settings().await;
            if settings.check {
                let last = self.update_info().await.checked_at.unwrap_or(0);
                if unix_now() - last >= CHECK_INTERVAL {
                    self.check_updates().await;
                }
            }
            self.scheduled_update(&settings).await;
        }
    }

    /// One minute of the schedule: is one due, may it start now, and off it goes.
    async fn scheduled_update(&self, settings: &UpdateSettings) {
        let now = unix_now();
        let status = self.update_status().await;
        if status.busy() {
            return;
        }
        let Some(planned) = scheduled_due(settings, &status, now) else { return };
        // The rule that decides when: never in the half hour around a backup, nor while one runs.
        // Inside the catch-up window it simply waits and asks again next minute.
        if self.update_near_backup(now).await {
            tracing::debug!("a scheduled update is waiting for the backup window to pass");
            return;
        }
        // Written down before starting, not after: the update takes the process with it, and a
        // minute that was acted on has to stay acted on either way.
        let mut marked = status;
        marked.handled = Some(planned);
        self.save_update_status(&marked).await;
        match self.start_update(None, settings.backup_first, "schedule").await {
            Ok(_) => tracing::info!("a scheduled update started"),
            // "nothing newer to install" is the usual answer here and no reason for a line a day.
            Err(err) => tracing::debug!(%err, "a scheduled update did not start"),
        }
    }
}

/// What to show for a job that did not end well.
fn job_error(job: Option<&crate::host::HostJob>) -> String {
    match job.map(|job| job.error.trim()).filter(|error| !error.is_empty()) {
        Some(error) => error.to_owned(),
        None => "the update did not work".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.into(),
            name: None,
            body: Some(format!("Notes for {tag}")),
            draft: false,
            prerelease,
            published_at: Some("2026-10-01T10:00:00Z".into()),
            html_url: format!("https://github.com/example/releases/{tag}"),
        }
    }

    #[test]
    fn versions_compare_like_semver() {
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_versions("v0.2.0", "0.1.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.2.0-beta.1", "0.2.0"), Ordering::Less);
        assert_eq!(compare_versions("0.2.0-beta.10", "0.2.0-beta.2"), Ordering::Greater);
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Ordering::Greater);
    }

    #[test]
    fn channels_see_their_releases() {
        let list = || vec![release("v0.1.0", false), release("v0.2.0-beta.1", true), release("v0.1.1", false)];
        let stable = newer_releases(list(), "0.1.0", Channel::Stable);
        assert_eq!(stable.iter().map(|release| release.version.as_str()).collect::<Vec<_>>(), ["0.1.1"]);
        let beta = newer_releases(list(), "0.1.0", Channel::Beta);
        assert_eq!(beta.iter().map(|release| release.version.as_str()).collect::<Vec<_>>(), ["0.2.0-beta.1", "0.1.1"]);
        assert!(newer_releases(list(), "0.2.0", Channel::Beta).is_empty());
        assert_eq!(stable[0].name, "v0.1.1");
    }

    /// 2026-09-14 00:00 UTC was a Monday.
    const MONDAY: i64 = 1_789_344_000;

    fn every_day_at(hour: u8, minute: u8) -> UpdateSettings {
        UpdateSettings { auto: true, weekday: None, hour, minute, ..UpdateSettings::default() }
    }

    #[test]
    fn the_week_starts_on_monday() {
        assert_eq!(weekday_of(MONDAY), 0);
        assert_eq!(weekday_of(MONDAY + 86_400), 1);
        assert_eq!(weekday_of(MONDAY + 6 * 86_400), 6, "sunday");
        assert_eq!(weekday_of(MONDAY + 7 * 86_400), 0, "and round again");
        assert_eq!(weekday_of(MONDAY - 1), 6, "a second before monday is still sunday");
    }

    #[test]
    fn a_schedule_looks_back_to_its_last_minute() {
        let daily = every_day_at(4, 30);
        let half_four = MONDAY + 4 * 3600 + 30 * 60;
        assert_eq!(planned_before(&daily, half_four), Some(half_four), "on the dot");
        assert_eq!(planned_before(&daily, half_four + 600), Some(half_four), "ten minutes later");
        // Before today's time the last one was yesterday, not today.
        assert_eq!(planned_before(&daily, MONDAY + 3600), Some(half_four - 86_400));

        let sundays = UpdateSettings { weekday: Some(6), ..daily.clone() };
        let sunday = half_four + 6 * 86_400;
        assert_eq!(planned_before(&sundays, sunday + 60), Some(sunday));
        assert_eq!(planned_before(&sundays, sunday - 60), Some(sunday - 7 * 86_400), "a week back");

        assert_eq!(planned_before(&UpdateSettings { auto: false, ..daily.clone() }, half_four), None);
        assert_eq!(planned_before(&UpdateSettings { hour: 24, ..daily.clone() }, half_four), None, "no such hour");
        assert_eq!(planned_before(&UpdateSettings { weekday: Some(9), ..daily }, half_four), None, "no such day");
    }

    #[test]
    fn a_minute_acted_on_does_not_come_round_again() {
        let daily = every_day_at(4, 0);
        let four = MONDAY + 4 * 3600;
        let fresh = UpdateStatus::default();
        assert_eq!(scheduled_due(&daily, &fresh, four), Some(four));
        assert_eq!(scheduled_due(&daily, &fresh, four + 3600), Some(four), "an hour late is still that minute");
        assert_eq!(scheduled_due(&daily, &fresh, four + CATCH_UP_SECS + 60), None, "too late, wait for tomorrow");

        let done = UpdateStatus { handled: Some(four), ..UpdateStatus::default() };
        assert_eq!(scheduled_due(&daily, &done, four + 600), None, "already acted on");
        assert_eq!(scheduled_due(&daily, &done, four + 86_400), Some(four + 86_400), "but tomorrow is another one");
    }

    #[test]
    fn a_schedule_keeps_away_from_the_backup() {
        // The two halves of the rule sit in different crates, so this is the place they meet: the
        // schedule says a minute is due, and the backup window is what may still hold it back.
        use uwumail_backup::{BackupSettings, BackupStatus, Backups, Login, Target};
        let backups = BackupSettings {
            enabled: true,
            hour: 4,
            minute: 0,
            target: Some(Target {
                host: "nas.example.de".into(),
                port: 22,
                user: "backup".into(),
                path: "uwumail".into(),
                login: Login::Password { password: "x".into() },
                host_key: None,
            }),
            ..BackupSettings::default()
        };
        let four = MONDAY + 4 * 3600;
        // Tonight's backup begins at four and takes twenty minutes; these are the two ways the
        // status looks while that happens.
        let midway = BackupStatus { started_at: Some(four), last_attempt_at: Some(four), ..BackupStatus::default() };
        let after =
            BackupStatus { finished_at: Some(four + 20 * 60), last_success_at: Some(four + 20 * 60), ..midway.clone() };
        let schedule = every_day_at(4, 15);
        // Quarter past four is the update's minute, and the backup is still running then: the
        // minute has come, the window has not passed.
        assert_eq!(scheduled_due(&schedule, &UpdateStatus::default(), four + 15 * 60), Some(four + 15 * 60));
        assert!(Backups::blocks(&backups, &midway, four + 15 * 60, BACKUP_MARGIN_SECS), "while it runs");
        assert!(Backups::blocks(&backups, &after, four + 40 * 60, BACKUP_MARGIN_SECS), "and the half hour after");
        // An hour on it is clear of the window and still inside the catch-up, so it goes then.
        assert!(!Backups::blocks(&backups, &after, four + 3600, BACKUP_MARGIN_SECS));
        assert_eq!(scheduled_due(&schedule, &UpdateStatus::default(), four + 3600), Some(four + 15 * 60));
        // Six hours late it gives up on today, backup or no backup.
        assert_eq!(scheduled_due(&schedule, &UpdateStatus::default(), four + 15 * 60 + CATCH_UP_SECS + 60), None);
    }

    #[test]
    fn the_repository_comes_from_the_package() {
        assert!(!repository().contains("://"));
        assert_eq!(repository().split('/').count(), 2);
        assert!(image().starts_with("ghcr.io/"));
    }
}
