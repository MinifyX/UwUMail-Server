//! What others did to one's events (migration 0051): the CalendarEventNotifications of JMAP
//! Calendars (draft-ietf-jmap-calendars, section 7).
//!
//! When someone changes an event in a calendar shared with others, over CalDAV or JMAP, or when
//! scheduling puts an invitation, an update or an answer into someone's calendar, everyone else
//! who sees that calendar gets a notification: who did it, what the event was before and what it
//! is now. The change is kept once (`calendar_changes`) and each person's notification points to
//! it until they dismiss it. An event its owner keeps private or secret is only told to the owner.
//! Each person keeps at most [`MAX_NOTIFICATIONS`], none older than [`KEPT_SECS`].

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::dav::{ChangeLog, DavCollection, DavKind, audience};
use crate::{Result, Store, StoreError, now};

/// Notifications one person keeps; a new one pushes out the oldest.
pub const MAX_NOTIFICATIONS: i64 = 200;
/// How long a notification is kept when nobody dismisses it.
pub const KEPT_SECS: i64 = 30 * 86_400;
/// The largest event a notification keeps, before and after; a larger one is told without it, so
/// someone who may write a shared calendar cannot fill its owner's disk with notifications.
pub const MAX_KEPT_EVENT_BYTES: usize = 128 * 1024;

/// Who changed an event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventAuthor {
    /// A person of this server, or `None` for someone elsewhere.
    pub account_id: Option<i64>,
    pub name: String,
    pub email: Option<String>,
    /// The calendar address a scheduling message came from.
    pub calendar_address: Option<String>,
    /// What they said with it (an iTIP COMMENT).
    pub comment: Option<String>,
}

/// Who a write of an event is by, for the notifications of those who see it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Author {
    /// The account that writes.
    #[default]
    Account,
    /// Someone else, as for scheduling messages.
    Someone(EventAuthor),
    /// Nobody to tell anyone about, as when the server writes default alerts into events.
    Nobody,
}

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarNotification {
    pub id: i64,
    pub created_at: i64,
    /// `created`, `updated` or `destroyed`.
    pub kind: String,
    pub event_id: i64,
    pub is_draft: bool,
    pub author: EventAuthor,
    /// The event before and after, as iCalendar; left out when not asked for.
    pub old_content: Option<String>,
    pub new_content: Option<String>,
}

/// An event's entry before or after a write, as far as notifications care.
pub(crate) struct Side<'a> {
    pub component: &'a str,
    pub content: &'a str,
}

fn event_of<'a>(side: &Option<Side<'a>>) -> Option<&'a str> {
    side.as_ref().filter(|side| side.component == "VEVENT").map(|side| side.content)
}

fn private(content: &str) -> bool {
    let unfolded = content.replace("\r\n ", "").replace("\n ", "").to_ascii_uppercase();
    unfolded.contains("\nCLASS:PRIVATE") || unfolded.contains("\nCLASS:CONFIDENTIAL")
}

fn author_of(conn: &Connection, account_id: i64) -> Result<EventAuthor> {
    let (login, name): (String, String) = conn
        .query_row("SELECT login, display_name FROM accounts WHERE id = ?1", [account_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?
        .unwrap_or_default();
    Ok(EventAuthor {
        account_id: Some(account_id),
        name: if name.trim().is_empty() { login.clone() } else { name },
        calendar_address: Some(format!("mailto:{login}")),
        email: Some(login),
        comment: None,
    })
}

/// Notes a change of an entry for everyone who sees `collection` (and `target`, when it moved
/// there) except whoever made it. Only events count, and only writes someone made.
pub(crate) fn entry_changed(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    collection: &DavCollection,
    target: Option<&DavCollection>,
    resource_id: i64,
    before: Option<Side<'_>>,
    after: Option<Side<'_>>,
) -> Result<()> {
    if collection.kind != DavKind::Calendar {
        return Ok(());
    }
    let Some(author) = log.author.clone() else { return Ok(()) };
    let (old, new) = (event_of(&before), event_of(&after));
    if old.is_none() && new.is_none() {
        return Ok(());
    }
    let author = match author {
        Author::Nobody => return Ok(()),
        Author::Account => author_of(tx, log.account_id())?,
        // A person of this server, by account.
        Author::Someone(EventAuthor { account_id: Some(account), name, .. }) if name.is_empty() => {
            author_of(tx, account)?
        }
        Author::Someone(author) => author,
    };
    let hidden = old.is_some_and(private) || new.is_some_and(private);
    let before_audience = audience(tx, collection)?;
    let after_audience = match target {
        Some(target) => audience(tx, target)?,
        None => before_audience.clone(),
    };
    let owner = target.unwrap_or(collection).account_id;
    let mut told = Vec::new();
    for account in before_audience.iter().chain(&after_audience) {
        if told.contains(account) || Some(*account) == author.account_id {
            continue;
        }
        // Only the owner hears about what they keep private.
        if hidden && *account != owner && *account != collection.account_id {
            continue;
        }
        told.push(*account);
    }
    if told.is_empty() {
        return Ok(());
    }
    let draft: bool = tx
        .query_row("SELECT draft FROM dav_resources WHERE id = ?1", [resource_id], |row| row.get(0))
        .optional()?
        .unwrap_or(false);
    tx.execute(
        "INSERT INTO calendar_changes (resource_id, created_at, by_account, by_name, by_email, by_address, comment,
             is_draft, old_content, new_content)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            resource_id,
            now(),
            author.account_id,
            author.name,
            author.email,
            author.calendar_address,
            author.comment,
            draft,
            old.filter(|content| content.len() <= MAX_KEPT_EVENT_BYTES),
            new.filter(|content| content.len() <= MAX_KEPT_EVENT_BYTES)
        ],
    )?;
    let change_id = tx.last_insert_rowid();
    for account in told {
        let kind = match (
            before_audience.contains(&account) && old.is_some(),
            after_audience.contains(&account) && new.is_some(),
        ) {
            (false, true) => "created",
            (true, true) => "updated",
            (true, false) => "destroyed",
            (false, false) => continue,
        };
        tx.execute(
            "INSERT INTO calendar_notifications (account_id, change_id, type, event_id) VALUES (?1, ?2, ?3, ?4)",
            params![account, change_id, kind, resource_id],
        )?;
        let id = tx.last_insert_rowid();
        log.record_for(tx, account, "CalendarEventNotification", id, "created")?;
        prune(tx, log, account)?;
    }
    tx.execute(
        "DELETE FROM calendar_changes WHERE id = ?1 AND NOT EXISTS
             (SELECT 1 FROM calendar_notifications WHERE change_id = ?1)",
        [change_id],
    )?;
    Ok(())
}

