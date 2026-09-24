//! Reading mail objects the way JMAP and IMAP need them: by id, per thread, and as changes.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::EmailAddress;
use crate::blobs::BlobHash;
use crate::{Result, Store, StoreError};

/// Everything the store knows about an email without reading its blob.
#[derive(Debug, Clone, Serialize)]
pub struct EmailRecord {
    pub id: i64,
    pub thread_id: i64,
    #[serde(skip)]
    pub blob: BlobHash,
    pub size: i64,
    pub received_at: i64,
    pub sent_at: Option<i64>,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Vec<EmailAddress>,
    pub sender: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    pub preview: String,
    pub has_attachment: bool,
    pub keywords: Vec<String>,
    pub mailbox_ids: Vec<i64>,
}

pub(crate) const EMAIL_COLUMNS: &str = "e.id, e.thread_id, e.blob_hash, e.size, e.received_at, e.sent_at, e.message_id,
     e.in_reply_to, e.refs, e.subject, e.from_addr, e.sender_addr, e.to_addr, e.cc_addr, e.bcc_addr, e.reply_to_addr,
     e.preview, e.has_attachment,
     (SELECT json_group_array(keyword) FROM email_keywords WHERE email_id = e.id),
     (SELECT json_group_array(mailbox_id) FROM email_mailboxes WHERE email_id = e.id)";

fn json<T: serde::de::DeserializeOwned + Default>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    Ok(serde_json::from_str(&row.get::<_, String>(index)?).unwrap_or_default())
}

pub(crate) fn email_from_row(row: &Row<'_>) -> rusqlite::Result<EmailRecord> {
    let blob: String = row.get(2)?;
    let mut mailbox_ids: Vec<i64> = json(row, 19)?;
    mailbox_ids.sort_unstable();
    let mut keywords: Vec<String> = json(row, 18)?;
    keywords.sort();
    Ok(EmailRecord {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        blob: BlobHash::parse(&blob)
            .map_err(|_| rusqlite::Error::InvalidColumnType(2, "blob_hash".into(), rusqlite::types::Type::Text))?,
        size: row.get(3)?,
        received_at: row.get(4)?,
        sent_at: row.get(5)?,
        message_id: row.get(6)?,
        in_reply_to: json(row, 7)?,
        references: json(row, 8)?,
        subject: row.get(9)?,
        from: json(row, 10)?,
        sender: json(row, 11)?,
        to: json(row, 12)?,
        cc: json(row, 13)?,
        bcc: json(row, 14)?,
        reply_to: json(row, 15)?,
        preview: row.get(16)?,
        has_attachment: row.get(17)?,
        keywords,
        mailbox_ids,
    })
}

/// What changed for one object type since a state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub created: Vec<i64>,
    pub updated: Vec<i64>,
    pub destroyed: Vec<i64>,
    pub new_state: i64,
    pub has_more: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    Created,
    Updated,
    Destroyed,
    CreatedThenDestroyed,
}

