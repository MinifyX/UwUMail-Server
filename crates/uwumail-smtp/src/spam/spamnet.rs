//! Spam-Netz (docs/spam-filter.md): text fingerprints of a mail (Digest v1), the points the shared list
//! gives, and the worker that fetches the list and sends reports to the hub every 15 minutes.
//!
//! Reports carry only four kinds: digests, registrable link domains, the delivering IP (IPv4 /32,
//! IPv6 /64) and the From domain when DMARC-aligned. Never recipients, never content. Registration
//! sends the host name and the version.

use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;
use uwumail_store::{SpamNetEntry, SpamNetFeatures, SpamNetRemoved};

use super::Score;
use crate::egress::{Egress, Purpose};
use crate::{Context, Smtp, now};

/// The most points the list adds to one mail, however much of it is listed.
const MAX_POINTS: f32 = 6.0;
const MAX_DIGESTS: usize = 8;
const MAX_DOMAINS: usize = 20;
/// Reports per request, as the hub takes them.
const BATCH: usize = 100;
/// Requests of reports per run at most, so a long queue does not take the whole run.
const BATCHES_PER_RUN: usize = 5;
const TICK: Duration = Duration::from_secs(15 * 60);
const MAX_ANSWER: usize = 16 * 1024 * 1024;
/// Entries and removals taken from one page of the list at most (security audit 0.38.0 S-6).
const MAX_ENTRIES: usize = 50_000;
const KEY_SETTING: &str = "spamnet.key";
const CURSOR_SETTING: &str = "spamnet.cursor";
const STATUS_SETTING: &str = "spamnet.status";

/// Digest v1, steps 1–3: NFKC, lowercase, drop words that are links or addresses and all digits,
/// keep only letters, one space between words. The hub's `src/digest.rs` is the reference.
fn normalize(text: &str) -> String {
    let folded = icu_normalizer::ComposingNormalizerBorrowed::new_nfkc().normalize(text).to_lowercase();
    let mut kept = String::with_capacity(folded.len());
    for token in folded.split(char::is_whitespace) {
        if !(token.contains("://") || token.starts_with("www.") || token.contains('@')) {
            kept.extend(token.chars().filter(|c| !c.is_numeric()));
        }
        kept.push(' ');
    }
    let letters: String = kept.chars().map(|c| if c.is_alphabetic() { c } else { ' ' }).collect();
    letters.split(' ').filter(|word| !word.is_empty()).collect::<Vec<_>>().join(" ")
}

fn hash(prefix: &str, text: &str) -> String {
    format!("{prefix}{}", hex::encode(&Sha256::digest(text.as_bytes())[..16]))
}

/// Digest v1 of a mail's readable text: `d1:` of the whole text when it is long enough, then up
/// to four `p1:` of its longest paragraphs.
pub(crate) fn digests(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let whole = normalize(text);
    if whole.chars().count() >= 40 {
        found.push(hash("d1:", &whole));
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut paragraphs: Vec<(usize, String)> = Vec::new();
    for paragraph in text.split('\n').collect::<Vec<_>>().split(|line| line.chars().all(char::is_whitespace)) {
        if paragraph.is_empty() {
            continue;
        }
        let normalized = normalize(&paragraph.join("\n"));
        let length = normalized.chars().count();
        if length >= 60 {
            paragraphs.push((length, normalized));
        }
    }
    // Longest first; equal lengths keep the order of the text (the sort is stable).
    paragraphs.sort_by_key(|paragraph| std::cmp::Reverse(paragraph.0));
    let mut kept: Vec<String> = Vec::new();
    for (_, paragraph) in paragraphs {
        if kept.len() == 4 {
            break;
        }
        let digest = hash("p1:", &paragraph);
        if !kept.contains(&digest) {
            kept.push(digest);
        }
    }
    found.extend(kept);
    found
}

/// What a mail would report, and look up in the list. The JSON form is the report's, minus
/// verdict and reason.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Features {
    pub digests: Vec<String>,
    pub domains: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// The registrable From domain, reported only when DMARC vouched for it: a forged From must not
    /// build up a score for somebody else's domain (security audit 0.38.0 S-1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The From domain looked up in the list when DMARC did not vouch for it; never sent.
    #[serde(skip)]
    pub unaligned_from: Option<String>,
    /// The full From address and host, only to rule out own and contact mail; never sent.
    #[serde(skip)]
    pub from_address: String,
    #[serde(skip)]
    pub from_host: String,
}

