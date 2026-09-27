//! TLS reports this server sends to other domains (SMTP TLS Reporting, RFC 8460).
//!
//! Every delivery session to another domain's MX counts once, with the policy it followed
//! (MTA-STS, DANE or none) and whether TLS worked. Once a UTC day is over, each domain that
//! publishes a `_smtp._tls` record gets one report about it: by mail through the normal queue,
//! or posted to its https address. A report that could not be sent is tried once more the next
//! day. Our own domains never get one.

use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use flate2::Compression;
use flate2::write::GzEncoder;
use http_body_util::Full;
use hyper::Request;
use hyper::header::{CONTENT_TYPE, USER_AGENT};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use mail_auth::DnsError;
use mail_auth::mta_sts::{ReportUri, TlsRpt};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::text::Text;
use mail_builder::mime::MimePart;
use serde::Serialize;
use tokio::sync::watch;
use uwumail_store::{NewQueueRecipient, TlsRptDue, TlsRptOutcome, TlsSession, TlsSessionCount, tls_rpt_day};

use crate::dane::Dane;
use crate::egress::Egress;
use crate::mta_sts::Policy;
use crate::{Context, Smtp, dkim, now, random_id};

const DAY_SECS: i64 = 24 * 3600;
/// Reports sent in one round at most; the rest follow an hour later.
const MAX_REPORTS_PER_RUN: usize = 200;
/// Places one report goes to at most.
const MAX_DESTINATIONS: usize = 5;
/// Policies and failure details one report lists at most, most sessions first.
const MAX_POLICIES: usize = 50;
const MAX_FAILURE_DETAILS: usize = 50;
/// A packed report bigger than this is not sent at all.
const MAX_REPORT_BYTES: usize = 1024 * 1024;
const POST_TIMEOUT: Duration = Duration::from_secs(30);
/// The local part reports by mail come from.
const REPORT_SENDER: &str = "noreply-tls-reports";

/// The kinds of policy a session follows (RFC 8460, section 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyType {
    Sts,
    Tlsa,
    NoPolicyFound,
}

impl PolicyType {
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyType::Sts => "sts",
            PolicyType::Tlsa => "tlsa",
            PolicyType::NoPolicyFound => "no-policy-found",
        }
    }
}

/// Why a session failed, in the words of RFC 8460, section 4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultType {
    StarttlsNotSupported,
    CertificateHostMismatch,
    CertificateExpired,
    CertificateNotTrusted,
    ValidationFailure,
    TlsaInvalid,
    DnssecInvalid,
    DaneRequired,
    StsPolicyFetchError,
    StsPolicyInvalid,
    StsWebpkiInvalid,
}

impl ResultType {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultType::StarttlsNotSupported => "starttls-not-supported",
            ResultType::CertificateHostMismatch => "certificate-host-mismatch",
            ResultType::CertificateExpired => "certificate-expired",
            ResultType::CertificateNotTrusted => "certificate-not-trusted",
            ResultType::ValidationFailure => "validation-failure",
            ResultType::TlsaInvalid => "tlsa-invalid",
            ResultType::DnssecInvalid => "dnssec-invalid",
            ResultType::DaneRequired => "dane-required",
            ResultType::StsPolicyFetchError => "sts-policy-fetch-error",
            ResultType::StsPolicyInvalid => "sts-policy-invalid",
            ResultType::StsWebpkiInvalid => "sts-webpki-invalid",
        }
    }
}

/// The policy sessions to one MX host follow, as a report names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportPolicy {
    pub kind: PolicyType,
    pub strings: Vec<String>,
    pub mx: Vec<String>,
    /// Something wrong with the policy itself, which makes every session a failure.
    pub failure: Option<ResultType>,
}

impl ReportPolicy {
    /// DANE comes first, then MTA-STS; a policy that could not be fetched still counts as one.
    pub(crate) fn of(host: &str, dane: &Dane, sts: Option<&Policy>, sts_failure: Option<ResultType>) -> ReportPolicy {
        if dane.applies() {
            let failure = matches!(dane, Dane::Bogus).then_some(ResultType::DnssecInvalid);
            return ReportPolicy {
                kind: PolicyType::Tlsa,
                strings: dane.policy_strings(),
                mx: vec![host.to_owned()],
                failure,
            };
        }
        match (sts, sts_failure) {
            (Some(policy), _) => ReportPolicy {
                kind: PolicyType::Sts,
                strings: policy.to_text().lines().map(str::to_owned).collect(),
                mx: policy.mx.clone(),
                failure: None,
            },
            (None, Some(failure)) => {
                ReportPolicy { kind: PolicyType::Sts, strings: Vec::new(), mx: Vec::new(), failure: Some(failure) }
            }
            (None, None) => {
                ReportPolicy { kind: PolicyType::NoPolicyFound, strings: Vec::new(), mx: Vec::new(), failure: None }
            }
        }
    }
}

