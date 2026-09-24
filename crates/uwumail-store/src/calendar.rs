//! What JMAP Calendars needs on top of the CalDAV tables: events by id across an account's
//! calendars, UIDs that stay unique per account, moving events between calendars, and the default
//! calendar. Everything is written through the same helpers as CalDAV, so ETags, sync tokens and
//! the change log move together.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::dav::{
    ChangeLog, DavCollection, DavCollectionUpdate, DavKind, DavPrecondition, DavWrite, DavWriteOutcome,
    NewDavCollection, apply_collection_update, delete_collection, delete_entry, move_entry, new_entry_name,
    own_collection, put_entry, set_default,
};
use crate::{DAV_RESOURCE_MAX_BYTES, Result, Store, StoreError};

/// An event as JMAP sees it: a VEVENT entry of one of the account's calendars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEventRecord {
    pub id: i64,
    pub calendar_id: i64,
    pub name: String,
    pub uid: String,
    pub etag: String,
    pub content: String,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub modified_at: i64,
}

/// A new or changed event, already checked the way CalDAV checks what clients store.
#[derive(Debug, Clone)]
pub struct CalendarEventWrite {
    /// `None` for a new event.
    pub id: Option<i64>,
    pub calendar_id: i64,
    pub content: String,
    pub uid: String,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    /// The ETag the event had when it was read: a write over a change made meanwhile fails with
    /// [`StoreError::Conflict`], so the caller can read it again.
    pub if_etag: Option<String>,
}

const EVENT_COLUMNS: &str =
    "r.id, r.collection_id, r.name, r.uid, r.etag, r.content, r.starts_at, r.ends_at, r.modified_at";

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CalendarEventRecord> {
    Ok(CalendarEventRecord {
        id: row.get(0)?,
        calendar_id: row.get(1)?,
        name: row.get(2)?,
        uid: row.get(3)?,
        etag: row.get(4)?,
        content: row.get(5)?,
        starts_at: row.get(6)?,
        ends_at: row.get(7)?,
        modified_at: row.get(8)?,
    })
}

fn own_calendar(tx: &Transaction<'_>, account_id: i64, calendar_id: i64) -> Result<DavCollection> {
    let collection = own_collection(tx, account_id, calendar_id)?;
    if collection.kind != DavKind::Calendar {
        return Err(StoreError::NotFound(format!("calendar {calendar_id}")));
    }
    Ok(collection)
}