impl Store {
    pub async fn emails_by_ids(&self, account_id: i64, ids: Vec<i64>) -> Result<Vec<EmailRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into());
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {EMAIL_COLUMNS} FROM emails e
                 WHERE e.account_id = ?1 AND e.id IN (SELECT value FROM json_each(?2))"
            ))?;
            let rows = stmt.query_map(params![account_id, ids_json], email_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn email(&self, account_id: i64, id: i64) -> Result<EmailRecord> {
        self.emails_by_ids(account_id, vec![id]).await?.pop().ok_or_else(|| StoreError::NotFound(format!("email {id}")))
    }

    /// Email ids of each thread, oldest first. Unknown threads are left out.
    pub async fn thread_emails(&self, account_id: i64, thread_ids: Vec<i64>) -> Result<BTreeMap<i64, Vec<i64>>> {
        let ids_json = serde_json::to_string(&thread_ids).unwrap_or_else(|_| "[]".into());
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT thread_id, id FROM emails
                 WHERE account_id = ?1 AND thread_id IN (SELECT value FROM json_each(?2))
                 ORDER BY received_at, id",
            )?;
            let mut threads: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
            let rows = stmt.query_map(params![account_id, ids_json], |row| Ok((row.get(0)?, row.get(1)?)))?;
            for row in rows {
                let (thread, email) = row?;
                threads.entry(thread).or_default().push(email);
            }
            Ok(threads)
        })
        .await
    }

    /// Object kinds that changed after `since`.
    pub async fn changed_kinds(&self, account_id: i64, since: i64) -> Result<Vec<String>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare("SELECT DISTINCT kind FROM changes WHERE account_id = ?1 AND modseq > ?2")?;
            let rows = stmt.query_map(params![account_id, since], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Changes of one kind (`Email`, `Mailbox`, `Thread`, ...) after `since`. Returns
    /// [`StoreError::Invalid`] for states this account never had.
    pub async fn changes(&self, account_id: i64, kind: &str, since: i64, max_changes: usize) -> Result<Changes> {
        let kind = kind.to_owned();
        self.read(move |conn| {
            let current: i64 = conn
                .query_row("SELECT modseq FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
            if since < 0 || since > current {
                return Err(StoreError::Invalid(format!("unknown state {since}")));
            }
            let mut stmt = conn.prepare(
                "SELECT modseq, object_id, change FROM changes
                 WHERE account_id = ?1 AND kind = ?2 AND modseq > ?3 ORDER BY modseq, object_id",
            )?;
            let rows = stmt
                .query_map(params![account_id, kind, since], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            let mut objects: BTreeMap<i64, Seen> = BTreeMap::new();
            let mut new_state = current;
            let mut has_more = false;
            let mut index = 0;
            while index < rows.len() {
                let modseq = rows[index].0;
                let group_end = rows[index..].iter().position(|r| r.0 != modseq).map_or(rows.len(), |p| index + p);
                let group = &rows[index..group_end];
                let new_objects = group.iter().filter(|r| !objects.contains_key(&r.1)).count();
                if max_changes > 0 && !objects.is_empty() && objects.len() + new_objects > max_changes {
                    new_state = rows[index - 1].0;
                    has_more = true;
                    break;
                }
                for (_, object, change) in group {
                    let next = match (objects.get(object).copied(), change.as_str()) {
                        (None, "created") => Seen::Created,
                        (None, "updated") => Seen::Updated,
                        (None, _) => Seen::Destroyed,
                        (Some(Seen::Created), "destroyed") => Seen::CreatedThenDestroyed,
                        (Some(Seen::Created), _) => Seen::Created,
                        (Some(Seen::Updated), "destroyed") => Seen::Destroyed,
                        (Some(seen), _) => seen,
                    };
                    objects.insert(*object, next);
                }
                index = group_end;
            }

            let mut changes = Changes { new_state, has_more, ..Changes::default() };
            for (object, seen) in objects {
                match seen {
                    Seen::Created => changes.created.push(object),
                    Seen::Updated => changes.updated.push(object),
                    Seen::Destroyed => changes.destroyed.push(object),
                    Seen::CreatedThenDestroyed => {}
                }
            }
            Ok(changes)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::store;
    use crate::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role};

    #[tokio::test]
    async fn reads_records_threads_and_changes() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let account = store
            .create_account(NewAccount {
                address: "mini@example.org".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        let start = store.account_modseq(account.id).await.unwrap();
        let mut ids = Vec::new();
        for (id, parent) in [("a@x", ""), ("b@x", "a@x"), ("c@x", "")] {
            let references = if parent.is_empty() { String::new() } else { format!("References: <{parent}>\r\n") };
            let raw = format!("From: Nyu <nyu@x>\r\nSubject: {id}\r\nMessage-ID: <{id}>\r\n{references}\r\nhi\r\n");
            let email = store
                .ingest(IngestRequest {
                    account_id: account.id,
                    raw: raw.into_bytes(),
                    mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox), MailboxTarget::Role(MailboxRole::Archive)],
                    keywords: vec!["$Seen".into()],
                    received_at: Some(1000 + ids.len() as i64),
                })
                .await
                .unwrap();
            ids.push(email);
        }

        let records = store.emails_by_ids(account.id, vec![ids[0].id, 999]).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].keywords, vec!["$seen"]);
        assert_eq!(records[0].mailbox_ids.len(), 2);
        assert_eq!(records[0].from[0].email, "nyu@x");

        let threads = store.thread_emails(account.id, vec![ids[0].thread_id]).await.unwrap();
        assert_eq!(threads[&ids[0].thread_id], vec![ids[0].id, ids[1].id]);

        let all = store.changes(account.id, "Email", start, 0).await.unwrap();
        assert_eq!(all.created.len(), 3);
        assert!(!all.has_more);

        let first_two = store.changes(account.id, "Email", start, 2).await.unwrap();
        assert_eq!(first_two.created, vec![ids[0].id, ids[1].id]);
        assert!(first_two.has_more);
        let rest = store.changes(account.id, "Email", first_two.new_state, 2).await.unwrap();
        assert_eq!(rest.created, vec![ids[2].id]);

        let mailboxes = store.changes(account.id, "Mailbox", start, 0).await.unwrap();
        assert_eq!(mailboxes.updated.len(), 2);
        assert!(store.changes(account.id, "Email", 10_000, 0).await.is_err());
    }
}
