//! Outbound queue: messages waiting for delivery to other servers.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::normalize_address;
use crate::blobs::BlobHash;
use crate::{Result, Store, StoreError, now};

/// How long a refused mail stays on the admin's list.
pub const REFUSED_KEPT_SECS: i64 = 30 * 24 * 3600;
/// And how many of them at most, so a flood of bounces cannot fill the disk.
const REFUSED_KEPT_ROWS: i64 = 5000;
/// Larger mail is not kept for sending again, so refusals cannot fill the disk with copies.
const REFUSED_COPY_MAX_BYTES: i64 = 10 * 1024 * 1024;
/// What the other server said can be a page of text; the list keeps the start of it.
const REFUSED_ERROR_MAX_CHARS: usize = 500;

/// An outgoing mail another server did not take.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusedDelivery {
    pub id: i64,
    pub at: i64,
    /// The return path, empty for bounces.
    pub from: String,
    pub to: String,
    pub error: String,
    /// Put off until the message expired, rather than refused outright.
    pub gave_up: bool,
    /// The message was kept, so it can be sent again; older entries and bounces have none.
    pub can_resend: bool,
}

/// What marking a refused mail as fixed did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusedResend {
    /// Mails put back into the queue.
    pub resent: usize,
    /// Entries taken off the list without a copy to send.
    pub dropped: usize,
    /// Left on the list: the sender's account is gone, in the trash, disabled, without SMTP, no
    /// longer owns the sender address, or may not send right now.
    pub skipped: usize,
}

/// All kept copies together stay under this, so many mid-sized refusals cannot fill the disk either.
const REFUSED_COPIES_MAX_BYTES: i64 = 1024 * 1024 * 1024;

/// Microsoft's consumer mail domains, which share one set of block lists. An exact list: a
/// prefix like "live." would also take in live.example.org.
const MICROSOFT_DOMAINS: [&str; 22] = [
    "hotmail.com",
    "hotmail.de",
    "hotmail.co.uk",
    "hotmail.fr",
    "hotmail.it",
    "hotmail.es",
    "hotmail.nl",
    "hotmail.be",
    "outlook.com",
    "outlook.de",
    "outlook.fr",
    "outlook.it",
    "outlook.es",
    "outlook.at",
    "live.com",
    "live.de",
    "live.co.uk",
    "live.fr",
    "live.it",
    "live.nl",
    "msn.com",
    "passport.com",
];

/// The reply code and the enhanced status code of a refusal, as far as the text has them.
fn refusal_codes(error: &str) -> (Option<&str>, Option<&str>) {
    let mut words = error.split_whitespace();
    let code = words.next().filter(|word| word.len() == 3 && word.bytes().all(|b| b.is_ascii_digit()));
    (code, code.and(words.next()).filter(|word| word.split('.').count() == 3))
}

/// Whether the other server said this one recipient does not exist (5.1.x): nothing to fix on
/// this side, so neither a kept copy nor a group with other recipients of the domain.
fn unknown_recipient(error: &str) -> bool {
    refusal_codes(error).1.is_some_and(|enhanced| enhanced.starts_with("5.1."))
}

/// Which refusals share a cause, so fixing one fixes them all: the reply code and the domain (the
/// address for an unknown recipient), or for Microsoft the named block list, which is the same
/// for hotmail.com, outlook.com and live.com.
fn refusal_cause(recipient: &str, error: &str) -> String {
    let (code, enhanced) = refusal_codes(error);
    let code = format!("{} {}", code.unwrap_or(""), enhanced.unwrap_or(""));
    let microsoft_list =
        error.split('(').skip(1).filter_map(|rest| rest.split_once(')')).map(|(inside, _)| inside).find(|inside| {
            inside.len() > 1 && inside.starts_with('S') && inside[1..].bytes().all(|b| b.is_ascii_digit())
        });
    let domain = recipient.rsplit_once('@').map_or(recipient, |(_, domain)| domain);
    // Only Microsoft's own domains: anyone else could write "(S3150)" to join the group.
    let microsoft = MICROSOFT_DOMAINS.iter().any(|known| domain.eq_ignore_ascii_case(known));
    match microsoft_list.filter(|_| microsoft) {
        Some(list) => format!("{code} {list}"),
        None if unknown_recipient(error) => format!("{code} {}", recipient.to_ascii_lowercase()),
        None => format!("{code} @{domain}"),
    }
}