impl Store {
    /// Events of the account's calendars: the given ones, or all.
    pub async fn calendar_events(&self, account_id: i64, ids: Option<Vec<i64>>) -> Result<Vec<CalendarEventRecord>> {
        let ids_json = ids.map(|ids| serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()));
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                 WHERE c.account_id = ?1 AND c.kind = 'calendar' AND r.component = 'VEVENT'
                   AND (?2 IS NULL OR r.id IN (SELECT value FROM json_each(?2)))
                 ORDER BY r.id"
            ))?;
            let rows = stmt.query_map(params![account_id, ids_json], event_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Events that may overlap `[after, before)` (UTC seconds; `None` is open), in one calendar or
    /// all. A rough cut by the times CalDAV stored; the caller decides exactly.
    pub async fn calendar_events_between(
        &self,
        account_id: i64,
        calendar_id: Option<i64>,
        after: Option<i64>,
        before: Option<i64>,
    ) -> Result<Vec<CalendarEventRecord>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                 WHERE c.account_id = ?1 AND c.kind = 'calendar' AND r.component = 'VEVENT'
                   AND (?2 IS NULL OR r.collection_id = ?2)
                   AND (?4 IS NULL OR r.starts_at IS NULL OR r.starts_at < ?4)
                   AND (?3 IS NULL OR r.ends_at IS NULL OR r.ends_at >= ?3)
                 ORDER BY r.id"
            ))?;
            let rows = stmt.query_map(params![account_id, calendar_id, after, before], event_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Creates a calendar with its properties in one step.
    pub async fn create_calendar(
        &self,
        account_id: i64,
        new: NewDavCollection,
        update: DavCollectionUpdate,
    ) -> Result<DavCollection> {
        let (collection, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let id = crate::dav::insert_collection(tx, &mut log, account_id, DavKind::Calendar, &new)?;
                apply_collection_update(tx, id, &update)?;
                Ok((own_collection(tx, account_id, id)?, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(collection)
    }

    /// Makes a calendar the account's default one.
    pub async fn set_default_calendar(&self, account_id: i64, calendar_id: i64) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let calendar = own_calendar(tx, account_id, calendar_id)?;
                set_default(tx, &mut log, &calendar)?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }

    /// Deletes a calendar; one with entries only when `with_events`. The last calendar stays.
    pub async fn destroy_calendar(&self, account_id: i64, calendar_id: i64, with_events: bool) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let calendar = own_calendar(tx, account_id, calendar_id)?;
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM dav_collections WHERE account_id = ?1 AND kind = 'calendar'",
                    [account_id],
                    |row| row.get(0),
                )?;
                if count <= 1 {
                    return Err(StoreError::Rule {
                        code: "forbidden",
                        message: "the only calendar cannot be deleted".into(),
                    });
                }
                // Tasks count too: deleting the calendar would take them along unasked.
                if calendar.resources > 0 && !with_events {
                    return Err(StoreError::Rule {
                        code: "calendarHasEvent",
                        message: "the calendar still has entries".into(),
                    });
                }
                delete_collection(tx, &mut log, &calendar)?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }

    /// Creates or changes an event, possibly moving it into another calendar. UIDs stay unique
    /// across the account's calendars. Returns the event's id and new ETag.
    pub async fn put_calendar_event(&self, account_id: i64, write: CalendarEventWrite) -> Result<(i64, String)> {
        if write.content.len() > DAV_RESOURCE_MAX_BYTES {
            return Err(StoreError::QuotaExceeded);
        }
        let (result, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let target = own_calendar(tx, account_id, write.calendar_id)?;
                let other: Option<i64> = tx
                    .query_row(
                        "SELECT r.id FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                         WHERE c.account_id = ?1 AND c.kind = 'calendar' AND r.uid = ?2 AND r.id <> ?3",
                        params![account_id, write.uid, write.id.unwrap_or(-1)],
                        |row| row.get(0),
                    )
                    .optional()?;
                if other.is_some() {
                    return Err(StoreError::Rule {
                        code: "alreadyExists",
                        message: "another event has the same uid".into(),
                    });
                }
                let entry = |name: String| DavWrite {
                    name,
                    content: write.content.clone(),
                    uid: write.uid.clone(),
                    component: "VEVENT".into(),
                    starts_at: write.starts_at,
                    ends_at: write.ends_at,
                };
                let Some(id) = write.id else {
                    let name = new_entry_name(tx, target.id, &write.uid, "ics")?;
                    let condition = DavPrecondition { if_none_match_any: true, ..Default::default() };
                    return match put_entry(tx, &mut log, &target, &entry(name), &condition)? {
                        (DavWriteOutcome::Created { etag }, Some(id)) => Ok(((id, etag), log.modseq())),
                        other => Err(StoreError::Internal(format!("a new event could not be stored: {other:?}"))),
                    };
                };
                let (source_id, name, etag): (i64, String, String) = tx
                    .query_row(
                        "SELECT r.collection_id, r.name, r.etag FROM dav_resources r
                         JOIN dav_collections c ON c.id = r.collection_id
                         WHERE r.id = ?1 AND c.account_id = ?2 AND c.kind = 'calendar' AND r.component = 'VEVENT'",
                        params![id, account_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("event {id}")))?;
                if write.if_etag.as_ref().is_some_and(|wanted| *wanted != etag) {
                    return Err(StoreError::Conflict(format!("event {id} changed meanwhile")));
                }
                if source_id == target.id {
                    let condition = DavPrecondition { if_match: Some(etag), ..Default::default() };
                    return match put_entry(tx, &mut log, &target, &entry(name), &condition)? {
                        (DavWriteOutcome::Updated { etag }, _) => Ok(((id, etag), log.modseq())),
                        other => Err(StoreError::Internal(format!("an event could not be updated: {other:?}"))),
                    };
                }
                // Into another calendar: the same entry under the same id, gone from the old
                // calendar's point of view and new in the other one.
                let etag = move_entry(tx, &mut log, id, source_id, &target, &entry(name), "ics")?;
                Ok(((id, etag), log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(result)
    }

    /// Deletes an event, unless it changed since it had `if_etag`.
    pub async fn destroy_calendar_event(&self, account_id: i64, id: i64, if_etag: Option<String>) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let (calendar_id, name): (i64, String) = tx
                    .query_row(
                        "SELECT r.collection_id, r.name FROM dav_resources r
                         JOIN dav_collections c ON c.id = r.collection_id
                         WHERE r.id = ?1 AND c.account_id = ?2 AND c.kind = 'calendar' AND r.component = 'VEVENT'",
                        params![id, account_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("event {id}")))?;
                let calendar = own_calendar(tx, account_id, calendar_id)?;
                if !delete_entry(tx, &mut log, &calendar, &name, if_etag.as_deref())? {
                    return Err(StoreError::Conflict(format!("event {id} changed meanwhile")));
                }
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    async fn account(store: &Store, address: &str) -> i64 {
        store.create_domain("example.org").await.ok();
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id
    }

    fn event(uid: &str, calendar_id: i64, id: Option<i64>) -> CalendarEventWrite {
        CalendarEventWrite {
            id,
            calendar_id,
            content: format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            ),
            uid: uid.into(),
            starts_at: None,
            ends_at: None,
            if_etag: None,
        }
    }

    #[tokio::test]
    async fn caldav_writes_reach_the_change_log() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.org").await;
        let start = store.account_modseq(mini).await.unwrap();
        let calendar =
            store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap()[0]
                .clone();
        let mut changes = store.subscribe_changes();
        let put = |uid: &str, component: &str| DavWrite {
            name: format!("{uid}.ics"),
            content: format!(
                "BEGIN:VCALENDAR\r\nBEGIN:{component}\r\nUID:{uid}\r\nEND:{component}\r\nEND:VCALENDAR\r\n"
            ),
            uid: uid.into(),
            component: component.into(),
            starts_at: None,
            ends_at: None,
        };
        store.dav_put(mini, calendar.id, put("e", "VEVENT"), DavPrecondition::default()).await.unwrap();
        assert_eq!(changes.recv().await.unwrap().account_id, mini, "push hears about it");
        store.dav_put(mini, calendar.id, put("t", "VTODO"), DavPrecondition::default()).await.unwrap();
        let events = store.calendar_events(mini, None).await.unwrap();
        assert_eq!(events.len(), 1, "tasks are not events");
        let logged = store.changes(mini, "CalendarEvent", start, 0).await.unwrap();
        assert_eq!(logged.created, vec![events[0].id]);
        assert_eq!(store.changes(mini, "Calendar", start, 0).await.unwrap().created, vec![calendar.id]);

        let after = store.account_modseq(mini).await.unwrap();
        assert!(store.dav_delete(mini, calendar.id, "e.ics", None).await.unwrap());
        assert_eq!(store.changes(mini, "CalendarEvent", after, 0).await.unwrap().destroyed, vec![events[0].id]);
        store.dav_update_collection(mini, calendar.id, DavCollectionUpdate::default()).await.unwrap();
        assert_eq!(store.changes(mini, "Calendar", after, 0).await.unwrap().updated, vec![calendar.id]);
    }

    #[tokio::test]
    async fn events_move_between_calendars_and_leave_changes() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.org").await;
        let leni = account(&store, "leni@example.org").await;
        let calendars =
            store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap();
        let personal = &calendars[0];
        assert!(personal.is_default && personal.is_visible);
        let start = store.account_modseq(mini).await.unwrap();
        let work = store
            .create_calendar(
                mini,
                NewDavCollection { slug: "work".into(), display_name: "Arbeit".into(), ..Default::default() },
                DavCollectionUpdate { is_visible: Some(false), ..Default::default() },
            )
            .await
            .unwrap();
        assert!(!work.is_default && !work.is_visible);

        let (id, etag) = store.put_calendar_event(mini, event("a@example.net", personal.id, None)).await.unwrap();
        let taken = store.put_calendar_event(mini, event("a@example.net", work.id, None)).await;
        assert!(matches!(taken, Err(StoreError::Rule { code: "alreadyExists", .. })));
        assert!(matches!(
            store.put_calendar_event(leni, event("b@example.net", personal.id, None)).await,
            Err(StoreError::NotFound(_))
        ));

        let stale = CalendarEventWrite { if_etag: Some("\"old\"".into()), ..event("a@example.net", work.id, Some(id)) };
        assert!(matches!(store.put_calendar_event(mini, stale).await, Err(StoreError::Conflict(_))));
        let moved = CalendarEventWrite { if_etag: Some(etag), ..event("a@example.net", work.id, Some(id)) };
        assert_eq!(store.put_calendar_event(mini, moved).await.unwrap().0, id);
        let personal_changes = store.dav_changes(mini, personal.id, 0).await.unwrap();
        assert_eq!(personal_changes.deleted, vec!["a@example.net.ics"]);
        assert_eq!(store.dav_changes(mini, work.id, 0).await.unwrap().changed.len(), 1);
        assert_eq!(store.calendar_events(mini, Some(vec![id])).await.unwrap()[0].calendar_id, work.id);
        assert!(store.calendar_events(leni, Some(vec![id])).await.unwrap().is_empty());

        let changes = store.changes(mini, "CalendarEvent", start, 0).await.unwrap();
        assert_eq!(changes.created, vec![id]);
        assert_eq!(store.changes(mini, "Calendar", start, 0).await.unwrap().created, vec![work.id]);

        store.set_default_calendar(mini, work.id).await.unwrap();
        assert!(matches!(
            store.destroy_calendar(mini, work.id, false).await,
            Err(StoreError::Rule { code: "calendarHasEvent", .. })
        ));
        store.destroy_calendar(mini, work.id, true).await.unwrap();
        let left =
            store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].is_default, "the default moves on");
        assert!(matches!(
            store.destroy_calendar(mini, personal.id, true).await,
            Err(StoreError::Rule { code: "forbidden", .. })
        ));
        let changes = store.changes(mini, "CalendarEvent", start, 0).await.unwrap();
        assert!(changes.created.is_empty() && changes.destroyed.is_empty(), "created and gone again");
    }
}
