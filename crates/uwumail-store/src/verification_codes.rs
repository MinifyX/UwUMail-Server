//! Codes and sign-in links found in delivered mail (migration 0081, docs/verification-codes.md):
//! kept per email for JMAP and push, limited per sender, and the mail tidied away once the code
//! is no use any more.
//!
//! What a mail contains is worked out in [`crate::verification`]; this is only what happens to the
//! answer. A mail is never refused or held over a code: past the limit, or with the guess switched
//! off, it is delivered like any other and simply has none.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::db::record_change;
use crate::mutate::{Batch, EmailUpdate, KeywordsChange, MailboxesChange, update_one};
use crate::verification::{Detected, Kind, Purpose, Source};
use crate::{Result, Store, now};

/// At most this many codes per account from one sender (registrable domain) within an hour. A
/// service that sends more is either broken or abused to flood someone's notifications; the mail
/// still arrives, as a normal mail.
pub const VERIFICATION_LIMIT_PER_HOUR: i64 = 10;

/// A code that was just found, for Web Push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationFound {
    pub account_id: i64,
    pub email_id: i64,
    pub detected: Detected,
}

/// What the cleanup setting `verificationCleanup` says: how long after expiry a mail with a code
/// moves to the Trash, or never.
fn cleanup_delay(preference: Option<&str>) -> Option<i64> {
    match preference.unwrap_or("24h") {
        "off" => None,
        "immediately" => Some(0),
        "7d" => Some(7 * 86_400),
        _ => Some(86_400),
    }
}

fn preference(conn: &Connection, account_id: i64, name: &str) -> Result<Option<String>> {
    let raw: Option<String> =
        conn.query_row("SELECT preferences FROM accounts WHERE id = ?1", [account_id], |row| row.get(0)).optional()?;
    let preferences: Value = raw.and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default();
    Ok(preferences.get(name).and_then(Value::as_str).map(str::to_owned))
}

fn within_limit(conn: &Connection, account_id: i64, author_domain: &str, now: i64) -> Result<bool> {
    let recent: i64 = conn.query_row(
        "SELECT COUNT(*) FROM email_verification WHERE account_id = ?1 AND author_domain = ?2 AND created > ?3",
        params![account_id, author_domain, now - 3600],
        |row| row.get(0),
    )?;
    Ok(recent < VERIFICATION_LIMIT_PER_HOUR)
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Option<Detected>)> {
    let email_id: i64 = row.get(0)?;
    let kind: String = row.get(1)?;
    let purpose: String = row.get(5)?;
    let source: String = row.get(7)?;
    let detected = (|| {
        Some(Detected {
            kind: Kind::parse(&kind)?,
            origin: row.get(2).ok()?,
            code: row.get(3).ok()?,
            url: row.get(4).ok()?,
            purpose: Purpose::parse(&purpose)?,
            expires_at: row.get(6).ok()?,
            source: Source::parse(&source)?,
            autofill: row.get(8).ok()?,
        })
    })();
    Ok((email_id, detected))
}

const COLUMNS: &str = "email_id, kind, origin, code, url, purpose, expires_at, source, autofill";

impl Store {
    /// Receives every code kept from now on.
    pub fn subscribe_verifications(&self) -> broadcast::Receiver<VerificationFound> {
        self.inner.verifications.subscribe()
    }

    /// Whether `account_id` may get another code from `author_domain` this hour. The SMTP server
    /// asks before it lets a mail with a signed code skip greylisting.
    pub async fn verification_within_limit(&self, account_id: i64, author_domain: &str) -> Result<bool> {
        let author_domain = author_domain.to_owned();
        self.read(move |conn| within_limit(conn, account_id, &author_domain, now())).await
    }

