//! Reading mail objects the way JMAP and IMAP need them: by id, per thread, and as changes.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::EmailAddress;
use crate::blobs::BlobHash;
use crate::{Result, Store, StoreError, now};

/// How long the change log and the vanished IMAP UIDs are kept: a client that was away for longer
/// loads its mail again.
pub const CHANGES_RETENTION_SECS: i64 = 90 * 24 * 3600;

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
    /// This server's SMTP delivery stored it: the headers it wrote on top are its own (security
    /// review 0.22 R2-L2). See [`Store::ingest_marked`].
    #[serde(skip)]
    pub smtp_delivered: bool,
}

pub(crate) const EMAIL_COLUMNS: &str = "e.id, e.thread_id, e.blob_hash, e.size, e.received_at, e.sent_at, e.message_id,
     e.in_reply_to, e.refs, e.subject, e.from_addr, e.sender_addr, e.to_addr, e.cc_addr, e.bcc_addr, e.reply_to_addr,
     e.preview, e.has_attachment,
     (SELECT json_group_array(keyword) FROM email_keywords WHERE email_id = e.id),
     (SELECT json_group_array(mailbox_id) FROM email_mailboxes WHERE email_id = e.id),
     e.smtp_delivered";

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
        smtp_delivered: row.get(20)?,
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
    ///
    /// Only changes up to `upto` count: a later one may already be written while an earlier
    /// notification is handled, and reporting it under the earlier state would hand out a state
    /// that is older than the change (it is reported with its own notification).
    pub async fn changed_kinds(&self, account_id: i64, since: i64, upto: i64) -> Result<Vec<String>> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare("SELECT DISTINCT kind FROM changes WHERE account_id = ?1 AND modseq > ?2 AND modseq <= ?3")?;
            let rows = stmt.query_map(params![account_id, since, upto], |row| row.get(0))?;
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
            if since < changes_floor(conn, account_id)? {
                return Err(StoreError::Invalid(format!("the changes since {since} are no longer kept")));
            }
            let mut stmt = conn.prepare(
                "SELECT modseq, object_id, change FROM changes
                 WHERE account_id = ?1 AND kind = ?2 AND modseq > ?3 ORDER BY modseq, object_id",
            )?;
            read_changes(stmt.query(params![account_id, kind, since])?, current, max_changes)
        })
        .await
    }

    /// Forgets changes older than `retention_secs`, for JMAP and IMAP alike, and remembers per
    /// account the lowest state that can still be answered. Once a day the account's modseq is
    /// noted with the time, since the change rows have none. Returns the change rows removed.
    ///
    /// Each account gets its own writes and rows go in batches, so the first prune of a big
    /// history does not hold the write lock for long. The floor moves up before any row goes:
    /// a `changes` call in between answers "no longer kept" rather than a delta with gaps.
    pub async fn prune_changes(&self, retention_secs: i64) -> Result<usize> {
        let floors = self
            .write(move |tx| {
                let now = now();
                tx.execute(
                    "INSERT INTO change_marks (account_id, at, modseq)
                     SELECT id, ?1, modseq FROM accounts a
                     WHERE NOT EXISTS (SELECT 1 FROM change_marks m WHERE m.account_id = a.id AND m.at > ?1 - 86400)",
                    [now],
                )?;
                // Per account the newest mark that is old enough, where it moves the floor up.
                let floors = tx
                    .prepare(
                        "SELECT m.account_id, m.at, m.modseq FROM change_marks m JOIN accounts a ON a.id = m.account_id
                         WHERE m.at = (SELECT max(at) FROM change_marks WHERE account_id = m.account_id AND at <= ?1)
                           AND m.modseq > a.changes_floor",
                    )?
                    .query_map([now - retention_secs], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(i64, i64, i64)>>>()?;
                Ok(floors)
            })
            .await?;
        let mut removed = 0;
        for (account_id, at, floor) in floors {
            self.write(move |tx| {
                tx.execute(
                    "UPDATE accounts SET changes_floor = max(changes_floor, ?2) WHERE id = ?1",
                    params![account_id, floor],
                )?;
                tx.execute("DELETE FROM change_marks WHERE account_id = ?1 AND at < ?2", params![account_id, at])?;
                Ok(())
            })
            .await?;
            // Rows at or below the floor are never read any more, so a crash between batches
            // only leaves rows that the next floor move takes along.
            loop {
                let (changes, vanished) = self
                    .write(move |tx| {
                        let changes = tx.execute(
                            "DELETE FROM changes WHERE (account_id, modseq, kind, object_id) IN
                               (SELECT account_id, modseq, kind, object_id FROM changes
                                WHERE account_id = ?1 AND modseq <= ?2 LIMIT ?3)",
                            params![account_id, floor, PRUNE_BATCH as i64],
                        )?;
                        let vanished = tx.execute(
                            "DELETE FROM imap_vanished WHERE (mailbox_id, uid) IN
                               (SELECT mailbox_id, uid FROM imap_vanished WHERE modseq <= ?2
                                  AND mailbox_id IN (SELECT id FROM mailboxes WHERE account_id = ?1) LIMIT ?3)",
                            params![account_id, floor, PRUNE_BATCH as i64],
                        )?;
                        Ok((changes, vanished))
                    })
                    .await?;
                removed += changes;
                if changes < PRUNE_BATCH && vanished < PRUNE_BATCH {
                    break;
                }
            }
        }
        Ok(removed)
    }
}