/// Counts one delivery session to `domain`'s MX `host` at `ip`. `result` is `None` when TLS
/// worked as the policy wants.
pub(crate) async fn record(
    ctx: &Context,
    domain: &str,
    policy: &ReportPolicy,
    result: Option<ResultType>,
    host: &str,
    ip: Option<IpAddr>,
    local_ip: Option<IpAddr>,
) {
    if !ctx.sends_tls_reports() {
        return;
    }
    let result = policy.failure.or(result);
    let text = |ip: Option<IpAddr>| ip.map(|ip| ip.to_string()).unwrap_or_default();
    // Where a session went wrong is worth telling; for the ones that worked, the number is enough.
    let failed = result.is_some();
    let session = TlsSession {
        day: tls_rpt_day(now()),
        policy_domain: domain.to_owned(),
        policy_type: policy.kind.as_str().to_owned(),
        policy_string: policy.strings.clone(),
        mx_host: policy.mx.clone(),
        result_type: result.map(|result| result.as_str().to_owned()),
        receiving_mx_hostname: if failed { host.trim_end_matches('.').to_ascii_lowercase() } else { String::new() },
        receiving_ip: if failed { text(ip) } else { String::new() },
        sending_ip: if failed { text(local_ip) } else { String::new() },
    };
    if let Err(err) = ctx.store.record_tls_session(session).await {
        tracing::warn!(%err, %domain, "counting a delivery session for the TLS reports failed");
    }
}

/// Sends the reports that are due every hour, and forgets old sessions. Reports posted to https
/// addresses leave through `egress`, straight from the server and only to public addresses.
pub async fn run_tls_reports(smtp: Smtp, egress: Egress, mut shutdown: watch::Receiver<bool>) {
    let ctx = smtp.inner.clone();
    // Not right at the start: a server restarting in a loop should not send in a loop.
    let mut wait = Duration::from_secs(600);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => return,
        }
        wait = Duration::from_secs(3600);
        let today = tls_rpt_day(now());
        if ctx.sends_tls_reports() {
            let sent = send_due(&ctx, &egress, today).await;
            if sent > 0 {
                tracing::info!(sent, "sent TLS reports to other domains");
            }
        }
        if let Err(err) = ctx.store.purge_tls_rpt(today).await {
            tracing::warn!(%err, "forgetting old TLS report sessions failed");
        }
    }
}

/// Sends every report that is due before `today`. Returns how many went out.
pub(crate) async fn send_due(ctx: &Context, egress: &Egress, today: i64) -> usize {
    let due = match ctx.store.tls_rpt_due(today).await {
        Ok(due) => due,
        Err(err) => {
            tracing::warn!(%err, "reading the TLS reports that are due failed");
            return 0;
        }
    };
    if due.is_empty() {
        return 0;
    }
    let local: HashSet<String> = match ctx.store.domains().await {
        Ok(domains) => domains.into_iter().map(|domain| domain.name.to_ascii_lowercase()).collect(),
        Err(err) => {
            tracing::warn!(%err, "reading our domains failed, no TLS reports this time");
            return 0;
        }
    };
    let mut sent = 0;
    for item in due.into_iter().take(MAX_REPORTS_PER_RUN) {
        let outcome = report(ctx, egress, &item, &local).await;
        match outcome.status.as_str() {
            "sent" => sent += 1,
            "failed" => {
                tracing::info!(domain = %item.policy_domain, error = %outcome.error, "a TLS report could not be sent")
            }
            _ => {}
        }
        if let Err(err) = ctx.store.tls_rpt_done(outcome).await {
            tracing::warn!(%err, "noting a sent TLS report failed");
        }
    }
    sent
}

/// The address reports by mail come from: at our domain the host name belongs to, or at the host
/// name itself when it belongs to none of `domains`.
pub fn report_sender(hostname: &str, domains: &[String]) -> String {
    let local: HashSet<String> = domains.iter().map(|domain| domain.to_ascii_lowercase()).collect();
    let submitter =
        host_domain(hostname, &local).unwrap_or_else(|| hostname.trim_end_matches('.').to_ascii_lowercase());
    format!("{REPORT_SENDER}@{submitter}")
}

/// Our domain the server's host name belongs to, if it is one: reports come from there.
fn host_domain(hostname: &str, local: &HashSet<String>) -> Option<String> {
    let mut name = hostname.trim_end_matches('.').to_ascii_lowercase();
    loop {
        if local.contains(&name) {
            return Some(name);
        }
        name = name.split_once('.')?.1.to_owned();
    }
}

