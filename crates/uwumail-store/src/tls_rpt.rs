//! The TLS reports this server sends to other domains (RFC 8460): daily counts of the delivery
//! sessions to each domain, and which of those days were reported.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Result, Store, now};

const DAY_SECS: i64 = 24 * 3600;
/// Different failures kept apart per domain and day. Past this, new ones only count, without the
/// host and addresses they happened at, so a domain with ever-changing hosts cannot grow the table.
const MAX_ROWS_PER_DOMAIN_DAY: i64 = 100;
/// Reports are sent for days at most this old, e.g. after the server was off for a while.
pub const TLS_RPT_MAX_AGE_DAYS: i64 = 3;
/// Sessions are kept this long, reports sent (for the portal) this long.
const SESSION_RETENTION_DAYS: i64 = 8;
const SENT_RETENTION_DAYS: i64 = 60;

/// The UTC day a moment falls on, as days since 1970-01-01.
pub fn tls_rpt_day(at: i64) -> i64 {
    at.div_euclid(DAY_SECS)
}

/// One delivery session and how its TLS went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsSession {
    pub day: i64,
    pub policy_domain: String,
    /// "sts", "tlsa" or "no-policy-found".
    pub policy_type: String,
    pub policy_string: Vec<String>,
    pub mx_host: Vec<String>,
    /// `None` for a successful session, otherwise the result type of RFC 8460, section 4.3.
    pub result_type: Option<String>,
    pub receiving_mx_hostname: String,
    pub receiving_ip: String,
    pub sending_ip: String,
}

/// Sessions of one kind within a day, with how many there were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsSessionCount {
    pub session: TlsSession,
    pub count: i64,
}

/// A day and domain whose report is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsRptDue {
    pub day: i64,
    pub policy_domain: String,
    /// How often sending was tried before.
    pub attempts: i64,
    /// The id of the earlier try, used again so a report that did arrive after all is a duplicate.
    pub report_id: Option<String>,
}

/// How dealing with a day's report went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsRptOutcome {
    pub day: i64,
    pub policy_domain: String,
    pub report_id: String,
    /// "sent", "failed", "none" or "skipped".
    pub status: String,
    pub destinations: Vec<String>,
    pub error: String,
    pub successful: i64,
    pub failed: i64,
}

/// A report as the portal lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsRptSent {
    /// The first second of the day the report covers.
    pub day: i64,
    pub domain: String,
    pub status: String,
    pub destinations: Vec<String>,
    pub error: String,
    pub successful: i64,
    pub failed: i64,
    pub updated_at: i64,
}

fn json_list(list: &[String]) -> String {
    serde_json::to_string(list).expect("strings serialize")
}

fn from_json_list(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}