/// Keeps one person's notifications within [`MAX_NOTIFICATIONS`] and [`KEPT_SECS`].
fn prune(tx: &Transaction<'_>, log: &mut ChangeLog, account_id: i64) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "SELECT n.id FROM calendar_notifications n JOIN calendar_changes c ON c.id = n.change_id
         WHERE n.account_id = ?1 AND (c.created_at < ?2 OR n.id NOT IN
             (SELECT id FROM calendar_notifications WHERE account_id = ?1 ORDER BY id DESC LIMIT ?3))",
    )?;
    let old: Vec<i64> = stmt
        .query_map(params![account_id, now() - KEPT_SECS, MAX_NOTIFICATIONS], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for id in old {
        forget(tx, id)?;
        log.record_for(tx, account_id, "CalendarEventNotification", id, "destroyed")?;
    }
    Ok(())
}

/// Deletes one notification, and the change it told about once nobody else has it.
fn forget(tx: &Transaction<'_>, id: i64) -> Result<()> {
    let change: Option<i64> = tx
        .query_row("DELETE FROM calendar_notifications WHERE id = ?1 RETURNING change_id", [id], |row| row.get(0))
        .optional()?;
    if let Some(change) = change {
        tx.execute(
            "DELETE FROM calendar_changes WHERE id = ?1 AND NOT EXISTS
                 (SELECT 1 FROM calendar_notifications WHERE change_id = ?1)",
            [change],
        )?;
    }
    Ok(())
}

impl Store {
    /// An account's notifications, newest last: the given ones, or all. `contents` reads the
    /// event before and after too.
    pub async fn calendar_notifications(
        &self,
        account_id: i64,
        ids: Option<Vec<i64>>,
        contents: bool,
    ) -> Result<Vec<CalendarNotification>> {
        let ids = ids.map(|ids| serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()));
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT n.id, c.created_at, n.type, n.event_id, c.is_draft, c.by_account, c.by_name, c.by_email,
                        c.by_address, c.comment,
                        CASE WHEN ?3 THEN c.old_content END, CASE WHEN ?3 THEN c.new_content END
                 FROM calendar_notifications n JOIN calendar_changes c ON c.id = n.change_id
                 WHERE n.account_id = ?1 AND (?2 IS NULL OR n.id IN (SELECT value FROM json_each(?2)))
                 ORDER BY n.id",
            )?;
            let rows = stmt.query_map(params![account_id, ids, contents], |row| {
                Ok(CalendarNotification {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    kind: row.get(2)?,
                    event_id: row.get(3)?,
                    is_draft: row.get(4)?,
                    author: EventAuthor {
                        account_id: row.get(5)?,
                        name: row.get(6)?,
                        email: row.get(7)?,
                        calendar_address: row.get(8)?,
                        comment: row.get(9)?,
                    },
                    old_content: row.get(10)?,
                    new_content: row.get(11)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Dismisses one of an account's notifications.
    pub async fn destroy_calendar_notification(&self, account_id: i64, id: i64) -> Result<()> {
        let logged = self
            .write(move |tx| {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM calendar_notifications WHERE id = ?1 AND account_id = ?2)",
                    params![id, account_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(StoreError::NotFound(format!("notification {id}")));
                }
                forget(tx, id)?;
                let mut log = ChangeLog::new(account_id);
                log.record(tx, "CalendarEventNotification", id, "destroyed")?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, logged);
        Ok(())
    }
}