    /// Keeps the code found in a delivered mail for each of `email_ids` (the copies one delivery
    /// made in an account). Returns the ids it was kept for: none when the account switched guessing
    /// off and this is a guess, or when `author_domain` already sent the hour's share.
    pub async fn record_verification(
        &self,
        account_id: i64,
        email_ids: Vec<i64>,
        detected: &Detected,
        author_domain: &str,
    ) -> Result<Vec<i64>> {
        let found = detected.clone();
        let author_domain = author_domain.to_owned();
        let (kept, modseq) = self
            .write(move |tx| {
                if found.source == Source::Heuristic
                    && preference(tx, account_id, "mailVerificationHeuristics")?.as_deref() == Some("off")
                {
                    return Ok((Vec::new(), None));
                }
                let created = now();
                if !within_limit(tx, account_id, &author_domain, created)? {
                    return Ok((Vec::new(), None));
                }
                let mut batch = Batch::new(account_id);
                let mut kept = Vec::new();
                for email_id in email_ids {
                    let inserted = tx.execute(
                        "INSERT OR IGNORE INTO email_verification
                             (email_id, account_id, kind, origin, code, url, purpose, expires_at, source, autofill,
                              author_domain, created)
                         SELECT id, account_id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
                         FROM emails WHERE id = ?1 AND account_id = ?2",
                        params![
                            email_id,
                            account_id,
                            found.kind.as_str(),
                            found.origin,
                            found.code,
                            found.url,
                            found.purpose.as_str(),
                            found.expires_at,
                            found.source.as_str(),
                            found.autofill,
                            author_domain,
                            created,
                        ],
                    )?;
                    if inserted == 0 {
                        continue;
                    }
                    // The email's `verification` property changed: clients that already fetched it
                    // see it in Email/changes.
                    let modseq = batch.modseq(tx)?;
                    tx.execute("UPDATE emails SET updated_modseq = ?1 WHERE id = ?2", params![modseq, email_id])?;
                    record_change(tx, account_id, modseq, "Email", email_id, "updated")?;
                    kept.push(email_id);
                }
                Ok((kept, batch.modseq))
            })
            .await?;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        for email_id in &kept {
            // Nobody listening is fine.
            let _ = self.inner.verifications.send(VerificationFound {
                account_id,
                email_id: *email_id,
                detected: detected.clone(),
            });
        }
        Ok(kept)
    }

    /// The codes of those of `email_ids` that have one.
    pub async fn verifications(&self, account_id: i64, email_ids: Vec<i64>) -> Result<HashMap<i64, Detected>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {COLUMNS} FROM email_verification WHERE account_id = ?1 AND email_id = ?2"
            ))?;
            let mut found = HashMap::new();
            for email_id in email_ids {
                if let Some((id, Some(detected))) =
                    stmt.query_row(params![account_id, email_id], from_row).optional()?
                {
                    found.insert(id, detected);
                }
            }
            Ok(found)
        })
        .await
    }

    /// The emails with a code, newest first. `expired`: only expired (`Some(true)`), only still
    /// valid (`Some(false)`) or all.
    pub async fn query_verifications(&self, account_id: i64, expired: Option<bool>) -> Result<Vec<i64>> {
        self.read(move |conn| {
            let at = now();
            let condition = match expired {
                Some(true) => "AND expires_at <= ?2",
                Some(false) => "AND expires_at > ?2",
                None => "AND ?2 = ?2",
            };
            let mut stmt = conn.prepare(&format!(
                "SELECT email_id FROM email_verification WHERE account_id = ?1 {condition}
                 ORDER BY created DESC, email_id DESC"
            ))?;
            let ids = stmt.query_map(params![account_id, at], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
            Ok(ids)
        })
        .await
    }

    /// Moves mail whose code expired out of the Inbox into the Trash, once the delay of the
    /// account's `verificationCleanup` passed: 24 hours unless the person chose otherwise.
    /// Flagged mail and mail moved elsewhere stay where they are. Returns how many mails moved.
    pub async fn clean_up_verifications(&self) -> Result<usize> {
        self.clean_up_verifications_at(now()).await
    }

    pub(crate) async fn clean_up_verifications_at(&self, at: i64) -> Result<usize> {
        let accounts: Vec<i64> = self
            .read(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT account_id FROM email_verification WHERE cleaned = 0 AND expires_at <= ?1",
                )?;
                let ids = stmt.query_map([at], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
                Ok(ids)
            })
            .await?;
        let mut moved = 0;
        for account_id in accounts {
            let (count, modseq) = self.write(move |tx| clean_up_account(tx, account_id, at)).await?;
            moved += count;
            if let Some(modseq) = modseq {
                self.notify_change(account_id, modseq);
            }
        }
        Ok(moved)
    }
}