/// The delivering IP as the list keys it: an IPv4 address as is, an IPv6 one as its /64.
fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => {
            let network = u128::from(ip) & !((1u128 << 64) - 1);
            format!("{}/64", std::net::Ipv6Addr::from(network))
        }
    }
}

impl Features {
    pub(crate) fn of(
        digests: Vec<String>,
        link_hosts: &[String],
        ip: Option<IpAddr>,
        from_address: Option<&str>,
        dmarc_aligned: bool,
    ) -> Features {
        let mut domains: Vec<String> = Vec::new();
        for domain in link_hosts.iter().filter_map(|host| uwumail_store::verification::registrable_domain(host)) {
            if domains.len() < MAX_DOMAINS && !domains.contains(&domain) {
                domains.push(domain);
            }
        }
        let from_address = from_address.unwrap_or_default().trim().to_lowercase();
        let from_host = from_address.rsplit_once('@').map(|(_, host)| host.trim_end_matches('.').to_owned());
        let from = from_host.as_deref().and_then(uwumail_store::verification::registrable_domain);
        let (from, unaligned_from) = if dmarc_aligned { (from, None) } else { (None, from) };
        Features {
            digests: digests.into_iter().take(MAX_DIGESTS).collect(),
            domains,
            ip: ip.map(ip_key),
            from,
            unaligned_from,
            from_host: from_host.unwrap_or_default(),
            from_address,
        }
    }

    fn for_store(&self, authenticated: bool) -> SpamNetFeatures {
        SpamNetFeatures {
            features: serde_json::to_string(self).unwrap_or_default(),
            from_address: self.from_address.clone(),
            from_host: self.from_host.clone(),
            authenticated,
        }
    }
}

/// The points the list gives a mail: per kind the best listed entry, together at most
/// [`MAX_POINTS`].
pub(crate) async fn add_findings(ctx: &Context, features: &Features, score: &mut Score) {
    let mut wanted: Vec<(&'static str, String)> = Vec::new();
    wanted.extend(features.digests.iter().map(|digest| ("digest", digest.clone())));
    wanted.extend(features.domains.iter().map(|domain| ("domain", domain.clone())));
    wanted.extend(features.ip.iter().map(|ip| ("ip", ip.clone())));
    // Mail authenticated for its From domain is not judged by what others say about that domain.
    wanted.extend(features.unaligned_from.iter().map(|from| ("from", from.clone())));
    let found = match ctx.store.spamnet_matches(wanted).await {
        Ok(found) => found,
        Err(err) => {
            tracing::warn!(%err, "looking up the Spam-Netz list failed");
            return;
        }
    };
    let mut left = MAX_POINTS;
    for (kind, rule) in
        [("digest", "SPAMNET_DIGEST"), ("domain", "SPAMNET_DOMAIN"), ("ip", "SPAMNET_IP"), ("from", "SPAMNET_FROM")]
    {
        let best = found.iter().filter(|(found, _, _)| found == kind).max_by(|a, b| a.2.total_cmp(&b.2));
        if let Some((_, value, points)) = best {
            let points = (*points as f32).clamp(0.0, 4.0).min(left);
            if points > 0.0 {
                left -= points;
                score.add(rule, points, Some(value.clone()));
            }
        }
    }
}

/// Keeps what a delivered mail would report, should a person mark it later.
pub(crate) async fn keep(ctx: &Context, blob: uwumail_store::BlobHash, features: &Features, authenticated: bool) {
    if ctx.spamnet().report
        && let Err(err) = ctx.store.keep_spamnet_features(blob, features.for_store(authenticated)).await
    {
        tracing::warn!(%err, "keeping a mail's Spam-Netz features failed");
    }
}

/// Queues a report of a mail the filter refused. Authenticated mail is left to people to judge.
pub(crate) async fn report_refused(ctx: &Context, features: &Features, authenticated: bool) {
    if authenticated || !ctx.spamnet().report {
        return;
    }
    if let Err(err) = ctx.store.queue_spamnet_report("spam", "filter", features.for_store(false)).await {
        tracing::warn!(%err, "queueing a Spam-Netz report failed");
    }
}

/// How the last fetch and the last reports went.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpamNetStatus {
    pub fetch: bool,
    pub report: bool,
    pub registered: bool,
    pub fetched_at: Option<i64>,
    pub fetch_error: Option<String>,
    /// Entries of the list in force.
    pub entries: i64,
    pub reported_at: Option<i64>,
    pub report_error: Option<String>,
    /// Reports the hub took in the last run.
    pub sent: usize,
    /// Reports waiting to be sent.
    pub queued: i64,
}