/// Rows [`Store::prune_changes`] removes per write.
const PRUNE_BATCH: usize = if cfg!(test) { 3 } else { 5000 };

/// The lowest state whose changes are still kept (see [`Store::prune_changes`]).
pub(crate) fn changes_floor(conn: &rusqlite::Connection, account_id: i64) -> rusqlite::Result<i64> {
    conn.query_row("SELECT changes_floor FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
        .optional()
        .map(Option::unwrap_or_default)
}

/// Reads change log rows (modseq, object, change) in modseq order and folds them with
/// [`aggregate`]. With `max_changes` it stops at the first modseq after more objects than that,
/// instead of reading the whole log.
pub(crate) fn read_changes(mut rows: rusqlite::Rows<'_>, current: i64, max_changes: usize) -> Result<Changes> {
    let mut read: Vec<(i64, i64, String)> = Vec::new();
    let mut objects = HashSet::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        let row = (row.get(0)?, row.get(1)?, row.get(2)?);
        if max_changes > 0 && objects.len() > max_changes && read.last().map(|last| last.0) != Some(row.0) {
            truncated = true;
            break;
        }
        objects.insert(row.1);
        read.push(row);
    }
    Ok(aggregate(&read, current, max_changes, truncated))
}

/// Folds change log rows (modseq, object, change), in modseq order, into what changed per
/// object. With `max_changes`, stops before the modseq that would pass it, and says so.
/// `truncated` says more rows follow the last one given, which end with a whole modseq.
pub(crate) fn aggregate(rows: &[(i64, i64, String)], current: i64, max_changes: usize, truncated: bool) -> Changes {
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
    if truncated && !has_more {
        // The first modseq alone held more objects than asked for; the rest comes next time.
        new_state = rows.last().map_or(current, |last| last.0);
        has_more = true;
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
    changes
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

        // Pruned changes cannot be answered any more; from the floor on everything still can.
        let now = store.account_modseq(account.id).await.unwrap();
        // More rows than one batch (3 in tests): all go, and the floor is the current state.
        let before = store.changes(account.id, "Email", start, 0).await.unwrap().created.len()
            + store.changes(account.id, "Thread", start, 0).await.unwrap().created.len();
        assert!(before > super::PRUNE_BATCH);
        assert!(store.prune_changes(0).await.unwrap() >= before);
        let (left, floor) = store
            .read(move |conn| {
                let left: i64 =
                    conn.query_row("SELECT count(*) FROM changes WHERE account_id = ?1", [account.id], |row| {
                        row.get(0)
                    })?;
                Ok((left, super::changes_floor(conn, account.id)?))
            })
            .await
            .unwrap();
        assert_eq!((left, floor), (0, now));
        assert!(matches!(store.changes(account.id, "Email", start, 0).await, Err(crate::StoreError::Invalid(_))));
        let none = store.changes(account.id, "Email", now, 0).await.unwrap();
        assert!(none.created.is_empty() && !none.has_more);
        assert_eq!(store.prune_changes(0).await.unwrap(), 0, "one mark a day");
    }

    #[test]
    fn a_change_list_cut_short_ends_with_a_whole_state() {
        let row = |modseq, object, change: &str| (modseq, object, change.to_owned());
        // The first state alone holds more than asked for: it comes whole, and more follows.
        let rows = [row(5, 1, "created"), row(5, 2, "created"), row(5, 3, "created")];
        let changes = super::aggregate(&rows, 9, 2, true);
        assert_eq!((changes.created.len(), changes.new_state, changes.has_more), (3, 5, true));
        // Rows overlapping the same object count once.
        let rows = [row(5, 1, "created"), row(6, 1, "updated"), row(7, 2, "created"), row(8, 3, "created")];
        let changes = super::aggregate(&rows, 9, 2, true);
        assert_eq!((changes.created, changes.new_state, changes.has_more), (vec![1, 2], 7, true));
    }
}