/// One account's share of the cleanup, in one transaction.
fn clean_up_account(tx: &Transaction<'_>, account_id: i64, at: i64) -> Result<(usize, Option<i64>)> {
    let Some(delay) = cleanup_delay(preference(tx, account_id, "mailVerificationCleanup")?.as_deref()) else {
        return Ok((0, None));
    };
    let role = |name: &str| -> Result<Option<i64>> {
        Ok(tx
            .query_row(
                "SELECT id FROM mailboxes WHERE account_id = ?1 AND role = ?2",
                params![account_id, name],
                |row| row.get(0),
            )
            .optional()?)
    };
    let (Some(inbox), Some(trash)) = (role("inbox")?, role("trash")?) else {
        return Ok((0, None));
    };
    let due: Vec<(i64, bool, bool)> = {
        let mut stmt = tx.prepare(
            "SELECT v.email_id,
                    EXISTS (SELECT 1 FROM email_keywords k WHERE k.email_id = v.email_id AND k.keyword = '$flagged'),
                    EXISTS (SELECT 1 FROM email_mailboxes m WHERE m.email_id = v.email_id AND m.mailbox_id = ?3)
             FROM email_verification v
             WHERE v.account_id = ?1 AND v.cleaned = 0 AND v.expires_at <= ?2",
        )?;
        stmt.query_map(params![account_id, at.saturating_sub(delay), inbox], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    // The server tidies up, not the person: nothing here teaches the labels.
    let mut batch = Batch { by_hand: false, ..Batch::new(account_id) };
    let mut moved = 0;
    for (email_id, flagged, in_inbox) in due {
        if !flagged && in_inbox {
            let update = EmailUpdate {
                id: email_id,
                keywords: KeywordsChange::Keep,
                mailboxes: MailboxesChange::Patch(vec![(inbox, false), (trash, true)]),
            };
            update_one(tx, &mut batch, &update)?;
            moved += 1;
        }
        // Left alone once is left alone for good: a flag taken off later does not bring it back.
        tx.execute("UPDATE email_verification SET cleaned = 1 WHERE email_id = ?1", [email_id])?;
    }
    Ok((moved, batch.modseq))
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::test_support::store;
    use crate::verification::{Authenticity, detect};
    use crate::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role};

    async fn setup() -> (Store, tempfile::TempDir, i64) {
        let (store, dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let account = store
            .create_account(NewAccount {
                address: "mini@example.org".into(),
                display_name: "Mini".into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        (store, dir, account)
    }

    async fn deliver(store: &Store, account: i64) -> i64 {
        let raw = "From: no-reply@shop.example\r\nTo: mini@example.org\r\nSubject: Code\r\n\r\nDein Code: 482913\r\n";
        let request = IngestRequest {
            account_id: account,
            raw: raw.as_bytes().to_vec(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        };
        store.ingest(request).await.unwrap().id
    }

    fn found(expires_at: i64) -> Detected {
        let auth = Authenticity { header_signed: false, dmarc_passed: true, from_domain: Some("shop.example".into()) };
        Detected {
            expires_at,
            ..detect(b"From: a@shop.example\r\nSubject: Code\r\n\r\nCode: 482913\r\n", &auth, 0).unwrap()
        }
    }

    async fn prefer(store: &Store, account: i64, name: &str, value: &str) {
        let mut changes = Map::new();
        changes.insert(name.into(), json!(value));
        store.update_preferences(account, changes).await.unwrap();
    }

    #[tokio::test]
    async fn codes_are_kept_limited_and_announced() {
        let (store, _dir, account) = setup().await;
        let mut announced = store.subscribe_verifications();
        let email = deliver(&store, account).await;
        let detected = found(now() + 600);
        assert_eq!(store.record_verification(account, vec![email], &detected, "shop.example").await.unwrap(), [email]);
        assert_eq!(announced.recv().await.unwrap().email_id, email);
        assert_eq!(store.verifications(account, vec![email, email + 99]).await.unwrap()[&email], detected);
        assert_eq!(store.query_verifications(account, Some(false)).await.unwrap(), [email]);
        assert!(store.query_verifications(account, Some(true)).await.unwrap().is_empty());

        // The hour's share from one sender, then mail without codes.
        for _ in 1..VERIFICATION_LIMIT_PER_HOUR {
            let email = deliver(&store, account).await;
            assert_eq!(
                store.record_verification(account, vec![email], &detected, "shop.example").await.unwrap().len(),
                1
            );
        }
        assert!(!store.verification_within_limit(account, "shop.example").await.unwrap());
        let over = deliver(&store, account).await;
        assert!(store.record_verification(account, vec![over], &detected, "shop.example").await.unwrap().is_empty());
        assert!(store.verification_within_limit(account, "bank.example").await.unwrap());

        // Guessing switched off: only signed headers count.
        prefer(&store, account, "mailVerificationHeuristics", "off").await;
        let email = deliver(&store, account).await;
        assert!(store.record_verification(account, vec![email], &detected, "bank.example").await.unwrap().is_empty());
        let signed = Detected { source: Source::Header, ..detected };
        assert_eq!(store.record_verification(account, vec![email], &signed, "bank.example").await.unwrap(), [email]);
    }

    #[tokio::test]
    async fn expired_codes_go_to_the_trash_as_set() {
        let (store, _dir, account) = setup().await;
        let mailboxes = store.imap_mailboxes(account).await.unwrap();
        let role = |role: MailboxRole| mailboxes.iter().find(|m| m.role == Some(role)).unwrap().id;
        let (inbox, trash) = (role(MailboxRole::Inbox), role(MailboxRole::Trash));
        let at = now();
        let plain = deliver(&store, account).await;
        let flagged = deliver(&store, account).await;
        let fresh = deliver(&store, account).await;
        for (email, expires) in [(plain, at - 3600), (flagged, at - 3600), (fresh, at + 600)] {
            store.record_verification(account, vec![email], &found(expires), "shop.example").await.unwrap();
        }
        store
            .update_emails(
                account,
                vec![EmailUpdate {
                    id: flagged,
                    keywords: KeywordsChange::Patch(vec![("$flagged".into(), true)]),
                    mailboxes: MailboxesChange::Keep,
                }],
            )
            .await
            .unwrap();

        // The default waits a day.
        assert_eq!(store.clean_up_verifications_at(at).await.unwrap(), 0);
        prefer(&store, account, "mailVerificationCleanup", "off").await;
        assert_eq!(store.clean_up_verifications_at(at + 30 * 86_400).await.unwrap(), 0);
        prefer(&store, account, "mailVerificationCleanup", "7d").await;
        assert_eq!(store.clean_up_verifications_at(at + 86_400).await.unwrap(), 0);
        prefer(&store, account, "mailVerificationCleanup", "immediately").await;
        assert_eq!(store.clean_up_verifications_at(at).await.unwrap(), 1);
        let in_mailbox = |id: i64| {
            let store = store.clone();
            async move { store.email(account, id).await.unwrap().mailbox_ids }
        };
        assert_eq!(in_mailbox(plain).await, [trash]);
        assert_eq!(in_mailbox(flagged).await, [inbox], "flagged mail stays");
        assert_eq!(in_mailbox(fresh).await, [inbox]);
        // Done once: nothing moves twice.
        assert_eq!(store.clean_up_verifications_at(at + 3600).await.unwrap(), 1);
        assert_eq!(in_mailbox(fresh).await, [trash]);
        assert_eq!(store.clean_up_verifications_at(at + 7200).await.unwrap(), 0);
    }
}
