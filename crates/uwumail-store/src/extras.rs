//! Uploads, sending identities, submissions and vacation responses.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::{EmailAddress, base_local_part, normalize_address};
use crate::blobs::BlobHash;
use crate::db::{next_modseq, record_change};
use crate::{Result, Store, StoreError, now};

/// Uploads are kept this long unless something else references their blob.
pub const UPLOAD_LIFETIME_SECS: i64 = 24 * 3600;
/// The most a signature of a sending identity may take, text and HTML each, in bytes. Room for a
/// small picture as a `data:` URI.
pub const IDENTITY_SIGNATURE_MAX_BYTES: usize = 256 * 1024;
/// The longest name of a sending identity, in characters.
const IDENTITY_NAME_MAX_CHARS: usize = 200;

/// Vacation replies go to each sender at most once in this period.
const VACATION_INTERVAL_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Identity {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub reply_to: Option<Vec<EmailAddress>>,
    pub bcc: Option<Vec<EmailAddress>>,
    pub text_signature: String,
    pub html_signature: String,
}

#[derive(Debug, Clone, Default)]
pub struct IdentityUpdate {
    pub name: Option<String>,
    pub reply_to: Option<Option<Vec<EmailAddress>>>,
    pub bcc: Option<Option<Vec<EmailAddress>>>,
    pub text_signature: Option<String>,
    pub html_signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SubmissionRecord {
    pub id: i64,
    pub identity_id: i64,
    pub email_id: i64,
    pub thread_id: i64,
    /// JSON `{mailFrom, rcptTo}` as sent.
    pub envelope: String,
    pub send_at: i64,
    pub undo_status: String,
    /// Why a held message could not be sent when its time came.
    pub release_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct VacationResponse {
    pub is_enabled: bool,
    pub from_date: Option<i64>,
    pub to_date: Option<i64>,
    pub subject: Option<String>,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
}

fn identity_from_row(row: &Row<'_>) -> rusqlite::Result<Identity> {
    let addresses = |value: Option<String>| value.and_then(|v| serde_json::from_str(&v).ok());
    Ok(Identity {
        id: row.get(0)?,
        name: row.get(1)?,
        email: row.get(2)?,
        reply_to: addresses(row.get(3)?),
        bcc: addresses(row.get(4)?),
        text_signature: row.get(5)?,
        html_signature: row.get(6)?,
    })
}

const IDENTITY_COLUMNS: &str = "id, name, email, reply_to, bcc, text_signature, html_signature";

/// Whether an account may send as `email`: its own addresses and their sub-addresses, and any address
/// of a domain an admin let it send as.
pub(crate) fn owns(conn: &rusqlite::Connection, account_id: i64, email: &str) -> Result<bool> {
    let Ok((local, domain)) = normalize_address(email) else {
        return Ok(false);
    };
    let base = base_local_part(&local).to_owned();
    let full = format!("{local}@{domain}");
    // The third case is a mailbox elsewhere that this account fetches and may answer from. It hangs
    // on the account, not on the address: two people can fetch the same provider, and neither may
    // send as the other's. Without a server to send through it is no address to send from either.
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM addresses a JOIN domains d ON d.id = a.domain_id
                        WHERE a.account_id = ?1 AND d.name = ?2 AND a.local_part IN (?3, ?4))
             OR EXISTS (SELECT 1 FROM send_as_domains s JOIN domains d ON d.id = s.domain_id
                        WHERE s.account_id = ?1 AND d.name = ?2)
             OR EXISTS (SELECT 1 FROM fetch_accounts f
                        WHERE f.account_id = ?1 AND f.address = ?5
                          AND f.send_enabled = 1 AND f.smtp_host <> '')",
        params![account_id, domain, local, base, full],
        |row| row.get(0),
    )?)
}

