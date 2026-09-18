//! Knowing when there is something newer: once a day the server asks GitHub for the releases of its
//! channel (stable or beta), or, for an `edge` build of `main`, which commits came since.
//!
//! Updating itself happens on the machine, with `update.sh`. The portal only ever says what is
//! there and what changed -- a mail server that can replace itself from a web page is one more way
//! in, and the script does the job better anyway: it can also bring a new compose file.

use std::cmp::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::Web;
use crate::health::unix_now;

const SETTINGS_KEY: &str = "updates.settings";
const INFO_KEY: &str = "updates.info";
const CHECK_INTERVAL: i64 = 24 * 3600;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;

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
    /// Ask GitHub at all. Off means the server never talks to it.
    pub check: bool,
    pub channel: Channel,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        UpdateSettings { check: true, channel: Channel::Stable }
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

    /// Asks once a day whether there is something newer.
    pub async fn run_updates(self, mut shutdown: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = shutdown.changed() => return,
            }
            if self.update_settings().await.check {
                let last = self.update_info().await.checked_at.unwrap_or(0);
                if unix_now() - last >= CHECK_INTERVAL {
                    self.check_updates().await;
                }
            }
        }
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

    #[test]
    fn the_repository_comes_from_the_package() {
        assert!(!repository().contains("://"));
        assert_eq!(repository().split('/').count(), 2);
        assert!(image().starts_with("ghcr.io/"));
    }
}