impl Store {
    /// Counts one delivery session towards its day.
    pub async fn record_tls_session(&self, session: TlsSession) -> Result<()> {
        self.write(move |tx| {
            let policy_domain = session.policy_domain.trim_end_matches('.').to_ascii_lowercase();
            let key = params![
                session.day,
                policy_domain,
                session.policy_type,
                json_list(&session.policy_string),
                json_list(&session.mx_host),
                session.result_type.clone().unwrap_or_default(),
                session.receiving_mx_hostname,
                session.receiving_ip,
                session.sending_ip,
            ];
            let updated = tx.execute(
                "UPDATE tls_rpt_sessions SET count = count + 1
                 WHERE day = ?1 AND policy_domain = ?2 AND policy_type = ?3 AND policy_string = ?4
                   AND mx_host = ?5 AND result_type = ?6 AND receiving_mx_hostname = ?7
                   AND receiving_ip = ?8 AND sending_ip = ?9",
                key,
            )?;
            if updated > 0 {
                return Ok(());
            }
            let rows: i64 = tx.query_row(
                "SELECT count(*) FROM tls_rpt_sessions WHERE day = ?1 AND policy_domain = ?2",
                params![session.day, policy_domain],
                |row| row.get(0),
            )?;
            let (host, receiving, sending) = if rows >= MAX_ROWS_PER_DOMAIN_DAY {
                (String::new(), String::new(), String::new())
            } else {
                (session.receiving_mx_hostname, session.receiving_ip, session.sending_ip)
            };
            tx.execute(
                "INSERT INTO tls_rpt_sessions (day, policy_domain, policy_type, policy_string, mx_host, result_type,
                     receiving_mx_hostname, receiving_ip, sending_ip, count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)
                 ON CONFLICT DO UPDATE SET count = count + 1",
                params![
                    session.day,
                    policy_domain,
                    session.policy_type,
                    json_list(&session.policy_string),
                    json_list(&session.mx_host),
                    session.result_type.unwrap_or_default(),
                    host,
                    receiving,
                    sending,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// The finished days, not older than [`TLS_RPT_MAX_AGE_DAYS`], whose report still has to be
    /// sent: never tried, or tried and failed once on an earlier day.
    pub async fn tls_rpt_due(&self, today: i64) -> Result<Vec<TlsRptDue>> {
        self.read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT s.day, s.policy_domain, coalesce(r.attempts, 0), r.report_id
                 FROM (SELECT DISTINCT day, policy_domain FROM tls_rpt_sessions WHERE day < ?1 AND day >= ?2) s
                 LEFT JOIN tls_rpt_sent r ON r.day = s.day AND r.policy_domain = s.policy_domain
                 WHERE r.day IS NULL OR (r.status = 'failed' AND r.attempts < 2 AND r.updated_at < ?3)
                 ORDER BY s.day, s.policy_domain",
            )?;
            let due = statement
                .query_map(params![today, today - TLS_RPT_MAX_AGE_DAYS, today * DAY_SECS], |row| {
                    Ok(TlsRptDue {
                        day: row.get(0)?,
                        policy_domain: row.get(1)?,
                        attempts: row.get(2)?,
                        report_id: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(due)
        })
        .await
    }

    /// Every kind of session to `domain` on `day`, most frequent first.
    pub async fn tls_rpt_sessions(&self, day: i64, domain: &str) -> Result<Vec<TlsSessionCount>> {
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        self.read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT policy_type, policy_string, mx_host, result_type, receiving_mx_hostname, receiving_ip,
                        sending_ip, count
                 FROM tls_rpt_sessions WHERE day = ?1 AND policy_domain = ?2
                 ORDER BY count DESC, policy_type, result_type",
            )?;
            let rows = statement
                .query_map(params![day, domain], |row| {
                    let result_type: String = row.get(3)?;
                    Ok(TlsSessionCount {
                        session: TlsSession {
                            day,
                            policy_domain: domain.clone(),
                            policy_type: row.get(0)?,
                            policy_string: from_json_list(&row.get::<_, String>(1)?),
                            mx_host: from_json_list(&row.get::<_, String>(2)?),
                            result_type: (!result_type.is_empty()).then_some(result_type),
                            receiving_mx_hostname: row.get(4)?,
                            receiving_ip: row.get(5)?,
                            sending_ip: row.get(6)?,
                        },
                        count: row.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Notes how dealing with a day's report went; a second try counts up.
    pub async fn tls_rpt_done(&self, outcome: TlsRptOutcome) -> Result<()> {
        self.write(move |tx| {
            let attempts: Option<i64> = tx
                .query_row(
                    "SELECT attempts FROM tls_rpt_sent WHERE day = ?1 AND policy_domain = ?2",
                    params![outcome.day, outcome.policy_domain],
                    |row| row.get(0),
                )
                .optional()?;
            tx.execute(
                "INSERT OR REPLACE INTO tls_rpt_sent (day, policy_domain, report_id, status, attempts, destinations,
                     error, successful, failed, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    outcome.day,
                    outcome.policy_domain,
                    outcome.report_id,
                    outcome.status,
                    attempts.unwrap_or(0) + 1,
                    json_list(&outcome.destinations),
                    outcome.error,
                    outcome.successful,
                    outcome.failed,
                    now(),
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// The reports of the last `days` days, newest first, for the portal.
    pub async fn tls_rpt_sent(&self, days: i64, limit: usize) -> Result<Vec<TlsRptSent>> {
        let since = tls_rpt_day(now()) - days;
        self.read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT day, policy_domain, status, destinations, error, successful, failed, updated_at
                 FROM tls_rpt_sent WHERE day >= ?1 ORDER BY day DESC, policy_domain LIMIT ?2",
            )?;
            let sent = statement
                .query_map(params![since, limit as i64], |row| {
                    Ok(TlsRptSent {
                        day: row.get::<_, i64>(0)? * DAY_SECS,
                        domain: row.get(1)?,
                        status: row.get(2)?,
                        destinations: from_json_list(&row.get::<_, String>(3)?),
                        error: row.get(4)?,
                        successful: row.get(5)?,
                        failed: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(sent)
        })
        .await
    }

    /// Forgets sessions whose report time has passed and old notes of sent reports.
    pub async fn purge_tls_rpt(&self, today: i64) -> Result<usize> {
        self.write(move |tx| {
            let mut removed =
                tx.execute("DELETE FROM tls_rpt_sessions WHERE day < ?1", [today - SESSION_RETENTION_DAYS])?;
            removed += tx.execute("DELETE FROM tls_rpt_sent WHERE day < ?1", [today - SENT_RETENTION_DAYS])?;
            Ok(removed)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;

    fn session(day: i64, domain: &str, result: Option<&str>, ip: &str) -> TlsSession {
        TlsSession {
            day,
            policy_domain: domain.into(),
            policy_type: "sts".into(),
            policy_string: vec!["version: STSv1".into(), "mode: enforce".into()],
            mx_host: vec!["mx.example.com".into()],
            result_type: result.map(str::to_owned),
            receiving_mx_hostname: if result.is_some() { "mx.example.com".into() } else { String::new() },
            receiving_ip: ip.into(),
            sending_ip: String::new(),
        }
    }

    #[tokio::test]
    async fn sessions_count_per_day_and_kind() {
        let (store, _dir) = store().await;
        let today = tls_rpt_day(now());
        let day = today - 1;
        for _ in 0..3 {
            store.record_tls_session(session(day, "Example.com.", None, "")).await.unwrap();
        }
        store.record_tls_session(session(day, "example.com", Some("certificate-expired"), "192.0.2.1")).await.unwrap();
        store.record_tls_session(session(today, "example.com", None, "")).await.unwrap();

        let rows = store.tls_rpt_sessions(day, "example.com").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].count, rows[0].session.result_type.as_deref()), (3, None));
        assert_eq!(rows[1].session.result_type.as_deref(), Some("certificate-expired"));
        assert_eq!(rows[1].session.mx_host, vec!["mx.example.com".to_owned()]);

        // Only finished days are due, and each once.
        let due = store.tls_rpt_due(today).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].day, due[0].policy_domain.as_str(), due[0].attempts), (day, "example.com", 0));
        let outcome = |status: &str| TlsRptOutcome {
            day,
            policy_domain: "example.com".into(),
            report_id: "r1".into(),
            status: status.into(),
            destinations: vec!["mailto:tls@example.com".into()],
            error: String::new(),
            successful: 3,
            failed: 1,
        };
        store.tls_rpt_done(outcome("failed")).await.unwrap();
        assert!(store.tls_rpt_due(today).await.unwrap().is_empty(), "not again on the same day");
        let retry = store.tls_rpt_due(today + 1).await.unwrap();
        assert_eq!((retry[0].attempts, retry[0].report_id.as_deref()), (1, Some("r1")), "once the next day");
        store.tls_rpt_done(outcome("failed")).await.unwrap();
        assert!(store.tls_rpt_due(today + 2).await.unwrap().iter().all(|due| due.day != day), "never a third time");

        let sent = store.tls_rpt_sent(7, 10).await.unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].day, sent[0].successful, sent[0].failed), (day * DAY_SECS, 3, 1));

        assert!(store.tls_rpt_due(today + TLS_RPT_MAX_AGE_DAYS + 1).await.unwrap().is_empty(), "too old");
        assert_eq!(store.purge_tls_rpt(today + SESSION_RETENTION_DAYS + 1).await.unwrap(), 3);
        assert!(store.tls_rpt_sessions(day, "example.com").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn many_different_failures_lose_their_detail() {
        let (store, _dir) = store().await;
        let day = tls_rpt_day(now()) - 1;
        for index in 0..(MAX_ROWS_PER_DOMAIN_DAY + 20) {
            let ip = format!("192.0.2.{}", index % 250);
            let ip = if index >= 250 { format!("198.51.100.{index}") } else { ip };
            store.record_tls_session(session(day, "example.com", Some("validation-failure"), &ip)).await.unwrap();
        }
        let rows = store.tls_rpt_sessions(day, "example.com").await.unwrap();
        assert_eq!(rows.len() as i64, MAX_ROWS_PER_DOMAIN_DAY + 1);
        assert_eq!(rows.iter().map(|row| row.count).sum::<i64>(), MAX_ROWS_PER_DOMAIN_DAY + 20);
        assert_eq!(rows[0].count, 20, "the rest only counts");
        assert!(rows[0].session.receiving_ip.is_empty());
    }
}