fn is_local(domain: &str, local: &HashSet<String>) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    local.iter().any(|ours| domain == *ours || domain.ends_with(&format!(".{ours}")))
}

/// The addresses in a `mailto:` of the `rua` list: without parameters, and only ones that look like one.
fn mail_addresses(uri: &str) -> Vec<String> {
    let uri = uri.trim().trim_start_matches("mailto:");
    let uri = uri.split('?').next().unwrap_or_default();
    uri.split(',')
        .map(|address| address.trim().replace("%40", "@"))
        .filter(|address| {
            address.len() <= 254
                && address.split('@').count() == 2
                && !address.starts_with('@')
                && !address.ends_with('@')
                && !address.contains(|c: char| c.is_whitespace() || c.is_control() || "<>\"(),;:\\[]".contains(c))
        })
        .collect()
}

async fn report(ctx: &Context, egress: &Egress, item: &TlsRptDue, local: &HashSet<String>) -> TlsRptOutcome {
    let domain = item.policy_domain.clone();
    let date = mail_parser::DateTime::from_timestamp(item.day * DAY_SECS).to_rfc3339();
    let report_id = item.report_id.clone().unwrap_or_else(|| {
        format!("{}.{}.{}@{}", date.get(..10).unwrap_or_default(), domain, random_id(), ctx.hostname)
    });
    let sessions = ctx.store.tls_rpt_sessions(item.day, &domain).await.unwrap_or_default();
    let successful = sessions.iter().filter(|row| row.session.result_type.is_none()).map(|row| row.count).sum();
    let failed = sessions.iter().filter(|row| row.session.result_type.is_some()).map(|row| row.count).sum();
    let outcome = |status: &str, destinations: Vec<String>, error: String| TlsRptOutcome {
        day: item.day,
        policy_domain: domain.clone(),
        report_id: report_id.clone(),
        status: status.to_owned(),
        destinations,
        error,
        successful,
        failed,
    };
    if sessions.is_empty() || is_local(&domain, local) {
        return outcome("skipped", Vec::new(), String::new());
    }

    let record = ctx.authenticator.txt_lookup::<TlsRpt>(format!("_smtp._tls.{domain}."), Some(&ctx.dns.txt)).await;
    let destinations: Vec<ReportUri> = match record {
        Ok(record) => record.rua.iter().take(MAX_DESTINATIONS).cloned().collect(),
        Err(mail_auth::Error::Dns(DnsError::RecordNotFound(_) | DnsError::InvalidRecordType)) => {
            return outcome("none", Vec::new(), String::new());
        }
        Err(err) => return outcome("failed", Vec::new(), format!("looking up _smtp._tls.{domain}: {err}")),
    };
    if destinations.is_empty() {
        return outcome("none", Vec::new(), String::new());
    }

    let submitter = host_domain(&ctx.hostname, local).unwrap_or_else(|| ctx.hostname.clone());
    let brand = ctx.brand();
    let organization = if brand.name.trim().is_empty() { ctx.hostname.clone() } else { brand.name().to_owned() };
    let json = report_json(&organization, &format!("postmaster@{submitter}"), &report_id, item.day, &domain, &sessions);
    let packed = match gzip(json.as_bytes()) {
        Ok(packed) if packed.len() <= MAX_REPORT_BYTES => packed,
        Ok(_) => return outcome("failed", Vec::new(), "the report is too big".into()),
        Err(err) => return outcome("failed", Vec::new(), format!("packing the report failed: {err}")),
    };

    let mut shown = Vec::new();
    let mut errors = Vec::new();
    let mut delivered = false;
    let mut recipients = Vec::new();
    for destination in &destinations {
        match destination {
            ReportUri::Mail(uri) => {
                for address in mail_addresses(uri) {
                    let target = address.rsplit_once('@').map(|(_, domain)| domain).unwrap_or_default();
                    if is_local(target, local) || recipients.contains(&address) {
                        continue;
                    }
                    shown.push(format!("mailto:{address}"));
                    recipients.push(address);
                }
            }
            ReportUri::Http(url) => {
                shown.push(url.clone());
                match post(egress, url, packed.clone()).await {
                    Ok(()) => delivered = true,
                    Err(error) => errors.push(format!("{url}: {error}")),
                }
            }
        }
    }
    if !recipients.is_empty() {
        let window = (item.day * DAY_SECS, item.day * DAY_SECS + DAY_SECS - 1);
        match mail(ctx, &submitter, &domain, &report_id, window, &recipients, packed).await {
            Ok(()) => delivered = true,
            Err(error) => errors.push(error),
        }
    }
    match (delivered, shown.is_empty()) {
        (true, _) => outcome("sent", shown, errors.join("; ")),
        (false, true) => outcome("none", shown, String::new()),
        (false, false) => outcome("failed", shown, errors.join("; ")),
    }
}

