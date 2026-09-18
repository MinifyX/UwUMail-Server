//! What the IMAP service needs: mailboxes with their UIDs, the messages of one mailbox, flag
//! changes, copying, moving and expunging, and the UIDs that vanished since a change.
//!
//! IMAP flags are the email's keywords (`\Seen` is `$seen`, `\Deleted` is `$deleted`), so a flag
//! set in IMAP shows in every mailbox the email is in and in JMAP too.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::blobs::BlobHash;
use crate::mail::MailboxRole;
use crate::mutate::{Batch, EmailUpdate, KeywordsChange, MailboxesChange, destroy_one, update_one};
use crate::{EmailAddress, Result, Store, StoreError};

/// The keyword IMAP's `\Deleted` flag is kept as.
pub const DELETED_KEYWORD: &str = "$deleted";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapMailbox {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub role: Option<MailboxRole>,
    pub subscribed: bool,
    pub uid_validity: u32,
    pub uid_next: u32,
}

/// A message as a selected mailbox tracks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapMessage {
    pub uid: u32,
    pub email_id: i64,
    pub modseq: u64,
    /// Sorted.
    pub keywords: Vec<String>,
}

/// The messages of a mailbox, sorted by UID, and the account's change sequence number they reflect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapMessages {
    pub uid_validity: u32,
    pub uid_next: u32,
    pub highest_modseq: u64,
    pub messages: Vec<ImapMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImapStatus {
    pub messages: u32,
    pub unseen: u32,
    pub deleted: u32,
    pub size: u64,
    pub uid_next: u32,
    pub uid_validity: u32,
    pub highest_modseq: u64,
}

/// What FETCH and SEARCH read about one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapEmail {
    pub uid: u32,
    pub email_id: i64,
    pub modseq: u64,
    pub keywords: Vec<String>,
    pub blob: BlobHash,
    pub size: u64,
    pub received_at: i64,
    pub sent_at: Option<i64>,
    pub subject: String,
    pub from: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagChange {
    Add(Vec<String>),
    Remove(Vec<String>),
    Replace(Vec<String>),
}

fn mailbox_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImapMailbox> {
    Ok(ImapMailbox {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        name: row.get(2)?,
        role: row.get::<_, Option<String>>(3)?.as_deref().and_then(MailboxRole::parse),
        subscribed: row.get(4)?,
        uid_validity: row.get::<_, i64>(5)?.clamp(1, u32::MAX as i64) as u32,
        uid_next: row.get::<_, i64>(6)?.clamp(1, u32::MAX as i64) as u32,
    })
}

const MAILBOX_COLUMNS: &str = "id, parent_id, name, role, subscribed, uid_validity, uid_next";

fn own_mailbox(conn: &Connection, account_id: i64, mailbox_id: i64) -> Result<ImapMailbox> {
    conn.query_row(
        &format!("SELECT {MAILBOX_COLUMNS} FROM mailboxes WHERE id = ?1 AND account_id = ?2"),
        params![mailbox_id, account_id],
        mailbox_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("mailbox {mailbox_id}")))
}

