//! `GET /metrics` for Prometheus (docs/metrics.md), in the text exposition format, written by hand.
//!
//! Off unless switched on. Then a scraper shows the bearer token from `metrics.token` and, when
//! `metrics.allowed_networks` lists any, comes from one of those networks. Networks alone, without a
//! token, are enough for a scraper on a trusted network. The client address is the one the rest of
//! the server believes too: the connection's, or what a trusted reverse proxy forwarded.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::sync::RwLock;

use axum::extract::State;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use uwumail_smtp::IpNetwork;
use uwumail_store::{AlertLevel, Stat};

use crate::Web;
use crate::health::Level;

/// The shortest token accepted, so it cannot be guessed within the login limits.
const MIN_TOKEN_CHARS: usize = 16;

/// The `[metrics]` part of the configuration; the admin panel can change it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub enabled: bool,
    /// Scrapers send it as `Authorization: Bearer <token>`.
    pub token: String,
    /// When not empty, only these addresses and networks may scrape, e.g. `10.0.0.0/8`.
    pub allowed_networks: Vec<String>,
}

impl MetricsConfig {
    /// Checks what types cannot: the networks, the token's length, and that switching the metrics
    /// on does not open them to everyone.
    pub fn check(&self) -> Result<(), String> {
        let networks =
            IpNetwork::parse_list(&self.allowed_networks).map_err(|err| format!("metrics.allowed_networks: {err}"))?;
        let token = self.token.trim();
        if self.enabled && token.is_empty() && networks.is_empty() {
            return Err("metrics.enabled needs metrics.token or metrics.allowed_networks, or anyone could read \
                        the metrics"
                .into());
        }
        if !token.is_empty() && token.chars().count() < MIN_TOKEN_CHARS {
            return Err(format!("metrics.token should be at least {MIN_TOKEN_CHARS} characters long"));
        }
        Ok(())
    }
}

struct Access {
    token: Option<String>,
    /// Empty: from anywhere.
    networks: Vec<IpNetwork>,
}

/// Who may read `/metrics` right now. The server changes it when the settings change.
#[derive(Default)]
pub struct MetricsGate {
    access: RwLock<Option<Access>>,
}

impl MetricsGate {
    pub fn new(config: &MetricsConfig) -> Result<MetricsGate, String> {
        let gate = MetricsGate::default();
        gate.configure(config)?;
        Ok(gate)
    }

    /// Takes a new configuration into use at once. A configuration that does not check out
    /// leaves the metrics off.
    pub fn configure(&self, config: &MetricsConfig) -> Result<(), String> {
        let checked = config.check();
        let access = match (&checked, config.enabled) {
            (Ok(()), true) => Some(Access {
                token: Some(config.token.trim().to_owned()).filter(|token| !token.is_empty()),
                networks: IpNetwork::parse_list(&config.allowed_networks)?,
            }),
            _ => None,
        };
        *self.access.write().expect("metrics gate poisoned") = access;
        checked
    }

    fn decide(&self, ip: IpAddr, headers: &HeaderMap) -> Decision {
        let access = self.access.read().expect("metrics gate poisoned");
        let Some(access) = access.as_ref() else { return Decision::Off };
        if !access.networks.is_empty() && !access.networks.iter().any(|network| network.contains(ip)) {
            return Decision::Forbidden;
        }
        let Some(token) = &access.token else { return Decision::Allowed };
        let sent = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim());
        match sent {
            Some(sent) if crate::session::same(sent, token) => Decision::Allowed,
            Some(_) => Decision::WrongToken,
            None => Decision::NoToken,
        }
    }
}

enum Decision {
    Off,
    Allowed,
    NoToken,
    WrongToken,
    Forbidden,
}

