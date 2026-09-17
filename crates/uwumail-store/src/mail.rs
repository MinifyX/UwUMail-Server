//! Mailboxes, message ingestion, threading and the change log.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;

use crate::address::EmailAddress;
use crate::blobs::BlobHash;
use crate::db::{next_modseq, record_change};
use crate::parse::{EmailMeta, parse};
use crate::{Result, Store, StoreError, now};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MailboxRole {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
}

impl MailboxRole {
    pub const ALL: [MailboxRole; 6] = [
        MailboxRole::Inbox,
        MailboxRole::Drafts,
        MailboxRole::Sent,
        MailboxRole::Archive,
        MailboxRole::Junk,
        MailboxRole::Trash,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MailboxRole::Inbox => "inbox",
            MailboxRole::Drafts => "drafts",
            MailboxRole::Sent => "sent",
            MailboxRole::Archive => "archive",
            MailboxRole::Junk => "junk",
            MailboxRole::Trash => "trash",
        }
    }

    pub fn parse(value: &str) -> Option<MailboxRole> {
        MailboxRole::ALL.into_iter().find(|role| role.as_str() == value)
    }

    /// Name used when the mailbox is created. Clients localize by role.
    fn default_name(self) -> &'static str {
        match self {
            MailboxRole::Inbox => "Inbox",
            MailboxRole::Drafts => "Drafts",
            MailboxRole::Sent => "Sent",
            MailboxRole::Archive => "Archive",
            MailboxRole::Junk => "Junk",
            MailboxRole::Trash => "Trash",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Mailbox {
    pub id: i64,
    pub account_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub role: Option<MailboxRole>,
    pub sort_order: i64,
    pub subscribed: bool,
    pub total_emails: i64,
    pub unread_emails: i64,
    pub total_threads: i64,
    pub unread_threads: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum MailboxTarget {
    Role(MailboxRole),
    Id(i64),
}

#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub account_id: i64,
    pub raw: Vec<u8>,
    /// At least one.
    pub mailboxes: Vec<MailboxTarget>,
    pub keywords: Vec<String>,
    /// Defaults to now.
    pub received_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestedEmail {
    pub id: i64,
    pub thread_id: i64,
    pub mailbox_ids: Vec<i64>,
    /// UID in the first mailbox.
    pub uid: i64,
    pub blob: BlobHash,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailSummary {
    pub id: i64,
    pub thread_id: i64,
    pub subject: String,
    pub from: Vec<EmailAddress>,
    pub preview: String,
    pub received_at: i64,
    pub size: i64,
    pub keywords: Vec<String>,
    pub blob: String,
}

pub(crate) fn create_default_mailboxes(tx: &Transaction<'_>, account_id: i64) -> Result<()> {
    let modseq = next_modseq(tx, account_id)?;
    let uid_validity = now();
    for (order, role) in MailboxRole::ALL.into_iter().enumerate() {
        tx.execute(
            "INSERT INTO mailboxes (account_id, name, role, sort_order, uid_validity, created_modseq, updated_modseq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![account_id, role.default_name(), role.as_str(), order as i64 + 1, uid_validity, modseq],
        )?;
        record_change(tx, account_id, modseq, "Mailbox", tx.last_insert_rowid(), "created")?;
    }
    Ok(())
}

fn resolve_mailbox(conn: &Connection, account_id: i64, target: MailboxTarget) -> Result<i64> {
    let found = match target {
        MailboxTarget::Role(role) => conn
            .query_row(
                "SELECT id FROM mailboxes WHERE account_id = ?1 AND role = ?2",
                params![account_id, role.as_str()],
                |row| row.get(0),
            )
            .optional()?,
        MailboxTarget::Id(id) => conn
            .query_row("SELECT id FROM mailboxes WHERE account_id = ?1 AND id = ?2", params![account_id, id], |row| {
                row.get(0)
            })
            .optional()?,
    };
    found.ok_or_else(|| StoreError::NotFound(format!("mailbox {target:?} of account {account_id}")))
}

/// Finds the thread for a new message: any known message id it references (or that
/// references it) wins; otherwise the message starts a new thread.
fn thread_for(tx: &Transaction<'_>, account_id: i64, meta: &EmailMeta) -> Result<(i64, bool)> {
    let candidates = meta.message_id.iter().chain(&meta.in_reply_to).chain(&meta.references);
    for message_id in candidates {
        let thread: Option<i64> = tx
            .query_row(
                "SELECT thread_id FROM thread_message_ids WHERE account_id = ?1 AND message_id = ?2",
                params![account_id, message_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(thread) = thread {
            return Ok((thread, false));
        }
    }
    tx.execute("INSERT INTO threads (account_id) VALUES (?1)", [account_id])?;
    Ok((tx.last_insert_rowid(), true))
}

/// Puts an email into a mailbox with the next UID of that mailbox.
pub(crate) fn add_to_mailbox(tx: &Transaction<'_>, email_id: i64, mailbox_id: i64, modseq: i64) -> Result<i64> {
    let uid: i64 = tx.query_row(
        "UPDATE mailboxes SET uid_next = uid_next + 1, updated_modseq = ?2 WHERE id = ?1 RETURNING uid_next - 1",
        params![mailbox_id, modseq],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO email_mailboxes (email_id, mailbox_id, uid, modseq) VALUES (?1, ?2, ?3, ?4)",
        params![email_id, mailbox_id, uid, modseq],
    )?;
    Ok(uid)
}

pub(crate) fn resolve_mailboxes(conn: &Connection, account_id: i64, targets: &[MailboxTarget]) -> Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(targets.len());
    for target in targets {
        let id = resolve_mailbox(conn, account_id, *target)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(StoreError::Invalid("an email needs at least one mailbox".into()));
    }
    Ok(ids)
}

fn addresses_json(addresses: &[EmailAddress]) -> String {
    serde_json::to_string(addresses).unwrap_or_else(|_| "[]".into())
}

impl Store {
    /// Stores a message in a mailbox of an account: blob, metadata, thread, search
    /// index and change log. Fails with [`StoreError::QuotaExceeded`] when the account is full.
    pub async fn ingest(&self, request: IngestRequest) -> Result<IngestedEmail> {
        let IngestRequest { account_id, raw, mailboxes, keywords, received_at } = request;
        let size = raw.len() as i64;

        let quota_ok = self
            .read(move |conn| {
                let (quota, used): (i64, i64) = conn
                    .query_row("SELECT quota_bytes, used_bytes FROM accounts WHERE id = ?1", [account_id], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
                Ok(quota == 0 || used + size <= quota)
            })
            .await?;
        if !quota_ok {
            return Err(StoreError::QuotaExceeded);
        }

        let meta = tokio::task::spawn_blocking({
            let raw = raw.clone();
            move || parse(&raw)
        })
        .await
        .map_err(|err| StoreError::Internal(err.to_string()))?;
        let blob = self.put_blob(&raw).await?;
        drop(raw);

        let blob_key = blob.as_str().to_owned();
        let received_at = received_at.unwrap_or_else(now);
        let keywords: Vec<String> = keywords.into_iter().map(|k| k.to_lowercase()).collect();

        let (email, modseq) = self
            .write(move |tx| {
                // Re-check inside the transaction; concurrent deliveries may have used the space.
                let (quota, used): (i64, i64) = tx
                    .query_row("SELECT quota_bytes, used_bytes FROM accounts WHERE id = ?1", [account_id], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?;
                if quota > 0 && used + size > quota {
                    return Err(StoreError::QuotaExceeded);
                }

                let mailbox_ids = resolve_mailboxes(tx, account_id, &mailboxes)?;
                let modseq = next_modseq(tx, account_id)?;
                let (thread_id, new_thread) = thread_for(tx, account_id, &meta)?;

                tx.execute(
                    "INSERT INTO emails (account_id, thread_id, blob_hash, size, received_at, sent_at, message_id,
                         in_reply_to, refs, subject, from_addr, sender_addr, to_addr, cc_addr, bcc_addr, reply_to_addr,
                         preview, has_attachment, created_modseq, updated_modseq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?19)",
                    params![
                        account_id,
                        thread_id,
                        blob_key,
                        size,
                        received_at,
                        meta.sent_at,
                        meta.message_id,
                        serde_json::to_string(&meta.in_reply_to).unwrap_or_default(),
                        serde_json::to_string(&meta.references).unwrap_or_default(),
                        meta.subject,
                        addresses_json(&meta.from),
                        addresses_json(&meta.sender),
                        addresses_json(&meta.to),
                        addresses_json(&meta.cc),
                        addresses_json(&meta.bcc),
                        addresses_json(&meta.reply_to),
                        meta.preview,
                        meta.has_attachment,
                        modseq,
                    ],
                )?;
                let email_id = tx.last_insert_rowid();

                let mut uid = 0;
                for (index, mailbox_id) in mailbox_ids.iter().enumerate() {
                    let assigned = add_to_mailbox(tx, email_id, *mailbox_id, modseq)?;
                    if index == 0 {
                        uid = assigned;
                    }
                    record_change(tx, account_id, modseq, "Mailbox", *mailbox_id, "updated")?;
                }
                for keyword in &keywords {
                    tx.execute(
                        "INSERT OR IGNORE INTO email_keywords (email_id, keyword) VALUES (?1, ?2)",
                        params![email_id, keyword],
                    )?;
                }
                for message_id in meta.message_id.iter().chain(&meta.in_reply_to).chain(&meta.references) {
                    tx.execute(
                        "INSERT OR IGNORE INTO thread_message_ids (account_id, message_id, thread_id) VALUES (?1, ?2, ?3)",
                        params![account_id, message_id, thread_id],
                    )?;
                }
                tx.execute(
                    "INSERT INTO email_fts (rowid, subject, addresses, body) VALUES (?1, ?2, ?3, ?4)",
                    params![email_id, meta.subject, meta.search_addresses, meta.search_body],
                )?;
                tx.execute("UPDATE accounts SET used_bytes = used_bytes + ?1 WHERE id = ?2", params![size, account_id])?;

                record_change(tx, account_id, modseq, "Email", email_id, "created")?;
                record_change(tx, account_id, modseq, "Thread", thread_id, if new_thread { "created" } else { "updated" })?;

                let email = IngestedEmail { id: email_id, thread_id, mailbox_ids, uid, blob: BlobHash::parse(&blob_key)?, size };
                Ok((email, modseq))
            })
            .await?;

        self.notify_change(account_id, modseq);
        Ok(email)
    }

    pub async fn mailboxes(&self, account_id: i64) -> Result<Vec<Mailbox>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.account_id, m.parent_id, m.name, m.role, m.sort_order, m.subscribed,
                        count(em.email_id),
                        count(em.email_id) - count(k.email_id),
                        count(DISTINCT e.thread_id),
                        count(DISTINCT CASE WHEN k.email_id IS NULL THEN e.thread_id END)
                 FROM mailboxes m
                 LEFT JOIN email_mailboxes em ON em.mailbox_id = m.id
                 LEFT JOIN emails e ON e.id = em.email_id
                 LEFT JOIN email_keywords k ON k.email_id = em.email_id AND k.keyword = '$seen'
                 WHERE m.account_id = ?1
                 GROUP BY m.id ORDER BY m.sort_order, m.name",
            )?;
            let rows = stmt.query_map([account_id], |row| {
                Ok(Mailbox {
                    id: row.get(0)?,
                    account_id: row.get(1)?,
                    parent_id: row.get(2)?,
                    name: row.get(3)?,
                    role: row.get::<_, Option<String>>(4)?.as_deref().and_then(MailboxRole::parse),
                    sort_order: row.get(5)?,
                    subscribed: row.get(6)?,
                    total_emails: row.get(7)?,
                    unread_emails: row.get(8)?,
                    total_threads: row.get(9)?,
                    unread_threads: row.get(10)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Newest messages of a mailbox.
    pub async fn emails_in_mailbox(&self, mailbox_id: i64, limit: i64) -> Result<Vec<EmailSummary>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT e.id, e.thread_id, e.subject, e.from_addr, e.preview, e.received_at, e.size, e.blob_hash,
                        (SELECT json_group_array(keyword) FROM email_keywords WHERE email_id = e.id)
                 FROM email_mailboxes em JOIN emails e ON e.id = em.email_id
                 WHERE em.mailbox_id = ?1 ORDER BY e.received_at DESC, e.id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![mailbox_id, limit], |row| {
                Ok(EmailSummary {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                    subject: row.get(2)?,
                    from: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
                    preview: row.get(4)?,
                    received_at: row.get(5)?,
                    size: row.get(6)?,
                    blob: row.get(7)?,
                    keywords: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Ids of messages matching a full-text query.
    pub async fn search_emails(&self, account_id: i64, query: &str) -> Result<Vec<i64>> {
        let fts_query = query
            .split_whitespace()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT e.id FROM email_fts f JOIN emails e ON e.id = f.rowid
                 WHERE email_fts MATCH ?1 AND e.account_id = ?2 ORDER BY e.received_at DESC",
            )?;
            let rows = stmt.query_map(params![fts_query, account_id], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Highest change sequence number of an account.
    pub async fn account_modseq(&self, account_id: i64) -> Result<i64> {
        self.read(move |conn| {
            conn.query_row("SELECT modseq FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    async fn account(store: &Store) -> i64 {
        store.create_domain("example.de").await.unwrap();
        store
            .create_account(NewAccount {
                address: "mini@example.de".into(),
                display_name: "Mini".into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
            })
            .await
            .unwrap()
            .id
    }

    fn message(id: &str, references: &str, subject: &str, body: &str) -> Vec<u8> {
        let mut raw = format!(
            "From: Nyu <nyu@example.org>\r\nTo: mini@example.de\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\n"
        );
        if !references.is_empty() {
            raw.push_str(&format!("In-Reply-To: <{references}>\r\nReferences: <{references}>\r\n"));
        }
        raw.push_str(&format!("\r\n{body}\r\n"));
        raw.into_bytes()
    }

    fn inbox(account_id: i64, raw: Vec<u8>) -> IngestRequest {
        IngestRequest {
            account_id,
            raw,
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        }
    }

    #[tokio::test]
    async fn new_accounts_get_default_mailboxes() {
        let (store, _dir) = store().await;
        let id = account(&store).await;
        let roles: Vec<_> = store.mailboxes(id).await.unwrap().into_iter().filter_map(|m| m.role).collect();
        assert_eq!(roles, MailboxRole::ALL);
    }

    #[tokio::test]
    async fn ingest_threads_indexes_and_counts() {
        let (store, _dir) = store().await;
        let id = account(&store).await;
        let mut changes = store.subscribe_changes();

        let first = store.ingest(inbox(id, message("root@x", "", "Katzenfutter", "Thunfisch bitte"))).await.unwrap();
        let reply = store.ingest(inbox(id, message("reply@x", "root@x", "Re: Katzenfutter", "Lachs"))).await.unwrap();
        let other = store.ingest(inbox(id, message("other@x", "", "Tierarzt", "Termin"))).await.unwrap();

        assert_eq!(first.thread_id, reply.thread_id);
        assert_ne!(first.thread_id, other.thread_id);
        assert_eq!((first.uid, reply.uid, other.uid), (1, 2, 3));
        assert_eq!(changes.recv().await.unwrap().account_id, id);

        let inbox_box =
            store.mailboxes(id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
        assert_eq!((inbox_box.total_emails, inbox_box.unread_emails), (3, 3));

        assert_eq!(store.search_emails(id, "thunfisch").await.unwrap(), vec![first.id]);
        assert_eq!(store.search_emails(id, "nyu@example.org").await.unwrap().len(), 3);

        let listed = store.emails_in_mailbox(inbox_box.id, 10).await.unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(store.blob(&first.blob).await.unwrap(), message("root@x", "", "Katzenfutter", "Thunfisch bitte"));
        assert_eq!(store.account_by_id(id).await.unwrap().unwrap().used_bytes, first.size + reply.size + other.size);
    }

    #[tokio::test]
    async fn replies_arriving_first_still_thread() {
        let (store, _dir) = store().await;
        let id = account(&store).await;
        let reply = store.ingest(inbox(id, message("reply@x", "root@x", "Re: Hallo", ""))).await.unwrap();
        let root = store.ingest(inbox(id, message("root@x", "", "Hallo", ""))).await.unwrap();
        assert_eq!(reply.thread_id, root.thread_id);
    }

    #[tokio::test]
    async fn quota_is_enforced() {
        let (store, _dir) = store().await;
        store.create_domain("example.de").await.unwrap();
        let account = store
            .create_account(NewAccount {
                address: "tiny@example.de".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 200,
            })
            .await
            .unwrap();
        store.ingest(inbox(account.id, message("a@x", "", "klein", ""))).await.unwrap();
        let big = message("b@x", "", "groß", &"x".repeat(300));
        assert!(matches!(store.ingest(inbox(account.id, big)).await, Err(StoreError::QuotaExceeded)));
    }

    #[tokio::test]
    async fn deleting_an_account_releases_blobs() {
        let (store, _dir) = store().await;
        let id = account(&store).await;
        let email = store.ingest(inbox(id, message("a@x", "", "weg damit", ""))).await.unwrap();
        store.delete_account("mini@example.de").await.unwrap();
        assert_eq!(store.collect_garbage(0).await.unwrap(), 1);
        assert!(matches!(store.blob(&email.blob).await, Err(StoreError::NotFound(_))));
    }
}

/// What happened to a test message: whether it arrived, and who answered it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestMessageStatus {
    pub arrived: bool,
    /// The sender of the first reply that referred to the test message.
    pub reply_from: Option<String>,
}

impl Store {
    /// Looks for a message by its Message-ID and for replies to it in an account.
    pub async fn test_message_status(&self, account_id: i64, message_id: &str) -> Result<TestMessageStatus> {
        let message_id = message_id.trim().trim_start_matches('<').trim_end_matches('>').to_owned();
        self.read(move |conn| {
            let arrived: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM emails WHERE account_id = ?1 AND message_id = ?2)",
                rusqlite::params![account_id, message_id],
                |row| row.get(0),
            )?;
            // in_reply_to is a JSON array of ids, so the quoted id matches a whole entry.
            let quoted = serde_json::to_string(&message_id).unwrap_or_default();
            let reply: Option<String> = conn
                .query_row(
                    "SELECT from_addr FROM emails WHERE account_id = ?1 AND instr(in_reply_to, ?2) > 0
                     ORDER BY received_at LIMIT 1",
                    rusqlite::params![account_id, quoted],
                    |row| row.get(0),
                )
                .optional()?;
            let reply_from = reply.and_then(|json| {
                serde_json::from_str::<Vec<crate::EmailAddress>>(&json).ok()?.into_iter().next().map(|a| a.email)
            });
            Ok(TestMessageStatus { arrived, reply_from })
        })
        .await
    }
}
