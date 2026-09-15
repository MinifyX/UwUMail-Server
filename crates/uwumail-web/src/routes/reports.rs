//! MTA-STS for a domain, the policy file senders fetch, and the DMARC and TLS reports other
//! servers send about the domain.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::dnscheck::DomainReport;
use uwumail_smtp::mta_sts::Policy;
use uwumail_store::{DmarcSummary, MtaStsMode, MtaStsSettings, TlsSummary};

use super::audit;
use super::domains::{detail_json, load, run_check};
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::health::{covers, unix_now};
use crate::session::Admin;

const DAY: i64 = 24 * 3600;
/// How long things have to look good before the portal suggests the stricter setting.
const SETTLE_SECS: i64 = 14 * DAY;

/// The host a request was sent to, without the port.
fn request_host(uri: &Uri, headers: &HeaderMap) -> Option<String> {
    let host = uri.host().map(str::to_owned).or_else(|| {
        let value = headers.get(header::HOST)?.to_str().ok()?;
        Some(
            value
                .rsplit_once(':')
                .filter(|(_, port)| port.parse::<u16>().is_ok())
                .map_or(value, |(host, _)| host)
                .to_owned(),
        )
    })?;
    Some(host.trim_end_matches('.').to_ascii_lowercase())
}

/// `https://mta-sts.<domain>/.well-known/mta-sts.txt` for our domains with MTA-STS on.
pub async fn policy(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "no MTA-STS policy here\n").into_response();
    let Some(domain) = request_host(&uri, &headers).and_then(|host| host.strip_prefix("mta-sts.").map(str::to_owned))
    else {
        return not_found();
    };
    match web.store().mta_sts(&domain).await {
        Ok(Some(settings)) => {
            let policy = Policy::ours(settings.mode, &settings.mx);
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], policy.to_text()).into_response()
        }
        Ok(None) | Err(uwumail_store::StoreError::Invalid(_)) => not_found(),
        Err(err) => {
            tracing::error!(%err, %domain, "loading the MTA-STS policy failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "try again later\n").into_response()
        }
    }
}

pub(crate) fn mta_sts_json(settings: Option<MtaStsSettings>) -> Value {
    match settings {
        None => Value::Null,
        Some(settings) => {
            let policy = Policy::ours(settings.mode, &settings.mx);
            json!({
                "mode": settings.mode,
                "mx": settings.mx,
                "changedAt": settings.changed_at,
                "policy": policy.to_text(),
                "id": policy.id(),
            })
        }
    }
}

/// The MX names the policy lists: this server, and when another server receives first, the
/// MX hosts the domain publishes.
fn policy_mx(hostname: &str, upstream: bool, report: Option<&DomainReport>) -> Vec<String> {
    let mut names = vec![hostname.trim_end_matches('.').to_ascii_lowercase()];
    if upstream && let Some(report) = report {
        for record in report.records.iter().filter(|record| record.kind == "mx") {
            for found in &record.found {
                if let Some((_, host)) = found.split_once(' ') {
                    let host = host.trim_end_matches('.').to_ascii_lowercase();
                    if !host.is_empty() && !names.contains(&host) {
                        names.push(host);
                    }
                }
            }
        }
    }
    names
}

#[derive(Deserialize)]
pub struct ModeRequest {
    /// "off", "testing" or "enforce".
    mode: String,
}

pub async fn set_mode(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(request): Json<ModeRequest>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let hostname = web.settings().hostname.clone();
    let upstream = web.smtp().behind_upstream_server();
    let settings = match request.mode.as_str() {
        "off" => None,
        mode => {
            let mode = MtaStsMode::parse(mode).ok_or_else(|| ApiError::Invalid(format!("unknown mode {mode}")))?;
            // Senders will insist on a valid certificate for our MX, so it has to be one.
            if mode == MtaStsMode::Enforce && !upstream {
                let trusted = web
                    .settings()
                    .certificate
                    .as_ref()
                    .and_then(|source| source())
                    .is_some_and(|cert| !cert.self_signed && covers(&cert.names, &hostname));
                if !trusted {
                    return Err(ApiError::Rule(
                        "mtaStsCertificate",
                        format!("the certificate is not valid for {hostname} yet"),
                    ));
                }
            }
            let report = if upstream { Some(run_check(&web, &domain.name).await?) } else { None };
            Some((mode, policy_mx(&hostname, upstream, report.as_ref())))
        }
    };
    let details = json!({
        "mode": settings.as_ref().map(|(mode, _)| mode.as_str()).unwrap_or("off"),
        "mx": settings.as_ref().map(|(_, mx)| mx.clone()),
    });
    web.store().set_mta_sts(&domain.name, settings).await?;
    audit(&web, &session, "domain.mtaSts", &domain.name, details).await;
    web.forget_report(&domain.name);
    Ok(Json(detail_json(&web, &domain.name).await?))
}

