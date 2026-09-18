//! The traffic light on the server overview: DNS, certificate, outgoing mail and storage.
//!
//! Cheap checks run on every request. DNS checks and the delivery probe run in the background
//! ([`Web::run_health_checks`]) or when an admin asks for them.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use uwumail_smtp::dnscheck::CheckStatus;
use uwumail_smtp::health::{DeliverySummary, ProbeStage, Route};
use uwumail_store::QueueRecipientStatus;

use crate::Web;
use crate::error::ApiResult;
use crate::gateway::{GatewayState, GatewayView};
use crate::routes::domains::run_check;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const DNS_INTERVAL: i64 = 6 * HOUR;
/// A relay is the admin's own server, so it is asked more often than a stranger's port 25.
const RELAY_PROBE_INTERVAL: i64 = HOUR;
const DIRECT_PROBE_INTERVAL: i64 = 6 * HOUR;
/// How long the tunnel to the gateway may be down before the light turns red.
const GATEWAY_GRACE: i64 = 300;

pub(crate) fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

/// The certificate the server presents right now.
#[derive(Debug, Clone)]
pub struct CertificateStatus {
    pub not_after: i64,
    pub names: Vec<String>,
    pub self_signed: bool,
    /// The server renews it itself (Let's Encrypt).
    pub automatic: bool,
}