#[derive(Serialize)]
struct Report<'a> {
    #[serde(rename = "organization-name")]
    organization_name: &'a str,
    #[serde(rename = "date-range")]
    date_range: DateRange,
    #[serde(rename = "contact-info")]
    contact_info: &'a str,
    #[serde(rename = "report-id")]
    report_id: &'a str,
    policies: Vec<PolicyReport>,
}

#[derive(Serialize)]
struct DateRange {
    #[serde(rename = "start-datetime")]
    start: String,
    #[serde(rename = "end-datetime")]
    end: String,
}

#[derive(Serialize)]
struct PolicyReport {
    policy: PolicyDetails,
    summary: Summary,
    #[serde(rename = "failure-details", skip_serializing_if = "Vec::is_empty")]
    failure_details: Vec<FailureDetails>,
}

#[derive(Serialize)]
struct PolicyDetails {
    #[serde(rename = "policy-type")]
    policy_type: String,
    #[serde(rename = "policy-string", skip_serializing_if = "Vec::is_empty")]
    policy_string: Vec<String>,
    #[serde(rename = "policy-domain")]
    policy_domain: String,
    #[serde(rename = "mx-host", skip_serializing_if = "Vec::is_empty")]
    mx_host: Vec<String>,
}

#[derive(Serialize)]
struct Summary {
    #[serde(rename = "total-successful-session-count")]
    successful: i64,
    #[serde(rename = "total-failure-session-count")]
    failed: i64,
}

#[derive(Serialize)]
struct FailureDetails {
    #[serde(rename = "result-type")]
    result_type: String,
    #[serde(rename = "sending-mta-ip", skip_serializing_if = "Option::is_none")]
    sending_mta_ip: Option<String>,
    #[serde(rename = "receiving-mx-hostname", skip_serializing_if = "Option::is_none")]
    receiving_mx_hostname: Option<String>,
    #[serde(rename = "receiving-ip", skip_serializing_if = "Option::is_none")]
    receiving_ip: Option<String>,
    #[serde(rename = "failed-session-count")]
    failed_session_count: i64,
}