fn plain(status: StatusCode, text: &'static str) -> Response {
    let mut response = (status, text).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn unauthorized() -> Response {
    let mut response = plain(StatusCode::UNAUTHORIZED, "a bearer token is needed\n");
    response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

pub(crate) async fn handler(State(web): State<Web>, parts: Parts) -> Response {
    let client = crate::session::client(&parts);
    let Some(gate) = web.metrics_gate() else { return plain(StatusCode::NOT_FOUND, "not found\n") };
    // A network that keeps guessing tokens is turned away like one that keeps guessing passwords.
    if web.limiter().is_blocked(client.ip) {
        return plain(StatusCode::TOO_MANY_REQUESTS, "too many attempts\n");
    }
    match gate.decide(client.ip, &parts.headers) {
        Decision::Off => return plain(StatusCode::NOT_FOUND, "not found\n"),
        Decision::Forbidden => return plain(StatusCode::FORBIDDEN, "not allowed from this address\n"),
        Decision::WrongToken => {
            web.limiter().record_unknown_login(client.ip);
            tracing::warn!(ip = %client.ip, "wrong metrics token");
            return unauthorized();
        }
        Decision::NoToken => return unauthorized(),
        Decision::Allowed => {}
    }
    match render(&web).await {
        Ok(text) => {
            let mut response = text.into_response();
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(err) => {
            tracing::warn!(?err, "collecting the metrics failed");
            plain(StatusCode::INTERNAL_SERVER_ERROR, "collecting the metrics failed\n")
        }
    }
}

/// Writes one metric family: its help, its type and its samples.
struct Family<'a> {
    out: &'a mut String,
}

impl Family<'_> {
    fn head(&mut self, name: &str, kind: &str, help: &str) -> &mut Self {
        let _ = writeln!(self.out, "# HELP {name} {help}");
        let _ = writeln!(self.out, "# TYPE {name} {kind}");
        self
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) -> &mut Self {
        self.out.push_str(name);
        if !labels.is_empty() {
            self.out.push('{');
            for (index, (label, value)) in labels.iter().enumerate() {
                if index > 0 {
                    self.out.push(',');
                }
                let _ = write!(self.out, "{label}=\"{}\"", escape(value));
            }
            self.out.push('}');
        }
        let _ = writeln!(self.out, " {value}");
        self
    }
}

/// Label values escape backslash, double quote and line feed.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn level_value(level: Level) -> i64 {
    match level {
        Level::Ok => 0,
        Level::Warning => 1,
        Level::Problem => 2,
        Level::Unknown => -1,
    }
}

/// A counter family: its name, its help, and the counters behind each label value.
type Counter = (&'static str, &'static str, &'static [(Stat, &'static str)]);

/// The counters in the order and with the labels they are shown with.
const COUNTERS: &[Counter] = &[
    ("uwumail_mail_received_total", "Messages from other servers that were accepted.", &[(Stat::Received, "")]),
    ("uwumail_mail_junk_total", "Accepted messages that went into Junk.", &[(Stat::Junk, "")]),
    (
        "uwumail_mail_refused_total",
        "Messages or recipients refused at the door, by reason.",
        &[
            (Stat::RefusedUnknownRecipient, "unknown_recipient"),
            (Stat::RefusedSpam, "spam"),
            (Stat::RefusedVirus, "virus"),
            (Stat::RefusedPolicy, "policy"),
            (Stat::RefusedGreylisted, "greylisted"),
        ],
    ),
    ("uwumail_mail_submitted_total", "Messages sent by this server's people.", &[(Stat::Submitted, "")]),
    ("uwumail_mail_delivered_total", "Recipients handed to other servers.", &[(Stat::Delivered, "")]),
    (
        "uwumail_mail_deferred_total",
        "Delivery attempts to other servers that will be retried.",
        &[(Stat::Deferred, "")],
    ),
    ("uwumail_mail_bounced_total", "Recipients given up on.", &[(Stat::Bounced, "")]),
    (
        "uwumail_login_failures_total",
        "Failed logins, by protocol.",
        &[
            (Stat::LoginFailedSmtp, "smtp"),
            (Stat::LoginFailedImap, "imap"),
            (Stat::LoginFailedJmap, "jmap"),
            (Stat::LoginFailedDav, "dav"),
            (Stat::LoginFailedManageSieve, "managesieve"),
            (Stat::LoginFailedPortal, "portal"),
            (Stat::LoginFailedOther, "other"),
        ],
    ),
];