#[derive(Debug, Clone)]
pub struct NewQueueRecipient {
    pub address: String,
    /// `smtp_proto::RCPT_NOTIFY_*` flags.
    pub notify_flags: u64,
    pub orcpt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueRecipientStatus {
    Pending,
    Delivered,
    Failed,
}

impl QueueRecipientStatus {
    fn parse(value: &str) -> QueueRecipientStatus {
        match value {
            "delivered" => QueueRecipientStatus::Delivered,
            "failed" => QueueRecipientStatus::Failed,
            _ => QueueRecipientStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueRecipient {
    pub id: i64,
    pub message_id: i64,
    pub address: String,
    pub domain: String,
    pub status: QueueRecipientStatus,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub notify_flags: u64,
    pub orcpt: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueuedMessage {
    pub id: i64,
    #[serde(skip)]
    pub blob: BlobHash,
    /// Empty for bounces.
    pub return_path: String,
    pub account_id: Option<i64>,
    pub size: i64,
    pub env_id: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueEntry {
    pub message: QueuedMessage,
    pub recipients: Vec<QueueRecipient>,
}

fn recipient_from_row(row: &Row<'_>) -> rusqlite::Result<QueueRecipient> {
    Ok(QueueRecipient {
        id: row.get(0)?,
        message_id: row.get(1)?,
        address: row.get(2)?,
        domain: row.get(3)?,
        status: QueueRecipientStatus::parse(&row.get::<_, String>(4)?),
        attempts: row.get(5)?,
        next_attempt_at: row.get(6)?,
        last_error: row.get(7)?,
        notify_flags: row.get::<_, i64>(8)? as u64,
        orcpt: row.get(9)?,
    })
}

const RECIPIENT_COLUMNS: &str =
    "id, message_id, address, domain, status, attempts, next_attempt_at, last_error, notify_flags, orcpt";

const MESSAGE_COLUMNS: &str = "id, blob_hash, return_path, account_id, size, env_id, created_at, expires_at";

type MessageRow = (i64, String, String, Option<i64>, i64, Option<String>, i64, i64);

fn message_row(row: &Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?))
}

fn message_from_row(
    (id, blob, return_path, account_id, size, env_id, created_at, expires_at): MessageRow,
) -> Result<QueuedMessage> {
    Ok(QueuedMessage {
        id,
        blob: BlobHash::parse(&blob)?,
        return_path,
        account_id,
        size,
        env_id,
        created_at,
        expires_at,
    })
}

fn load_message(conn: &Connection, id: i64) -> Result<Option<QueuedMessage>> {
    let row = conn
        .query_row(&format!("SELECT {MESSAGE_COLUMNS} FROM queue_messages WHERE id = ?1"), [id], message_row)
        .optional()?;
    row.map(message_from_row).transpose()
}

fn set_recipient(
    conn: &Connection,
    recipient_id: i64,
    status: &str,
    message: Option<&str>,
    next_attempt_at: Option<i64>,
) -> Result<()> {
    let changed = conn.execute(
        "UPDATE queue_recipients
         SET status = ?1, last_error = ?2, attempts = attempts + 1,
             next_attempt_at = coalesce(?3, next_attempt_at), updated_at = ?4, leased_until = 0
         WHERE id = ?5",
        params![status, message, next_attempt_at, now(), recipient_id],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound(format!("queue recipient {recipient_id}")));
    }
    Ok(())
}

impl Store {
    /// Adds a message for delivery to remote recipients and wakes the delivery worker.
    pub async fn enqueue(
        &self,
        return_path: &str,
        recipients: Vec<NewQueueRecipient>,
        raw: &[u8],
        account_id: Option<i64>,
        env_id: Option<String>,
        lifetime_secs: i64,
    ) -> Result<i64> {
        if recipients.is_empty() {
            return Err(StoreError::Invalid("a queued message needs recipients".into()));
        }
        let mut normalized = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            let (local, domain) = normalize_address(&recipient.address)?;
            normalized.push((format!("{local}@{domain}"), domain, recipient));
        }
        let blob = self.put_blob(raw).await?;
        let (blob_key, size, return_path) = (blob.as_str().to_owned(), raw.len() as i64, return_path.to_owned());
        let id = self
            .write(move |tx| {
                let created_at = now();
                tx.execute(
                    "INSERT INTO queue_messages (blob_hash, return_path, account_id, size, env_id, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![blob_key, return_path, account_id, size, env_id, created_at, created_at + lifetime_secs],
                )?;
                let id = tx.last_insert_rowid();
                for (address, domain, recipient) in normalized {
                    tx.execute(
                        "INSERT INTO queue_recipients (message_id, address, domain, next_attempt_at, notify_flags, orcpt, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?4)",
                        params![id, address, domain, created_at, recipient.notify_flags as i64, recipient.orcpt],
                    )?;
                }
                Ok(id)
            })
            .await?;
        self.inner.queue_wakeup.notify_one();
        Ok(id)
    }

    /// Takes pending recipients that are due and leases them for `lease_secs`, so a
    /// second pass does not pick them up while delivery is still running.
    pub async fn claim_due_deliveries(&self, limit: usize, lease_secs: i64) -> Result<Vec<QueueEntry>> {
        self.write(move |tx| {
            let now = now();
            let mut stmt = tx.prepare(&format!(
                "UPDATE queue_recipients SET next_attempt_at = ?1, leased_until = ?1
                 WHERE id IN (SELECT id FROM queue_recipients WHERE status = 'pending' AND next_attempt_at <= ?2
                              ORDER BY next_attempt_at LIMIT ?3)
                 RETURNING {RECIPIENT_COLUMNS}"
            ))?;
            let claimed = stmt
                .query_map(params![now + lease_secs, now, limit as i64], recipient_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            let mut entries: Vec<QueueEntry> = Vec::new();
            for recipient in claimed {
                if let Some(entry) = entries.iter_mut().find(|e| e.message.id == recipient.message_id) {
                    entry.recipients.push(recipient);
                } else if let Some(message) = load_message(tx, recipient.message_id)? {
                    entries.push(QueueEntry { message, recipients: vec![recipient] });
                }
            }
            Ok(entries)
        })
        .await
    }

    /// When the next pending recipient is due, if any.
    pub async fn next_queue_attempt_at(&self) -> Result<Option<i64>> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT min(next_attempt_at) FROM queue_recipients WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn mark_recipient_delivered(&self, recipient_id: i64, response: &str) -> Result<()> {
        self.update_recipient(recipient_id, "delivered", Some(response.to_owned()), None).await
    }

    pub async fn mark_recipient_deferred(&self, recipient_id: i64, error: &str, next_attempt_at: i64) -> Result<()> {
        self.update_recipient(recipient_id, "pending", Some(error.to_owned()), Some(next_attempt_at)).await
    }

    pub async fn mark_recipient_failed(&self, recipient_id: i64, error: &str) -> Result<()> {
        self.fail_recipient(recipient_id, error, false).await
    }

    /// Like [`Self::mark_recipient_failed`], for a recipient that was put off until the message
    /// expired.
    pub async fn mark_recipient_given_up(&self, recipient_id: i64, error: &str) -> Result<()> {
        self.fail_recipient(recipient_id, error, true).await
    }

    /// Marks a recipient failed and writes down in the same transaction that its server did not
    /// take the mail, for the admin to read after the queue has forgotten the message. Kept
    /// [`REFUSED_KEPT_SECS`] long.
    async fn fail_recipient(&self, recipient_id: i64, error: &str, gave_up: bool) -> Result<()> {
        let error = error.to_owned();
        self.write(move |tx| {
            set_recipient(tx, recipient_id, "failed", Some(&error), None)?;
            let keep_copy = !unknown_recipient(&error);
            let error: String = error.chars().take(REFUSED_ERROR_MAX_CHARS).collect();
            let now = now();
            tx.execute(
                "INSERT INTO refused_deliveries
                     (at, sender, recipient, error, gave_up, blob_hash, account_id, env_id, notify_flags, orcpt, lifetime)
                 SELECT ?1, m.return_path, r.address, ?2, ?3,
                        -- Only what a person here wrote: forwards (SRS return path) and bounces keep nothing.
                        CASE WHEN ?6 AND m.size <= ?5 AND EXISTS (
                            SELECT 1 FROM addresses a JOIN domains d ON d.id = a.domain_id
                            JOIN accounts acc ON acc.id = a.account_id
                            WHERE a.account_id = m.account_id AND acc.deleted_at IS NULL
                              AND a.local_part || '@' || d.name = lower(m.return_path)
                        ) AND (SELECT coalesce(sum(size), 0) FROM blobs
                               WHERE hash IN (SELECT blob_hash FROM refused_deliveries)) < ?7
                        THEN m.blob_hash END,
                        m.account_id, m.env_id, r.notify_flags, r.orcpt, m.expires_at - m.created_at
                 FROM queue_recipients r JOIN queue_messages m ON m.id = r.message_id WHERE r.id = ?4",
                params![now, error, gave_up, recipient_id, REFUSED_COPY_MAX_BYTES, keep_copy, REFUSED_COPIES_MAX_BYTES],
            )?;
            tx.execute("DELETE FROM refused_deliveries WHERE at < ?1", [now - REFUSED_KEPT_SECS])?;
            tx.execute(
                "DELETE FROM refused_deliveries WHERE id <= (SELECT max(id) FROM refused_deliveries) - ?1",
                [REFUSED_KEPT_ROWS],
            )?;
            Ok(())
        })
        .await
    }

    /// The newest outgoing mails another server did not take, newest first.
    pub async fn refused_deliveries(&self, limit: usize) -> Result<Vec<RefusedDelivery>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, at, sender, recipient, error, gave_up, blob_hash IS NOT NULL FROM refused_deliveries
                 WHERE at >= ?1 ORDER BY at DESC, id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now() - REFUSED_KEPT_SECS, limit as i64], |row| {
                Ok(RefusedDelivery {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    from: row.get(2)?,
                    to: row.get(3)?,
                    error: row.get(4)?,
                    gave_up: row.get(5)?,
                    can_resend: row.get(6)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// Marks a refused mail as fixed: it and every other refusal with the same cause leave the list,
    /// and those with a kept copy go back into the queue.
    pub async fn resend_refused(&self, id: i64) -> Result<RefusedResend> {
        let outcome = self
            .write(move |tx| {
                let now = now();
                let mut stmt = tx.prepare(
                    "SELECT r.id, r.recipient, r.error, r.blob_hash, r.sender,
                            CASE WHEN acc.deleted_at IS NULL AND acc.disabled = 0 AND acc.smtp_enabled AND NOT EXISTS (
                                SELECT 1 FROM send_suspensions s WHERE s.account_id = acc.id AND s.until > ?2
                            ) AND EXISTS (
                                SELECT 1 FROM addresses a JOIN domains d ON d.id = a.domain_id
                                WHERE a.account_id = acc.id AND a.local_part || '@' || d.name = lower(r.sender)
                            ) THEN acc.id END,
                            r.env_id, r.notify_flags, r.orcpt, r.lifetime
                     FROM refused_deliveries r LEFT JOIN accounts acc ON acc.id = r.account_id WHERE r.at >= ?1",
                )?;
                type Row = (i64, String, String, Option<String>, String, Option<i64>, Option<String>, i64, Option<String>, i64);
                let rows = stmt
                    .query_map([now - REFUSED_KEPT_SECS, now], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<Row>>>()?;
                drop(stmt);
                let Some(fixed) = rows.iter().find(|row| row.0 == id) else {
                    return Err(StoreError::NotFound(format!("refused mail {id}")));
                };
                let cause = refusal_cause(&fixed.1, &fixed.2);
                let mut outcome = RefusedResend { resent: 0, dropped: 0, skipped: 0 };
                for (row_id, recipient, error, blob, sender, account_id, env_id, notify_flags, orcpt, lifetime) in &rows {
                    if refusal_cause(recipient, error) != cause {
                        continue;
                    }
                    // Mail of a sender who may not send now stays, so fixing one entry never sends theirs.
                    if blob.is_some() && account_id.is_none() {
                        outcome.skipped += 1;
                        continue;
                    }
                    let queued = match blob {
                        Some(blob) => tx.execute(
                            "INSERT INTO queue_messages (blob_hash, return_path, account_id, size, env_id, created_at, expires_at)
                             SELECT hash, ?2, ?3, size, ?4, ?5, ?6 FROM blobs WHERE hash = ?1",
                            params![blob, sender, account_id, env_id, now, now + (*lifetime).max(3600)],
                        )? == 1,
                        None => false,
                    };
                    if queued {
                        let domain = recipient.rsplit_once('@').map_or("", |(_, domain)| domain);
                        tx.execute(
                            "INSERT INTO queue_recipients (message_id, address, domain, next_attempt_at, notify_flags, orcpt, updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?4)",
                            params![tx.last_insert_rowid(), recipient, domain, now, notify_flags, orcpt],
                        )?;
                        outcome.resent += 1;
                    } else {
                        outcome.dropped += 1;
                    }
                    tx.execute("DELETE FROM refused_deliveries WHERE id = ?1", [row_id])?;
                }
                Ok(outcome)
            })
            .await?;
        if outcome.resent > 0 {
            self.inner.queue_wakeup.notify_one();
        }
        Ok(outcome)
    }

    /// Empties the list of refused mails. Returns how many there were.
    pub async fn clear_refused_deliveries(&self) -> Result<usize> {
        self.write(|tx| Ok(tx.execute("DELETE FROM refused_deliveries", [])?)).await
    }

    async fn update_recipient(
        &self,
        recipient_id: i64,
        status: &'static str,
        message: Option<String>,
        next_attempt_at: Option<i64>,
    ) -> Result<()> {
        self.write(move |tx| set_recipient(tx, recipient_id, status, message.as_deref(), next_attempt_at)).await
    }

    /// Removes a message once no recipient is pending. Returns whether it was removed.
    pub async fn complete_queue_message(&self, message_id: i64) -> Result<bool> {
        self.write(move |tx| {
            let pending: i64 = tx.query_row(
                "SELECT count(*) FROM queue_recipients WHERE message_id = ?1 AND status = 'pending'",
                [message_id],
                |row| row.get(0),
            )?;
            if pending > 0 {
                return Ok(false);
            }
            tx.execute("DELETE FROM queue_messages WHERE id = ?1", [message_id])?;
            Ok(true)
        })
        .await
    }

    /// Every queued message with its recipients, oldest first, in two queries.
    pub async fn queue_entries(&self) -> Result<Vec<QueueEntry>> {
        self.oldest_queue_entries(-1).await
    }

    /// The `limit` oldest queued messages with their recipients; -1 for all of them.
    pub async fn oldest_queue_entries(&self, limit: i64) -> Result<Vec<QueueEntry>> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare(&format!("SELECT {MESSAGE_COLUMNS} FROM queue_messages ORDER BY created_at, id LIMIT ?1"))?;
            let messages = stmt.query_map([limit], message_row)?.collect::<Result<Vec<_>, _>>()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {RECIPIENT_COLUMNS} FROM queue_recipients
                 WHERE message_id IN (SELECT id FROM queue_messages ORDER BY created_at, id LIMIT ?1) ORDER BY id"
            ))?;
            let mut recipients: std::collections::HashMap<i64, Vec<QueueRecipient>> = Default::default();
            for recipient in stmt.query_map([limit], recipient_from_row)? {
                let recipient = recipient?;
                recipients.entry(recipient.message_id).or_default().push(recipient);
            }
            messages
                .into_iter()
                .map(|row| {
                    let message = message_from_row(row)?;
                    Ok(QueueEntry { recipients: recipients.remove(&message.id).unwrap_or_default(), message })
                })
                .collect()
        })
        .await
    }

