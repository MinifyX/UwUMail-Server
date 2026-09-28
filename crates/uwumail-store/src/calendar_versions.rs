//! Earlier versions of recurring events (migration 0052), for `CalendarEvent/queryChanges` of a
//! query that expands recurrences: its results are instances, and after a change the client has to
//! be told which of the instances it had are gone, which only the event as it was can say.
//!
//! For every change of an event that recurs before or after it, each account that sees the event
//! keeps the account's modseq of that change and the event before it (none when it did not recur
//! then, as its only id was the event's own). The first such row after a query state holds the
//! event as it was at that state. Rows are kept for [`KEPT_SECS`] and at most [`MAX_VERSIONS`] per
//! account; what goes raises the state from which an account's versions are complete.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::dav::ChangeLog;
use crate::{Result, Store, now};

/// How long earlier versions are kept, and how many per account.
pub const KEPT_SECS: i64 = 30 * 86_400;
pub const MAX_VERSIONS: i64 = 500;
/// The largest version kept. A change of a larger recurring event is not kept: queries from before
/// it cannot be replayed, as after a version went for its age.
pub const MAX_VERSION_BYTES: usize = 128 * 1024;

/// Whether an event object recurs or holds single instances: its ids are more than its own.
fn recurs(content: &str) -> bool {
    let upper = content.to_ascii_uppercase();
    ["\nRRULE", "\nRDATE", "\nRECURRENCE-ID"].iter().any(|name| upper.contains(name))
}

/// Notes a change of an event for the accounts that see it (`audience`), after their change log
/// entries were made. `before` and `after` are the object's contents, `None` when it was not or is
/// no longer there.
pub(crate) fn note(
    tx: &Connection,
    log: &ChangeLog,
    audience: &[i64],
    resource_id: i64,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<()> {
    let old = before.filter(|content| recurs(content));
    if old.is_none() && !after.is_some_and(recurs) {
        return Ok(());
    }
    let time = now();
    if old.is_some_and(|content| content.len() > MAX_VERSION_BYTES) {
        for account in audience {
            if let Some(modseq) = log.modseq_of(*account) {
                raise_floor(tx, *account, modseq)?;
            }
        }
        return Ok(());
    }
    let content_id: Option<i64> = match old {
        Some(content) => {
            let hash = hex::encode(Sha256::digest(content.as_bytes()));
            tx.execute(
                "INSERT INTO calendar_event_contents (hash, content) VALUES (?1, ?2) ON CONFLICT (hash) DO NOTHING",
                params![hash, content],
            )?;
            Some(tx.query_row("SELECT id FROM calendar_event_contents WHERE hash = ?1", [hash], |row| row.get(0))?)
        }
        None => None,
    };
    for account in audience {
        let Some(modseq) = log.modseq_of(*account) else { continue };
        tx.execute(
            "INSERT OR REPLACE INTO calendar_event_versions (account_id, resource_id, modseq, content_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account, resource_id, modseq, content_id, time],
        )?;
        prune(tx, *account, time)?;
    }
    Ok(())
}

/// Keeps one account's versions within [`KEPT_SECS`] and [`MAX_VERSIONS`].
fn prune(tx: &Connection, account_id: i64, time: i64) -> Result<()> {
    let gone: Option<i64> = tx.query_row(
        "SELECT max(modseq) FROM calendar_event_versions WHERE account_id = ?1 AND (created_at < ?2 OR modseq <=
             (SELECT modseq FROM calendar_event_versions WHERE account_id = ?1 ORDER BY modseq DESC LIMIT 1 OFFSET ?3))",
        params![account_id, time - KEPT_SECS, MAX_VERSIONS],
        |row| row.get(0),
    )?;
    let Some(gone) = gone else { return Ok(()) };
    tx.execute(
        "DELETE FROM calendar_event_versions WHERE account_id = ?1 AND modseq <= ?2",
        params![account_id, gone],
    )?;
    raise_floor(tx, account_id, gone)?;
    tx.execute(
        "DELETE FROM calendar_event_contents WHERE NOT EXISTS
             (SELECT 1 FROM calendar_event_versions v WHERE v.content_id = calendar_event_contents.id)",
        [],
    )?;
    Ok(())
}

/// Versions of an account are complete only from `modseq` on.
fn raise_floor(tx: &Connection, account_id: i64, modseq: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO calendar_event_versions_since (account_id, modseq) VALUES (?1, ?2)
         ON CONFLICT (account_id) DO UPDATE SET modseq = max(modseq, excluded.modseq)",
        params![account_id, modseq],
    )?;
    Ok(())
}

impl Store {
    /// The state from which on an account's earlier versions of events are complete.
    pub async fn calendar_versions_since(&self, account_id: i64) -> Result<i64> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT modseq FROM calendar_event_versions_since WHERE account_id = ?1",
                    [account_id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0))
        })
        .await
    }

    /// What an event was at state `since`, as far as expanding it goes: `None` when it did not
    /// change in a way that matters since; `Some(None)` when it did not recur then (or was not
    /// there); `Some(Some(content))` for the recurring event it was.
    pub async fn calendar_event_version(
        &self,
        account_id: i64,
        resource_id: i64,
        since: i64,
    ) -> Result<Option<Option<String>>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT c.content FROM calendar_event_versions v
                     LEFT JOIN calendar_event_contents c ON c.id = v.content_id
                     WHERE v.account_id = ?1 AND v.resource_id = ?2 AND v.modseq > ?3
                     ORDER BY v.modseq LIMIT 1",
                    params![account_id, resource_id, since],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::store;
    use crate::{DavKind, DavPrecondition, DavWrite, NewAccount, NewDavCollection, Role};

    fn write(rule: &str) -> DavWrite {
        DavWrite {
            name: "a.ics".into(),
            content: format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261020T090000Z\r\n{rule}END:VEVENT\r\nEND:VCALENDAR\r\n"
            ),
            uid: "a".into(),
            component: "VEVENT".into(),
            starts_at: None,
            ends_at: None,
        }
    }

    #[tokio::test]
    async fn the_event_as_it_was_at_a_state() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = store
            .create_account(NewAccount {
                address: "mini@example.org".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let calendar =
            store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap()[0]
                .clone();
        assert_eq!(store.calendar_versions_since(mini).await.unwrap(), 0);
        let plain = store.account_modseq(mini).await.unwrap();
        store.dav_put(mini, calendar.id, write(""), DavPrecondition::default()).await.unwrap();
        let id = store.calendar_events(mini, None).await.unwrap()[0].id;
        let single = store.account_modseq(mini).await.unwrap();
        assert_eq!(store.calendar_event_version(mini, id, plain).await.unwrap(), None, "no series yet");
        store
            .dav_put(mini, calendar.id, write("RRULE:FREQ=DAILY;COUNT=3\r\n"), DavPrecondition::default())
            .await
            .unwrap();
        let series = store.account_modseq(mini).await.unwrap();
        store
            .dav_put(mini, calendar.id, write("RRULE:FREQ=DAILY;COUNT=2\r\n"), DavPrecondition::default())
            .await
            .unwrap();
        assert_eq!(store.calendar_event_version(mini, id, single).await.unwrap(), Some(None), "not a series then");
        let then = store.calendar_event_version(mini, id, series).await.unwrap().flatten().unwrap();
        assert!(then.contains("COUNT=3"), "{then}");
        assert!(store.dav_delete(mini, calendar.id, "a.ics", None).await.unwrap());
        let last = store.calendar_event_version(mini, id, store.account_modseq(mini).await.unwrap() - 1).await.unwrap();
        assert!(last.flatten().unwrap().contains("COUNT=2"));
    }
}
