//! DMARC aggregate reports (RFC 7489) and TLS reports (RFC 8460) that other servers send to
//! `dmarc-reports@` and `tls-reports@` our domains. They are read here and kept as numbers;
//! nothing lands in a mailbox.

use std::sync::Arc;

use mail_auth::report::tlsrpt::{PolicyType, TlsReport};
use mail_auth::report::{ActionDisposition, Disposition, DmarcResult, Report};
use uwumail_store::{DmarcRow, NewDmarcReport, NewTlsReport, ReportKind, ReportStored, TlsFailure};

use crate::Context;

/// Reports come zipped; this is the most one may unpack to. A real aggregate report is well under a
/// megabyte unpacked, and everything past this only buys an outsider our memory and our time.
const MAX_UNPACKED_BYTES: usize = 8 * 1024 * 1024;

/// The most a message may weigh to be read as a report at all.
///
/// A real aggregate report is a few kilobytes; the largest senders stay well under a megabyte. The
/// cap matters because the parser tries every part of the message in turn, and each one may unpack
/// to [`MAX_UNPACKED_BYTES`] before it turns out to be nonsense — so a message full of small,
/// densely packed parts costs far more to read than it costs to send. Anything this size is not a
/// report, and refusing it here keeps that multiplication small.
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

/// How many reports are read at the same time. The rest are let go rather than queued: the raw
/// message would have to be held all the while, and another copy arrives with every connection.
pub(crate) const AT_ONCE: usize = 4;

/// The oldest and the newest period a report may claim to cover. `purge_reports` deletes by the end
/// of the period, so a made-up date would either keep a report forever or throw it away at once.
const OLDEST_PERIOD_SECS: i64 = 400 * 24 * 3600;
const NEWEST_PERIOD_SECS: i64 = 24 * 3600;

/// Takes a report off a connection and reads it in the background, unless too many are being read
/// already. The message counts as delivered either way: whoever sent it did nothing wrong.
pub(crate) fn receive_soon(ctx: Arc<Context>, kind: ReportKind, address: String, raw: Vec<u8>, authenticated: bool) {
    if raw.len() > MAX_REPORT_BYTES {
        tracing::info!(%address, size = raw.len(), "ignored a report far too big to be one");
        return;
    }
    let Ok(permit) = ctx.reports.clone().try_acquire_owned() else {
        tracing::warn!(%address, "{AT_ONCE} reports are being read already; this one is let go");
        return;
    };
    tokio::spawn(async move {
        receive(ctx, kind, address, raw, authenticated).await;
        drop(permit);
    });
}

/// Reads the report in `raw` and stores it for the domain of `address`. Errors are only logged:
/// the message was already accepted, and a broken report is not worth a bounce.
async fn receive(ctx: Arc<Context>, kind: ReportKind, address: String, raw: Vec<u8>, authenticated: bool) {
    let domain = address.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase()).unwrap_or_default();
    let parse_domain = domain.clone();
    let now = crate::now();
    let parsed = tokio::task::spawn_blocking(move || match kind {
        ReportKind::Tls => TlsReport::parse_rfc5322(&raw, MAX_UNPACKED_BYTES)
            .map_err(|err| format!("{err:?}"))
            .and_then(|report| tls_report(&parse_domain, report, authenticated, now))
            .map(Parsed::Tls),
        ReportKind::Dmarc => Report::parse_rfc5322(&raw, MAX_UNPACKED_BYTES)
            .map_err(|err| format!("{err:?}"))
            .and_then(|report| dmarc_report(&parse_domain, report, authenticated, now))
            .map(Parsed::Dmarc),
    })
    .await;
    let parsed = match parsed {
        Ok(Ok(parsed)) => parsed,
        Ok(Err(error)) => {
            tracing::info!(%domain, %error, "ignored a report that could not be read");
            return;
        }
        Err(err) => {
            tracing::error!(%err, "reading a report crashed");
            return;
        }
    };
    let (label, stored) = match parsed {
        Parsed::Tls(report) => ("TLS", ctx.store.add_tls_report(report).await),
        Parsed::Dmarc(report) => ("DMARC", ctx.store.add_dmarc_report(report).await),
    };
    match stored {
        Ok(ReportStored::Added) => tracing::info!(%domain, kind = label, "stored a report"),
        Ok(ReportStored::Duplicate) => tracing::debug!(%domain, kind = label, "the report arrived before"),
        Ok(ReportStored::TooMany) => tracing::warn!(%domain, kind = label, "too many reports today, dropped one"),
        Err(err) => tracing::error!(%err, %domain, "storing a report failed"),
    }
}