    /// Makes all pending recipients of a message due now, except those a worker is sending right now.
    pub async fn retry_queue_message(&self, message_id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "UPDATE queue_recipients SET next_attempt_at = ?1
                 WHERE message_id = ?2 AND status = 'pending' AND leased_until <= ?1",
                params![now(), message_id],
            )?;
            let pending: i64 = tx.query_row(
                "SELECT count(*) FROM queue_recipients WHERE message_id = ?1 AND status = 'pending'",
                [message_id],
                |row| row.get(0),
            )?;
            if pending == 0 {
                return Err(StoreError::NotFound(format!("pending queue message {message_id}")));
            }
            Ok(())
        })
        .await?;
        self.inner.queue_wakeup.notify_one();
        Ok(())
    }

    pub async fn delete_queue_message(&self, message_id: i64) -> Result<()> {
        self.write(move |tx| {
            if tx.execute("DELETE FROM queue_messages WHERE id = ?1", [message_id])? == 0 {
                return Err(StoreError::NotFound(format!("queue message {message_id}")));
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;

    fn recipient(address: &str) -> NewQueueRecipient {
        NewQueueRecipient { address: address.into(), notify_flags: 0, orcpt: None }
    }

    #[tokio::test]
    async fn queue_lifecycle() {
        let (store, _dir) = store().await;
        let id = store
            .enqueue(
                "mini@example.org",
                vec![recipient("a@gmx.de"), recipient("B@Web.de")],
                b"Subject: hi\r\n\r\nhi\r\n",
                None,
                None,
                3600,
            )
            .await
            .unwrap();

        let claimed = store.claim_due_deliveries(10, 600).await.unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].message.id, id);
        let domains: Vec<_> = claimed[0].recipients.iter().map(|r| r.domain.as_str()).collect();
        assert_eq!(domains, ["gmx.de", "web.de"]);
        // Leased: a second pass gets nothing.
        assert!(store.claim_due_deliveries(10, 600).await.unwrap().is_empty());

        let (first, second) = (&claimed[0].recipients[0], &claimed[0].recipients[1]);
        store.mark_recipient_delivered(first.id, "250 ok").await.unwrap();
        store.mark_recipient_deferred(second.id, "451 try later", 0).await.unwrap();
        assert!(!store.complete_queue_message(id).await.unwrap());

        let retry = store.claim_due_deliveries(10, 600).await.unwrap();
        assert_eq!(retry[0].recipients.len(), 1);
        assert_eq!(retry[0].recipients[0].attempts, 1);
        store.mark_recipient_failed(second.id, "550 no such user").await.unwrap();
        assert!(store.complete_queue_message(id).await.unwrap());
        assert!(store.queue_entries().await.unwrap().is_empty());
        // The queue has forgotten the message; the admin can still read who refused it.
        let refused = store.refused_deliveries(10).await.unwrap();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].to, second.address);
        assert_eq!(refused[0].error, "550 no such user");
        assert!(!refused[0].gave_up);
        assert_eq!(store.clear_refused_deliveries().await.unwrap(), 1);
        assert!(store.refused_deliveries(10).await.unwrap().is_empty());
        assert_eq!(store.collect_garbage(0).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn retry_leaves_a_leased_recipient_alone() {
        let (store, _dir) = store().await;
        let id = store
            .enqueue(
                "mini@example.org",
                vec![recipient("a@example.com")],
                b"Subject: hi\r\n\r\nhi\r\n",
                None,
                None,
                3600,
            )
            .await
            .unwrap();
        let claimed = store.claim_due_deliveries(10, 600).await.unwrap();
        store.retry_queue_message(id).await.unwrap();
        assert!(store.claim_due_deliveries(10, 600).await.unwrap().is_empty(), "still being sent");

        // Once the attempt is over, Retry makes it due again.
        store.mark_recipient_deferred(claimed[0].recipients[0].id, "451 try later", now() + 3600).await.unwrap();
        store.retry_queue_message(id).await.unwrap();
        assert_eq!(store.claim_due_deliveries(10, 600).await.unwrap().len(), 1);
    }

    #[test]
    fn refusals_share_a_cause_by_code_and_domain_or_microsoft_list() {
        let s3150 = "550 5.7.1 Unfortunately, messages from [192.0.2.6] weren't sent. part of their network is on our block list (S3150). [AGT=PFA]";
        assert_eq!(refusal_cause("a@hotmail.com", s3150), refusal_cause("b@outlook.com", s3150));
        assert_ne!(refusal_cause("a@example.com", "550 5.7.1 no"), refusal_cause("a@example.net", "550 5.7.1 no"));
        assert_ne!(refusal_cause("a@example.com", s3150), refusal_cause("b@hotmail.com", s3150), "only Microsoft's");
        assert_ne!(refusal_cause("a@example.com", "550 5.7.1 no"), refusal_cause("b@example.com", "550 5.1.1 no"));
        assert_eq!(refusal_cause("a@example.com", "connection refused"), refusal_cause("b@example.com", "timed out"));
        // An unknown recipient is about that one address, not the whole domain.
        assert_ne!(refusal_cause("a@example.com", "550 5.1.1 no"), refusal_cause("b@example.com", "550 5.1.1 no"));
        assert_eq!(refusal_cause("A@example.com", "550 5.1.1 no"), refusal_cause("a@example.com", "550 5.1.1 gone"));
        // Microsoft's domains are listed exactly: a look-alike is not one of them.
        assert_eq!(refusal_cause("a@hotmail.co.uk", s3150), refusal_cause("b@live.de", s3150));
        assert_ne!(refusal_cause("a@live.example.org", s3150), refusal_cause("b@live.com", s3150));
    }

    #[tokio::test]
    async fn a_fixed_refusal_sends_again_what_shares_its_cause() {
        let (store, _dir) = store().await;
        store.create_domain("uwu.test").await.unwrap();
        let account = crate::NewAccount {
            address: "mini@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: crate::Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let account = store.create_account(account).await.unwrap().id;
        let s3150 = "550 5.7.1 part of their network is on our block list (S3150).";
        let mine = vec![recipient("a@hotmail.com"), recipient("b@outlook.com"), recipient("c@example.net")];
        let id = store.enqueue("mini@uwu.test", mine, b"Subject: hi\r\n\r\nhi\r\n", Some(account), None, 7200).await;
        let id = id.unwrap();
        let bounce = store.enqueue("", vec![recipient("d@live.com")], b"bounce", None, None, 600).await.unwrap();
        // A forward is queued for the forwarding account, but with someone else's mail.
        let forward =
            store.enqueue("SRS0=x=y@uwu.test", vec![recipient("e@live.com")], b"fwd", Some(account), None, 600);
        let forward = forward.await.unwrap();
        let claimed = store.claim_due_deliveries(10, 600).await.unwrap();
        let ids: Vec<i64> = claimed.iter().flat_map(|entry| entry.recipients.iter().map(|r| r.id)).collect();
        for (n, recipient) in ids.iter().enumerate() {
            store
                .mark_recipient_failed(*recipient, if n == 2 { "550 5.1.1 no such user" } else { s3150 })
                .await
                .unwrap();
        }
        for message in [id, bounce, forward] {
            assert!(store.complete_queue_message(message).await.unwrap());
        }
        // The copy outlives the queue, kept by the refusals that still point at it.
        assert_eq!(store.collect_garbage(0).await.unwrap(), 2, "the bounce's and the forward's files go");
        let refused = store.refused_deliveries(10).await.unwrap();
        // Not for the unknown recipient: sending it again cannot help.
        assert_eq!(refused.iter().filter(|mail| mail.can_resend).count(), 2, "{refused:?}");

        let hotmail = refused.iter().find(|mail| mail.to == "a@hotmail.com").unwrap().id;
        // While the sender may not send, their mail stays on the list.
        store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO send_suspensions (account_id, since, until, reason) VALUES (?1, 0, ?2, 'spam')",
                    params![account, now() + 600],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let outcome = store.resend_refused(hotmail).await.unwrap();
        assert_eq!(
            outcome,
            RefusedResend { resent: 0, dropped: 2, skipped: 2 },
            "the bounce and the forward have no copy"
        );
        store.lift_send_suspension(account, now()).await.unwrap();
        let outcome = store.resend_refused(hotmail).await.unwrap();
        assert_eq!(outcome, RefusedResend { resent: 2, dropped: 0, skipped: 0 }, "outlook.com too");
        let left = store.refused_deliveries(10).await.unwrap();
        assert_eq!(left.iter().map(|mail| mail.to.as_str()).collect::<Vec<_>>(), ["c@example.net"]);
        let queued = store.claim_due_deliveries(10, 600).await.unwrap();
        let mut again: Vec<_> = queued.iter().map(|entry| entry.recipients[0].address.as_str()).collect();
        again.sort();
        assert_eq!(again, ["a@hotmail.com", "b@outlook.com"]);
        assert!(
            queued.iter().all(|entry| entry.message.account_id == Some(account) && entry.message.expires_at > now())
        );
        assert_eq!(store.blob(&queued[0].message.blob).await.unwrap(), b"Subject: hi\r\n\r\nhi\r\n");
        assert!(matches!(store.resend_refused(hotmail).await, Err(StoreError::NotFound(_))));
        // A disabled account, or one without SMTP, does not send through a fix either.
        for switch in ["disabled = 1", "disabled = 0, smtp_enabled = 0"] {
            let again =
                store.enqueue("mini@uwu.test", vec![recipient("f@hotmail.com")], b"again", Some(account), None, 600);
            again.await.unwrap();
            let claimed = store.claim_due_deliveries(10, 600).await.unwrap();
            store.mark_recipient_failed(claimed[0].recipients[0].id, s3150).await.unwrap();
            let sql = format!("UPDATE accounts SET {switch} WHERE id = ?1");
            store.write(move |tx| Ok(tx.execute(&sql, [account])?)).await.unwrap();
            let refused = store.refused_deliveries(10).await.unwrap();
            let entry = refused.iter().find(|mail| mail.to == "f@hotmail.com").unwrap().id;
            let outcome = store.resend_refused(entry).await.unwrap();
            assert_eq!(outcome, RefusedResend { resent: 0, dropped: 0, skipped: 1 }, "{switch}");
            store.clear_refused_deliveries().await.unwrap();
            store.write(move |tx| Ok(tx.execute("DELETE FROM queue_messages", [])?)).await.unwrap();
            store
                .write(move |tx| Ok(tx.execute("UPDATE accounts SET disabled = 0, smtp_enabled = 1", [])?))
                .await
                .unwrap();
        }
        // Deleting the person takes their refused mail and its copies along.
        store.delete_account("mini@uwu.test").await.unwrap();
        assert!(store.refused_deliveries(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn refused_copies_stay_within_their_limits() {
        let (store, _dir) = store().await;
        store.create_domain("uwu.test").await.unwrap();
        let account = crate::NewAccount {
            address: "mini@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: crate::Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let account = store.create_account(account).await.unwrap().id;
        let to = vec![recipient("a@example.net"), recipient("b@example.net")];
        store.enqueue("mini@uwu.test", to, b"Subject: hi\r\n\r\nhi\r\n", Some(account), None, 600).await.unwrap();
        let claimed = store.claim_due_deliveries(10, 600).await.unwrap();
        let (unknown, blocked) = (claimed[0].recipients[0].id, claimed[0].recipients[1].id);
        store.mark_recipient_failed(unknown, "550 5.1.1 no such user").await.unwrap();
        // Copies already kept use up the room for more.
        let huge = "f".repeat(64);
        store
            .write(move |tx| {
                tx.execute("INSERT INTO blobs (hash, size, created_at) VALUES (?1, ?2, 0)", params![huge, REFUSED_COPIES_MAX_BYTES])?;
                tx.execute(
                    "INSERT INTO refused_deliveries (at, sender, recipient, error, gave_up, blob_hash) VALUES (?1, '', 'x@example.org', '', 0, ?2)",
                    params![now(), huge],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        store.mark_recipient_failed(blocked, "550 5.7.1 blocked").await.unwrap();
        let refused = store.refused_deliveries(10).await.unwrap();
        let kept = |to: &str| refused.iter().find(|mail| mail.to == to).unwrap().can_resend;
        assert!(!kept("a@example.net"), "an unknown recipient keeps no copy");
        assert!(!kept("b@example.net"), "over the total limit");
    }

    #[tokio::test]
    async fn queue_entries_lists_each_message_with_its_recipients() {
        let (store, _dir) = store().await;
        let first =
            store.enqueue("", vec![recipient("a@example.net"), recipient("b@example.org")], b"1", None, None, 60);
        let first = first.await.unwrap();
        let second = store.enqueue("", vec![recipient("c@example.net")], b"2", None, None, 60).await.unwrap();
        let entries = store.queue_entries().await.unwrap();
        let listed: Vec<_> = entries
            .iter()
            .map(|entry| (entry.message.id, entry.recipients.iter().map(|r| r.address.as_str()).collect::<Vec<_>>()))
            .collect();
        assert_eq!(listed, [(first, vec!["a@example.net", "b@example.org"]), (second, vec!["c@example.net"])]);
        let oldest = store.oldest_queue_entries(1).await.unwrap();
        assert_eq!((oldest.len(), oldest[0].message.id, oldest[0].recipients.len()), (1, first, 2));
    }

    #[tokio::test]
    async fn rejects_invalid_recipients() {
        let (store, _dir) = store().await;
        assert!(store.enqueue("", vec![recipient("nope")], b"x", None, None, 60).await.is_err());
        assert!(store.enqueue("", vec![], b"x", None, None, 60).await.is_err());
    }
}
