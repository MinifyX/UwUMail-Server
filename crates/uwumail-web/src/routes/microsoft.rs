//! Delivery to Microsoft (docs/microsoft.md): the refusals and throttling the delivery worker
//! saw, and a checklist of what Microsoft asks of senders.

use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Value, json};
use uwumail_smtp::bimi::dmarc_fit;
use uwumail_smtp::dnscheck::{CheckStatus, DomainReport, RecordCheck};
use uwumail_smtp::health::Route;
use uwumail_smtp::microsoft::{DELIST_URL, IssueGroup};
use uwumail_store::DkimKeyState;

use super::audit;
use crate::Web;
use crate::error::ApiResult;
use crate::health::unix_now;
use crate::session::Admin;

/// Reverse lookups this fresh are shown again instead of asked anew.
const ADDRESSES_FRESH_SECS: i64 = 3600;

async fn issues_json(web: &Web) -> ApiResult<Value> {
    let now = unix_now();
    web.store().resolve_microsoft_issues(now).await?;
    let issues: Vec<Value> = web
        .store()
        .microsoft_issues(now)
        .await?
        .into_iter()
        .map(|issue| {
            // What the portal sorts and words the issue by: blocked, throttled or authentication.
            let kind = IssueGroup::parse(&issue.group).map(|group| group.kind().as_str());
            let mut value = json!(issue);
            value["kind"] = json!(kind);
            value
        })
        .collect();
    Ok(json!({ "issues": issues, "delistUrl": DELIST_URL }))
}

pub async fn issues(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(issues_json(&web).await?))
}

/// An admin says an issue is fixed, e.g. after Microsoft delisted the address.
pub async fn resolve(State(web): State<Web>, Admin(session): Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let issue = web.store().resolve_microsoft_issue(id, &session.account.login, unix_now()).await?;
    audit(&web, &session, "microsoft.resolve", &issue.subject, json!({ "code": issue.code })).await;
    Ok(Json(issues_json(&web).await?))
}

/// ok, warning, problem or unknown, the way the checklist says it.
fn level(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Ok => "ok",
        CheckStatus::Warning => "warning",
        CheckStatus::Missing | CheckStatus::Wrong => "problem",
        CheckStatus::Error => "unknown",
    }
}

fn record<'a>(report: &'a DomainReport, kind: &str) -> Option<&'a RecordCheck> {
    report.records.iter().find(|record| record.kind == kind)
}

/// One domain's part of the checklist, from its last DNS check.
pub(crate) fn domain_checklist(domain: &str, report: Option<&DomainReport>) -> Value {
    let Some(report) = report else {
        return json!({
            "domain": domain, "checkedAt": null, "spf": "unknown", "dkim": "unknown", "dmarc": "unknown",
            "dmarcPolicy": null, "dmarcPct": null, "aligned": "unknown",
        });
    };
    let spf = record(report, "spf").map_or("problem", |record| level(record.status));
    let active: Vec<&RecordCheck> = report
        .records
        .iter()
        .filter(|record| record.kind == "dkim" && record.key_state == Some(DkimKeyState::Active))
        .collect();
    let dkim = match active.iter().map(|record| record.status).max() {
        None => "problem",
        Some(status) => level(status),
    };
    let dmarc_record = record(report, "dmarc");
    // Microsoft asks for a DMARC record at all; `p=none` is its minimum.
    let dmarc = match dmarc_record.map(|record| record.status) {
        None | Some(CheckStatus::Missing | CheckStatus::Wrong) => "problem",
        Some(CheckStatus::Error) => "unknown",
        Some(_) => "ok",
    };
    let fit = dmarc_fit(dmarc_record.and_then(|record| record.found.first()).map(String::as_str));
    // Mail from here is signed with the domain's own key and sent with its address on the
    // envelope, so SPF or DKIM passing for the domain is aligned with the From.
    let aligned = if dmarc == "unknown" || (spf == "unknown" && dkim == "unknown") {
        "unknown"
    } else if spf == "ok" || dkim == "ok" {
        "ok"
    } else {
        "problem"
    };
    json!({
        "domain": domain,
        "checkedAt": report.checked_at,
        "spf": spf,
        "dkim": dkim,
        "dmarc": dmarc,
        "dmarcPolicy": fit.policy,
        "dmarcPct": fit.pct,
        "aligned": aligned,
    })
}

async fn checklist_json(web: &Web, fresh: bool) -> ApiResult<Value> {
    let now = unix_now();
    let cached = web.inner.microsoft_addresses.lock().expect("sending addresses poisoned").clone();
    let (checked_at, addresses) = match cached {
        Some((at, addresses)) if !fresh && now - at < ADDRESSES_FRESH_SECS => (at, addresses),
        _ => {
            let addresses = match web.dns() {
                Some(dns) => web.smtp().sending_addresses(dns).await,
                None => Vec::new(),
            };
            *web.inner.microsoft_addresses.lock().expect("sending addresses poisoned") = Some((now, addresses.clone()));
            (now, addresses)
        }
    };
    let relay_host = web.smtp().relay_host();
    let route: Route = web.smtp().delivery_summary().route;
    let mut domains = Vec::new();
    for domain in web.store().domains().await? {
        domains.push(domain_checklist(&domain.name, web.report(&domain.name).as_ref()));
    }
    Ok(json!({
        "checkedAt": checked_at,
        "hostname": web.settings().hostname,
        "route": route,
        "relayHost": relay_host,
        "addresses": addresses,
        "domains": domains,
        "tls": if web.smtp().relay_without_tls() { "warning" } else { "ok" },
    }))
}

pub async fn checklist(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(checklist_json(&web, false).await?))
}

/// Checks the DNS of every domain and the reverse names of the sending addresses now.
pub async fn check(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    web.check_all_domains().await;
    Ok(Json(checklist_json(&web, true).await?))
}

#[cfg(test)]
mod tests {
    use uwumail_smtp::dnscheck::{evaluate_dmarc, evaluate_srv};

    use super::*;

    fn report(records: Vec<RecordCheck>) -> DomainReport {
        DomainReport {
            domain: "example.org".into(),
            checked_at: 5,
            source: "authoritative",
            nameservers: Vec::new(),
            status: CheckStatus::Ok,
            records,
        }
    }

    #[test]
    fn domains_are_judged_by_their_last_dns_check() {
        let unknown = domain_checklist("example.org", None);
        assert_eq!((unknown["spf"].as_str(), unknown["aligned"].as_str()), (Some("unknown"), Some("unknown")));

        let dmarc = evaluate_dmarc("example.org", Ok(vec!["v=DMARC1; p=none; pct=50".into()]));
        let other = evaluate_srv("jmap", "_jmap._tcp.example.org", "mail.example.org", 443, Ok(vec![]));
        let checked = domain_checklist("example.org", Some(&report(vec![dmarc, other])));
        // No SPF record and no active key: nothing aligns.
        assert_eq!(checked["spf"], "problem");
        assert_eq!(checked["dkim"], "problem");
        assert_eq!(checked["dmarc"], "ok");
        assert_eq!(checked["dmarcPolicy"], "none");
        assert_eq!(checked["dmarcPct"], 50);
        assert_eq!(checked["aligned"], "problem");
        assert_eq!(checked["checkedAt"], 5);

        let missing = evaluate_dmarc("example.org", Ok(vec![]));
        assert_eq!(domain_checklist("example.org", Some(&report(vec![missing])))["dmarc"], "problem");
    }
}
