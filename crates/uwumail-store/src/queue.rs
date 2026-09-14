//! Outbound queue: messages waiting for delivery to other servers.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::normalize_address;
use crate::blobs::BlobHash;
use crate::{Result, Store, StoreError, now};

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

fn load_message(conn: &Connection, id: i64) -> Result<Option<QueuedMessage>> {
    let row = conn
        .query_row(
            "SELECT id, blob_hash, return_path, account_id, size, env_id, created_at, expires_at
             FROM queue_messages WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, blob, return_path, account_id, size, env_id, created_at, expires_at)| {
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
    })
    .transpose()
}

fn load_recipients(conn: &Connection, message_id: i64) -> Result<Vec<QueueRecipient>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {RECIPIENT_COLUMNS} FROM queue_recipients WHERE message_id = ?1 ORDER BY id"))?;
    let rows = stmt.query_map([message_id], recipient_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
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
                "UPDATE queue_recipients SET next_attempt_at = ?1
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
        self.update_recipient(recipient_id, "failed", Some(error.to_owned()), None).await
    }

    async fn update_recipient(
        &self,
        recipient_id: i64,
        status: &'static str,
        message: Option<String>,
        next_attempt_at: Option<i64>,
    ) -> Result<()> {
        self.write(move |tx| {
            let now = now();
            let changed = tx.execute(
                "UPDATE queue_recipients
                 SET status = ?1, last_error = ?2, attempts = attempts + 1,
                     next_attempt_at = coalesce(?3, next_attempt_at), updated_at = ?4
                 WHERE id = ?5",
                params![status, message, next_attempt_at, now, recipient_id],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("queue recipient {recipient_id}")));
            }
            Ok(())
        })
        .await
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

    pub async fn queue_entries(&self) -> Result<Vec<QueueEntry>> {
        self.read(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM queue_messages ORDER BY created_at, id")?;
            let ids = stmt.query_map([], |row| row.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
            let mut entries = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(message) = load_message(conn, id)? {
                    entries.push(QueueEntry { recipients: load_recipients(conn, id)?, message });
                }
            }
            Ok(entries)
        })
        .await
    }

    /// Makes all pending recipients of a message due now.
    pub async fn retry_queue_message(&self, message_id: i64) -> Result<()> {
        self.write(move |tx| {
            let changed = tx.execute(
                "UPDATE queue_recipients SET next_attempt_at = ?1 WHERE message_id = ?2 AND status = 'pending'",
                params![now(), message_id],
            )?;
            if changed == 0 {
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
                "mini@example.de",
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
        assert_eq!(store.collect_garbage(0).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn rejects_invalid_recipients() {
        let (store, _dir) = store().await;
        assert!(store.enqueue("", vec![recipient("nope")], b"x", None, None, 60).await.is_err());
        assert!(store.enqueue("", vec![], b"x", None, None, 60).await.is_err());
    }
}
