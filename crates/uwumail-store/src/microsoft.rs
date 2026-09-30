//! Refusals and throttling by Microsoft's mail servers (docs/microsoft.md), kept as issues per
//! sending address or sender domain and code. The delivery worker records each refusal and each
//! mail Microsoft accepted; an issue is over once mail went through after it and no new refusal
//! came for [`MICROSOFT_RESOLVE_AFTER_SECS`], or when an admin says it is fixed.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::{Result, Store, StoreError};

/// How long after the last refusal, with mail going through since, an issue counts as over.
pub const MICROSOFT_RESOLVE_AFTER_SECS: i64 = 24 * 3600;
/// Resolved issues shown in the portal.
const SHOWN_RESOLVED_SECS: i64 = 30 * 24 * 3600;
/// Resolved issues kept at all.
const KEPT_RESOLVED_SECS: i64 = 90 * 24 * 3600;
/// Microsoft's answers are short; anything longer is cut.
const MAX_REPLY_CHARS: usize = 1000;
/// Open issues kept at most. A real server has a handful of sending addresses and domains; past
/// this, new refusals only count up the issues already open, so a flood of odd answers cannot
/// fill the table or the admins' alerts.
const MAX_OPEN_MICROSOFT_ISSUES: i64 = 32;

/// One refusal as the delivery worker saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftRefusal {
    /// "ip" or "domain".
    pub scope: &'static str,
    /// The address or the domain the refusal is about; empty when it is not known.
    pub subject: String,
    /// What it means: blockList, banned, ipRefused, throttled, authentication or dmarc.
    pub group: &'static str,
    pub code: String,
    /// The address mail left from, when known.
    pub ip: String,
    /// The domain of the envelope sender.
    pub domain: String,
    pub reply: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrosoftIssue {
    pub id: i64,
    pub scope: String,
    pub subject: String,
    pub group: String,
    pub code: String,
    pub ip: String,
    pub domain: String,
    pub reply: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub count: i64,
    pub resolved_at: Option<i64>,
    /// "auto", or the login of the admin who closed it.
    pub resolved_by: Option<String>,
}

const COLUMNS: &str =
    "id, scope, subject, grp, code, ip, domain, reply, first_seen, last_seen, count, resolved_at, resolved_by";

