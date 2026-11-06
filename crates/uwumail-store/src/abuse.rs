//! Outbound abuse (docs/outbound-abuse.md): strikes against an account whose mail looks like abuse,
//! the send suspension a few of them lead to, the Message-IDs an account sent out, and the
//! complaints (ARF, RFC 5965) other providers send to abuse@ our domains.
//!
//! A suspension only stops mail to other servers; the account still receives and still writes to
//! people here. It ends by itself, or earlier when an admin lifts it.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::{Result, Store, StoreError};

/// Sent Message-IDs are kept this long: complaints come within days, rarely later.
const SENT_KEPT_SECS: i64 = 30 * 24 * 3600;
/// Complaints are kept this long, and at most this many.
const REPORTS_KEPT_SECS: i64 = 180 * 24 * 3600;
const REPORTS_KEPT_ROWS: i64 = 5000;
/// Strikes older than this are gone whatever the window says.
const STRIKES_KEPT_SECS: i64 = 7 * 24 * 3600;
/// Texts out of a complaint are cut to this; they are only shown.
const MAX_TEXT_CHARS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrikeKind {
    /// The outbound spam scan refused a message.
    Spam,
    /// The account reached its sending limit.
    RateLimit,
    /// Another server refused a recipient for good (5xx).
    Bounce,
    /// Somebody complained about a message (an ARF report).
    Complaint,
}

impl StrikeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StrikeKind::Spam => "spam",
            StrikeKind::RateLimit => "rateLimit",
            StrikeKind::Bounce => "bounce",
            StrikeKind::Complaint => "complaint",
        }
    }
}

/// When strikes lead to a suspension; a zero turns that part off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrikeLimits {
    /// Spam, rate-limit and complaint strikes within the window.
    pub strikes: u32,
    /// Hard bounces within the window. Counted apart: a newsletter to an old list bounces a lot
    /// without any of it being abuse, so it gets a much higher bar.
    pub bounces: u32,
    pub window_secs: i64,
    pub suspend_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendSuspension {
    pub account_id: i64,
    pub login: String,
    pub since: i64,
    pub until: i64,
    pub reason: String,
}

/// A complaint as read from an ARF report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewAbuseReport {
    pub received_at: i64,
    pub domain: String,
    pub reporter: String,
    pub authenticated: bool,
    pub feedback_type: String,
    pub user_agent: Option<String>,
    pub source_ip: Option<String>,
    pub original_message_id: Option<String>,
    pub original_from: Option<String>,
    pub original_subject: Option<String>,
    /// The account that sent the original, as far as it could be told.
    pub account_id: Option<i64>,
    /// Whether the report may count against that account: a complaint about spam (not a "this is
    /// not spam" or an authentication report), tied to the account by something an outsider cannot
    /// make up. A name in a header anyone could have written is shown but never counted.
    pub may_count: bool,
    /// When set, the report does not count if one from the same reporter domain already counted
    /// against the account since then, so one domain cannot complain an account into a suspension.
    pub once_per_reporter_since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AbuseReport {
    pub id: i64,
    pub received_at: i64,
    pub domain: String,
    pub reporter: String,
    pub authenticated: bool,
    pub feedback_type: String,
    pub user_agent: Option<String>,
    pub source_ip: Option<String>,
    pub original_message_id: Option<String>,
    pub original_from: Option<String>,
    pub original_subject: Option<String>,
    pub login: Option<String>,
    pub counted: bool,
}

fn report_from_row(row: &Row<'_>) -> rusqlite::Result<AbuseReport> {
    Ok(AbuseReport {
        id: row.get(0)?,
        received_at: row.get(1)?,
        domain: row.get(2)?,
        reporter: row.get(3)?,
        authenticated: row.get(4)?,
        feedback_type: row.get(5)?,
        user_agent: row.get(6)?,
        source_ip: row.get(7)?,
        original_message_id: row.get(8)?,
        original_from: row.get(9)?,
        original_subject: row.get(10)?,
        login: row.get(11)?,
        counted: row.get(12)?,
    })
}

fn suspension_from_row(row: &Row<'_>) -> rusqlite::Result<SendSuspension> {
    Ok(SendSuspension {
        account_id: row.get(0)?,
        login: row.get(1)?,
        since: row.get(2)?,
        until: row.get(3)?,
        reason: row.get(4)?,
    })
}

fn cut(text: Option<String>) -> Option<String> {
    text.map(|text| text.chars().filter(|c| !c.is_control()).take(MAX_TEXT_CHARS).collect())
}