fn said(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// What tells one policy apart from another in a report: its type, text and MX hosts.
type PolicyKey = (String, Vec<String>, Vec<String>);

/// The report of one domain's day as JSON (RFC 8460, section 4).
pub(crate) fn report_json(
    organization: &str,
    contact: &str,
    report_id: &str,
    day: i64,
    domain: &str,
    sessions: &[TlsSessionCount],
) -> String {
    // The sessions of each policy together, in the order they first come (most sessions first).
    let mut policies: BTreeMap<PolicyKey, (usize, PolicyReport)> = BTreeMap::new();
    for row in sessions {
        let session = &row.session;
        let key = (session.policy_type.clone(), session.policy_string.clone(), session.mx_host.clone());
        let order = policies.len();
        let (_, policy) = policies.entry(key).or_insert_with(|| {
            (
                order,
                PolicyReport {
                    policy: PolicyDetails {
                        policy_type: session.policy_type.clone(),
                        policy_string: session.policy_string.clone(),
                        policy_domain: domain.to_owned(),
                        mx_host: session.mx_host.clone(),
                    },
                    summary: Summary { successful: 0, failed: 0 },
                    failure_details: Vec::new(),
                },
            )
        });
        match &session.result_type {
            None => policy.summary.successful += row.count,
            Some(result_type) => {
                policy.summary.failed += row.count;
                if policy.failure_details.len() < MAX_FAILURE_DETAILS {
                    policy.failure_details.push(FailureDetails {
                        result_type: result_type.clone(),
                        sending_mta_ip: said(&session.sending_ip),
                        receiving_mx_hostname: said(&session.receiving_mx_hostname),
                        receiving_ip: said(&session.receiving_ip),
                        failed_session_count: row.count,
                    });
                }
            }
        }
    }
    let mut policies: Vec<(usize, PolicyReport)> = policies.into_values().collect();
    policies.sort_by_key(|(order, _)| *order);
    let report = Report {
        organization_name: organization,
        date_range: DateRange {
            start: mail_parser::DateTime::from_timestamp(day * DAY_SECS).to_rfc3339(),
            end: mail_parser::DateTime::from_timestamp(day * DAY_SECS + DAY_SECS - 1).to_rfc3339(),
        },
        contact_info: contact,
        report_id,
        policies: policies.into_iter().take(MAX_POLICIES).map(|(_, policy)| policy).collect(),
    };
    serde_json::to_string(&report).expect("the report serializes")
}

fn gzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// The report as a mail (RFC 8460, section 5.3), queued like any other, signed with the key of
/// our domain the host name belongs to when there is one.
async fn mail(
    ctx: &Context,
    submitter: &str,
    domain: &str,
    report_id: &str,
    (begin, end): (i64, i64),
    recipients: &[String],
    packed: Vec<u8>,
) -> Result<(), String> {
    let from = format!("{REPORT_SENDER}@{submitter}");
    let text = format!(
        "This is an aggregate TLS report (RFC 8460) from {submitter}.\r\n\r\n\
         Report Domain: {domain}\r\nSubmitter: {submitter}\r\nReport-ID: {report_id}\r\n"
    );
    let to: Vec<Address<'_>> =
        recipients.iter().map(|address| Address::new_address(None::<&str>, address.as_str())).collect();
    let raw = MessageBuilder::new()
        .from(("TLS reports", from.as_str()))
        .to(Address::new_list(to))
        .subject(format!("Report Domain: {domain} Submitter: {submitter} Report-ID: <{report_id}>"))
        .date(Date::now())
        .message_id(format!("{}.tlsrpt@{}", random_id(), ctx.hostname))
        .header("TLS-Report-Domain", Text::new(domain.to_owned()))
        .header("TLS-Report-Submitter", Text::new(submitter.to_owned()))
        .header("Auto-Submitted", Text::new("auto-generated"))
        .body(MimePart::new(
            ContentType::new("multipart/report").attribute("report-type", "tlsrpt"),
            vec![
                MimePart::new("text/plain; charset=utf-8", text),
                MimePart::new("application/tlsrpt+gzip", packed)
                    .attachment(format!("{submitter}!{domain}!{begin}!{end}.json.gz")),
            ],
        ))
        .write_to_vec()
        .map_err(|err| format!("writing the report mail failed: {err}"))?;
    let keys = ctx.store.dkim_keys(submitter).await.unwrap_or_default();
    let mut signed = dkim::sign(&raw, &keys)
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "signing a TLS report failed, sending it unsigned");
            String::new()
        })
        .into_bytes();
    signed.extend_from_slice(&raw);
    let recipients = recipients
        .iter()
        .map(|address| NewQueueRecipient { address: address.clone(), notify_flags: 0, orcpt: None })
        .collect();
    let lifetime = ctx.live().delivery.max_lifetime_hours as i64 * 3600;
    // The null sender, as for bounces: a report that does not arrive must not come back.
    ctx.store.enqueue("", recipients, &signed, None, None, lifetime).await.map_err(|err| err.to_string())?;
    Ok(())
}