/// The key the hub gave, for the hub it was registered at.
#[derive(Serialize, Deserialize)]
struct StoredKey {
    url: String,
    sealed: String,
}

#[derive(Debug)]
enum Failure {
    /// The hub no longer knows the key: register again next time.
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Unauthorized => f.write_str("the hub refused the key (401)"),
            Failure::Other(reason) => f.write_str(reason),
        }
    }
}

fn base(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

async fn call(egress: &Egress, url: &str, key: Option<&str>, body: Option<String>) -> Result<(u16, Vec<u8>), Failure> {
    match egress.json_request(Purpose::SpamNet, url, key, body, MAX_ANSWER).await {
        Ok((401, _)) => Err(Failure::Unauthorized),
        Ok((status, body)) => Ok((status, body.to_vec())),
        Err(err) => Err(Failure::Other(err.to_string())),
    }
}

fn hub_error(status: u16, body: &[u8]) -> Failure {
    #[derive(Deserialize)]
    struct Answer {
        error: String,
    }
    let said = serde_json::from_slice::<Answer>(body).map(|answer| answer.error).unwrap_or_default();
    let said: String = said.chars().take(200).collect();
    Failure::Other(if said.is_empty() { format!("the hub answered {status}") } else { format!("{status}: {said}") })
}

/// The key for the hub as set now, registering this server first when there is none.
async fn key(ctx: &Context, egress: &Egress, url: &str) -> Result<String, Failure> {
    let stored = ctx.store.setting(KEY_SETTING).await.ok().flatten();
    if let Some(stored) = stored.and_then(|stored| serde_json::from_str::<StoredKey>(&stored).ok())
        && stored.url == url
        && let Ok(key) = ctx.store.unseal_secret(&stored.sealed).await
    {
        return Ok(key);
    }
    #[derive(Deserialize)]
    struct Registered {
        key: String,
    }
    let request = serde_json::json!({
        "name": ctx.hostname,
        "software": format!("UwUMail {}", env!("CARGO_PKG_VERSION")),
    });
    let (status, body) = call(egress, &format!("{url}/v1/register"), None, Some(request.to_string())).await?;
    if status != 201 && status != 200 {
        return Err(hub_error(status, &body));
    }
    let key = serde_json::from_slice::<Registered>(&body)
        .map_err(|_| Failure::Other("the hub's answer to registering is not understood".into()))?
        .key;
    // It goes into a header: only the characters a key of the hub has.
    let fits = key.starts_with("usn_")
        && key.len() <= 200
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if !fits {
        return Err(Failure::Other("the hub sent no usable key".into()));
    }
    let sealed = ctx.store.seal_secret(&key).await.map_err(|err| Failure::Other(err.to_string()))?;
    let stored = serde_json::to_string(&StoredKey { url: url.to_owned(), sealed }).unwrap_or_default();
    ctx.store.set_setting(KEY_SETTING, &stored).await.map_err(|err| Failure::Other(err.to_string()))?;
    // A new registration starts the list afresh.
    let _ = ctx.store.delete_setting(CURSOR_SETTING).await;
    tracing::info!(hub = url, "registered with the Spam-Netz");
    Ok(key)
}

/// Sends the waiting reports. Answers how many the hub took.
async fn send_reports(ctx: &Context, egress: &Egress, url: &str, key: &str) -> Result<usize, Failure> {
    let own: Vec<String> = match ctx.store.domains().await {
        Ok(domains) => domains.into_iter().map(|domain| domain.name.to_lowercase()).collect(),
        Err(err) => return Err(Failure::Other(err.to_string())),
    };
    let mut sent = 0;
    for _ in 0..BATCHES_PER_RUN {
        let waiting = ctx.store.spamnet_reports(BATCH).await.map_err(|err| Failure::Other(err.to_string()))?;
        if waiting.is_empty() {
            break;
        }
        let ids: Vec<i64> = waiting.iter().map(|report| report.id).collect();
        let reports: Vec<serde_json::Value> = waiting
            .into_iter()
            .filter_map(|report| {
                let mut features: Features = serde_json::from_str(&report.features).ok()?;
                // Links to the server's own domains say nothing about the sender.
                features.domains.retain(|domain| !own.contains(domain));
                let mut value = serde_json::to_value(features).ok()?;
                value["verdict"] = report.verdict.into();
                value["reason"] = report.reason.into();
                Some(value)
            })
            .collect();
        let count = reports.len();
        if count > 0 {
            let body = serde_json::json!({ "reports": reports }).to_string();
            let (status, answer) = call(egress, &format!("{url}/v1/reports"), Some(key), Some(body)).await?;
            if status != 202 && status != 200 {
                return Err(hub_error(status, &answer));
            }
        }
        ctx.store.drop_spamnet_reports(ids).await.map_err(|err| Failure::Other(err.to_string()))?;
        sent += count;
    }
    Ok(sent)
}

#[derive(Deserialize)]
struct List {
    cursor: String,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    entries: Vec<SpamNetEntry>,
    #[serde(default)]
    removed: Vec<SpamNetRemoved>,
}

fn known_kind(kind: &str) -> bool {
    matches!(kind, "digest" | "domain" | "ip" | "from")
}

/// Fetches what changed in the list since last time, or all of it.
async fn fetch_list(ctx: &Context, egress: &Egress, url: &str, key: &str) -> Result<(), Failure> {
    let cursor = ctx.store.setting(CURSOR_SETTING).await.ok().flatten();
    let address = match &cursor {
        Some(cursor) => {
            let mut address = url::Url::parse(&format!("{url}/v1/list"))
                .map_err(|_| Failure::Other("the hub's address is not a web address".into()))?;
            address.query_pairs_mut().append_pair("since", cursor);
            address.to_string()
        }
        None => format!("{url}/v1/list"),
    };
    let (status, body) = call(egress, &address, Some(key), None).await?;
    if status != 200 {
        return Err(hub_error(status, &body));
    }
    let list: List =
        serde_json::from_slice(&body).map_err(|_| Failure::Other("the hub's list is not understood".into()))?;
    let entries = list
        .entries
        .into_iter()
        .filter(|entry| known_kind(&entry.kind) && !entry.value.is_empty() && entry.value.len() <= 255)
        .take(MAX_ENTRIES)
        .collect();
    let removed = list.removed.into_iter().filter(|gone| known_kind(&gone.kind)).take(MAX_ENTRIES).collect();
    if let Err(err) = ctx.store.apply_spamnet_list(list.full || cursor.is_none(), entries, removed).await {
        // Applied in chunks, the list may be half new: the next run fetches it whole.
        let _ = ctx.store.delete_setting(CURSOR_SETTING).await;
        return Err(Failure::Other(err.to_string()));
    }
    let _ = ctx.store.set_setting(CURSOR_SETTING, &list.cursor).await;
    Ok(())
}

async fn saved_status(ctx: &Context) -> SpamNetStatus {
    let saved = ctx.store.setting(STATUS_SETTING).await.ok().flatten();
    saved.and_then(|saved| serde_json::from_str(&saved).ok()).unwrap_or_default()
}

pub(crate) async fn spamnet_status(ctx: &Context) -> SpamNetStatus {
    let config = ctx.spamnet();
    let mut status = saved_status(ctx).await;
    status.fetch = config.fetch;
    status.report = config.report;
    status.registered = ctx.store.setting(KEY_SETTING).await.ok().flatten().is_some();
    status.entries = ctx.store.spamnet_list_len().await.unwrap_or(0);
    status.queued = ctx.store.spamnet_queue_len().await.unwrap_or(0);
    status
}

/// One run: send what waits, fetch the list, forget what is too old.
pub(crate) async fn sync_spamnet(ctx: &Context, egress: &Egress) -> SpamNetStatus {
    let config = ctx.spamnet();
    let mut status = saved_status(ctx).await;
    if !config.report
        && let Err(err) = ctx.store.clear_spamnet_reports().await
    {
        tracing::warn!(%err, "clearing Spam-Netz reports failed");
    }
    if config.fetch || config.report {
        let url = base(&config.url);
        let mut unauthorized = false;
        match key(ctx, egress, url).await {
            Err(failure) => {
                let error = Some(failure.to_string());
                if config.report {
                    status.report_error = error.clone();
                }
                if config.fetch {
                    status.fetch_error = error;
                }
            }
            Ok(key) => {
                if config.report {
                    let result = send_reports(ctx, egress, url, &key).await;
                    status.reported_at = Some(now());
                    unauthorized |= matches!(result, Err(Failure::Unauthorized));
                    (status.sent, status.report_error) = match result {
                        Ok(sent) => (sent, None),
                        Err(failure) => (0, Some(failure.to_string())),
                    };
                }
                if config.fetch {
                    let result = fetch_list(ctx, egress, url, &key).await;
                    status.fetched_at = Some(now());
                    unauthorized |= matches!(result, Err(Failure::Unauthorized));
                    status.fetch_error = result.err().map(|failure| failure.to_string());
                }
            }
        }
        if unauthorized {
            let _ = ctx.store.delete_setting(KEY_SETTING).await;
        }
    }
    if let Err(err) = ctx.store.prune_spamnet().await {
        tracing::warn!(%err, "forgetting old Spam-Netz data failed");
    }
    let _ = ctx.store.set_setting(STATUS_SETTING, &serde_json::to_string(&status).unwrap_or_default()).await;
    spamnet_status(ctx).await
}

/// Every 15 minutes, for as long as the server runs.
pub async fn run_spamnet(smtp: Smtp, egress: Egress, mut shutdown: watch::Receiver<bool>) {
    let ctx = smtp.inner.clone();
    // Not right at the start: a server restarting in a loop should not register in a loop.
    let mut wait = Duration::from_secs(120);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => return,
        }
        wait = TICK;
        sync_spamnet(&ctx, &egress).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_match_the_hubs_test_vectors() {
        #[derive(Deserialize)]
        struct Vector {
            name: String,
            input: String,
            normalized: String,
            digests: Vec<String>,
        }
        #[derive(Deserialize)]
        struct Vectors {
            vectors: Vec<Vector>,
        }
        let vectors: Vectors =
            serde_json::from_str(include_str!("../../tests/fixtures/spamnet-digest-v1-vectors.json")).unwrap();
        assert!(vectors.vectors.len() >= 10);
        for vector in vectors.vectors {
            assert_eq!(normalize(&vector.input), vector.normalized, "{}", vector.name);
            assert_eq!(digests(&vector.input), vector.digests, "{}", vector.name);
        }
    }

    #[test]
    fn features_keep_only_the_four_kinds() {
        let hosts = ["click.mailer.example.com".to_owned(), "www.example.com".to_owned(), "x".to_owned()];
        let ip: IpAddr = "2001:db8:1:2:3:4:5:6".parse().unwrap();
        let features = Features::of(vec!["d1:00".into()], &hosts, Some(ip), Some("Spam@News.Example.ORG"), true);
        assert_eq!(features.domains, ["example.com"]);
        assert_eq!(features.ip.as_deref(), Some("2001:db8:1:2::/64"));
        assert_eq!(features.from.as_deref(), Some("example.org"));
        let sent: serde_json::Value = serde_json::to_value(&features).unwrap();
        let mut keys: Vec<_> = sent.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["digests", "domains", "from", "ip"], "the address itself is never sent");
        let forged = Features::of(vec![], &[], None, Some("spam@example.org"), false);
        assert_eq!((forged.from, forged.unaligned_from.as_deref()), (None, Some("example.org")), "only looked up");
        let v4 = Features::of(vec![], &[], Some("192.0.2.7".parse().unwrap()), None, false);
        assert_eq!((v4.ip.as_deref(), v4.from), (Some("192.0.2.7"), None));
    }

    async fn server(fetch: bool, report: bool) -> (Smtp, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = uwumail_store::Store::open(dir.path()).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let smtp = Smtp::new(
            store,
            crate::SmtpSettings {
                hostname: "mx.example.org".into(),
                smtp: crate::SmtpConfig::default(),
                spam: crate::SpamConfig::default(),
                delivery: crate::DeliveryConfig::default(),
                tone: crate::ToneConfig::default(),
                server_tls: None,
            },
        )
        .unwrap();
        smtp.set_spamnet(&crate::SpamNetConfig { fetch, report, url: "https://spamnet.example/".into() });
        (smtp, dir)
    }

    fn hub(path: &str) -> &'static str {
        match path {
            "/v1/register" => {
                "HTTP/1.1 201 Created\r\nConnection: close\r\n\r\n{\"serverId\":\"s1\",\"key\":\"usn_test1\"}"
            }
            "/v1/reports" => "HTTP/1.1 202 Accepted\r\nConnection: close\r\n\r\n{\"accepted\":1}",
            "/v1/list" => {
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"cursor\":\"c1\",\"full\":true,\"entries\":[\
                 {\"kind\":\"digest\",\"value\":\"d1:aa\",\"score\":4.0,\"expires\":\"2999-01-01T00:00:00Z\"},\
                 {\"kind\":\"domain\",\"value\":\"example.net\",\"score\":3.5,\"expires\":\"2999-01-01T00:00:00Z\"},\
                 {\"kind\":\"ip\",\"value\":\"192.0.2.7\",\"score\":2.0,\"expires\":\"2999-01-01T00:00:00Z\"},\
                 {\"kind\":\"recipient\",\"value\":\"x\",\"score\":2.0,\"expires\":\"2999-01-01T00:00:00Z\"}],\"removed\":[]}"
            }
            "/v1/list?since=c1" => {
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"cursor\":\"c2\",\"full\":false,\"entries\":[],\
                 \"removed\":[{\"kind\":\"ip\",\"value\":\"192.0.2.7\"}]}"
            }
            _ => "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n{\"error\":\"no\"}",
        }
    }

    #[tokio::test]
    async fn a_run_registers_reports_and_keeps_the_list() {
        let (smtp, _dir) = server(true, true).await;
        let (egress, seen) = crate::egress::tests::tls_site(hub).await;
        let features = Features::of(
            vec!["d1:aa".into()],
            &["www.example.net".into(), "mx.example.org".into()],
            Some("192.0.2.7".parse().unwrap()),
            Some("spam@example.com"),
            false,
        );
        smtp.inner.store.queue_spamnet_report("spam", "filter", features.for_store(false)).await.unwrap();

        let status = smtp.sync_spamnet(&egress).await;
        assert_eq!((status.fetch_error, status.report_error), (None, None));
        assert!(status.registered);
        assert_eq!((status.sent, status.queued, status.entries), (1, 0, 3), "the unknown kind is left out");
        {
            let seen = seen.lock().unwrap();
            assert!(seen[0].starts_with("POST /v1/register") && !seen[0].contains("contact"), "{}", seen[0]);
            let report = &seen[1];
            assert!(report.starts_with("POST /v1/reports") && report.contains("Bearer usn_test1"), "{report}");
            let body: serde_json::Value =
                serde_json::from_str(&report[report.find("\r\n\r\n").unwrap() + 4..]).unwrap();
            let sent = &body["reports"][0];
            assert_eq!((sent["verdict"].as_str(), sent["reason"].as_str()), (Some("spam"), Some("filter")));
            assert_eq!(sent["domains"], serde_json::json!(["example.net"]), "the server's own domain is not sent");
            assert!(!report.contains("spam@example.com"), "no address leaves the server");
            assert!(seen[2].starts_with("GET /v1/list HTTP"), "{}", seen[2]);
        }

        // The list gives points, at most six together.
        let mut score = Score::default();
        add_findings(&smtp.inner, &features, &mut score).await;
        let rules: Vec<_> = score.hits.iter().map(|hit| (hit.rule, hit.points)).collect();
        assert_eq!(rules, [("SPAMNET_DIGEST", 4.0), ("SPAMNET_DOMAIN", 2.0)]);

        let status = smtp.sync_spamnet(&egress).await;
        assert_eq!((status.fetch_error, status.entries), (None, 2));
        assert!(seen.lock().unwrap().last().unwrap().starts_with("GET /v1/list?since=c1 "));
    }

    #[tokio::test]
    async fn a_refused_key_is_dropped_and_switched_off_forgets_the_queue() {
        let (smtp, _dir) = server(true, false).await;
        let (egress, _) = crate::egress::tests::tls_site(|path| match path {
            "/v1/register" => "HTTP/1.1 201 Created\r\nConnection: close\r\n\r\n{\"key\":\"usn_test2\"}",
            _ => "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n{\"error\":\"unknown key\"}",
        })
        .await;
        let features = Features::of(vec!["d1:bb".into()], &[], None, Some("spam@example.com"), false);
        smtp.inner.store.queue_spamnet_report("spam", "filter", features.for_store(false)).await.unwrap();
        let status = smtp.sync_spamnet(&egress).await;
        assert!(status.fetch_error.is_some());
        assert!(!status.registered, "a key the hub refuses is registered anew next time");
        assert_eq!(status.queued, 0, "reports switched off: nothing is kept to send");
    }
}