/// Enough for the recipient domains of a message to a few dozen companies.
const MAX_DOMAINS_CHARS: usize = 2000;

const SUSPENSION_COLUMNS: &str = "s.account_id, a.login, s.since, s.until, s.reason";

impl Store {
    /// Counts a strike against the account at `at`. Returns the suspension when this strike
    /// started one; strikes while suspended only count.
    pub async fn record_send_strike(
        &self,
        account_id: i64,
        kind: StrikeKind,
        at: i64,
        limits: StrikeLimits,
    ) -> Result<Option<SendSuspension>> {
        self.write(move |tx| {
            tx.execute("DELETE FROM send_strikes WHERE at < ?1", [at - STRIKES_KEPT_SECS.max(limits.window_secs)])?;
            // A mail client retries what the rate limit refused every few minutes, and someone near
            // the limit hits it again and again. That is one offence per window, so the limit alone
            // never suspends anyone (security review 0.28.0 OA-06); it only adds to other strikes.
            if kind == StrikeKind::RateLimit {
                let recent: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM send_strikes WHERE account_id = ?1 AND kind = ?2 AND at > ?3)",
                    params![account_id, kind.as_str(), at - limits.window_secs],
                    |row| row.get(0),
                )?;
                if recent {
                    return Ok(None);
                }
            }
            tx.execute(
                "INSERT INTO send_strikes (account_id, kind, at) VALUES (?1, ?2, ?3)",
                params![account_id, kind.as_str(), at],
            )?;
            let suspended: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM send_suspensions WHERE account_id = ?1 AND until > ?2)",
                params![account_id, at],
                |row| row.get(0),
            )?;
            if suspended || limits.suspend_secs <= 0 {
                return Ok(None);
            }
            let (bounce, limit) = match kind {
                StrikeKind::Bounce => (true, limits.bounces),
                _ => (false, limits.strikes),
            };
            if limit == 0 {
                return Ok(None);
            }
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM send_strikes
                 WHERE account_id = ?1 AND at > ?2 AND (kind = 'bounce') = ?3",
                params![account_id, at - limits.window_secs, bounce],
                |row| row.get(0),
            )?;
            if count < i64::from(limit) {
                return Ok(None);
            }
            // The count starts over: after the suspension the account gets a clean slate, and a
            // single new strike does not send it straight back.
            tx.execute("DELETE FROM send_strikes WHERE account_id = ?1", [account_id])?;
            tx.execute(
                "INSERT INTO send_suspensions (account_id, since, until, reason) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account_id) DO UPDATE SET since = excluded.since, until = excluded.until,
                                                        reason = excluded.reason",
                params![account_id, at, at + limits.suspend_secs, kind.as_str()],
            )?;
            Ok(tx
                .query_row(
                    &format!(
                        "SELECT {SUSPENSION_COLUMNS} FROM send_suspensions s JOIN accounts a ON a.id = s.account_id
                         WHERE s.account_id = ?1"
                    ),
                    [account_id],
                    suspension_from_row,
                )
                .optional()?)
        })
        .await
    }

    /// The account's suspension, if one is running at `at`.
    pub async fn send_suspension(&self, account_id: i64, at: i64) -> Result<Option<SendSuspension>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {SUSPENSION_COLUMNS} FROM send_suspensions s JOIN accounts a ON a.id = s.account_id
                         WHERE s.account_id = ?1 AND s.until > ?2"
                    ),
                    params![account_id, at],
                    suspension_from_row,
                )
                .optional()?)
        })
        .await
    }

    /// Every suspension running at `at`, the one ending last first.
    pub async fn send_suspensions(&self, at: i64) -> Result<Vec<SendSuspension>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SUSPENSION_COLUMNS} FROM send_suspensions s JOIN accounts a ON a.id = s.account_id
                 WHERE s.until > ?1 ORDER BY s.until DESC, s.account_id"
            ))?;
            let rows = stmt.query_map([at], suspension_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Ends a suspension early, and forgets the strikes that led to it.
    pub async fn lift_send_suspension(&self, account_id: i64, at: i64) -> Result<SendSuspension> {
        self.write(move |tx| {
            let found = tx
                .query_row(
                    &format!(
                        "SELECT {SUSPENSION_COLUMNS} FROM send_suspensions s JOIN accounts a ON a.id = s.account_id
                         WHERE s.account_id = ?1 AND s.until > ?2"
                    ),
                    params![account_id, at],
                    suspension_from_row,
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("suspension of account {account_id}")))?;
            tx.execute("DELETE FROM send_suspensions WHERE account_id = ?1", [account_id])?;
            tx.execute("DELETE FROM send_strikes WHERE account_id = ?1", [account_id])?;
            Ok(found)
        })
        .await
    }

    /// Notes that the account sent a message with this Message-ID to `recipient_domains`, to tell
    /// later whose message a complaint is about and whether the one complaining got it. A
    /// Message-ID two accounts used belongs to neither: the client chooses it, so one account could
    /// otherwise reuse another's to turn complaints about its own mail onto it.
    pub async fn record_sent_message_id(
        &self,
        account_id: i64,
        message_id: &str,
        recipient_domains: Vec<String>,
        at: i64,
    ) -> Result<()> {
        let message_id = message_id.trim().trim_matches(['<', '>']).to_owned();
        if message_id.is_empty() || message_id.len() > 255 {
            return Ok(());
        }
        let mut domains: Vec<String> = recipient_domains.iter().map(|domain| domain.to_ascii_lowercase()).collect();
        domains.sort();
        domains.dedup();
        let domains: String = domains.join(" ").chars().take(MAX_DOMAINS_CHARS).collect();
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO sent_message_ids (message_id, account_id, recipient_domains, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (message_id) DO UPDATE SET
                     account_id = CASE WHEN account_id = excluded.account_id THEN account_id END,
                     recipient_domains = substr(recipient_domains || ' ' || excluded.recipient_domains, 1, ?5),
                     at = excluded.at",
                params![message_id, account_id, domains, at, MAX_DOMAINS_CHARS as i64],
            )?;
            tx.execute("DELETE FROM sent_message_ids WHERE at < ?1", [at - SENT_KEPT_SECS])?;
            Ok(())
        })
        .await
    }

    /// The account that sent the message with this Message-ID, if it was sent from here lately by
    /// exactly one account, and the domains it went to.
    pub async fn sent_message_account(&self, message_id: &str) -> Result<Option<(i64, Vec<String>)>> {
        let message_id = message_id.trim().trim_matches(['<', '>']).to_owned();
        self.read(move |conn| {
            let found: Option<(Option<i64>, String)> = conn
                .query_row(
                    "SELECT account_id, recipient_domains FROM sent_message_ids WHERE message_id = ?1",
                    [message_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            Ok(found.and_then(|(account_id, domains)| {
                Some((account_id?, domains.split_whitespace().map(str::to_owned).collect()))
            }))
        })
        .await
    }

    /// The account that owns `address` itself, as its login or an alias. Groups, forwarding
    /// addresses and catch-alls do not count: a complaint names who sent, and those only receive.
    pub async fn address_owner(&self, address: &str) -> Result<Option<i64>> {
        let Ok((local, domain)) = crate::normalize_address(address) else { return Ok(None) };
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT a.account_id FROM addresses a JOIN domains d ON d.id = a.domain_id
                     JOIN accounts acc ON acc.id = a.account_id
                     WHERE a.local_part = ?1 AND d.name = ?2 AND acc.deleted_at IS NULL",
                    params![local, domain],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// Stores a complaint. Returns its id, and whether it counts as a strike: it does when it
    /// passed DMARC, may count, names an account, and nobody complained about the same message
    /// before.
    pub async fn add_abuse_report(&self, report: NewAbuseReport) -> Result<(i64, bool)> {
        self.write(move |tx| {
            let original = cut(report.original_message_id.map(|id| id.trim().trim_matches(['<', '>']).to_owned()))
                .filter(|id| !id.is_empty());
            let repeated = match &original {
                Some(id) => tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM abuse_reports WHERE original_message_id = ?1 AND counted)",
                    [id],
                    |row| row.get(0),
                )?,
                None => false,
            };
            let reporter_domain = report.reporter.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase());
            let reporter_counted = match (report.once_per_reporter_since, report.account_id, &reporter_domain) {
                (Some(since), Some(account_id), Some(domain)) => tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM abuse_reports WHERE account_id = ?1 AND counted
                         AND received_at >= ?2 AND lower(substr(reporter, instr(reporter, '@') + 1)) = ?3)",
                    params![account_id, since, domain],
                    |row| row.get(0),
                )?,
                _ => false,
            };
            let counted = report.authenticated
                && report.may_count
                && report.account_id.is_some()
                && !repeated
                && !reporter_counted;
            let login: Option<String> = match report.account_id {
                Some(id) => {
                    tx.query_row("SELECT login FROM accounts WHERE id = ?1", [id], |row| row.get(0)).optional()?
                }
                None => None,
            };
            tx.execute(
                "INSERT INTO abuse_reports (received_at, domain, reporter, authenticated, feedback_type, user_agent,
                     source_ip, original_message_id, original_from, original_subject, account_id, login, counted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    report.received_at,
                    report.domain,
                    cut(Some(report.reporter)).unwrap_or_default(),
                    report.authenticated,
                    cut(Some(report.feedback_type)).unwrap_or_default(),
                    cut(report.user_agent),
                    cut(report.source_ip),
                    original,
                    cut(report.original_from),
                    cut(report.original_subject),
                    report.account_id.filter(|_| login.is_some()),
                    login,
                    counted,
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute("DELETE FROM abuse_reports WHERE received_at < ?1", [report.received_at - REPORTS_KEPT_SECS])?;
            tx.execute("DELETE FROM abuse_reports WHERE id <= ?1 - ?2", params![id, REPORTS_KEPT_ROWS])?;
            Ok((id, counted))
        })
        .await
    }

    /// The newest complaints first.
    pub async fn abuse_reports(&self, limit: usize) -> Result<Vec<AbuseReport>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, received_at, domain, reporter, authenticated, feedback_type, user_agent, source_ip,
                        original_message_id, original_from, original_subject, login, counted
                 FROM abuse_reports ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit.clamp(1, 1000) as i64], report_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    const T0: i64 = 1_800_000_000;
    const LIMITS: StrikeLimits = StrikeLimits { strikes: 3, bounces: 2, window_secs: 3600, suspend_secs: 3600 };

    async fn setup() -> (Store, tempfile::TempDir, i64) {
        let (store, dir) = crate::test_support::store().await;
        store.create_domain("uwu.test").await.unwrap();
        let account = NewAccount {
            address: "mini@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let id = store.create_account(account).await.unwrap().id;
        (store, dir, id)
    }

    #[tokio::test]
    async fn strikes_in_the_window_suspend_and_the_suspension_ends() {
        let (store, _dir, id) = setup().await;
        assert!(store.record_send_strike(id, StrikeKind::Spam, T0, LIMITS).await.unwrap().is_none());
        // An old strike falls out of the window.
        assert!(store.record_send_strike(id, StrikeKind::Complaint, T0 + 3700, LIMITS).await.unwrap().is_none());
        assert!(store.record_send_strike(id, StrikeKind::Spam, T0 + 3800, LIMITS).await.unwrap().is_none());
        // Bounces count apart.
        assert!(store.record_send_strike(id, StrikeKind::Bounce, T0 + 3800, LIMITS).await.unwrap().is_none());
        let started = store.record_send_strike(id, StrikeKind::Spam, T0 + 3900, LIMITS).await.unwrap().unwrap();
        assert_eq!(
            (started.login.as_str(), started.until, started.reason.as_str()),
            ("mini@uwu.test", T0 + 7500, "spam")
        );
        // Strikes while suspended start nothing new.
        assert!(store.record_send_strike(id, StrikeKind::Spam, T0 + 4000, LIMITS).await.unwrap().is_none());
        assert!(store.send_suspension(id, T0 + 7499).await.unwrap().is_some());
        assert!(store.send_suspension(id, T0 + 7500).await.unwrap().is_none());
        assert_eq!(store.send_suspensions(T0 + 5000).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn bounces_rate_limits_and_lifting() {
        let (store, _dir, id) = setup().await;
        // Rate-limit refusals in one window are one strike.
        for n in 0..10 {
            store.record_send_strike(id, StrikeKind::RateLimit, T0 + n * 340, LIMITS).await.unwrap();
        }
        assert!(store.send_suspension(id, T0 + 10).await.unwrap().is_none());
        store.record_send_strike(id, StrikeKind::Bounce, T0 + 20, LIMITS).await.unwrap();
        let started = store.record_send_strike(id, StrikeKind::Bounce, T0 + 30, LIMITS).await.unwrap().unwrap();
        assert_eq!(started.reason, "bounce");
        let lifted = store.lift_send_suspension(id, T0 + 40).await.unwrap();
        assert_eq!(lifted.account_id, id);
        assert!(store.send_suspension(id, T0 + 41).await.unwrap().is_none());
        assert!(store.lift_send_suspension(id, T0 + 42).await.is_err());
        // Zero turns a limit off.
        let off = StrikeLimits { bounces: 0, ..LIMITS };
        for n in 0..5 {
            store.record_send_strike(id, StrikeKind::Bounce, T0 + 50 + n, off).await.unwrap();
        }
        assert!(store.send_suspension(id, T0 + 60).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn complaints_count_once_per_message_and_only_when_authenticated() {
        let (store, _dir, id) = setup().await;
        store.record_sent_message_id(id, "<abc@uwu.test>", vec!["Example.NET".into()], T0).await.unwrap();
        assert_eq!(store.sent_message_account("abc@uwu.test").await.unwrap(), Some((id, vec!["example.net".into()])));
        assert_eq!(store.address_owner("Mini@UWU.test").await.unwrap(), Some(id));
        assert_eq!(store.address_owner("nobody@uwu.test").await.unwrap(), None);
        let report = NewAbuseReport {
            received_at: T0,
            domain: "uwu.test".into(),
            reporter: "fbl@example.net".into(),
            authenticated: true,
            feedback_type: "abuse".into(),
            original_message_id: Some("<abc@uwu.test>".into()),
            account_id: Some(id),
            may_count: true,
            ..NewAbuseReport::default()
        };
        assert!(store.add_abuse_report(report.clone()).await.unwrap().1);
        assert!(!store.add_abuse_report(report.clone()).await.unwrap().1);
        let forged = NewAbuseReport { authenticated: false, original_message_id: None, ..report.clone() };
        assert!(!store.add_abuse_report(forged).await.unwrap().1);
        let unproven = NewAbuseReport { may_count: false, original_message_id: None, ..report };
        assert!(!store.add_abuse_report(unproven).await.unwrap().1);
        let listed = store.abuse_reports(10).await.unwrap();
        assert_eq!(listed.len(), 4);
        assert_eq!(listed[3].login.as_deref(), Some("mini@uwu.test"));
        assert_eq!(listed[3].original_message_id.as_deref(), Some("abc@uwu.test"));
        // Old Message-IDs are forgotten.
        store.record_sent_message_id(id, "new@uwu.test", vec![], T0 + SENT_KEPT_SECS + 1).await.unwrap();
        assert_eq!(store.sent_message_account("abc@uwu.test").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_message_id_two_accounts_used_belongs_to_neither() {
        let (store, _dir, id) = setup().await;
        let other = NewAccount {
            address: "leni@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let other = store.create_account(other).await.unwrap().id;
        store.record_sent_message_id(id, "same@uwu.test", vec!["example.net".into()], T0).await.unwrap();
        store.record_sent_message_id(id, "same@uwu.test", vec!["example.com".into()], T0 + 1).await.unwrap();
        let domains = vec!["example.net".to_owned(), "example.com".to_owned()];
        assert_eq!(store.sent_message_account("same@uwu.test").await.unwrap(), Some((id, domains)));
        store.record_sent_message_id(other, "same@uwu.test", vec![], T0 + 2).await.unwrap();
        assert_eq!(store.sent_message_account("same@uwu.test").await.unwrap(), None);
        store.record_sent_message_id(id, "same@uwu.test", vec![], T0 + 3).await.unwrap();
        assert_eq!(store.sent_message_account("same@uwu.test").await.unwrap(), None);
    }

    #[tokio::test]
    async fn one_reporter_domain_counts_once_per_window() {
        let (store, _dir, id) = setup().await;
        let report = |n: u32, reporter: &str| NewAbuseReport {
            received_at: T0 + i64::from(n),
            domain: "uwu.test".into(),
            reporter: reporter.into(),
            authenticated: true,
            feedback_type: "abuse".into(),
            original_message_id: Some(format!("m{n}@uwu.test")),
            account_id: Some(id),
            may_count: true,
            once_per_reporter_since: Some(T0),
            ..NewAbuseReport::default()
        };
        assert!(store.add_abuse_report(report(1, "a@example.net")).await.unwrap().1);
        assert!(!store.add_abuse_report(report(2, "b@Example.net")).await.unwrap().1);
        assert!(store.add_abuse_report(report(3, "a@example.com")).await.unwrap().1);
        let trusted = NewAbuseReport { once_per_reporter_since: None, ..report(4, "a@example.net") };
        assert!(store.add_abuse_report(trusted).await.unwrap().1);
    }
}
