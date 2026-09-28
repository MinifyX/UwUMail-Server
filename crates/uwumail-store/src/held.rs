//! Submissions that wait before they go: JMAP's undo window and send later (`sendAt`,
//! FUTURERELEASE). The message is kept with its release time in `email_submissions`, so it
//! survives a restart; the JMAP service hands it to SMTP when its time comes.

use rusqlite::{Connection, params};

use crate::blobs::BlobHash;
use crate::db::{next_modseq, record_change};
use crate::{Result, Store, StoreError, now};

/// Why a held message was not sent after all, in its release error.
const NOT_SENT_LOGIN_ENDED: &str =
    "not sent: the login that scheduled it was revoked, or the account may no longer log in";
pub(crate) const NOT_SENT_DISABLED: &str = "not sent: the account was disabled or moved to the trash";
pub(crate) const NOT_SENT_PASSWORD: &str = "not sent: the password changed after it was scheduled";
pub(crate) const NOT_SENT_REVOKED: &str = "not sent: the app password or app that scheduled it was revoked";

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
    /// The login that held it, as push subscriptions name it (`password`, `app:<id>`,
    /// `oauth:<grant>`, `session:<hash>`): when that credential is revoked the message stays
    /// unsent. `None` counts as the account password.
    pub credential: Option<String>,
}