fn issue_from_row(row: &Row<'_>) -> rusqlite::Result<MicrosoftIssue> {
    Ok(MicrosoftIssue {
        id: row.get(0)?,
        scope: row.get(1)?,
        subject: row.get(2)?,
        group: row.get(3)?,
        code: row.get(4)?,
        ip: row.get(5)?,
        domain: row.get(6)?,
        reply: row.get(7)?,
        first_seen: row.get(8)?,
        last_seen: row.get(9)?,
        count: row.get(10)?,
        resolved_at: row.get(11)?,
        resolved_by: row.get(12)?,
    })
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(MAX_REPLY_CHARS) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

impl Store {
    /// Records a refusal at `at`. Returns true when it opened a new issue.
    pub async fn record_microsoft_refusal(&self, refusal: MicrosoftRefusal, at: i64) -> Result<bool> {
        if !matches!(refusal.scope, "ip" | "domain") {
            return Err(StoreError::Invalid(format!("unknown scope {}", refusal.scope)));
        }
        self.write(move |tx| {
            let reply = cut(&refusal.reply);
            let changed = tx.execute(
                "UPDATE microsoft_issues
                 SET last_seen = MAX(last_seen, ?4), count = count + 1, reply = ?5, grp = ?6,
                     ip = CASE WHEN ?7 = '' THEN ip ELSE ?7 END,
                     domain = CASE WHEN ?8 = '' THEN domain ELSE ?8 END
                 WHERE scope = ?1 AND subject = ?2 AND code = ?3 AND resolved_at IS NULL",
                params![
                    refusal.scope,
                    refusal.subject,
                    refusal.code,
                    at,
                    reply,
                    refusal.group,
                    refusal.ip,
                    refusal.domain
                ],
            )?;
            if changed > 0 {
                return Ok(false);
            }
            let open: i64 =
                tx.query_row("SELECT COUNT(*) FROM microsoft_issues WHERE resolved_at IS NULL", [], |row| row.get(0))?;
            if open >= MAX_OPEN_MICROSOFT_ISSUES {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO microsoft_issues (scope, subject, grp, code, ip, domain, reply, first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    refusal.scope,
                    refusal.subject,
                    refusal.group,
                    refusal.code,
                    refusal.ip,
                    refusal.domain,
                    reply,
                    at
                ],
            )?;
            Ok(true)
        })
        .await
    }

    /// Microsoft accepted mail sent from `ip` for `domain` at `at`.
    pub async fn record_microsoft_delivery(&self, ip: Option<String>, domain: String, at: i64) -> Result<()> {
        self.write(move |tx| {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO microsoft_deliveries (scope, subject, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (scope, subject) DO UPDATE SET at = MAX(at, excluded.at)",
            )?;
            upsert.execute(params!["any", "", at])?;
            if let Some(ip) = ip.filter(|ip| !ip.is_empty()) {
                upsert.execute(params!["ip", ip, at])?;
            }
            if !domain.is_empty() {
                upsert.execute(params!["domain", domain, at])?;
            }
            Ok(())
        })
        .await
    }

    /// Closes issues that are over at `at`: mail went through since the last refusal, and that
    /// refusal is a day old. Issues about an unknown address or domain close once any mail went
    /// through, and so do address issues on a server that never learns its own public address
    /// (behind NAT, only Microsoft's answer names it). Old resolved ones are forgotten. Returns
    /// how many were closed.
    pub async fn resolve_microsoft_issues(&self, at: i64) -> Result<usize> {
        self.write(move |tx| {
            let closed = tx.execute(
                "UPDATE microsoft_issues SET resolved_at = ?1, resolved_by = 'auto'
                 WHERE resolved_at IS NULL AND last_seen <= ?2
                   AND EXISTS (SELECT 1 FROM microsoft_deliveries AS d
                               WHERE d.at >= microsoft_issues.last_seen
                                 AND (d.scope = microsoft_issues.scope AND d.subject = microsoft_issues.subject
                                      OR d.scope = 'any' AND microsoft_issues.subject = ''
                                      OR d.scope = 'any' AND microsoft_issues.scope = 'ip'
                                         AND NOT EXISTS (SELECT 1 FROM microsoft_deliveries WHERE scope = 'ip')))",
                params![at, at - MICROSOFT_RESOLVE_AFTER_SECS],
            )?;
            tx.execute(
                "DELETE FROM microsoft_issues WHERE resolved_at IS NOT NULL AND resolved_at < ?1",
                [at - KEPT_RESOLVED_SECS],
            )?;
            Ok(closed)
        })
        .await
    }

    /// Open issues, newest refusal first, then those resolved in the last 30 days (before `at`).
    pub async fn microsoft_issues(&self, at: i64) -> Result<Vec<MicrosoftIssue>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM microsoft_issues WHERE resolved_at IS NULL OR resolved_at >= ?1
                 ORDER BY resolved_at IS NOT NULL, COALESCE(resolved_at, last_seen) DESC, id DESC"
            ))?;
            let rows = stmt.query_map([at - SHOWN_RESOLVED_SECS], issue_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// The open issues only.
    pub async fn open_microsoft_issues(&self) -> Result<Vec<MicrosoftIssue>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM microsoft_issues WHERE resolved_at IS NULL ORDER BY last_seen DESC, id DESC"
            ))?;
            let rows = stmt.query_map([], issue_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// An admin says an issue is fixed (delisted, say).
    pub async fn resolve_microsoft_issue(&self, id: i64, by: &str, at: i64) -> Result<MicrosoftIssue> {
        let by = by.to_owned();
        self.write(move |tx| {
            tx.execute(
                "UPDATE microsoft_issues SET resolved_at = ?1, resolved_by = ?2 WHERE id = ?3 AND resolved_at IS NULL",
                params![at, by, id],
            )?;
            tx.query_row(&format!("SELECT {COLUMNS} FROM microsoft_issues WHERE id = ?1"), [id], issue_from_row)
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("issue {id}")))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;
    const HOUR: i64 = 3600;

    fn refusal(code: &str, ip: &str) -> MicrosoftRefusal {
        MicrosoftRefusal {
            scope: "ip",
            subject: ip.into(),
            group: "blockList",
            code: code.into(),
            ip: ip.into(),
            domain: "example.org".into(),
            reply: format!("550 5.7.1 messages from [{ip}] weren't sent ({code})"),
        }
    }

    #[tokio::test]
    async fn refusals_count_up_per_address_and_code() {
        let (store, _dir) = crate::test_support::store().await;
        assert!(store.record_microsoft_refusal(refusal("S3150", "203.0.113.5"), T0).await.unwrap());
        assert!(!store.record_microsoft_refusal(refusal("S3150", "203.0.113.5"), T0 + 60).await.unwrap());
        assert!(store.record_microsoft_refusal(refusal("5.7.708", "203.0.113.5"), T0 + 90).await.unwrap());
        assert!(store.record_microsoft_refusal(refusal("S3150", "198.51.100.7"), T0 + 120).await.unwrap());
        let issues = store.microsoft_issues(T0 + 200).await.unwrap();
        assert_eq!(issues.len(), 3);
        let first = issues.iter().find(|issue| issue.code == "S3150" && issue.subject == "203.0.113.5").unwrap();
        assert_eq!((first.count, first.first_seen, first.last_seen), (2, T0, T0 + 60));
        // Newest refusal first.
        assert_eq!(issues[0].subject, "198.51.100.7");
    }

    #[tokio::test]
    async fn open_issues_are_capped() {
        let (store, _dir) = crate::test_support::store().await;
        for n in 0..MAX_OPEN_MICROSOFT_ISSUES + 5 {
            store.record_microsoft_refusal(refusal(&format!("S{}", 3000 + n), "203.0.113.5"), T0 + n).await.unwrap();
        }
        assert_eq!(store.open_microsoft_issues().await.unwrap().len() as i64, MAX_OPEN_MICROSOFT_ISSUES);
        // Issues already open still count up.
        assert!(!store.record_microsoft_refusal(refusal("S3000", "203.0.113.5"), T0 + 500).await.unwrap());
        let issues = store.open_microsoft_issues().await.unwrap();
        assert_eq!(issues.iter().find(|issue| issue.code == "S3000").unwrap().count, 2);
    }

    #[tokio::test]
    async fn issues_end_a_day_after_the_last_refusal_once_mail_went_through() {
        let (store, _dir) = crate::test_support::store().await;
        store.record_microsoft_refusal(refusal("S3150", "203.0.113.5"), T0).await.unwrap();
        // No mail through yet: open however long it has been.
        assert_eq!(store.resolve_microsoft_issues(T0 + 48 * HOUR).await.unwrap(), 0);
        // Mail through from another address changes nothing.
        store.record_microsoft_delivery(Some("198.51.100.7".into()), String::new(), T0 + HOUR).await.unwrap();
        assert_eq!(store.resolve_microsoft_issues(T0 + 48 * HOUR).await.unwrap(), 0);
        // Mail through from the address, but the refusal is not a day old yet.
        store.record_microsoft_delivery(Some("203.0.113.5".into()), "example.org".into(), T0 + HOUR).await.unwrap();
        assert_eq!(store.resolve_microsoft_issues(T0 + 23 * HOUR).await.unwrap(), 0);
        assert_eq!(store.open_microsoft_issues().await.unwrap().len(), 1);
        assert_eq!(store.resolve_microsoft_issues(T0 + 24 * HOUR).await.unwrap(), 1);
        let issues = store.microsoft_issues(T0 + 24 * HOUR).await.unwrap();
        assert_eq!(issues[0].resolved_by.as_deref(), Some("auto"));
        assert!(store.open_microsoft_issues().await.unwrap().is_empty());
        // A new refusal opens a new issue.
        assert!(store.record_microsoft_refusal(refusal("S3150", "203.0.113.5"), T0 + 25 * HOUR).await.unwrap());
        // Resolved ones fall out of the list after 30 days and out of the table after 90.
        store.resolve_microsoft_issue(issues[0].id + 1, "admin@example.org", T0 + 26 * HOUR).await.unwrap();
        assert_eq!(store.microsoft_issues(T0 + 26 * HOUR).await.unwrap().len(), 2);
        assert!(store.microsoft_issues(T0 + 60 * 24 * HOUR).await.unwrap().is_empty());
        store.resolve_microsoft_issues(T0 + 120 * 24 * HOUR).await.unwrap();
        let left: i64 = store
            .read(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM microsoft_issues", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn behind_nat_any_mail_through_counts() {
        let (store, _dir) = crate::test_support::store().await;
        store.record_microsoft_refusal(refusal("4.7.650", "203.0.113.5"), T0).await.unwrap();
        // The server only knows its private address, so it records none.
        store.record_microsoft_delivery(None, "example.org".into(), T0 + HOUR).await.unwrap();
        assert_eq!(store.resolve_microsoft_issues(T0 + 24 * HOUR).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn admins_close_issues_and_long_replies_are_cut() {
        let (store, _dir) = crate::test_support::store().await;
        let mut long = refusal("S3150", "203.0.113.5");
        long.reply = "x".repeat(5000);
        store.record_microsoft_refusal(long, T0).await.unwrap();
        let issue = &store.open_microsoft_issues().await.unwrap()[0];
        assert_eq!(issue.reply.len(), MAX_REPLY_CHARS);
        let closed = store.resolve_microsoft_issue(issue.id, "admin@example.org", T0 + 5).await.unwrap();
        assert_eq!((closed.resolved_at, closed.resolved_by.as_deref()), (Some(T0 + 5), Some("admin@example.org")));
        assert!(store.resolve_microsoft_issue(9999, "admin@example.org", T0).await.is_err());
        let mut bad = refusal("S3150", "203.0.113.5");
        bad.scope = "host";
        assert!(store.record_microsoft_refusal(bad, T0).await.is_err());
    }
}