enum Parsed {
    Tls(NewTlsReport),
    Dmarc(NewDmarcReport),
}

/// The report is about our domain or one of its subdomains.
fn covers(domain: &str, reported: &str) -> bool {
    let reported = reported.trim_end_matches('.').to_ascii_lowercase();
    reported == domain || reported.ends_with(&format!(".{domain}"))
}

/// A name for one of mail-auth's report enums, lower case, as the portal shows it. Going through
/// serde rather than matching means a value the crate learns later still arrives with a name.
fn name_of(value: impl serde::Serialize) -> Option<String> {
    let text = serde_json::to_value(value).ok()?.as_str()?.to_ascii_lowercase();
    (!text.is_empty() && text != "unspecified").then_some(text)
}

/// Empty strings mean "the report did not say", which is not the same as an empty value.
fn said(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The period a report covers, if it is one a report could honestly cover. Nothing else checks
/// these numbers afterwards, and they decide when the report is cleared away again.
fn period(begin: i64, end: i64, now: i64) -> Result<(i64, i64), String> {
    let possible = (now - OLDEST_PERIOD_SECS)..=(now + NEWEST_PERIOD_SECS);
    if !possible.contains(&begin) || !possible.contains(&end) || end < begin {
        return Err(format!("the report claims to cover {begin} to {end}"));
    }
    Ok((begin, end))
}

fn tls_report(domain: &str, report: TlsReport, authenticated: bool, now: i64) -> Result<NewTlsReport, String> {
    let policies: Vec<_> = report
        .policies
        .iter()
        .filter(|policy| policy.policy.policy_domain.is_empty() || covers(domain, &policy.policy.policy_domain))
        .collect();
    if policies.is_empty() {
        return Err(format!("the report is not about {domain}"));
    }
    let mut failures = Vec::new();
    for policy in &policies {
        let policy_type = match policy.policy.policy_type {
            PolicyType::Sts => "sts",
            PolicyType::Tlsa => "tlsa",
            PolicyType::NoPolicyFound => "no-policy-found",
            PolicyType::Other => "other",
        };
        for detail in &policy.failure_details {
            let result_type = serde_json::to_value(detail.result_type)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "other".into());
            failures.push(TlsFailure {
                policy_type: policy_type.into(),
                result_type,
                mx_host: detail
                    .receiving_mx_hostname
                    .clone()
                    .or_else(|| policy.policy.mx_host.first().cloned())
                    .unwrap_or_default()
                    .trim_end_matches('.')
                    .to_ascii_lowercase(),
                sending_ip: detail.sending_mta_ip.map(|ip| ip.to_string()).unwrap_or_default(),
                sessions: i64::from(detail.failed_session_count),
                // What the sender says went wrong, which is the part that explains a failure.
                failure_code: detail.failure_reason_code.as_deref().and_then(said),
                receiving_ip: detail.receiving_ip.map(|ip| ip.to_string()),
                helo: detail.receiving_mx_helo.as_deref().and_then(said),
                detail: detail.additional_information.as_deref().and_then(said),
            });
        }
    }
    let (begin_at, end_at) =
        period(report.date_range.start_datetime.to_timestamp(), report.date_range.end_datetime.to_timestamp(), now)?;
    Ok(NewTlsReport {
        domain: domain.to_owned(),
        organization: report.organization_name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| "?".into()),
        report_id: report.report_id,
        begin_at,
        end_at,
        authenticated,
        successful: policies.iter().map(|policy| i64::from(policy.summary.total_success)).sum(),
        failed: policies.iter().map(|policy| i64::from(policy.summary.total_failure)).sum(),
        // The policy the sender actually applied. When it does not match what we publish, that is
        // the whole explanation for a run of failures, and it was thrown away until now.
        policy_domain: policies.first().and_then(|policy| said(&policy.policy.policy_domain)),
        policy_string: policies.first().map(|policy| policy.policy.policy_string.join("\n")).as_deref().and_then(said),
        contact: report.contact_info.as_deref().and_then(said),
        failures,
    })
}

