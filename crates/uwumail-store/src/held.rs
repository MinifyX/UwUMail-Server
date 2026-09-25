//! Submissions that wait before they go: JMAP's undo window and send later (`sendAt`,
//! FUTURERELEASE). The message is kept with its release time in `email_submissions`, so it
//! survives a restart; the JMAP service hands it to SMTP when its time comes.

use rusqlite::params;

use crate::blobs::BlobHash;
use crate::db::{next_modseq, record_change};
use crate::{Result, Store, StoreError, now};

/// A held submission whose time has come, as it is handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSubmission {
    pub id: i64,
    pub account_id: i64,
    /// JSON `{mailFrom, rcptTo}`.
    pub envelope: String,
    pub blob: BlobHash,
}

/// What a new held submission is.
#[derive(Debug, Clone)]
pub struct NewHeldSubmission {
    pub account_id: i64,
    pub identity_id: i64,
    pub email_id: i64,
    pub thread_id: i64,
    pub envelope: String,
    pub send_at: i64,
    /// The message as it will be sent.
    pub blob: BlobHash,
}

impl Store {
    /// Records a submission that waits until `send_at` (undo status `pending`).
    pub async fn hold_submission(&self, new: NewHeldSubmission) -> Result<i64> {
        let (id, modseq) = self
            .write(move |tx| {
                let modseq = next_modseq(tx, new.account_id)?;
                tx.execute(
                    "INSERT INTO email_submissions (account_id, identity_id, email_id, thread_id, envelope, send_at,
                         undo_status, held_blob, created_modseq, updated_modseq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?8)",
                    params![
                        new.account_id,
                        new.identity_id,
                        new.email_id,
                        new.thread_id,
                        new.envelope,
                        new.send_at,
                        new.blob.as_str(),
                        modseq
                    ],
                )?;
                let id = tx.last_insert_rowid();
                record_change(tx, new.account_id, modseq, "EmailSubmission", id, "created")?;
                Ok((id, modseq))
            })
            .await?;
        self.notify_change(new.account_id, modseq);
        Ok(id)
    }

    /// Cancels a submission that is still waiting. One that is already on its way is the rule
    /// `cannotUnsend`.
    pub async fn cancel_submission(&self, account_id: i64, id: i64) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let status: Option<String> = tx
                    .query_row(
                        "SELECT undo_status FROM email_submissions WHERE id = ?1 AND account_id = ?2",
                        params![id, account_id],
                        |row| row.get(0),
                    )
                    .ok();
                match status.as_deref() {
                    None => return Err(StoreError::NotFound(format!("submission {id}"))),
                    Some("pending") => {}
                    Some("canceled") => return Ok(None),
                    Some(_) => {
                        return Err(StoreError::Rule {
                            code: "cannotUnsend",
                            message: "the message was already sent".into(),
                        });
                    }
                }
                let modseq = next_modseq(tx, account_id)?;
                tx.execute(
                    "UPDATE email_submissions SET undo_status = 'canceled', held_blob = NULL, updated_modseq = ?1
                     WHERE id = ?2",
                    params![modseq, id],
                )?;
                record_change(tx, account_id, modseq, "EmailSubmission", id, "updated")?;
                Ok(Some(modseq))
            })
            .await?;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        Ok(())
    }

    /// When the next held submission is due, as a Unix time; `now` or earlier when one is due
    /// already or was being handed over when the server stopped.
    pub async fn next_held_submission(&self) -> Result<Option<i64>> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT MIN(CASE WHEN undo_status = 'pending' THEN send_at ELSE 0 END)
                 FROM email_submissions WHERE held_blob IS NOT NULL",
                [],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// Takes the held submissions that are due: they can no longer be cancelled (undo status
    /// `final`) and are handed over by the caller, who calls [`Store::finish_held_submission`]
    /// for each. Ones taken earlier and never finished (the server stopped) come again.
    pub async fn claim_due_submissions(&self, limit: usize) -> Result<Vec<HeldSubmission>> {
        let (claimed, changes) = self
            .write(move |tx| {
                let at = now();
                let mut stmt = tx.prepare(
                    "SELECT id, account_id, envelope, held_blob, undo_status FROM email_submissions
                     WHERE held_blob IS NOT NULL AND (undo_status = 'final' OR send_at <= ?1)
                       AND undo_status != 'canceled'
                     ORDER BY send_at, id LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(params![at, limit as i64], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(stmt);
                let mut claimed = Vec::with_capacity(rows.len());
                let mut changes = Vec::new();
                for (id, account_id, envelope, blob, status) in rows {
                    if status == "pending" {
                        let modseq = next_modseq(tx, account_id)?;
                        tx.execute(
                            "UPDATE email_submissions SET undo_status = 'final', updated_modseq = ?1 WHERE id = ?2",
                            params![modseq, id],
                        )?;
                        record_change(tx, account_id, modseq, "EmailSubmission", id, "updated")?;
                        changes.push((account_id, modseq));
                    }
                    let Ok(blob) = BlobHash::parse(&blob) else {
                        tx.execute(
                            "UPDATE email_submissions SET held_blob = NULL, release_error = ?1 WHERE id = ?2",
                            params!["the message was lost", id],
                        )?;
                        continue;
                    };
                    claimed.push(HeldSubmission { id, account_id, envelope, blob });
                }
                Ok((claimed, changes))
            })
            .await?;
        for (account_id, modseq) in changes {
            self.notify_change(account_id, modseq);
        }
        Ok(claimed)
    }

    /// Records how handing a held submission over went: the queue entry it became, or why it
    /// could not go. The kept message is let go either way.
    pub async fn finish_held_submission(
        &self,
        id: i64,
        queue_message_id: Option<i64>,
        error: Option<String>,
    ) -> Result<()> {
        let change = self
            .write(move |tx| {
                let account_id: Option<i64> = tx
                    .query_row("SELECT account_id FROM email_submissions WHERE id = ?1", [id], |row| row.get(0))
                    .ok();
                // Destroyed while it was being sent: nothing left to record.
                let Some(account_id) = account_id else { return Ok(None) };
                let modseq = next_modseq(tx, account_id)?;
                tx.execute(
                    "UPDATE email_submissions SET held_blob = NULL, queue_message_id = ?1, release_error = ?2,
                         updated_modseq = ?3
                     WHERE id = ?4",
                    params![queue_message_id, error, modseq, id],
                )?;
                record_change(tx, account_id, modseq, "EmailSubmission", id, "updated")?;
                Ok(Some((account_id, modseq)))
            })
            .await?;
        if let Some((account_id, modseq)) = change {
            self.notify_change(account_id, modseq);
        }
        Ok(())
    }
}