pub type CertificateSource = Arc<dyn Fn() -> Option<CertificateStatus> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    /// Not checked yet, or the check itself failed.
    Unknown,
    Warning,
    Problem,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Stable code the portal turns into a sentence.
    pub code: &'static str,
    pub level: Level,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub params: Value,
    /// The portal page where it can be fixed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Finding {
    fn new(code: &'static str, level: Level, params: Value) -> Finding {
        Finding { code, level, params, link: None }
    }

    fn link(mut self, link: impl Into<String>) -> Finding {
        self.link = Some(link.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Area {
    pub area: &'static str,
    pub level: Level,
    pub findings: Vec<Finding>,
}

impl Area {
    fn new(area: &'static str, findings: Vec<Finding>) -> Area {
        let level = findings.iter().map(|finding| finding.level).max().unwrap_or(Level::Ok);
        Area { area, level, findings }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub level: Level,
    /// When the background checks last ran.
    pub checked_at: Option<i64>,
    pub areas: Vec<Area>,
}

/// `viewer` is the admin looking at it, so their own findings can link to their own settings.
pub(crate) async fn health(web: &Web, viewer: &str) -> ApiResult<Health> {
    let now = unix_now();
    let mut areas = vec![dns_area(web).await?];
    if let Some(certificate) = &web.settings().certificate {
        areas.push(certificate_area(&web.settings().hostname, certificate(), now));
    }
    if let Some(area) = web.gateway().and_then(|gateway| gateway_area(&gateway.view(), now)) {
        areas.push(area);
    }
    areas.push(delivery_area(web, now).await?);
    if let Some(area) = antivirus_area(web, now).await {
        areas.push(area);
    }
    areas.push(storage_area(web).await?);
    areas.push(security_area(web, viewer).await?);
    let level = areas.iter().map(|area| area.level).max().unwrap_or(Level::Ok);
    Ok(Health { level, checked_at: web.last_health_check(), areas })
}

async fn dns_area(web: &Web) -> ApiResult<Area> {
    let domains = web.store().domains().await?;
    if domains.is_empty() {
        return Ok(Area::new(
            "dns",
            vec![Finding::new("noDomains", Level::Warning, Value::Null).link("/admin/domains")],
        ));
    }
    let mut findings = Vec::new();
    let mut pending = 0;
    let own = crate::routes::reports::own_sending_addresses(web).await;
    for domain in &domains {
        // What other servers reported about the last week.
        let (tls_failed, dmarc_failed) = crate::routes::reports::problems(web, &domain.name, &own).await?;
        // Reports have their own section now; the domain's page keeps the detail, so a finding that
        // names a number points at the place that shows every domain's numbers together.
        if tls_failed > 0 {
            let params = json!({ "domain": domain.name, "count": tls_failed });
            findings.push(Finding::new("tlsFailures", Level::Warning, params).link("/admin/reports"));
        }
        if dmarc_failed > 0 {
            let params = json!({ "domain": domain.name, "count": dmarc_failed });
            findings.push(Finding::new("dmarcOwnFailures", Level::Warning, params).link("/admin/reports"));
        }
        let Some(report) = web.report(&domain.name) else {
            pending += 1;
            continue;
        };
        let level = match report.status {
            CheckStatus::Ok => continue,
            CheckStatus::Warning => Level::Warning,
            CheckStatus::Missing | CheckStatus::Wrong => Level::Problem,
            CheckStatus::Error => Level::Unknown,
        };
        let params = json!({ "domain": domain.name, "status": report.status });
        findings.push(Finding::new("dnsDomain", level, params).link(format!("/admin/domains/{}", domain.name)));
    }
    if pending > 0 {
        let code = if web.dns().is_some() { "dnsPending" } else { "dnsUnavailable" };
        findings.push(Finding::new(code, Level::Unknown, json!({ "count": pending })));
    }
    if findings.is_empty() {
        findings.push(Finding::new("dnsOk", Level::Ok, json!({ "count": domains.len() })));
    }
    Ok(Area::new("dns", findings))
}

pub(crate) fn covers(names: &[String], hostname: &str) -> bool {
    names.iter().any(|name| {
        name.eq_ignore_ascii_case(hostname)
            || name.strip_prefix("*.").is_some_and(|parent| {
                hostname.split_once('.').is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(parent))
            })
    })
}

fn certificate_area(hostname: &str, certificate: Option<CertificateStatus>, now: i64) -> Area {
    let Some(cert) = certificate else {
        return Area::new("certificate", vec![Finding::new("certMissing", Level::Problem, Value::Null)]);
    };
    let days = (cert.not_after - now).div_euclid(DAY);
    let mut findings = Vec::new();
    if cert.self_signed {
        let code = if cert.automatic { "certWaiting" } else { "certSelfSigned" };
        findings.push(Finding::new(code, Level::Warning, Value::Null));
    } else {
        if !covers(&cert.names, hostname) {
            findings.push(Finding::new("certWrongName", Level::Problem, json!({ "hostname": hostname })));
        }
        let params = json!({ "days": days, "notAfter": cert.not_after });
        // Let's Encrypt certificates are renewed 30 days ahead, so fewer days left means renewals fail.
        let (problem_days, warning_days) = if cert.automatic { (7, 20) } else { (3, 14) };
        if cert.not_after <= now {
            findings.push(Finding::new("certExpired", Level::Problem, params));
        } else if days < problem_days {
            findings.push(Finding::new("certExpiresSoon", Level::Problem, params));
        } else if days < warning_days {
            findings.push(Finding::new("certExpiresSoon", Level::Warning, params));
        } else if findings.is_empty() {
            let code = if cert.automatic { "certOkAutomatic" } else { "certOk" };
            findings.push(Finding::new(code, Level::Ok, params));
        }
    }
    Area::new("certificate", findings)
}

fn route_finding(summary: &DeliverySummary) -> Finding {
    let host = summary.relay_host.clone().unwrap_or_default();
    let delivered_after = |at: i64| summary.last_delivered_at.is_some_and(|delivered| delivered >= at);
    let failed_probe = summary.probe.as_ref().filter(|probe| !probe.ok && !delivered_after(probe.at));
    match summary.route {
        Route::Relay => {
            if let Some(trouble) = &summary.last_trouble {
                let code = if trouble.stage == ProbeStage::Login { "relayLogin" } else { "relayUnreachable" };
                return Finding::new(
                    code,
                    Level::Problem,
                    json!({ "host": host, "error": trouble.error, "at": trouble.at }),
                )
                .link("/admin/settings");
            }
            if let Some(probe) = failed_probe {
                let code = match probe.stage {
                    Some(ProbeStage::Login) => "relayLogin",
                    Some(ProbeStage::Tls) => "relayTls",
                    _ => "relayUnreachable",
                };
                return Finding::new(
                    code,
                    Level::Problem,
                    json!({ "host": host, "error": probe.error, "at": probe.at }),
                )
                .link("/admin/settings");
            }
        }
        Route::Direct | Route::Gateway => {
            let gateway = summary.route == Route::Gateway;
            if summary.unreachable_domains >= 3 {
                let error = summary.last_trouble.as_ref().map(|trouble| trouble.error.clone());
                let params = json!({ "count": summary.unreachable_domains, "error": error });
                let code = if gateway { "gatewayOutboundBlocked" } else { "outboundBlocked" };
                return Finding::new(code, Level::Problem, params).link("/admin/settings");
            }
            if let Some(probe) = failed_probe {
                let (code, level) = match probe.stage {
                    Some(ProbeStage::Dns) => ("probeDns", Level::Warning),
                    _ if gateway => ("gatewayPort25Blocked", Level::Problem),
                    _ => ("port25Blocked", Level::Problem),
                };
                let params = json!({ "target": probe.target, "error": probe.error, "at": probe.at });
                return Finding::new(code, level, params).link("/admin/settings");
            }
        }
    }
    let probed_ok = summary.probe.as_ref().filter(|probe| probe.ok).map(|probe| probe.at);
    let params = json!({ "host": host, "lastDeliveredAt": summary.last_delivered_at, "probedAt": probed_ok });
    match (summary.route, summary.last_delivered_at.or(probed_ok)) {
        (_, None) => Finding::new("deliveryNotChecked", Level::Unknown, params),
        (Route::Relay, Some(_)) => Finding::new("relayOk", Level::Ok, params),
        (Route::Direct, Some(_)) => Finding::new("directOk", Level::Ok, params),
        (Route::Gateway, Some(_)) => Finding::new("gatewayOk", Level::Ok, params),
    }
}

/// The tunnel to the UwUMail Gateway, when there is one.
fn gateway_area(view: &GatewayView, now: i64) -> Option<Area> {
    let params = json!({
        "addresses": view.addresses,
        "connectedSince": view.connected_since,
        "downSince": view.down_since,
        "error": view.error,
        "refusal": view.refusal,
    });
    let finding = match view.state {
        GatewayState::None => return None,
        GatewayState::Connected => Finding::new("gatewayConnected", Level::Ok, params),
        GatewayState::Refused => Finding::new("gatewayRefused", Level::Problem, params),
        // Reconnecting after a new home address takes seconds; minutes mean something is wrong.
        GatewayState::Connecting => {
            let down_for = view.down_since.map_or(0, |since| now - since);
            Finding::new("gatewayDown", if down_for > GATEWAY_GRACE { Level::Problem } else { Level::Warning }, params)
        }
    };
    Some(Area::new("gateway", vec![finding.link("/admin/setup")]))
}

async fn delivery_area(web: &Web, now: i64) -> ApiResult<Area> {
    let summary = web.smtp().delivery_summary();
    let mut findings = vec![route_finding(&summary)];

    let entries = web.store().queue_entries().await?;
    let stuck: Vec<i64> = entries
        .iter()
        .filter(|entry| {
            entry.message.created_at < now - HOUR
                && entry.recipients.iter().any(|r| r.status == QueueRecipientStatus::Pending && r.attempts > 0)
        })
        .map(|entry| entry.message.created_at)
        .collect();
    if let Some(&oldest) = stuck.iter().min() {
        let level = if oldest < now - DAY { Level::Problem } else { Level::Warning };
        let params = json!({ "count": stuck.len(), "oldestAt": oldest, "ageSecs": now - oldest });
        findings.push(Finding::new("queueStuck", level, params).link("/admin/queue"));
    }

    // A few bounces are normal (typos); many mean other servers refuse our mail.
    let finished = summary.delivered + summary.failed;
    if summary.failed >= 3 && summary.failed * 4 >= finished {
        let params = json!({ "count": summary.failed, "total": finished });
        findings.push(Finding::new("manyBounces", Level::Warning, params));
    }
    Ok(Area::new("delivery", findings))
}

/// How old the scanner's signatures may be before that is worth saying. ClamAV publishes several
/// times a day, so a database this old means its updater is not running.
pub(crate) const SIGNATURES_OLD: i64 = 3 * DAY;

/// Only there while the virus scanner is switched on, and only saying anything when it cannot do
/// its job: a scanner that is quietly away would otherwise let mail through unchecked for weeks.
async fn antivirus_area(web: &Web, now: i64) -> Option<Area> {
    let mut findings = Vec::new();
    match web.smtp().virus_status().await? {
        Err(error) => {
            let params = json!({ "error": error });
            findings.push(Finding::new("virusScannerAway", Level::Problem, params).link("/admin/spam/antivirus"));
        }
        Ok(status) => {
            if let Some(built) = status.signatures_at.filter(|built| *built < now - SIGNATURES_OLD) {
                let params = json!({ "ageSecs": now - built, "at": built });
                findings.push(Finding::new("virusSignaturesOld", Level::Warning, params).link("/admin/spam/antivirus"));
            }
        }
    }
    Some(Area::new("antivirus", findings))
}

#[cfg(unix)]
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some((stat.f_bavail.saturating_mul(stat.f_frsize), stat.f_blocks.saturating_mul(stat.f_frsize)))
}

#[cfg(not(unix))]
fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

fn disk_level(free: u64, total: u64) -> Level {
    if free < 500 * MIB || free.saturating_mul(100) < total.saturating_mul(3) {
        Level::Problem
    } else if free < 2 * GIB || free.saturating_mul(10) < total {
        Level::Warning
    } else {
        Level::Ok
    }
}

async fn storage_area(web: &Web) -> ApiResult<Area> {
    let mut findings = Vec::new();
    if let Some((free, total)) = disk_space(web.store().data_dir()) {
        let level = disk_level(free, total);
        let code = if level == Level::Ok { "diskOk" } else { "diskLow" };
        findings.push(Finding::new(code, level, json!({ "freeBytes": free, "totalBytes": total })));
    }
    let accounts = web.store().accounts().await?;
    let nearly_full: Vec<_> = accounts
        .iter()
        .filter(|a| a.deleted_at.is_none() && a.quota_bytes > 0 && a.used_bytes.saturating_mul(10) >= a.quota_bytes * 9)
        .collect();
    match nearly_full.as_slice() {
        [] if findings.is_empty() => findings.push(Finding::new("mailboxesOk", Level::Ok, Value::Null)),
        [] => {}
        [only] => findings.push(
            Finding::new("mailboxesNearlyFull", Level::Warning, json!({ "count": 1, "login": only.login }))
                .link(format!("/admin/people/{}", only.login)),
        ),
        [first, ..] => findings.push(
            Finding::new(
                "mailboxesNearlyFull",
                Level::Warning,
                json!({ "count": nearly_full.len(), "login": first.login }),
            )
            .link("/admin/people"),
        ),
    }
    Ok(Area::new("storage", findings))
}

async fn security_area(web: &Web, viewer: &str) -> ApiResult<Area> {
    let without = web.store().admins_without_second_factor().await?;
    let finding = match without.as_slice() {
        [] => Finding::new("adminsSecure", Level::Ok, Value::Null),
        [only] if only == viewer => {
            Finding::new("youWithoutSecondFactor", Level::Warning, Value::Null).link("/account/security")
        }
        [only] => Finding::new("adminsWithoutSecondFactor", Level::Warning, json!({ "count": 1, "login": only }))
            .link(format!("/admin/people/{only}")),
        [first, ..] => {
            let link = if without.iter().any(|login| login == viewer) { "/account/security" } else { "/admin/people" };
            let params =
                json!({ "count": without.len(), "login": first, "includesYou": without.iter().any(|l| l == viewer) });
            Finding::new("adminsWithoutSecondFactor", Level::Warning, params).link(link)
        }
    };
    Ok(Area::new("security", vec![finding]))
}

impl Web {
    /// Checks DNS and outgoing mail regularly until `shutdown` changes.
    pub async fn run_health_checks(self, mut shutdown: watch::Receiver<bool>) {
        // Let the listeners come up first.
        let mut next_dns = unix_now() + 30;
        let mut next_probe = unix_now() + 60;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                _ = shutdown.changed() => return,
            }
            let now = unix_now();
            if now >= next_dns {
                self.check_all_domains().await;
                next_dns = now + DNS_INTERVAL;
                self.mark_health_checked();
            }
            if now >= next_probe {
                let summary = self.smtp().delivery_summary();
                let interval = if summary.route == Route::Relay { RELAY_PROBE_INTERVAL } else { DIRECT_PROBE_INTERVAL };
                // Mail that went out recently already shows the way is open.
                if summary.last_delivered_at.is_none_or(|at| at < now - interval) {
                    self.smtp().probe_delivery().await;
                }
                next_probe = now + interval;
                self.mark_health_checked();
            }
        }
    }

    /// Runs the background checks right away, unless they just ran.
    pub(crate) async fn check_health_now(&self) {
        let _running = self.inner.health_check.lock().await;
        if self.last_health_check().is_some_and(|at| at > unix_now() - 15) {
            return;
        }
        tokio::join!(self.check_all_domains(), self.smtp().probe_delivery());
        self.mark_health_checked();
    }

    async fn check_all_domains(&self) {
        let domains = match self.store().domains().await {
            Ok(domains) => domains,
            Err(err) => {
                tracing::warn!(%err, "listing domains for the DNS check failed");
                return;
            }
        };
        for domain in domains {
            if let Err(err) = run_check(self, &domain.name).await {
                tracing::debug!(?err, domain = %domain.name, "DNS check failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(days: i64, automatic: bool) -> CertificateStatus {
        CertificateStatus {
            not_after: 1_000_000 + days * DAY,
            names: vec!["*.example.de".into()],
            self_signed: false,
            automatic,
        }
    }

    #[test]
    fn certificates_warn_before_they_expire() {
        let codes = |area: Area| (area.level, area.findings.iter().map(|f| f.code).collect::<Vec<_>>());
        let host = "mail.example.de";
        assert_eq!(
            codes(certificate_area(host, Some(cert(60, true)), 1_000_000)),
            (Level::Ok, vec!["certOkAutomatic"])
        );
        assert_eq!(codes(certificate_area(host, Some(cert(15, true)), 1_000_000)).0, Level::Warning);
        assert_eq!(codes(certificate_area(host, Some(cert(15, false)), 1_000_000)).0, Level::Ok);
        assert_eq!(
            codes(certificate_area(host, Some(cert(-1, false)), 1_000_000)),
            (Level::Problem, vec!["certExpired"])
        );
        assert_eq!(
            codes(certificate_area("mail.other.de", Some(cert(60, false)), 1_000_000)),
            (Level::Problem, vec!["certWrongName"])
        );
        assert_eq!(codes(certificate_area(host, None, 0)).0, Level::Problem);
    }

    #[test]
    fn disks_fill_up_in_steps() {
        assert_eq!(disk_level(50 * GIB, 100 * GIB), Level::Ok);
        assert_eq!(disk_level(8 * GIB, 100 * GIB), Level::Warning);
        assert_eq!(disk_level(1500 * MIB, 10 * GIB), Level::Warning);
        assert_eq!(disk_level(2 * GIB, 100 * GIB), Level::Problem);
        assert_eq!(disk_level(400 * MIB, GIB), Level::Problem);
    }
}