pub(crate) async fn render(web: &Web) -> crate::ApiResult<String> {
    let counts = web.store().server_counts().await?;
    let health = web.server_health().await?;
    let alerts = web.store().open_alert_counts().await?;
    let mut out = String::with_capacity(4096);
    let mut family = Family { out: &mut out };

    let version = env!("CARGO_PKG_VERSION");
    family.head("uwumail_build_info", "gauge", "The running version; always 1.").sample(
        "uwumail_build_info",
        &[("version", version)],
        1,
    );
    family.head("uwumail_uptime_seconds", "gauge", "Seconds since the server started.").sample(
        "uwumail_uptime_seconds",
        &[],
        web.settings().started.elapsed().as_secs(),
    );
    family.head("uwumail_accounts", "gauge", "Accounts, not counting those in the trash.").sample(
        "uwumail_accounts",
        &[],
        counts.accounts,
    );
    family.head("uwumail_domains", "gauge", "Domains.").sample("uwumail_domains", &[], counts.domains);
    family.head("uwumail_aliases", "gauge", "Additional addresses of accounts.").sample(
        "uwumail_aliases",
        &[],
        counts.aliases,
    );
    family.head("uwumail_storage_used_bytes", "gauge", "Bytes of mail stored in all mailboxes.").sample(
        "uwumail_storage_used_bytes",
        &[],
        counts.used_bytes,
    );
    if let Some((free, total)) = crate::health::disk_space(web.store().data_dir()) {
        family.head("uwumail_data_disk_free_bytes", "gauge", "Free bytes on the disk of the data directory.").sample(
            "uwumail_data_disk_free_bytes",
            &[],
            free,
        );
        family
            .head("uwumail_data_disk_size_bytes", "gauge", "Size of the disk of the data directory in bytes.")
            .sample("uwumail_data_disk_size_bytes", &[], total);
    }
    family.head("uwumail_queue_messages", "gauge", "Messages waiting to be delivered to other servers.").sample(
        "uwumail_queue_messages",
        &[],
        counts.queued_messages,
    );
    family
        .head(
            "uwumail_queue_recipients",
            "gauge",
            "Recipients waiting in the queue: not tried yet (pending) or tried and waiting to retry (deferred).",
        )
        .sample(
            "uwumail_queue_recipients",
            &[("state", "pending")],
            counts.pending_recipients - counts.deferred_recipients,
        )
        .sample("uwumail_queue_recipients", &[("state", "deferred")], counts.deferred_recipients);

    let totals: std::collections::HashMap<Stat, u64> = web.store().stats().since_start().into_iter().collect();
    for (name, help, stats) in COUNTERS {
        family.head(name, "counter", help);
        for (stat, label) in *stats {
            let value = totals.get(stat).copied().unwrap_or(0);
            if label.is_empty() {
                family.sample(name, &[], value);
            } else {
                let key = if name.starts_with("uwumail_login") { "protocol" } else { "reason" };
                family.sample(name, &[(key, label)], value);
            }
        }
    }

    family.head(
        "uwumail_health_level",
        "gauge",
        "Health of each area as on the server overview: 0 fine, 1 warning, 2 problem, -1 not checked yet.",
    );
    for area in &health.areas {
        family.sample("uwumail_health_level", &[("area", area.area)], level_value(area.level));
    }
    family.head("uwumail_alerts_open", "gauge", "Open admin alerts, by level.");
    for level in [AlertLevel::Info, AlertLevel::Warning, AlertLevel::Problem] {
        let count = alerts.iter().find(|(found, _)| *found == level).map_or(0, |(_, count)| *count);
        family.sample("uwumail_alerts_open", &[("level", level.as_str())], count);
    }
    if let Some(backups) = web.backups() {
        let status = backups.status().await;
        if let Some(at) = status.last_success_at {
            family
                .head(
                    "uwumail_backup_last_success_timestamp_seconds",
                    "gauge",
                    "When the last backup succeeded, in Unix seconds.",
                )
                .sample("uwumail_backup_last_success_timestamp_seconds", &[], at);
        }
    }
    if let Some(certificate) = web.settings().certificate.as_ref().and_then(|source| source()) {
        family
            .head(
                "uwumail_certificate_expiry_timestamp_seconds",
                "gauge",
                "When the certificate in use expires, in Unix seconds.",
            )
            .sample("uwumail_certificate_expiry_timestamp_seconds", &[], certificate.not_after);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(enabled: bool, token: &str, networks: &[&str]) -> MetricsConfig {
        MetricsConfig {
            enabled,
            token: token.into(),
            allowed_networks: networks.iter().map(|network| (*network).to_owned()).collect(),
        }
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
        headers
    }

    #[test]
    fn configurations_never_open_the_metrics_to_everyone() {
        assert!(config(false, "", &[]).check().is_ok(), "off needs nothing");
        assert!(config(true, "", &[]).check().is_err());
        assert!(config(true, "short", &[]).check().is_err());
        assert!(config(true, "0123456789abcdef0123", &[]).check().is_ok());
        assert!(config(true, "", &["127.0.0.1/32", "10.0.0.0/8", "2001:db8::/32"]).check().is_ok());
        assert!(config(true, "", &["10.0.0.0/33"]).check().is_err());
        assert!(config(false, "", &["not a network"]).check().is_err(), "checked even while off");
    }

    #[test]
    fn the_gate_wants_the_network_and_the_token() {
        let inside: IpAddr = "192.0.2.7".parse().unwrap();
        let outside: IpAddr = "198.51.100.7".parse().unwrap();
        let token = "0123456789abcdef0123";

        let networks_only = MetricsGate::new(&config(true, "", &["192.0.2.0/24"])).unwrap();
        assert!(matches!(networks_only.decide(inside, &HeaderMap::new()), Decision::Allowed));
        assert!(matches!(networks_only.decide(outside, &bearer(token)), Decision::Forbidden));

        let token_only = MetricsGate::new(&config(true, token, &[])).unwrap();
        assert!(matches!(token_only.decide(outside, &bearer(token)), Decision::Allowed));
        assert!(matches!(token_only.decide(outside, &HeaderMap::new()), Decision::NoToken));
        assert!(matches!(token_only.decide(outside, &bearer("0123456789abcdef0124")), Decision::WrongToken));
        let mut basic = HeaderMap::new();
        basic.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic MDEyMzQ1Njc4OWFiY2RlZjAxMjM="));
        assert!(matches!(token_only.decide(outside, &basic), Decision::NoToken));

        let both = MetricsGate::new(&config(true, token, &["192.0.2.0/24"])).unwrap();
        assert!(matches!(both.decide(inside, &bearer(token)), Decision::Allowed));
        assert!(matches!(both.decide(inside, &HeaderMap::new()), Decision::NoToken));
        assert!(matches!(both.decide(outside, &bearer(token)), Decision::Forbidden));

        // A configuration that does not check out switches the metrics off.
        assert!(both.configure(&config(true, "", &[])).is_err());
        assert!(matches!(both.decide(inside, &bearer(token)), Decision::Off));
        assert!(both.configure(&config(false, token, &[])).is_ok());
        assert!(matches!(both.decide(inside, &bearer(token)), Decision::Off));
    }

    #[test]
    fn labels_are_escaped() {
        assert_eq!(escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
