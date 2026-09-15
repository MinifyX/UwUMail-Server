//! DMARC aggregate reports (RFC 7489) and TLS reports (RFC 8460) that other servers send to
//! `dmarc-reports@` and `tls-reports@` our domains. They are read here and kept as numbers;
//! nothing lands in a mailbox.

use std::sync::Arc;

use mail_auth::report::tlsrpt::{PolicyType, TlsReport};
use mail_auth::report::{ActionDisposition, Disposition, DmarcResult, Report};
use uwumail_store::{DmarcRow, NewDmarcReport, NewTlsReport, ReportKind, ReportStored, TlsFailure};

use crate::Context;

/// Reports come zipped; this is the most one may unpack to.
const MAX_UNPACKED_BYTES: usize = 20 * 1024 * 1024;

/// Reads the report in `raw` and stores it for the domain of `address`. Errors are only logged:
/// the message was already accepted, and a broken report is not worth a bounce.
pub(crate) async fn receive(ctx: Arc<Context>, kind: ReportKind, address: String, raw: Vec<u8>, authenticated: bool) {
    let domain = address.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase()).unwrap_or_default();
    let parse_domain = domain.clone();
    let parsed = tokio::task::spawn_blocking(move || match kind {
        ReportKind::Tls => TlsReport::parse_rfc5322(&raw, MAX_UNPACKED_BYTES)
            .map_err(|err| format!("{err:?}"))
            .and_then(|report| tls_report(&parse_domain, report, authenticated))
            .map(Parsed::Tls),
        ReportKind::Dmarc => Report::parse_rfc5322(&raw, MAX_UNPACKED_BYTES)
            .map_err(|err| format!("{err:?}"))
            .and_then(|report| dmarc_report(&parse_domain, report, authenticated))
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

fn tls_report(domain: &str, report: TlsReport, authenticated: bool) -> Result<NewTlsReport, String> {
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
            });
        }
    }
    Ok(NewTlsReport {
        domain: domain.to_owned(),
        organization: report.organization_name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| "?".into()),
        report_id: report.report_id,
        begin_at: report.date_range.start_datetime.to_timestamp(),
        end_at: report.date_range.end_datetime.to_timestamp(),
        authenticated,
        successful: policies.iter().map(|policy| i64::from(policy.summary.total_success)).sum(),
        failed: policies.iter().map(|policy| i64::from(policy.summary.total_failure)).sum(),
        failures,
    })
}

fn dmarc_report(domain: &str, report: Report, authenticated: bool) -> Result<NewDmarcReport, String> {
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
            }
        })
        .collect();
    let metadata = report.report_metadata;
    Ok(NewDmarcReport {
        domain: domain.to_owned(),
        organization: if metadata.org_name.trim().is_empty() { "?".into() } else { metadata.org_name },
        report_id: metadata.report_id,
        begin_at: i64::try_from(metadata.date_range.begin).unwrap_or_default(),
        end_at: i64::try_from(metadata.date_range.end).unwrap_or_default(),
        authenticated,
        policy: policy.into(),
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
    <domain>example.de</domain><adkim>s</adkim><aspf>s</aspf><p>quarantine</p><sp>quarantine</sp><pct>100</pct>
  </policy_published>
  <record>
    <row>
      <source_ip>192.0.2.10</source_ip><count>12</count>
      <policy_evaluated><disposition>none</disposition><dkim>pass</dkim><spf>fail</spf></policy_evaluated>
    </row>
    <identifiers><header_from>example.de</header_from></identifiers>
    <auth_results><dkim><domain>example.de</domain><result>pass</result><selector>uwu202609e</selector></dkim></auth_results>
  </record>
  <record>
    <row>
      <source_ip>198.51.100.7</source_ip><count>3</count>
      <policy_evaluated><disposition>quarantine</disposition><dkim>fail</dkim><spf>fail</spf></policy_evaluated>
    </row>
    <identifiers><header_from>example.de</header_from></identifiers>
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
          "policy-string": ["version: STSv1", "mode: testing", "mx: mail.example.de", "max_age: 86400"],
          "policy-domain": "example.de",
          "mx-host": ["mail.example.de"]
        },
        "summary": { "total-successful-session-count": 5326, "total-failure-session-count": 303 },
        "failure-details": [{
          "result-type": "certificate-expired",
          "sending-mta-ip": "2001:db8:abcd:0012::1",
          "receiving-mx-hostname": "Mail.Example.de.",
          "failed-session-count": 100
        }, {
          "result-type": "starttls-not-supported",
          "sending-mta-ip": "2001:db8:abcd:0013::1",
          "failed-session-count": 203
        }]
      }]
    }"#;

    #[test]
    fn dmarc_reports_become_rows() {
        let report = Report::parse_xml(DMARC_XML.as_bytes()).unwrap();
        let parsed = dmarc_report("example.de", report.clone(), true).unwrap();
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
                header_from: "example.de".into(),
            }
        );
        assert_eq!(parsed.rows[1].disposition, "quarantine");
        assert!(dmarc_report("other.example", report, true).is_err(), "not our domain");
    }

    #[test]
    fn tls_reports_keep_the_failures() {
        let report = TlsReport::parse_json(TLS_JSON.as_bytes()).unwrap();
        let parsed = tls_report("example.de", report.clone(), false).unwrap();
        assert_eq!((parsed.successful, parsed.failed), (5326, 303));
        assert_eq!(parsed.organization, "Company-X");
        assert_eq!(parsed.failures.len(), 2);
        assert_eq!(parsed.failures[0].result_type, "certificate-expired");
        assert_eq!(parsed.failures[0].mx_host, "mail.example.de");
        assert_eq!(parsed.failures[1].mx_host, "mail.example.de", "falls back to the policy's MX");
        assert_eq!(parsed.failures[1].sending_ip, "2001:db8:abcd:13::1");
        assert!(parsed.begin_at > 1_700_000_000 && parsed.end_at > parsed.begin_at);
        assert!(tls_report("other.example", report, false).is_err());
    }
}