fn account_modseq(conn: &Connection, account_id: i64) -> Result<u64> {
    let modseq: i64 = conn.query_row("SELECT modseq FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))?;
    Ok(modseq.max(0) as u64)
}

fn keywords_of(json: &str) -> Vec<String> {
    let mut keywords: Vec<String> = serde_json::from_str(json).unwrap_or_default();
    keywords.sort();
    keywords
}

/// The emails behind `uids` in a mailbox, by UID. UIDs that are not there are left out.
fn emails_by_uid(conn: &Connection, mailbox_id: i64, uids: &[u32]) -> Result<BTreeMap<u32, (i64, u64)>> {
    let mut stmt =
        conn.prepare_cached("SELECT email_id, modseq FROM email_mailboxes WHERE mailbox_id = ?1 AND uid = ?2")?;
    let mut found = BTreeMap::new();
    for uid in uids {
        if let Some((email, modseq)) = stmt
            .query_row(params![mailbox_id, uid], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .optional()?
        {
            found.insert(*uid, (email, modseq.max(0) as u64));
        }
    }
    Ok(found)
}

fn uid_in(conn: &Connection, mailbox_id: i64, email_id: i64) -> Result<Option<u32>> {
    Ok(conn
        .query_row(
            "SELECT uid FROM email_mailboxes WHERE mailbox_id = ?1 AND email_id = ?2",
            params![mailbox_id, email_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|uid| uid as u32))
}

fn finish(store: &Store, account_id: i64, modseq: Option<i64>) {
    if let Some(modseq) = modseq {
        store.notify_change(account_id, modseq);
    }
}

fn copy_in(
    tx: &Transaction<'_>,
    batch: &mut Batch,
    source: i64,
    uids: &[u32],
    target: i64,
    remove_source: bool,
) -> Result<Vec<(u32, u32)>> {
    let mut pairs = Vec::new();
    for (uid, (email_id, _)) in emails_by_uid(tx, source, uids)? {
        if source == target {
            pairs.push((uid, uid));
            continue;
        }
        let mut patch = Vec::new();
        if uid_in(tx, target, email_id)?.is_none() {
            patch.push((target, true));
        }
        if remove_source {
            patch.push((source, false));
        }
        if !patch.is_empty() {
            let update = EmailUpdate { id: email_id, mailboxes: MailboxesChange::Patch(patch), ..Default::default() };
            update_one(tx, batch, &update)?;
        }
        let new_uid = uid_in(tx, target, email_id)?
            .ok_or_else(|| StoreError::Internal(format!("email {email_id} did not arrive in mailbox {target}")))?;
        pairs.push((uid, new_uid));
    }
    Ok(pairs)
}

impl Store {
    /// All mailboxes of an account.
    pub async fn imap_mailboxes(&self, account_id: i64) -> Result<Vec<ImapMailbox>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {MAILBOX_COLUMNS} FROM mailboxes WHERE account_id = ?1 ORDER BY sort_order, name, id"
            ))?;
            let rows = stmt.query_map([account_id], mailbox_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// The messages of a mailbox with their flags, for selecting it and for noticing changes.
    pub async fn imap_messages(&self, account_id: i64, mailbox_id: i64) -> Result<ImapMessages> {
        self.read(move |conn| {
            let mailbox = own_mailbox(conn, account_id, mailbox_id)?;
            let highest_modseq = account_modseq(conn, account_id)?;
            let mut stmt = conn.prepare(
                "SELECT em.uid, em.email_id, em.modseq,
                        (SELECT json_group_array(keyword) FROM email_keywords WHERE email_id = em.email_id)
                 FROM email_mailboxes em WHERE em.mailbox_id = ?1 ORDER BY em.uid",
            )?;
            let rows = stmt.query_map([mailbox_id], |row| {
                Ok(ImapMessage {
                    uid: row.get::<_, i64>(0)? as u32,
                    email_id: row.get(1)?,
                    modseq: row.get::<_, i64>(2)?.max(0) as u64,
                    keywords: keywords_of(&row.get::<_, String>(3)?),
                })
            })?;
            let messages = rows.collect::<Result<_, _>>()?;
            Ok(ImapMessages {
                uid_validity: mailbox.uid_validity,
                uid_next: mailbox.uid_next,
                highest_modseq,
                messages,
            })
        })
        .await
    }

    pub async fn imap_status(&self, account_id: i64, mailbox_id: i64) -> Result<ImapStatus> {
        self.read(move |conn| {
            let mailbox = own_mailbox(conn, account_id, mailbox_id)?;
            let highest_modseq = account_modseq(conn, account_id)?;
            let (messages, unseen, deleted, size): (i64, i64, i64, i64) = conn.query_row(
                "SELECT count(*),
                        count(*) - count(seen.email_id),
                        count(deleted.email_id),
                        coalesce(sum(e.size), 0)
                 FROM email_mailboxes em
                 JOIN emails e ON e.id = em.email_id
                 LEFT JOIN email_keywords seen ON seen.email_id = em.email_id AND seen.keyword = '$seen'
                 LEFT JOIN email_keywords deleted ON deleted.email_id = em.email_id AND deleted.keyword = '$deleted'
                 WHERE em.mailbox_id = ?1",
                [mailbox_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            Ok(ImapStatus {
                messages: messages as u32,
                unseen: unseen as u32,
                deleted: deleted as u32,
                size: size.max(0) as u64,
                uid_next: mailbox.uid_next,
                uid_validity: mailbox.uid_validity,
                highest_modseq,
            })
        })
        .await
    }

    /// The messages behind `uids`, in UID order.
    pub async fn imap_emails(&self, account_id: i64, mailbox_id: i64, uids: Vec<u32>) -> Result<Vec<ImapEmail>> {
        self.read(move |conn| {
            own_mailbox(conn, account_id, mailbox_id)?;
            let mut stmt = conn.prepare_cached(
                "SELECT em.uid, e.id, em.modseq,
                        (SELECT json_group_array(keyword) FROM email_keywords WHERE email_id = e.id),
                        e.blob_hash, e.size, e.received_at, e.sent_at, e.subject, e.from_addr, e.to_addr, e.cc_addr,
                        e.bcc_addr
                 FROM email_mailboxes em JOIN emails e ON e.id = em.email_id
                 WHERE em.mailbox_id = ?1 AND em.uid = ?2",
            )?;
            let addresses = |json: String| serde_json::from_str(&json).unwrap_or_default();
            let mut sorted = uids;
            sorted.sort_unstable();
            sorted.dedup();
            let mut found = Vec::with_capacity(sorted.len());
            for uid in sorted {
                let row = stmt
                    .query_row(params![mailbox_id, uid], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, Option<i64>>(7)?,
                            row.get::<_, String>(8)?,
                            (row.get::<_, String>(9)?, row.get::<_, String>(10)?),
                            (row.get::<_, String>(11)?, row.get::<_, String>(12)?),
                        ))
                    })
                    .optional()?;
                let Some((
                    uid,
                    email_id,
                    modseq,
                    keywords,
                    blob,
                    size,
                    received_at,
                    sent_at,
                    subject,
                    (from, to),
                    (cc, bcc),
                )) = row
                else {
                    continue;
                };
                found.push(ImapEmail {
                    uid: uid as u32,
                    email_id,
                    modseq: modseq.max(0) as u64,
                    keywords: keywords_of(&keywords),
                    blob: BlobHash::parse(&blob)?,
                    size: size.max(0) as u64,
                    received_at,
                    sent_at,
                    subject,
                    from: addresses(from),
                    to: addresses(to),
                    cc: addresses(cc),
                    bcc: addresses(bcc),
                });
            }
            Ok(found)
        })
        .await
    }

    /// Changes the flags of messages. With `unchanged_since`, messages changed after that modseq
    /// are left alone and returned (CONDSTORE's UNCHANGEDSINCE).
    pub async fn imap_store_flags(
        &self,
        account_id: i64,
        mailbox_id: i64,
        uids: Vec<u32>,
        change: FlagChange,
        unchanged_since: Option<u64>,
    ) -> Result<Vec<u32>> {
        let keywords = match change {
            FlagChange::Add(flags) => KeywordsChange::Patch(flags.into_iter().map(|flag| (flag, true)).collect()),
            FlagChange::Remove(flags) => KeywordsChange::Patch(flags.into_iter().map(|flag| (flag, false)).collect()),
            FlagChange::Replace(flags) => KeywordsChange::Replace(flags),
        };
        let (skipped, modseq) = self
            .write(move |tx| {
                own_mailbox(tx, account_id, mailbox_id)?;
                let mut batch = Batch { account_id, modseq: None };
                let mut skipped = Vec::new();
                for (uid, (email_id, modseq)) in emails_by_uid(tx, mailbox_id, &uids)? {
                    if unchanged_since.is_some_and(|since| modseq > since) {
                        skipped.push(uid);
                        continue;
                    }
                    let update = EmailUpdate { id: email_id, keywords: keywords.clone(), ..Default::default() };
                    update_one(tx, &mut batch, &update)?;
                }
                Ok((skipped, batch.modseq))
            })
            .await?;
        finish(self, account_id, modseq);
        Ok(skipped)
    }

    /// Copies messages to another mailbox, or moves them with `remove_source`. Returns the pairs of
    /// source and target UIDs. A message that is in the target already keeps its UID there.
    pub async fn imap_copy(
        &self,
        account_id: i64,
        source: i64,
        uids: Vec<u32>,
        target: i64,
        remove_source: bool,
    ) -> Result<Vec<(u32, u32)>> {
        let (pairs, modseq) = self
            .write(move |tx| {
                own_mailbox(tx, account_id, source)?;
                own_mailbox(tx, account_id, target)?;
                let mut batch = Batch { account_id, modseq: None };
                let pairs = copy_in(tx, &mut batch, source, &uids, target, remove_source)?;
                Ok((pairs, batch.modseq))
            })
            .await?;
        finish(self, account_id, modseq);
        Ok(pairs)
    }

    /// Removes the messages flagged `\Deleted` from a mailbox, only those in `uids` when given.
    /// Messages in no other mailbox are destroyed. Returns the removed UIDs.
    pub async fn imap_expunge(&self, account_id: i64, mailbox_id: i64, uids: Option<Vec<u32>>) -> Result<Vec<u32>> {
        let (removed, modseq) = self
            .write(move |tx| {
                own_mailbox(tx, account_id, mailbox_id)?;
                let mut stmt = tx.prepare(
                    "SELECT em.uid, em.email_id, (SELECT count(*) FROM email_mailboxes o WHERE o.email_id = em.email_id)
                     FROM email_mailboxes em
                     JOIN email_keywords k ON k.email_id = em.email_id AND k.keyword = ?2
                     WHERE em.mailbox_id = ?1 ORDER BY em.uid",
                )?;
                let flagged = stmt
                    .query_map(params![mailbox_id, DELETED_KEYWORD], |row| {
                        Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                drop(stmt);
                let mut batch = Batch { account_id, modseq: None };
                let mut removed = Vec::new();
                for (uid, email_id, memberships) in flagged {
                    if uids.as_ref().is_some_and(|uids| !uids.contains(&uid)) {
                        continue;
                    }
                    if memberships > 1 {
                        let update = EmailUpdate {
                            id: email_id,
                            mailboxes: MailboxesChange::Patch(vec![(mailbox_id, false)]),
                            ..Default::default()
                        };
                        update_one(tx, &mut batch, &update)?;
                    } else {
                        destroy_one(tx, &mut batch, email_id)?;
                    }
                    removed.push(uid);
                }
                Ok((removed, batch.modseq))
            })
            .await?;
        finish(self, account_id, modseq);
        Ok(removed)
    }

    /// UIDs that left a mailbox after the change `since`, in UID order.
    pub async fn imap_vanished(&self, account_id: i64, mailbox_id: i64, since: u64) -> Result<Vec<u32>> {
        self.read(move |conn| {
            own_mailbox(conn, account_id, mailbox_id)?;
            let mut stmt =
                conn.prepare("SELECT uid FROM imap_vanished WHERE mailbox_id = ?1 AND modseq > ?2 ORDER BY uid")?;
            let rows = stmt.query_map(params![mailbox_id, since as i64], |row| row.get::<_, i64>(0))?;
            Ok(rows.map(|uid| uid.map(|uid| uid as u32)).collect::<Result<_, _>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{IngestRequest, MailboxTarget, NewAccount, Role};

    async fn setup() -> (Store, tempfile::TempDir, i64, BTreeMap<MailboxRole, i64>) {
        let (store, dir) = store().await;
        store.create_domain("example.de").await.unwrap();
        let account = store
            .create_account(NewAccount {
                address: "mini@example.de".into(),
                display_name: "Mini".into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let roles = store
            .imap_mailboxes(account)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|mailbox| Some((mailbox.role?, mailbox.id)))
            .collect();
        (store, dir, account, roles)
    }

    async fn deliver(store: &Store, account: i64, subject: &str) -> u32 {
        let raw = format!("From: nyu@example.org\r\nTo: mini@example.de\r\nSubject: {subject}\r\n\r\nHallo\r\n");
        let request = IngestRequest {
            account_id: account,
            raw: raw.into_bytes(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        };
        store.ingest(request).await.unwrap().uid as u32
    }

    #[tokio::test]
    async fn flags_are_keywords_and_respect_unchanged_since() {
        let (store, _dir, account, roles) = setup().await;
        let inbox = roles[&MailboxRole::Inbox];
        let (first, second) = (deliver(&store, account, "eins").await, deliver(&store, account, "zwei").await);

        let before = store.imap_messages(account, inbox).await.unwrap();
        assert_eq!(before.messages.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![first, second]);
        store
            .imap_store_flags(
                account,
                inbox,
                vec![first],
                FlagChange::Add(vec!["$seen".into(), "$flagged".into()]),
                None,
            )
            .await
            .unwrap();
        let after = store.imap_messages(account, inbox).await.unwrap();
        assert_eq!(after.messages[0].keywords, vec!["$flagged", "$seen"]);
        assert!(after.messages[0].modseq > before.highest_modseq);

        let skipped = store
            .imap_store_flags(
                account,
                inbox,
                vec![first, second],
                FlagChange::Remove(vec!["$seen".into()]),
                Some(before.highest_modseq),
            )
            .await
            .unwrap();
        assert_eq!(skipped, vec![first], "the first message changed after the client's state");
        let status = store.imap_status(account, inbox).await.unwrap();
        assert_eq!((status.messages, status.unseen), (2, 1));
    }

    #[tokio::test]
    async fn moving_and_expunging_leave_vanished_uids_behind() {
        let (store, _dir, account, roles) = setup().await;
        let (inbox, archive, trash) =
            (roles[&MailboxRole::Inbox], roles[&MailboxRole::Archive], roles[&MailboxRole::Trash]);
        let first = deliver(&store, account, "eins").await;
        let second = deliver(&store, account, "zwei").await;
        let state = store.imap_messages(account, inbox).await.unwrap().highest_modseq;

        let moved = store.imap_copy(account, inbox, vec![first], archive, true).await.unwrap();
        assert_eq!(moved, vec![(first, 1)]);
        let copied = store.imap_copy(account, inbox, vec![second], trash, false).await.unwrap();
        assert_eq!(copied, vec![(second, 1)]);
        let again = store.imap_copy(account, inbox, vec![second], trash, false).await.unwrap();
        assert_eq!(again, vec![(second, 1)], "a message is in a mailbox once");
        assert_eq!(store.imap_vanished(account, inbox, state).await.unwrap(), vec![first]);

        // In two mailboxes: expunging from the Inbox keeps the copy in the Trash.
        store
            .imap_store_flags(account, inbox, vec![second], FlagChange::Add(vec![DELETED_KEYWORD.into()]), None)
            .await
            .unwrap();
        assert_eq!(store.imap_expunge(account, inbox, Some(vec![first])).await.unwrap(), Vec::<u32>::new());
        assert_eq!(store.imap_expunge(account, inbox, None).await.unwrap(), vec![second]);
        assert_eq!(store.imap_vanished(account, inbox, state).await.unwrap(), vec![first, second]);
        assert_eq!(store.imap_status(account, trash).await.unwrap().messages, 1);

        // In one mailbox: expunging destroys the email.
        assert_eq!(store.imap_expunge(account, trash, None).await.unwrap(), vec![1]);
        assert_eq!(store.imap_status(account, trash).await.unwrap().messages, 0);
        assert!(store.imap_emails(account, archive, vec![1]).await.unwrap()[0].keywords.is_empty());
    }

    #[tokio::test]
    async fn mailboxes_of_other_accounts_stay_out_of_reach() {
        let (store, _dir, account, roles) = setup().await;
        let other = store
            .create_account(NewAccount {
                address: "leni@example.de".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let inbox = roles[&MailboxRole::Inbox];
        deliver(&store, account, "privat").await;
        assert!(matches!(store.imap_messages(other, inbox).await, Err(StoreError::NotFound(_))));
        assert!(matches!(store.imap_emails(other, inbox, vec![1]).await, Err(StoreError::NotFound(_))));
        assert!(matches!(store.imap_expunge(other, inbox, None).await, Err(StoreError::NotFound(_))));
    }
}