fn dmarc_report(domain: &str, report: Report, authenticated: bool, now: i64) -> Result<NewDmarcReport, String> {
    if !covers(domain, &report.policy_published.domain) {
        return Err(format!("the report is about {}, not {domain}", report.policy_published.domain));
    }
    let policy = match report.policy_published.p {
        Disposition::None => "none",
        Disposition::Quarantine => "quarantine",
        Disposition::Reject => "reject",
        Disposition::Unspecified => "unknown",
    };
    let rows = report
        .record
        .iter()
        .map(|record| {
            let evaluated = &record.row.policy_evaluated;
            // The signature and the SPF check the reporter looked at. A record may carry several;
            // the first is the one its verdict rests on, and more than one is vanishingly rare.
            let dkim = record.auth_results.dkim.first();
            let spf = record.auth_results.spf.first();
            DmarcRow {
                source_ip: record.row.source_ip.map(|ip| ip.to_string()).unwrap_or_default(),
                messages: i64::from(record.row.count),
                dkim_aligned: evaluated.dkim == DmarcResult::Pass,
                spf_aligned: evaluated.spf == DmarcResult::Pass,
                disposition: match evaluated.disposition {
                    ActionDisposition::None | ActionDisposition::Pass => "none",
                    ActionDisposition::Quarantine => "quarantine",
                    ActionDisposition::Reject => "reject",
                    ActionDisposition::Unspecified => "unknown",
                }
                .into(),
                header_from: record.identifiers.header_from.trim().to_ascii_lowercase(),
                dkim_domain: dkim.and_then(|result| said(&result.domain)),
                dkim_selector: dkim.and_then(|result| said(&result.selector)),
                dkim_result: dkim.and_then(|result| name_of(result.result)),
                spf_domain: spf.and_then(|result| said(&result.domain)),
                spf_result: spf.and_then(|result| name_of(result.result)),
                override_reason: evaluated.reason.first().and_then(|reason| {
                    let name = name_of(reason.type_)?;
                    Some(match reason.comment.as_deref().and_then(said) {
                        Some(comment) => format!("{name}: {comment}"),
                        None => name,
                    })
                }),
                envelope_from: said(&record.identifiers.envelope_from),
                envelope_to: record.identifiers.envelope_to.as_deref().and_then(said),
            }
        })
        .collect();
    let metadata = report.report_metadata;
    let (begin_at, end_at) = period(
        i64::try_from(metadata.date_range.begin).unwrap_or(i64::MAX),
        i64::try_from(metadata.date_range.end).unwrap_or(i64::MAX),
        now,
    )?;
    Ok(NewDmarcReport {
        domain: domain.to_owned(),
        organization: if metadata.org_name.trim().is_empty() { "?".into() } else { metadata.org_name },
        report_id: metadata.report_id,
        begin_at,
        end_at,
        authenticated,
        policy: policy.into(),
        // The domain the report names is the one the policy was published for, which for a
        // subdomain is not the domain we file the report under.
        reported_domain: said(&report.policy_published.domain).map(|name| name.to_ascii_lowercase()),
        subdomain_policy: name_of(report.policy_published.sp),
        alignment: match (name_of(report.policy_published.adkim), name_of(report.policy_published.aspf)) {
            (None, None) => None,
            (dkim, spf) => {
                Some(format!("adkim={} aspf={}", dkim.as_deref().unwrap_or("?"), spf.as_deref().unwrap_or("?")))
            }
        },
        contact: said(&metadata.email).or_else(|| metadata.extra_contact_info.as_deref().and_then(said)),
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DMARC_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<feedback>
  <report_metadata>
    <org_name>google.com</org_name>
    <email>noreply-dmarc-support@google.com</email>
    <report_id>12345678901234567890</report_id>
    <date_range><begin>1757894400</begin><end>1757980799</end></date_range>
  </report_metadata>
  <policy_published>
    <domain>example.org</domain><adkim>s</adkim><aspf>s</aspf><p>quarantine</p><sp>quarantine</sp><pct>100</pct>
  </policy_published>
  <record>
    <row>
      <source_ip>192.0.2.10</source_ip><count>12</count>
      <policy_evaluated><disposition>none</disposition><dkim>pass</dkim><spf>fail</spf></policy_evaluated>
    </row>
    <identifiers><header_from>example.org</header_from></identifiers>
    <auth_results><dkim><domain>example.org</domain><result>pass</result><selector>uwu202609e</selector></dkim></auth_results>
  </record>
  <record>
    <row>
      <source_ip>198.51.100.7</source_ip><count>3</count>
      <policy_evaluated><disposition>quarantine</disposition><dkim>fail</dkim><spf>fail</spf></policy_evaluated>
    </row>
    <identifiers><header_from>example.org</header_from></identifiers>
    <auth_results><spf><domain>spammer.example</domain><result>pass</result></spf></auth_results>
  </record>
</feedback>"#;

    const TLS_JSON: &str = r#"{
      "organization-name": "Company-X",
      "date-range": { "start-datetime": "2026-09-14T00:00:00Z", "end-datetime": "2026-09-14T23:59:59Z" },
      "contact-info": "sts-reporting@company-x.example",
      "report-id": "5065427c-23d3-47ca-b6e0-946ea0e8c4be",
      "policies": [{
        "policy": {
          "policy-type": "sts",
          "policy-string": ["version: STSv1", "mode: testing", "mx: mail.example.org", "max_age: 86400"],
          "policy-domain": "example.org",
          "mx-host": ["mail.example.org"]
        },
        "summary": { "total-successful-session-count": 5326, "total-failure-session-count": 303 },
        "failure-details": [{
          "result-type": "certificate-expired",
          "sending-mta-ip": "2001:db8:abcd:0012::1",
          "receiving-mx-hostname": "Mail.Example.org.",
          "failed-session-count": 100
        }, {
          "result-type": "starttls-not-supported",
          "sending-mta-ip": "2001:db8:abcd:0013::1",
          "failed-session-count": 203
        }]
      }]
    }"#;

    /// A day after the period each fixture covers, so no test starts failing as time passes.
    const DMARC_NOW: i64 = 1_758_067_200;
    const TLS_NOW: i64 = 1_789_516_800;

    #[test]
    fn dmarc_reports_become_rows() {
        let report = Report::parse_xml(DMARC_XML.as_bytes()).unwrap();
        let parsed = dmarc_report("example.org", report.clone(), true, DMARC_NOW).unwrap();
        assert_eq!((parsed.organization.as_str(), parsed.policy.as_str()), ("google.com", "quarantine"));
        assert_eq!((parsed.begin_at, parsed.end_at), (1_757_894_400, 1_757_980_799));
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(
            parsed.rows[0],
            DmarcRow {
                source_ip: "192.0.2.10".into(),
                messages: 12,
                dkim_aligned: true,
                spf_aligned: false,
                disposition: "none".into(),
                header_from: "example.org".into(),
                // What the reporter checked, which used to be dropped on the floor.
                dkim_domain: Some("example.org".into()),
                dkim_selector: Some("uwu202609e".into()),
                dkim_result: Some("pass".into()),
                ..Default::default()
            }
        );
        assert_eq!(parsed.rows[1].disposition, "quarantine");
        // The second record is the interesting one: SPF passes for a domain that is not ours, so
        // the mail is not aligned. Without these fields nothing could say that.
        assert_eq!(parsed.rows[1].spf_domain.as_deref(), Some("spammer.example"));
        assert_eq!(parsed.rows[1].spf_result.as_deref(), Some("pass"));
        assert_eq!(parsed.reported_domain.as_deref(), Some("example.org"));
        assert_eq!(parsed.alignment.as_deref(), Some("adkim=strict aspf=strict"));
        assert_eq!(parsed.subdomain_policy.as_deref(), Some("quarantine"));
        assert_eq!(parsed.contact.as_deref(), Some("noreply-dmarc-support@google.com"));
        assert!(dmarc_report("other.example", report.clone(), true, DMARC_NOW).is_err(), "not our domain");
        assert!(dmarc_report("example.org", report, true, DMARC_NOW + 500 * 24 * 3600).is_err(), "far too old");
    }

    #[test]
    fn a_report_about_a_made_up_period_is_refused() {
        let now = DMARC_NOW;
        assert!(period(now - 86_400, now, now).is_ok());
        assert!(period(now, now - 86_400, now).is_err(), "ends before it begins");
        assert!(period(0, 0, now).is_err(), "no dates at all");
        assert!(period(now, now + 10 * 365 * 24 * 3600, now).is_err(), "would never be cleared away");
        assert!(period(now - 500 * 24 * 3600, now - 499 * 24 * 3600, now).is_err(), "older than we keep anything");
        assert!(period(i64::MAX, i64::MAX, now).is_err(), "a number that did not fit");
    }

    #[test]
    fn tls_reports_keep_the_failures() {
        let report = TlsReport::parse_json(TLS_JSON.as_bytes()).unwrap();
        let parsed = tls_report("example.org", report.clone(), false, TLS_NOW).unwrap();
        assert_eq!((parsed.successful, parsed.failed), (5326, 303));
        assert_eq!(parsed.organization, "Company-X");
        assert_eq!(parsed.failures.len(), 2);
        assert_eq!(parsed.failures[0].result_type, "certificate-expired");
        assert_eq!(parsed.failures[0].mx_host, "mail.example.org");
        assert_eq!(parsed.failures[1].mx_host, "mail.example.org", "falls back to the policy's MX");
        assert_eq!(parsed.failures[1].sending_ip, "2001:db8:abcd:13::1");
        assert!(parsed.begin_at > 1_700_000_000 && parsed.end_at > parsed.begin_at);
        assert!(tls_report("other.example", report, false, TLS_NOW).is_err());
    }
}