/// Posts the packed report to an https address (RFC 8460, section 5.4): straight from the server,
/// to public addresses only, without following redirects.
async fn post(egress: &Egress, url: &str, packed: Vec<u8>) -> Result<(), String> {
    let url = crate::fetch::check_url(url, false)?;
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .with_root_certificates(egress.roots())
        .with_no_client_auth();
    let client: Client<_, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).build(egress.direct_dialer().https_connector(tls));
    let request = Request::post(url.as_str())
        .header(CONTENT_TYPE, "application/tlsrpt+gzip")
        .header(USER_AGENT, concat!("UwUMail/", env!("CARGO_PKG_VERSION")))
        .body(Full::new(Bytes::from(packed)))
        .map_err(|err| err.to_string())?;
    let response = tokio::time::timeout(POST_TIMEOUT, client.request(request))
        .await
        .map_err(|_| "no answer in time".to_owned())?
        .map_err(|err| err.to_string())?;
    if response.status().is_success() { Ok(()) } else { Err(format!("the answer was {}", response.status())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(policy_type: &str, strings: &[&str], result: Option<&str>, count: i64) -> TlsSessionCount {
        TlsSessionCount {
            session: TlsSession {
                day: 20_000,
                policy_domain: "example.com".into(),
                policy_type: policy_type.into(),
                policy_string: strings.iter().map(|s| s.to_string()).collect(),
                mx_host: if policy_type == "no-policy-found" { vec![] } else { vec!["mx.example.com".into()] },
                result_type: result.map(str::to_owned),
                receiving_mx_hostname: if result.is_some() { "mx.example.com".into() } else { String::new() },
                receiving_ip: if result.is_some() { "192.0.2.25".into() } else { String::new() },
                sending_ip: if result.is_some() { "198.51.100.7".into() } else { String::new() },
            },
            count,
        }
    }

    #[test]
    fn the_report_has_the_shape_of_rfc_8460() {
        let sts = ["version: STSv1", "mode: testing", "mx: mx.example.com", "max_age: 86400"];
        let sessions = [
            session("sts", &sts, None, 15),
            session("sts", &sts, Some("certificate-expired"), 2),
            session("no-policy-found", &[], None, 3),
        ];
        let json = report_json(
            "mail.example.org",
            "postmaster@example.org",
            "r-1@mail.example.org",
            20_000,
            "example.com",
            &sessions,
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "organization-name": "mail.example.org",
                "date-range": {
                    "start-datetime": "2024-10-04T00:00:00Z",
                    "end-datetime": "2024-10-04T23:59:59Z"
                },
                "contact-info": "postmaster@example.org",
                "report-id": "r-1@mail.example.org",
                "policies": [
                    {
                        "policy": {
                            "policy-type": "sts",
                            "policy-string": sts,
                            "policy-domain": "example.com",
                            "mx-host": ["mx.example.com"]
                        },
                        "summary": { "total-successful-session-count": 15, "total-failure-session-count": 2 },
                        "failure-details": [{
                            "result-type": "certificate-expired",
                            "sending-mta-ip": "198.51.100.7",
                            "receiving-mx-hostname": "mx.example.com",
                            "receiving-ip": "192.0.2.25",
                            "failed-session-count": 2
                        }]
                    },
                    {
                        "policy": { "policy-type": "no-policy-found", "policy-domain": "example.com" },
                        "summary": { "total-successful-session-count": 3, "total-failure-session-count": 0 }
                    }
                ]
            })
        );
        // What other servers (and our own reading of reports) make of it.
        let parsed: mail_auth::report::tlsrpt::TlsReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.policies.len(), 2);
        assert_eq!(parsed.policies[0].failure_details[0].failed_session_count, 2);
        assert_eq!(parsed.date_range.end_datetime.to_timestamp(), 20_000 * DAY_SECS + DAY_SECS - 1);
    }

    /// The example report of RFC 8460, appendix B, as this server writes it. Only the two free-form
    /// fields it never fills in (`additional-information`, `failure-reason-code`) are left out.
    #[test]
    fn the_example_of_rfc_8460_comes_out_as_printed() {
        let strings = ["version: STSv1", "mode: testing", "mx: *.mail.company-y.example", "max_age: 86400"];
        let row = |result: Option<&str>, sending: &str, host: &str, receiving: &str, count: i64| TlsSessionCount {
            session: TlsSession {
                day: 16_892,
                policy_domain: "company-y.example".into(),
                policy_type: "sts".into(),
                policy_string: strings.iter().map(|s| s.to_string()).collect(),
                mx_host: vec!["*.mail.company-y.example".into()],
                result_type: result.map(str::to_owned),
                receiving_mx_hostname: host.into(),
                receiving_ip: receiving.into(),
                sending_ip: sending.into(),
            },
            count,
        };
        let sessions = [
            row(None, "", "", "", 5326),
            row(Some("certificate-expired"), "2001:db8:abcd:0012::1", "mx1.mail.company-y.example", "", 100),
            row(
                Some("starttls-not-supported"),
                "2001:db8:abcd:0013::1",
                "mx2.mail.company-y.example",
                "203.0.113.56",
                200,
            ),
            row(Some("validation-failure"), "198.51.100.62", "mx-backup.mail.company-y.example", "203.0.113.58", 3),
        ];
        let json = report_json(
            "Company-X",
            "sts-reporting@company-x.example",
            "5065427c-23d3-47ca-b6e0-946ea0e8c4be",
            16_892,
            "company-y.example",
            &sessions,
        );
        let printed = r#"{
          "organization-name": "Company-X",
          "date-range": {
            "start-datetime": "2016-04-01T00:00:00Z",
            "end-datetime": "2016-04-01T23:59:59Z"
          },
          "contact-info": "sts-reporting@company-x.example",
          "report-id": "5065427c-23d3-47ca-b6e0-946ea0e8c4be",
          "policies": [{
            "policy": {
              "policy-type": "sts",
              "policy-string": ["version: STSv1","mode: testing",
                    "mx: *.mail.company-y.example","max_age: 86400"],
              "policy-domain": "company-y.example",
              "mx-host": ["*.mail.company-y.example"]
            },
            "summary": {
              "total-successful-session-count": 5326,
              "total-failure-session-count": 303
            },
            "failure-details": [{
              "result-type": "certificate-expired",
              "sending-mta-ip": "2001:db8:abcd:0012::1",
              "receiving-mx-hostname": "mx1.mail.company-y.example",
              "failed-session-count": 100
            }, {
              "result-type": "starttls-not-supported",
              "sending-mta-ip": "2001:db8:abcd:0013::1",
              "receiving-mx-hostname": "mx2.mail.company-y.example",
              "receiving-ip": "203.0.113.56",
              "failed-session-count": 200
            }, {
              "result-type": "validation-failure",
              "sending-mta-ip": "198.51.100.62",
              "receiving-ip": "203.0.113.58",
              "receiving-mx-hostname": "mx-backup.mail.company-y.example",
              "failed-session-count": 3
            }]
          }]
        }"#;
        let ours: serde_json::Value = serde_json::from_str(&json).unwrap();
        let printed: serde_json::Value = serde_json::from_str(printed).unwrap();
        assert_eq!(ours, printed);
    }

    #[test]
    fn report_addresses_and_our_own_domains() {
        assert_eq!(mail_addresses("mailto:tls@example.com"), ["tls@example.com"]);
        assert_eq!(
            mail_addresses("mailto:a@example.com,b%40example.net?subject=x"),
            ["a@example.com", "b@example.net"]
        );
        assert!(mail_addresses("mailto:not an address").is_empty());
        assert!(mail_addresses("mailto:a@b@example.com").is_empty());

        let local: HashSet<String> = ["example.org".to_owned()].into();
        assert!(is_local("Example.org.", &local));
        assert!(is_local("sub.example.org", &local));
        assert!(!is_local("example.com", &local));
        assert_eq!(host_domain("mail.example.org", &local).as_deref(), Some("example.org"));
        assert_eq!(host_domain("mail.example.net", &local), None);
        assert_eq!(report_sender("mail.example.org", &["example.org".into()]), "noreply-tls-reports@example.org");
        assert_eq!(report_sender("Mail.Example.net.", &["example.org".into()]), "noreply-tls-reports@mail.example.net");
    }

    #[test]
    fn dane_comes_before_mta_sts() {
        let sts = Policy::parse("version: STSv1\nmode: enforce\nmx: mx.example.com\nmax_age: 60").unwrap();
        let records: std::sync::Arc<[crate::dane::Tlsa]> = vec![crate::dane::Tlsa::parse("3 1 1 0a0b").unwrap()].into();
        let tlsa = ReportPolicy::of("mx.example.com", &Dane::Verify(records), Some(&sts), None);
        assert_eq!((tlsa.kind, tlsa.strings.as_slice()), (PolicyType::Tlsa, ["3 1 1 0a0b".to_owned()].as_slice()));
        let policy = ReportPolicy::of("mx.example.com", &Dane::Off, Some(&sts), None);
        assert_eq!((policy.kind, policy.mx.as_slice()), (PolicyType::Sts, ["mx.example.com".to_owned()].as_slice()));
        assert_eq!(policy.strings[1], "mode: enforce");
        let failed = ReportPolicy::of("mx.example.com", &Dane::Off, None, Some(ResultType::StsPolicyFetchError));
        assert_eq!((failed.kind, failed.failure), (PolicyType::Sts, Some(ResultType::StsPolicyFetchError)));
        let bogus = ReportPolicy::of("mx.example.com", &Dane::Bogus, None, None);
        assert_eq!((bogus.kind, bogus.failure), (PolicyType::Tlsa, Some(ResultType::DnssecInvalid)));
        assert_eq!(ReportPolicy::of("mx.example.com", &Dane::Off, None, None).kind, PolicyType::NoPolicyFound);
    }

    /// A server for `example.org` at `mx.example.org`, with its DKIM keys, that has seen one
    /// session to `example.com` yesterday and one to its own domain.
    async fn sender() -> (Smtp, tempfile::TempDir, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = uwumail_store::Store::open(dir.path()).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        dkim::ensure_domain_keys(&store, "example.org").await.unwrap();
        let smtp = Smtp::new(
            store.clone(),
            crate::SmtpSettings {
                hostname: "mx.example.org".into(),
                smtp: crate::SmtpConfig::default(),
                spam: crate::SpamConfig { enabled: false, ..crate::SpamConfig::default() },
                delivery: crate::DeliveryConfig::default(),
                tone: crate::ToneConfig::default(),
                server_tls: None,
            },
        )
        .unwrap();
        let today = tls_rpt_day(now());
        for (domain, result) in
            [("example.com", None), ("example.com", Some("starttls-not-supported")), ("example.org", None)]
        {
            let mut row = session("sts", &["version: STSv1", "mode: enforce"], result, 1).session;
            row.day = today - 1;
            row.policy_domain = domain.into();
            store.record_tls_session(row).await.unwrap();
        }
        (smtp, dir, today)
    }

    /// An https endpoint for `reports.example.com` on this machine; hands over every request it gets.
    async fn endpoint() -> (Egress, tokio::sync::mpsc::UnboundedReceiver<(String, Vec<u8>)>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let generated = rcgen::generate_simple_self_signed(vec!["reports.example.com".into()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![generated.cert.der().clone()], key)
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (seen, requests) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let (acceptor, seen) = (acceptor.clone(), seen.clone());
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(socket).await else { return };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() == 1 {
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8(head).unwrap();
                    let length = head
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(str::to_owned))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    stream.read_exact(&mut body).await.unwrap();
                    seen.send((head, body)).unwrap();
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await.unwrap();
                    let _ = stream.shutdown().await;
                });
            }
        });
        (Egress::pinned_trusting(address, generated.cert.der().clone()), requests)
    }

    fn gunzip(packed: &[u8]) -> String {
        use std::io::Read;
        let mut text = String::new();
        flate2::read::GzDecoder::new(packed).read_to_string(&mut text).unwrap();
        text
    }

    #[tokio::test]
    async fn reports_are_posted_to_https_addresses() {
        let (smtp, _dir, today) = sender().await;
        let (egress, mut requests) = endpoint().await;
        smtp.dns_cache()
            .pin_txt("_smtp._tls.example.com", "v=TLSRPTv1; rua=https://reports.example.com/tlsrpt")
            .unwrap();

        assert_eq!(send_due(&smtp.inner, &egress, today).await, 1, "our own domain gets none");
        let (head, body) = requests.recv().await.unwrap();
        assert!(head.starts_with("POST /tlsrpt HTTP/1.1\r\n"), "{head}");
        assert!(head.to_ascii_lowercase().contains("content-type: application/tlsrpt+gzip"), "{head}");
        let report: mail_auth::report::tlsrpt::TlsReport = serde_json::from_str(&gunzip(&body)).unwrap();
        assert_eq!(report.policies[0].policy.policy_domain, "example.com");
        assert_eq!(report.policies[0].summary.total_success, 1);
        assert_eq!(report.policies[0].summary.total_failure, 1);
        assert_eq!(report.contact_info.as_deref(), Some("postmaster@example.org"));

        let sent = smtp.store().tls_rpt_sent(7, 10).await.unwrap();
        let status: Vec<_> = sent.iter().map(|sent| (sent.domain.as_str(), sent.status.as_str())).collect();
        assert_eq!(status, [("example.com", "sent"), ("example.org", "skipped")]);
        assert_eq!(sent[0].destinations, ["https://reports.example.com/tlsrpt"]);
        assert_eq!(send_due(&smtp.inner, &egress, today).await, 0, "only once");
    }

    #[tokio::test]
    async fn addresses_in_private_networks_get_nothing() {
        let (smtp, _dir, today) = sender().await;
        smtp.dns_cache().pin_txt("_smtp._tls.example.com", "v=TLSRPTv1; rua=https://10.0.0.1/tlsrpt").unwrap();
        assert_eq!(send_due(&smtp.inner, &Egress::direct(), today).await, 0);
        let sent = smtp.store().tls_rpt_sent(7, 10).await.unwrap();
        assert_eq!(sent[0].status, "failed");
        assert!(sent[0].error.contains("private network"), "{}", sent[0].error);
    }

    #[tokio::test]
    async fn reports_by_mail_are_signed_and_named_as_rfc_8460_wants() {
        let (smtp, _dir, today) = sender().await;
        smtp.dns_cache()
            .pin_txt("_smtp._tls.example.com", "v=TLSRPTv1; rua=mailto:tls@example.com,mailto:postmaster@example.org")
            .unwrap();
        assert_eq!(send_due(&smtp.inner, &Egress::direct(), today).await, 1);

        let entries = smtp.store().queue_entries().await.unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.message.return_path, "");
        let to: Vec<_> = entry.recipients.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(to, ["tls@example.com"], "never to our own domains");
        let raw = smtp.store().blob(&entry.message.blob).await.unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("DKIM-Signature:"), "{text}");
        assert!(text.contains("d=example.org"));
        let message = mail_parser::MessageParser::default().parse(&raw[..]).unwrap();
        assert_eq!(message.from().unwrap().first().unwrap().address(), Some("noreply-tls-reports@example.org"));
        assert!(
            message.subject().unwrap().starts_with("Report Domain: example.com Submitter: example.org Report-ID: <")
        );
        assert_eq!(message.header_raw("TLS-Report-Domain").map(str::trim), Some("example.com"));
        use mail_parser::MimeHeaders;
        let report = message.attachment(0).unwrap();
        let begin = (today - 1) * DAY_SECS;
        assert_eq!(
            report.attachment_name(),
            Some(format!("example.org!example.com!{begin}!{}.json.gz", begin + DAY_SECS - 1).as_str())
        );
        assert!(gunzip(report.contents()).contains("\"policy-domain\":\"example.com\""));
    }
}