/// The addresses mail from this server leaves from: the server itself, or the relay.
async fn own_addresses(web: &Web) -> HashSet<String> {
    let mut addresses = HashSet::new();
    let Some(dns) = web.dns() else { return addresses };
    let mut hosts = vec![web.settings().hostname.clone()];
    hosts.extend(web.smtp().relay_host());
    for host in hosts {
        addresses.extend(dns.host_addresses(&host).await.into_iter().map(|ip| ip.to_string()));
    }
    addresses
}

/// Messages from our own addresses that did and did not pass DMARC.
fn own_results(summary: &DmarcSummary, own: &HashSet<String>) -> (i64, i64) {
    summary
        .sources
        .iter()
        .filter(|source| own.contains(&source.ip))
        .fold((0, 0), |(passed, failed), source| (passed + source.passed, failed + source.messages - source.passed))
}

fn dmarc_policy(report: Option<&DomainReport>) -> Option<String> {
    let record = report?.records.iter().find(|record| record.kind == "dmarc")?;
    let text = record.found.first()?;
    text.split(';')
        .filter_map(|tag| tag.trim().split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("p"))
        .map(|(_, value)| value.trim().to_ascii_lowercase())
}

pub(crate) struct SuggestionInput<'a> {
    pub mta_sts: Option<&'a MtaStsSettings>,
    /// TLS reports since MTA-STS last changed.
    pub tls_since_change: &'a TlsSummary,
    /// DMARC reports of the settling time.
    pub dmarc: &'a DmarcSummary,
    pub own: &'a HashSet<String>,
    pub dmarc_policy: Option<String>,
}

/// What the portal can recommend once enough reports looked good.
pub(crate) fn suggestions(input: &SuggestionInput<'_>, now: i64) -> Vec<Value> {
    let mut found = Vec::new();
    if let Some(settings) = input.mta_sts
        && settings.mode == MtaStsMode::Testing
        && now - settings.changed_at >= SETTLE_SECS
        && input.tls_since_change.reports > 0
        && input.tls_since_change.failed == 0
    {
        found.push(json!({ "code": "mtaStsEnforce" }));
    }
    let (own_passed, own_failed) = own_results(input.dmarc, input.own);
    let covered = input.dmarc.first_begin.is_some_and(|begin| begin <= now - SETTLE_SECS + DAY);
    let next = match input.dmarc_policy.as_deref() {
        Some("none") => Some("quarantine"),
        Some("quarantine") => Some("reject"),
        _ => None,
    };
    if let Some(next) = next
        && covered
        && own_passed > 0
        && own_failed == 0
    {
        found.push(json!({ "code": "dmarcStricter", "params": { "from": input.dmarc_policy, "to": next } }));
    }
    found
}

#[derive(Deserialize)]
pub struct ReportQuery {
    days: Option<i64>,
}

pub async fn domain_reports(
    State(web): State<Web>,
    _admin: Admin,
    Path(name): Path<String>,
    Query(query): Query<ReportQuery>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let now = unix_now();
    let days = query.days.unwrap_or(30).clamp(1, 180);
    let store = web.store();
    let summary = store.report_summary(&domain.name, now - days * DAY).await?;
    let own = own_addresses(&web).await;

    let mta_sts = store.mta_sts(&domain.name).await?;
    let since_change = mta_sts.as_ref().map_or(now, |settings| settings.changed_at);
    let tls_since_change = store.report_summary(&domain.name, since_change).await?.tls;
    let settling = store.report_summary(&domain.name, now - SETTLE_SECS - DAY).await?.dmarc;
    let report = web.report(&domain.name);
    let suggestions = suggestions(
        &SuggestionInput {
            mta_sts: mta_sts.as_ref(),
            tls_since_change: &tls_since_change,
            dmarc: &settling,
            own: &own,
            dmarc_policy: dmarc_policy(report.as_ref()),
        },
        now,
    );

    let mut dmarc = serde_json::to_value(&summary.dmarc).map_err(|_| ApiError::Internal)?;
    dmarc["sources"] = summary
        .dmarc
        .sources
        .iter()
        .map(|source| {
            let mut value = json!(source);
            value["ours"] = json!(own.contains(&source.ip));
            value
        })
        .collect();
    Ok(Json(json!({ "days": days, "dmarc": dmarc, "tls": summary.tls, "suggestions": suggestions })))
}

