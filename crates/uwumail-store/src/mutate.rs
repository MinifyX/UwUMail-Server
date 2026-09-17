//! Changing mail objects: keywords, mailbox membership, destroying emails, managing mailboxes.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::db::{next_modseq, record_change};
use crate::mail::{MailboxRole, add_to_mailbox};
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum KeywordsChange {
    #[default]
    Keep,
    Replace(Vec<String>),
    Patch(Vec<(String, bool)>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MailboxesChange {
    #[default]
    Keep,
    Replace(Vec<i64>),
    Patch(Vec<(i64, bool)>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmailUpdate {
    pub id: i64,
    pub keywords: KeywordsChange,
    pub mailboxes: MailboxesChange,
}

#[derive(Debug, Clone, Default)]
pub struct MailboxUpdate {
    pub name: Option<String>,
    pub parent_id: Option<Option<i64>>,
    pub role: Option<Option<MailboxRole>>,
    pub sort_order: Option<i64>,
    pub subscribed: Option<bool>,
}

/// One modseq for a whole batch, taken on the first actual change.
pub(crate) struct Batch {
    pub(crate) account_id: i64,
    pub(crate) modseq: Option<i64>,
}

impl Batch {
    pub(crate) fn modseq(&mut self, tx: &Transaction<'_>) -> Result<i64> {
        if let Some(modseq) = self.modseq {
            return Ok(modseq);
        }
        let modseq = next_modseq(tx, self.account_id)?;
        self.modseq = Some(modseq);
        Ok(modseq)
    }
}

fn rule(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Rule { code, message: message.into() }
}

fn email_mailboxes(conn: &Connection, email_id: i64) -> Result<BTreeSet<i64>> {
    let mut stmt = conn.prepare("SELECT mailbox_id FROM email_mailboxes WHERE email_id = ?1")?;
    let rows = stmt.query_map([email_id], |row| row.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn email_keywords(conn: &Connection, email_id: i64) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare("SELECT keyword FROM email_keywords WHERE email_id = ?1")?;
    let rows = stmt.query_map([email_id], |row| row.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn valid_keyword(keyword: &str) -> bool {
    !keyword.is_empty()
        && keyword.len() <= 255
        && keyword.bytes().all(|b| (0x21..=0x7e).contains(&b) && !b"()]{%*\"\\".contains(&b))
}

fn mailbox_belongs(conn: &Connection, account_id: i64, mailbox_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mailboxes WHERE id = ?1 AND account_id = ?2)",
        params![mailbox_id, account_id],
        |row| row.get(0),
    )?)
}

/// What a person said about an email with this change: `Some(true)` for "Spam", `Some(false)` for
/// "Not spam". Apps say it with the `$junk` / `$notjunk` keywords or by moving the email into or out
/// of the Junk mailbox; the UwUMail apps do both at once, which still counts once. Moving spam from
/// Junk to the Trash is only tidying up, not "Not spam".
fn junk_signal(
    tx: &Transaction<'_>,
    account_id: i64,
    (old_keywords, new_keywords): (&BTreeSet<String>, &BTreeSet<String>),
    (old_mailboxes, new_mailboxes): (&BTreeSet<i64>, &BTreeSet<i64>),
) -> Result<Option<bool>> {
    let added = |keyword: &str| new_keywords.contains(keyword) && !old_keywords.contains(keyword);
    let mut spam = added("$junk");
    let mut not_spam = added("$notjunk");
    if old_mailboxes != new_mailboxes {
        let role = |name: &str| -> Result<Option<i64>> {
            Ok(tx
                .query_row(
                    "SELECT id FROM mailboxes WHERE account_id = ?1 AND role = ?2",
                    params![account_id, name],
                    |row| row.get(0),
                )
                .optional()?)
        };
        if let Some(junk) = role("junk")? {
            let (was, is) = (old_mailboxes.contains(&junk), new_mailboxes.contains(&junk));
            let to_trash = role("trash")?.is_some_and(|trash| new_mailboxes.contains(&trash));
            spam |= is && !was;
            not_spam |= was && !is && !to_trash;
        }
    }
    Ok(match (spam, not_spam) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        // Nothing said, or both at once: nothing to learn.
        _ => None,
    })
}

pub(crate) fn update_one(tx: &Transaction<'_>, batch: &mut Batch, update: &EmailUpdate) -> Result<()> {
    let account_id = batch.account_id;
    let exists: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM emails WHERE id = ?1 AND account_id = ?2)",
        params![update.id, account_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(StoreError::NotFound(format!("email {}", update.id)));
    }

    let old_keywords = email_keywords(tx, update.id)?;
    let new_keywords: BTreeSet<String> = match &update.keywords {
        KeywordsChange::Keep => old_keywords.clone(),
        KeywordsChange::Replace(list) => list.iter().map(|k| k.to_lowercase()).collect(),
        KeywordsChange::Patch(patch) => {
            let mut set = old_keywords.clone();
            for (keyword, present) in patch {
                if *present {
                    set.insert(keyword.to_lowercase());
                } else {
                    set.remove(&keyword.to_lowercase());
                }
            }
            set
        }
    };
    if let Some(bad) = new_keywords.iter().find(|k| !valid_keyword(k)) {
        return Err(rule("invalidProperties", format!("'{bad}' is not a valid keyword")));
    }

    let old_mailboxes = email_mailboxes(tx, update.id)?;
    let new_mailboxes: BTreeSet<i64> = match &update.mailboxes {
        MailboxesChange::Keep => old_mailboxes.clone(),
        MailboxesChange::Replace(list) => list.iter().copied().collect(),
        MailboxesChange::Patch(patch) => {
            let mut set = old_mailboxes.clone();
            for (mailbox, present) in patch {
                if *present {
                    set.insert(*mailbox);
                } else {
                    set.remove(mailbox);
                }
            }
            set
        }
    };
    if new_mailboxes.is_empty() {
        return Err(rule("invalidProperties", "an email must be in at least one mailbox"));
    }
    for mailbox in new_mailboxes.difference(&old_mailboxes) {
        if !mailbox_belongs(tx, account_id, *mailbox)? {
            return Err(rule("invalidProperties", format!("mailbox {mailbox} does not exist")));
        }
    }

    if new_keywords == old_keywords && new_mailboxes == old_mailboxes {
        return Ok(());
    }
    let modseq = batch.modseq(tx)?;

    for keyword in new_keywords.difference(&old_keywords) {
        tx.execute("INSERT INTO email_keywords (email_id, keyword) VALUES (?1, ?2)", params![update.id, keyword])?;
    }
    for keyword in old_keywords.difference(&new_keywords) {
        tx.execute("DELETE FROM email_keywords WHERE email_id = ?1 AND keyword = ?2", params![update.id, keyword])?;
    }
    for mailbox in new_mailboxes.difference(&old_mailboxes) {
        add_to_mailbox(tx, update.id, *mailbox, modseq)?;
    }
    for mailbox in old_mailboxes.difference(&new_mailboxes) {
        tx.execute("DELETE FROM email_mailboxes WHERE email_id = ?1 AND mailbox_id = ?2", params![update.id, mailbox])?;
    }
    // Flag changes are visible in IMAP per mailbox.
    tx.execute("UPDATE email_mailboxes SET modseq = ?1 WHERE email_id = ?2", params![modseq, update.id])?;
    tx.execute("UPDATE emails SET updated_modseq = ?1 WHERE id = ?2", params![modseq, update.id])?;
    record_change(tx, account_id, modseq, "Email", update.id, "updated")?;

    // "Spam" and "Not spam" from a person teach the filter about the sender.
    if let Some(junk) = junk_signal(tx, account_id, (&old_keywords, &new_keywords), (&old_mailboxes, &new_mailboxes))? {
        crate::spam::rebook_verdict(tx, update.id, junk)?;
        crate::bayes::queue_marked(tx, update.id, account_id, junk)?;
    }

    // Counts change in every mailbox the email was or is in.
    let seen_changed = old_keywords.contains("$seen") != new_keywords.contains("$seen");
    for mailbox in old_mailboxes.symmetric_difference(&new_mailboxes) {
        record_change(tx, account_id, modseq, "Mailbox", *mailbox, "updated")?;
    }
    if seen_changed {
        for mailbox in &new_mailboxes {
            record_change(tx, account_id, modseq, "Mailbox", *mailbox, "updated")?;
        }
    }
    Ok(())
}

pub(crate) fn destroy_one(tx: &Transaction<'_>, batch: &mut Batch, email_id: i64) -> Result<()> {
    let account_id = batch.account_id;
    let found: Option<(i64, i64)> = tx
        .query_row(
            "SELECT thread_id, size FROM emails WHERE id = ?1 AND account_id = ?2",
            params![email_id, account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((thread_id, size)) = found else {
        return Err(StoreError::NotFound(format!("email {email_id}")));
    };
    let modseq = batch.modseq(tx)?;
    let mailboxes = email_mailboxes(tx, email_id)?;
    tx.execute("DELETE FROM emails WHERE id = ?1", [email_id])?;
    tx.execute("UPDATE accounts SET used_bytes = max(0, used_bytes - ?1) WHERE id = ?2", params![size, account_id])?;
    record_change(tx, account_id, modseq, "Email", email_id, "destroyed")?;
    for mailbox in mailboxes {
        tx.execute("UPDATE mailboxes SET updated_modseq = ?1 WHERE id = ?2", params![modseq, mailbox])?;
        record_change(tx, account_id, modseq, "Mailbox", mailbox, "updated")?;
    }
    let remaining: i64 =
        tx.query_row("SELECT count(*) FROM emails WHERE thread_id = ?1", [thread_id], |row| row.get(0))?;
    if remaining == 0 {
        tx.execute("DELETE FROM threads WHERE id = ?1", [thread_id])?;
        record_change(tx, account_id, modseq, "Thread", thread_id, "destroyed")?;
    } else {
        record_change(tx, account_id, modseq, "Thread", thread_id, "updated")?;
    }
    Ok(())
}

fn valid_mailbox_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 255 || name.chars().any(char::is_control) {
        return Err(rule("invalidProperties", "the mailbox name is empty, too long or contains control characters"));
    }
    Ok(name.to_owned())
}

fn check_parent(conn: &Connection, account_id: i64, mailbox_id: Option<i64>, parent: Option<i64>) -> Result<()> {
    let mut current = parent;
    let mut depth = 0;
    while let Some(id) = current {
        if Some(id) == mailbox_id {
            return Err(rule("invalidProperties", "a mailbox cannot be inside itself"));
        }
        current = conn
            .query_row(
                "SELECT parent_id FROM mailboxes WHERE id = ?1 AND account_id = ?2",
                params![id, account_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .ok_or_else(|| rule("invalidProperties", format!("parent mailbox {id} does not exist")))?;
        depth += 1;
        if depth > 64 {
            return Err(rule("invalidProperties", "mailboxes are nested too deeply"));
        }
    }
    Ok(())
}

fn map_unique(err: rusqlite::Error) -> StoreError {
    match err {
        rusqlite::Error::SqliteFailure(e, Some(message)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
            if message.contains("role") {
                rule("invalidProperties", "another mailbox already has this role")
            } else {
                rule("invalidProperties", "a mailbox with this name already exists here")
            }
        }
        other => other.into(),
    }
}

impl Store {
    /// Applies updates in one transaction. Returns one result per update, in order.
    pub async fn update_emails(&self, account_id: i64, updates: Vec<EmailUpdate>) -> Result<Vec<Result<()>>> {
        let (results, modseq) = self
            .write(move |tx| {
                let mut batch = Batch { account_id, modseq: None };
                let results = updates.iter().map(|update| update_one(tx, &mut batch, update)).collect::<Vec<_>>();
                Ok((results, batch.modseq))
            })
            .await?;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        Ok(results)
    }

    pub async fn destroy_emails(&self, account_id: i64, ids: Vec<i64>) -> Result<Vec<Result<()>>> {
        let (results, modseq) = self
            .write(move |tx| {
                let mut batch = Batch { account_id, modseq: None };
                let results = ids.iter().map(|id| destroy_one(tx, &mut batch, *id)).collect::<Vec<_>>();
                Ok((results, batch.modseq))
            })
            .await?;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        Ok(results)
    }

    pub async fn create_mailbox(
        &self,
        account_id: i64,
        name: &str,
        parent_id: Option<i64>,
        role: Option<MailboxRole>,
        sort_order: i64,
        subscribed: bool,
    ) -> Result<i64> {
        let name = valid_mailbox_name(name)?;
        let (id, modseq) = self
            .write(move |tx| {
                check_parent(tx, account_id, None, parent_id)?;
                let modseq = next_modseq(tx, account_id)?;
                tx.execute(
                    "INSERT INTO mailboxes (account_id, parent_id, name, role, sort_order, subscribed, uid_validity,
                         created_modseq, updated_modseq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                    params![
                        account_id,
                        parent_id,
                        name,
                        role.map(MailboxRole::as_str),
                        sort_order,
                        subscribed,
                        crate::now(),
                        modseq
                    ],
                )
                .map_err(map_unique)?;
                let id = tx.last_insert_rowid();
                record_change(tx, account_id, modseq, "Mailbox", id, "created")?;
                Ok((id, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(id)
    }

    pub async fn update_mailbox(&self, account_id: i64, id: i64, update: MailboxUpdate) -> Result<()> {
        let name = update.name.as_deref().map(valid_mailbox_name).transpose()?;
        let modseq = self
            .write(move |tx| {
                if !mailbox_belongs(tx, account_id, id)? {
                    return Err(StoreError::NotFound(format!("mailbox {id}")));
                }
                if let Some(parent) = update.parent_id {
                    check_parent(tx, account_id, Some(id), parent)?;
                }
                let modseq = next_modseq(tx, account_id)?;
                if let Some(name) = name {
                    tx.execute("UPDATE mailboxes SET name = ?1 WHERE id = ?2", params![name, id])
                        .map_err(map_unique)?;
                }
                if let Some(parent) = update.parent_id {
                    tx.execute("UPDATE mailboxes SET parent_id = ?1 WHERE id = ?2", params![parent, id])
                        .map_err(map_unique)?;
                }
                if let Some(role) = update.role {
                    tx.execute(
                        "UPDATE mailboxes SET role = ?1 WHERE id = ?2",
                        params![role.map(MailboxRole::as_str), id],
                    )
                    .map_err(map_unique)?;
                }
                if let Some(order) = update.sort_order {
                    tx.execute("UPDATE mailboxes SET sort_order = ?1 WHERE id = ?2", params![order, id])?;
                }
                if let Some(subscribed) = update.subscribed {
                    tx.execute("UPDATE mailboxes SET subscribed = ?1 WHERE id = ?2", params![subscribed, id])?;
                }
                tx.execute("UPDATE mailboxes SET updated_modseq = ?1 WHERE id = ?2", params![modseq, id])?;
                record_change(tx, account_id, modseq, "Mailbox", id, "updated")?;
                Ok(modseq)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    /// Destroys a mailbox. With `remove_emails`, emails only in this mailbox are destroyed
    /// and the others just leave it; without, a mailbox that still has emails is refused.
    pub async fn destroy_mailbox(&self, account_id: i64, id: i64, remove_emails: bool) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                if !mailbox_belongs(tx, account_id, id)? {
                    return Err(StoreError::NotFound(format!("mailbox {id}")));
                }
                let children: i64 =
                    tx.query_row("SELECT count(*) FROM mailboxes WHERE parent_id = ?1", [id], |row| row.get(0))?;
                if children > 0 {
                    return Err(rule("mailboxHasChild", "the mailbox has child mailboxes"));
                }
                let mut stmt = tx.prepare(
                    "SELECT em.email_id, (SELECT count(*) FROM email_mailboxes o WHERE o.email_id = em.email_id)
                     FROM email_mailboxes em WHERE em.mailbox_id = ?1",
                )?;
                let emails = stmt
                    .query_map([id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                drop(stmt);
                if !emails.is_empty() && !remove_emails {
                    return Err(rule("mailboxHasEmail", "the mailbox still has emails"));
                }
                let mut batch = Batch { account_id, modseq: None };
                let modseq = batch.modseq(tx)?;
                for (email, memberships) in emails {
                    if memberships <= 1 {
                        destroy_one(tx, &mut batch, email)?;
                    } else {
                        tx.execute(
                            "DELETE FROM email_mailboxes WHERE email_id = ?1 AND mailbox_id = ?2",
                            params![email, id],
                        )?;
                        tx.execute("UPDATE emails SET updated_modseq = ?1 WHERE id = ?2", params![modseq, email])?;
                        record_change(tx, account_id, modseq, "Email", email, "updated")?;
                    }
                }
                tx.execute("DELETE FROM mailboxes WHERE id = ?1", [id])?;
                record_change(tx, account_id, modseq, "Mailbox", id, "destroyed")?;
                Ok(modseq)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{IngestRequest, MailboxTarget, NewAccount, Role};

    async fn setup() -> (Store, tempfile::TempDir, i64, i64, i64) {
        let (store, dir) = store().await;
        store.create_domain("example.de").await.unwrap();
        let account = store
            .create_account(NewAccount {
                address: "mini@example.de".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
            })
            .await
            .unwrap()
            .id;
        let boxes = store.mailboxes(account).await.unwrap();
        let inbox = boxes.iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap().id;
        let archive = boxes.iter().find(|m| m.role == Some(MailboxRole::Archive)).unwrap().id;
        (store, dir, account, inbox, archive)
    }

    async fn email(store: &Store, account: i64, id: &str) -> crate::IngestedEmail {
        store
            .ingest(IngestRequest {
                account_id: account,
                raw: format!("Subject: {id}\r\nMessage-ID: <{id}>\r\n\r\nhi\r\n").into_bytes(),
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec![],
                received_at: None,
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn updates_keywords_and_mailboxes() {
        let (store, _dir, account, inbox, archive) = setup().await;
        let first = email(&store, account, "a@x").await;
        let before = store.account_modseq(account).await.unwrap();

        let results = store
            .update_emails(
                account,
                vec![
                    EmailUpdate {
                        id: first.id,
                        keywords: KeywordsChange::Patch(vec![("$Seen".into(), true)]),
                        mailboxes: MailboxesChange::Patch(vec![(archive, true), (inbox, false)]),
                    },
                    EmailUpdate { id: 9999, ..EmailUpdate::default() },
                    EmailUpdate { id: first.id, mailboxes: MailboxesChange::Replace(vec![]), ..EmailUpdate::default() },
                ],
            )
            .await
            .unwrap();
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(StoreError::NotFound(_))));
        assert!(matches!(results[2], Err(StoreError::Rule { code: "invalidProperties", .. })));

        let record = store.email(account, first.id).await.unwrap();
        assert_eq!(record.keywords, vec!["$seen"]);
        assert_eq!(record.mailbox_ids, vec![archive]);
        let changes = store.changes(account, "Mailbox", before, 0).await.unwrap();
        assert_eq!(changes.updated, vec![inbox, archive]);
    }

    #[tokio::test]
    async fn destroys_emails_threads_and_mailboxes() {
        let (store, _dir, account, inbox, _archive) = setup().await;
        let first = email(&store, account, "a@x").await;
        let before = store.account_modseq(account).await.unwrap();
        let results = store.destroy_emails(account, vec![first.id, first.id]).await.unwrap();
        assert!(results[0].is_ok() && results[1].is_err());
        assert_eq!(store.changes(account, "Thread", before, 0).await.unwrap().destroyed, vec![first.thread_id]);
        assert_eq!(store.account_by_id(account).await.unwrap().unwrap().used_bytes, 0);

        let parent = store.create_mailbox(account, "Katzen", None, None, 10, true).await.unwrap();
        let child = store.create_mailbox(account, "Nyu", Some(parent), None, 0, true).await.unwrap();
        assert!(matches!(
            store.create_mailbox(account, "Katzen", None, None, 0, true).await,
            Err(StoreError::Rule { code: "invalidProperties", .. })
        ));
        assert!(matches!(
            store
                .update_mailbox(account, parent, MailboxUpdate { parent_id: Some(Some(child)), ..Default::default() })
                .await,
            Err(StoreError::Rule { .. })
        ));
        assert!(matches!(
            store.destroy_mailbox(account, parent, false).await,
            Err(StoreError::Rule { code: "mailboxHasChild", .. })
        ));

        let second = email(&store, account, "b@x").await;
        store
            .update_emails(
                account,
                vec![EmailUpdate {
                    id: second.id,
                    mailboxes: MailboxesChange::Replace(vec![child]),
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
        assert!(matches!(
            store.destroy_mailbox(account, child, false).await,
            Err(StoreError::Rule { code: "mailboxHasEmail", .. })
        ));
        store.destroy_mailbox(account, child, true).await.unwrap();
        assert!(store.emails_by_ids(account, vec![second.id]).await.unwrap().is_empty());
        store
            .update_mailbox(account, parent, MailboxUpdate { name: Some("Katzen & Co".into()), ..Default::default() })
            .await
            .unwrap();
        assert!(store.mailboxes(account).await.unwrap().iter().any(|m| m.name == "Katzen & Co"));
        assert!(store.mailboxes(account).await.unwrap().iter().any(|m| m.id == inbox));
    }
}