/// Which waiting submissions of an account [`cancel_held`] stops.
pub(crate) enum HeldBy<'a> {
    /// All of them: the account was disabled or moved to the trash.
    Anyone,
    /// Those held with this app password or OAuth app, which was just revoked.
    Credential(&'a str),
    /// Those held with the account password or a webmail session, whose password just changed;
    /// except those of the session `keep` (`session:<hash>`) of the person who changed it.
    Password { keep: Option<&'a str> },
}

/// Stops the account's submissions that still wait to be sent, when the login that made them no
/// longer holds: they become `canceled`, with `reason` as their release error, and their message
/// is let go. Someone who got hold of an account for a moment could otherwise have queued mail
/// for up to 30 days that neither disabling the account nor a new password stopped
/// (security audit 0.16.0 PROTOCOLS-10). Returns the new modseq when anything was stopped.
pub(crate) fn cancel_held(conn: &Connection, account_id: i64, by: HeldBy<'_>, reason: &str) -> Result<Option<i64>> {
    let (condition, credential) = match by {
        HeldBy::Anyone => ("?2 IS NULL", None),
        HeldBy::Credential(credential) => ("credential = ?2", Some(credential)),
        HeldBy::Password { keep } => (
            "(credential IS NULL OR credential = 'password' OR (credential LIKE 'session:%' AND credential IS NOT ?2))",
            keep,
        ),
    };
    let ids: Vec<i64> = conn
        .prepare(&format!(
            "SELECT id FROM email_submissions
             WHERE account_id = ?1 AND undo_status = 'pending' AND held_blob IS NOT NULL AND {condition}"
        ))?
        .query_map(params![account_id, credential], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if ids.is_empty() {
        return Ok(None);
    }
    let modseq = next_modseq(conn, account_id)?;
    for id in ids {
        conn.execute(
            "UPDATE email_submissions SET undo_status = 'canceled', held_blob = NULL, release_error = ?1,
                 updated_modseq = ?2
             WHERE id = ?3",
            params![reason, modseq, id],
        )?;
        record_change(conn, account_id, modseq, "EmailSubmission", id, "updated")?;
    }
    Ok(Some(modseq))
}

/// Whether the login that held a submission `s` (joined with its account `a`) still stands: the
/// account may log in, and an app password or OAuth app it was held with was not revoked.
const HELD_STILL_VALID: &str = "a.disabled = 0 AND a.deleted_at IS NULL AND CASE
    WHEN s.credential LIKE 'app:%' THEN EXISTS (SELECT 1 FROM app_passwords ap
        WHERE ap.id = CAST(substr(s.credential, 5) AS INTEGER) AND ap.account_id = s.account_id)
    WHEN s.credential LIKE 'oauth:%' THEN EXISTS (SELECT 1 FROM oauth_grants g
        WHERE g.id = CAST(substr(s.credential, 7) AS INTEGER) AND g.account_id = s.account_id)
    ELSE 1 END";

impl Store {
    /// Records a submission that waits until `send_at` (undo status `pending`).
    pub async fn hold_submission(&self, new: NewHeldSubmission) -> Result<i64> {
        let (id, modseq) = self
            .write(move |tx| {
                let modseq = next_modseq(tx, new.account_id)?;
                tx.execute(
                    "INSERT INTO email_submissions (account_id, identity_id, email_id, thread_id, envelope, send_at,
                         undo_status, held_blob, created_modseq, updated_modseq, credential)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?8, ?9)",
                    params![
                        new.account_id,
                        new.identity_id,
                        new.email_id,
                        new.thread_id,
                        new.envelope,
                        new.send_at,
                        new.blob.as_str(),
                        modseq,
                        new.credential
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
    ///
    /// Due ones whose login no longer stands (the account disabled, in the trash, or the app
    /// password or OAuth app they were held with revoked) are not handed over but cancelled.
    pub async fn claim_due_submissions(&self, limit: usize) -> Result<Vec<HeldSubmission>> {
        let (claimed, changes) = self
            .write(move |tx| {
                let at = now();
                let mut changes = Vec::new();
                let stale: Vec<(i64, i64)> = tx
                    .prepare(&format!(
                        "SELECT s.id, s.account_id FROM email_submissions s JOIN accounts a ON a.id = s.account_id
                         WHERE s.held_blob IS NOT NULL AND (s.undo_status = 'final' OR s.send_at <= ?1)
                           AND s.undo_status != 'canceled' AND NOT ({HELD_STILL_VALID})"
                    ))?
                    .query_map([at], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                for (id, account_id) in stale {
                    let modseq = next_modseq(tx, account_id)?;
                    tx.execute(
                        "UPDATE email_submissions SET undo_status = CASE undo_status WHEN 'pending' THEN 'canceled'
                                 ELSE undo_status END,
                             held_blob = NULL, release_error = ?1, updated_modseq = ?2
                         WHERE id = ?3",
                        params![NOT_SENT_LOGIN_ENDED, modseq, id],
                    )?;
                    record_change(tx, account_id, modseq, "EmailSubmission", id, "updated")?;
                    changes.push((account_id, modseq));
                }
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
                let account_id: Option<i64> =
                    tx.query_row("SELECT account_id FROM email_submissions WHERE id = ?1", [id], |row| row.get(0)).ok();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{AppScope, IngestRequest, MailboxRole, MailboxTarget, NewAccount, NewAppPassword, Role};

    async fn account(store: &Store, user: &str) -> i64 {
        store.create_domain("example.org").await.ok();
        let new = NewAccount {
            address: format!("{user}@example.org"),
            display_name: String::new(),
            password: Some("katzenpfote-123".into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        store.create_account(new).await.unwrap().id
    }

    /// Holds a message a minute ahead, made with `credential`.
    async fn hold(store: &Store, account_id: i64, credential: &str) -> i64 {
        let raw = b"From: mini@example.org\r\nTo: nyu@example.org\r\nSubject: Later\r\n\r\nMiau\r\n".to_vec();
        let request = IngestRequest {
            account_id,
            raw,
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Drafts)],
            keywords: vec![],
            received_at: None,
        };
        let email = store.ingest(request).await.unwrap();
        store
            .hold_submission(NewHeldSubmission {
                account_id,
                identity_id: 1,
                email_id: email.id,
                thread_id: email.thread_id,
                envelope: r#"{"mailFrom":{"email":"mini@example.org"},"rcptTo":[{"email":"nyu@example.org"}]}"#.into(),
                send_at: now() + 60,
                blob: email.blob,
                credential: Some(credential.into()),
            })
            .await
            .unwrap()
    }

    async fn status(store: &Store, account_id: i64, id: i64) -> (String, Option<String>) {
        let record = store.submissions(account_id, Some(vec![id])).await.unwrap().remove(0);
        (record.undo_status, record.release_error)
    }

    /// Makes every held submission due now.
    async fn due(store: &Store) {
        store.write(|tx| Ok(tx.execute("UPDATE email_submissions SET send_at = 0", [])?)).await.unwrap();
    }

    /// Mail someone held back for later used to go out whatever happened to the account or the
    /// login in the meantime (security audit 0.16.0 PROTOCOLS-10).
    #[tokio::test]
    async fn held_mail_stays_unsent_when_its_login_ends() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let app = store
            .create_app_password(
                mini,
                NewAppPassword { name: "phone".into(), scopes: vec![AppScope::Mail], expires_at: None },
            )
            .await
            .unwrap();
        let app_credential = crate::push_credential_for_app_password(app.app_password.id);
        let by_app = hold(&store, mini, &app_credential).await;
        let by_password = hold(&store, mini, "password").await;

        // Revoking the app password stops what it held, and only that.
        store.revoke_app_password(mini, app.app_password.id).await.unwrap();
        let (undo, error) = status(&store, mini, by_app).await;
        assert_eq!(undo, "canceled");
        assert!(error.unwrap().contains("revoked"));
        assert_eq!(status(&store, mini, by_password).await.0, "pending");

        // A new password stops what the password held.
        store.set_password("mini@example.org", "neues-passwort-1").await.unwrap();
        assert_eq!(status(&store, mini, by_password).await.0, "canceled");

        // Disabling the account stops the rest, even if it is switched on again before the time.
        let later = hold(&store, mini, "password").await;
        store.set_account_disabled("mini@example.org", true).await.unwrap();
        store.set_account_disabled("mini@example.org", false).await.unwrap();
        due(&store).await;
        assert!(store.claim_due_submissions(10).await.unwrap().is_empty());
        assert_eq!(status(&store, mini, later).await.0, "canceled");

        // So does the trash.
        let nyu = account(&store, "nyu").await;
        let held = hold(&store, nyu, "password").await;
        store.trash_account("nyu@example.org").await.unwrap();
        assert_eq!(status(&store, nyu, held).await.0, "canceled");
    }

    /// Whatever slipped past the moment a login ended is caught when its time comes.
    #[tokio::test]
    async fn due_mail_of_a_login_that_ended_is_not_handed_over() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let gone = hold(&store, mini, "app:4711").await;
        let fine = hold(&store, mini, "password").await;
        due(&store).await;
        let claimed = store.claim_due_submissions(10).await.unwrap();
        assert_eq!(claimed.iter().map(|held| held.id).collect::<Vec<_>>(), vec![fine]);
        let (undo, error) = status(&store, mini, gone).await;
        assert_eq!(undo, "canceled");
        assert!(error.is_some());
        // An account that was disabled behind the store's back (a row changed by hand) too.
        let other = hold(&store, mini, "password").await;
        store.write(move |tx| Ok(tx.execute("UPDATE accounts SET disabled = 1 WHERE id = ?1", [mini])?)).await.unwrap();
        due(&store).await;
        assert!(store.claim_due_submissions(10).await.unwrap().is_empty());
        assert_eq!(status(&store, mini, other).await.0, "canceled");
    }
}