/// For the traffic light: TLS failures and own mail failing DMARC in the last week.
pub(crate) async fn problems(web: &Web, domain: &str, own: &HashSet<String>) -> ApiResult<(i64, i64)> {
    let week = web.store().report_summary(domain, unix_now() - 7 * DAY).await?;
    let (_, own_failed) = own_results(&week.dmarc, own);
    Ok((week.tls.failed, own_failed))
}

pub(crate) async fn own_sending_addresses(web: &Web) -> HashSet<String> {
    own_addresses(web).await
}

#[cfg(test)]
mod tests {
    use uwumail_store::{DmarcSource, TlsSummary};

    use super::*;

    #[test]
    fn hosts_come_from_the_uri_or_the_host_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "MTA-STS.Example.de:8443".parse().unwrap());
        assert_eq!(request_host(&"/.well-known/mta-sts.txt".parse().unwrap(), &headers).unwrap(), "mta-sts.example.de");
        let uri: Uri = "https://mta-sts.example.de/.well-known/mta-sts.txt".parse().unwrap();
        assert_eq!(request_host(&uri, &HeaderMap::new()).unwrap(), "mta-sts.example.de");
        headers.insert(header::HOST, "[2001:db8::1]:443".parse().unwrap());
        assert_eq!(request_host(&"/".parse().unwrap(), &headers).unwrap(), "[2001:db8::1]");
    }

    #[test]
    fn policies_list_the_upstream_mx_hosts_too() {
        assert_eq!(policy_mx("Mail.Example.de.", false, None), vec!["mail.example.de".to_owned()]);
    }

    #[test]
    fn stricter_settings_are_suggested_once_things_settled() {
        let now = 100 * DAY;
        let settings = MtaStsSettings {
            mode: MtaStsMode::Testing,
            mx: vec!["mail.example.de".into()],
            changed_at: now - 15 * DAY,
        };
        let clean_tls = TlsSummary { reports: 3, successful: 40, ..TlsSummary::default() };
        let own: HashSet<String> = ["192.0.2.10".to_owned()].into();
        let dmarc = DmarcSummary {
            reports: 20,
            messages: 60,
            passed: 50,
            first_begin: Some(now - 14 * DAY),
            sources: vec![
                DmarcSource { ip: "192.0.2.10".into(), messages: 50, passed: 50, header_from: vec![] },
                DmarcSource { ip: "198.51.100.7".into(), messages: 10, passed: 0, header_from: vec![] },
            ],
            ..DmarcSummary::default()
        };
        let codes = |settings: &MtaStsSettings, tls: &TlsSummary, policy: &str| {
            let input = SuggestionInput {
                mta_sts: Some(settings),
                tls_since_change: tls,
                dmarc: &dmarc,
                own: &own,
                dmarc_policy: Some(policy.to_owned()),
            };
            suggestions(&input, now)
        };
        let names = |found: &[Value]| found.iter().map(|s| s["code"].as_str().unwrap().to_owned()).collect::<Vec<_>>();

        let found = codes(&settings, &clean_tls, "quarantine");
        assert_eq!(names(&found), ["mtaStsEnforce", "dmarcStricter"], "strangers failing DMARC do not matter");
        assert_eq!(found[1]["params"], json!({ "from": "quarantine", "to": "reject" }));

        let failing = TlsSummary { failed: 1, ..clean_tls.clone() };
        assert_eq!(names(&codes(&settings, &failing, "reject")), Vec::<String>::new());
        let fresh = MtaStsSettings { changed_at: now - DAY, ..settings.clone() };
        assert_eq!(names(&codes(&fresh, &clean_tls, "none")), ["dmarcStricter"]);
        assert_eq!(own_results(&dmarc, &own), (50, 0));
    }
}