impl Store {
    /// Stores uploaded bytes for an account and returns the blob id.
    pub async fn upload(&self, account_id: i64, bytes: &[u8], media_type: &str) -> Result<BlobHash> {
        let hash = self.put_blob(bytes).await?;
        let (key, media_type) = (hash.as_str().to_owned(), media_type.to_owned());
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO uploads (account_id, blob_hash, media_type, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account_id, blob_hash) DO UPDATE SET created_at = excluded.created_at, media_type = excluded.media_type",
                params![account_id, key, media_type, now()],
            )?;
            Ok(())
        })
        .await?;
        Ok(hash)
    }

    /// Whether an account may read a blob: one of its emails or its recent uploads uses it.
    pub async fn blob_accessible(&self, account_id: i64, hash: &BlobHash) -> Result<bool> {
        let key = hash.as_str().to_owned();
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM emails WHERE account_id = ?1 AND blob_hash = ?2)
                     OR EXISTS (SELECT 1 FROM uploads WHERE account_id = ?1 AND blob_hash = ?2)",
                params![account_id, key],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn upload_media_type(&self, account_id: i64, hash: &BlobHash) -> Result<Option<String>> {
        let key = hash.as_str().to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT media_type FROM uploads WHERE account_id = ?1 AND blob_hash = ?2",
                    params![account_id, key],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// Sending identities; an account starts with one per address.
    pub async fn identities(&self, account_id: i64) -> Result<Vec<Identity>> {
        let list = self.read(move |conn| load_identities(conn, account_id)).await?;
        if !list.is_empty() {
            return Ok(list);
        }
        let (list, modseq) = self
            .write(move |tx| {
                let existing = load_identities(tx, account_id)?;
                if !existing.is_empty() {
                    return Ok((existing, None));
                }
                let (login, name): (String, String) = tx
                    .query_row("SELECT login, display_name FROM accounts WHERE id = ?1", [account_id], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
                let mut stmt = tx.prepare(
                    "SELECT a.local_part || '@' || d.name FROM addresses a JOIN domains d ON d.id = a.domain_id
                     WHERE a.account_id = ?1 ORDER BY a.kind = 'primary' DESC, a.id",
                )?;
                let mut addresses = stmt.query_map([account_id], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                drop(stmt);
                if addresses.is_empty() {
                    addresses.push(login);
                }
                let modseq = next_modseq(tx, account_id)?;
                for email in addresses {
                    tx.execute(
                        "INSERT INTO identities (account_id, name, email, created_modseq, updated_modseq) VALUES (?1, ?2, ?3, ?4, ?4)",
                        params![account_id, name, email, modseq],
                    )?;
                    record_change(tx, account_id, modseq, "Identity", tx.last_insert_rowid(), "created")?;
                }
                Ok((load_identities(tx, account_id)?, Some(modseq)))
            })
            .await?;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        Ok(list)
    }

    pub async fn create_identity(&self, account_id: i64, name: &str, email: &str) -> Result<i64> {
        let (name, email) = (name.trim().to_owned(), email.trim().to_owned());
        let (id, modseq) = self
            .write(move |tx| {
                if !owns(tx, account_id, &email)? {
                    return Err(StoreError::Rule { code: "forbiddenFrom", message: format!("{email} is not an address of this account") });
                }
                let modseq = next_modseq(tx, account_id)?;
                tx.execute(
                    "INSERT INTO identities (account_id, name, email, created_modseq, updated_modseq) VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![account_id, name, email.to_lowercase(), modseq],
                )?;
                let id = tx.last_insert_rowid();
                record_change(tx, account_id, modseq, "Identity", id, "created")?;
                Ok((id, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(id)
    }

    pub async fn update_identity(&self, account_id: i64, id: i64, update: IdentityUpdate) -> Result<()> {
        if update.name.as_ref().is_some_and(|name| name.trim().chars().count() > IDENTITY_NAME_MAX_CHARS) {
            return Err(StoreError::Invalid(format!("a name may have at most {IDENTITY_NAME_MAX_CHARS} characters")));
        }
        for signature in [&update.text_signature, &update.html_signature].into_iter().flatten() {
            if signature.len() > IDENTITY_SIGNATURE_MAX_BYTES {
                return Err(StoreError::Invalid(format!(
                    "a signature may take at most {IDENTITY_SIGNATURE_MAX_BYTES} bytes"
                )));
            }
        }
        let modseq = self
            .write(move |tx| {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM identities WHERE id = ?1 AND account_id = ?2)",
                    params![id, account_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(StoreError::NotFound(format!("identity {id}")));
                }
                let modseq = next_modseq(tx, account_id)?;
                let json = |v: &Option<Vec<EmailAddress>>| v.as_ref().and_then(|v| serde_json::to_string(v).ok());
                if let Some(name) = &update.name {
                    tx.execute("UPDATE identities SET name = ?1 WHERE id = ?2", params![name.trim(), id])?;
                }
                if let Some(reply_to) = &update.reply_to {
                    tx.execute("UPDATE identities SET reply_to = ?1 WHERE id = ?2", params![json(reply_to), id])?;
                }
                if let Some(bcc) = &update.bcc {
                    tx.execute("UPDATE identities SET bcc = ?1 WHERE id = ?2", params![json(bcc), id])?;
                }
                if let Some(text) = &update.text_signature {
                    tx.execute("UPDATE identities SET text_signature = ?1 WHERE id = ?2", params![text, id])?;
                }
                if let Some(html) = &update.html_signature {
                    tx.execute("UPDATE identities SET html_signature = ?1 WHERE id = ?2", params![html, id])?;
                }
                tx.execute("UPDATE identities SET updated_modseq = ?1 WHERE id = ?2", params![modseq, id])?;
                record_change(tx, account_id, modseq, "Identity", id, "updated")?;
                Ok(modseq)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    pub async fn destroy_identity(&self, account_id: i64, id: i64) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                if tx.execute("DELETE FROM identities WHERE id = ?1 AND account_id = ?2", params![id, account_id])? == 0
                {
                    return Err(StoreError::NotFound(format!("identity {id}")));
                }
                let modseq = next_modseq(tx, account_id)?;
                record_change(tx, account_id, modseq, "Identity", id, "destroyed")?;
                Ok(modseq)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    pub async fn record_submission(
        &self,
        account_id: i64,
        identity_id: i64,
        email_id: i64,
        thread_id: i64,
        envelope: String,
        queue_message_id: Option<i64>,
    ) -> Result<i64> {
        let (id, modseq) = self
            .write(move |tx| {
                let modseq = next_modseq(tx, account_id)?;
                tx.execute(
                    "INSERT INTO email_submissions (account_id, identity_id, email_id, thread_id, envelope, send_at,
                         queue_message_id, created_modseq, updated_modseq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                    params![account_id, identity_id, email_id, thread_id, envelope, now(), queue_message_id, modseq],
                )?;
                let id = tx.last_insert_rowid();
                record_change(tx, account_id, modseq, "EmailSubmission", id, "created")?;
                Ok((id, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(id)
    }

    /// Submissions of an account, newest first; `ids` narrows the list.
    pub async fn submissions(&self, account_id: i64, ids: Option<Vec<i64>>) -> Result<Vec<SubmissionRecord>> {
        let ids_json = ids.map(|ids| serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()));
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, identity_id, email_id, thread_id, envelope, send_at, undo_status, release_error FROM email_submissions
                 WHERE account_id = ?1 AND (?2 IS NULL OR id IN (SELECT value FROM json_each(?2)))
                 ORDER BY send_at DESC, id DESC",
            )?;
            let rows = stmt.query_map(params![account_id, ids_json], |row| {
                Ok(SubmissionRecord {
                    id: row.get(0)?,
                    identity_id: row.get(1)?,
                    email_id: row.get(2)?,
                    thread_id: row.get(3)?,
                    envelope: row.get(4)?,
                    send_at: row.get(5)?,
                    undo_status: row.get(6)?,
                    release_error: row.get(7)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn destroy_submission(&self, account_id: i64, id: i64) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                if tx.execute(
                    "DELETE FROM email_submissions WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                )? == 0
                {
                    return Err(StoreError::NotFound(format!("submission {id}")));
                }
                let modseq = next_modseq(tx, account_id)?;
                record_change(tx, account_id, modseq, "EmailSubmission", id, "destroyed")?;
                Ok(modseq)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    pub async fn vacation_response(&self, account_id: i64) -> Result<VacationResponse> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT is_enabled, from_date, to_date, subject, text_body, html_body FROM vacation_responses WHERE account_id = ?1",
                    [account_id],
                    |row| {
                        Ok(VacationResponse {
                            is_enabled: row.get(0)?,
                            from_date: row.get(1)?,
                            to_date: row.get(2)?,
                            subject: row.get(3)?,
                            text_body: row.get(4)?,
                            html_body: row.get(5)?,
                        })
                    },
                )
                .optional()?
                .unwrap_or_default())
        })
        .await
    }

    pub async fn set_vacation_response(&self, account_id: i64, vacation: VacationResponse) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let modseq = next_modseq(tx, account_id)?;
                tx.execute(
                    "INSERT INTO vacation_responses (account_id, is_enabled, from_date, to_date, subject, text_body, html_body, updated_modseq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (account_id) DO UPDATE SET is_enabled = excluded.is_enabled, from_date = excluded.from_date,
                         to_date = excluded.to_date, subject = excluded.subject, text_body = excluded.text_body,
                         html_body = excluded.html_body, updated_modseq = excluded.updated_modseq",
                    params![
                        account_id,
                        vacation.is_enabled,
                        vacation.from_date,
                        vacation.to_date,
                        vacation.subject,
                        vacation.text_body,
                        vacation.html_body,
                        modseq
                    ],
                )?;
                if vacation.is_enabled {
                    // A new away period starts fresh.
                    tx.execute("DELETE FROM vacation_replies WHERE account_id = ?1", [account_id])?;
                }
                record_change(tx, account_id, modseq, "VacationResponse", 0, "updated")?;
                Ok(modseq)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    /// The vacation response to send to `sender` now, if one is due; remembers that it was sent.
    pub async fn take_vacation_reply(&self, account_id: i64, sender: &str) -> Result<Option<VacationResponse>> {
        let vacation = self.vacation_response(account_id).await?;
        let now = now();
        let active = vacation.is_enabled
            && vacation.from_date.is_none_or(|from| from <= now)
            && vacation.to_date.is_none_or(|to| now < to);
        if !active {
            return Ok(None);
        }
        let sender = sender.trim().to_lowercase();
        let due = self
            .write(move |tx| {
                let last: Option<i64> = tx
                    .query_row(
                        "SELECT sent_at FROM vacation_replies WHERE account_id = ?1 AND sender = ?2",
                        params![account_id, sender],
                        |row| row.get(0),
                    )
                    .optional()?;
                if last.is_some_and(|sent| now - sent < VACATION_INTERVAL_SECS) {
                    return Ok(false);
                }
                tx.execute(
                    "INSERT INTO vacation_replies (account_id, sender, sent_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT (account_id, sender) DO UPDATE SET sent_at = excluded.sent_at",
                    params![account_id, sender, now],
                )?;
                Ok(true)
            })
            .await?;
        Ok(due.then_some(vacation))
    }
}

fn load_identities(conn: &rusqlite::Connection, account_id: i64) -> Result<Vec<Identity>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {IDENTITY_COLUMNS} FROM identities WHERE account_id = ?1 ORDER BY id"))?;
    let rows = stmt.query_map([account_id], identity_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    #[tokio::test]
    async fn identities_uploads_and_vacation() {
        let (store, _dir) = store().await;
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
        store.add_alias("hallo@example.org", "mini@example.org").await.unwrap();

        let identities = store.identities(account).await.unwrap();
        assert_eq!(
            identities.iter().map(|i| i.email.as_str()).collect::<Vec<_>>(),
            ["mini@example.org", "hallo@example.org"]
        );
        assert_eq!(store.identities(account).await.unwrap().len(), 2);
        assert!(matches!(
            store.create_identity(account, "X", "boss@bank.example").await,
            Err(StoreError::Rule { code: "forbiddenFrom", .. })
        ));
        let extra = store.create_identity(account, "Mini Shop", "mini+shop@example.org").await.unwrap();
        store
            .update_identity(
                account,
                extra,
                IdentityUpdate { text_signature: Some("Liebe Grüße".into()), ..Default::default() },
            )
            .await
            .unwrap();
        assert_eq!(store.identities(account).await.unwrap()[2].text_signature, "Liebe Grüße");
        store.destroy_identity(account, extra).await.unwrap();

        let hash = store.upload(account, b"attachment", "text/plain").await.unwrap();
        assert!(store.blob_accessible(account, &hash).await.unwrap());
        assert_eq!(store.upload_media_type(account, &hash).await.unwrap().as_deref(), Some("text/plain"));
        assert!(!store.blob_accessible(account + 1, &hash).await.unwrap());

        assert!(store.take_vacation_reply(account, "nyu@x.example").await.unwrap().is_none());
        store
            .set_vacation_response(
                account,
                VacationResponse { is_enabled: true, subject: Some("Weg".into()), ..Default::default() },
            )
            .await
            .unwrap();
        assert!(store.take_vacation_reply(account, "Nyu@x.example").await.unwrap().is_some());
        assert!(store.take_vacation_reply(account, "nyu@x.example").await.unwrap().is_none());
    }
}
