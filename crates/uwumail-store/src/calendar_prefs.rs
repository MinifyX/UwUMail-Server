//! What each person keeps for themselves about a calendar and its events (migration 0049): the
//! per-user properties of JMAP Calendars.
//!
//! Someone a calendar is shared with gives it their own name, colour, order, visibility and time
//! zone, and keeps their own alerts, keywords and colour on its events; none of it touches the
//! owner's data, so the owner's CalDAV clients see nothing of it. Whether a calendar makes its
//! person busy and the alerts events get by default are kept here for the owner too. A change is
//! logged for the one account it belongs to only.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use crate::dav::{ChangeLog, DavKind};
use crate::sharing::{VISIBLE, access};
use crate::{Result, Store, StoreError, now};

/// The most bytes one person's properties of one event may take, as JSON.
pub const CALENDAR_EVENT_PREFS_MAX_BYTES: usize = 256 * 1024;
/// The most bytes the default alerts of one calendar may take, as JSON.
pub const CALENDAR_DEFAULT_ALERTS_MAX_BYTES: usize = 64 * 1024;

/// One person's own settings of a calendar; `None` is "not set".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarPrefs {
    pub name: Option<String>,
    pub color: Option<String>,
    pub sort_order: Option<i64>,
    pub is_visible: Option<bool>,
    pub timezone: Option<String>,
    /// `all`, `attending` or `none`.
    pub include_in_availability: Option<String>,
    /// JSCalendar Alert maps as JSON.
    pub default_alerts_with_time: Option<String>,
    pub default_alerts_without_time: Option<String>,
}

/// A change of [`CalendarPrefs`]: `Some(None)` takes a setting back.
#[derive(Debug, Clone, Default)]
pub struct CalendarPrefsUpdate {
    pub name: Option<Option<String>>,
    pub color: Option<Option<String>>,
    pub sort_order: Option<Option<i64>>,
    pub is_visible: Option<Option<bool>>,
    pub timezone: Option<Option<String>>,
    pub include_in_availability: Option<Option<String>>,
    pub default_alerts_with_time: Option<Option<String>>,
    pub default_alerts_without_time: Option<Option<String>>,
}

impl CalendarPrefsUpdate {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.color.is_none()
            && self.sort_order.is_none()
            && self.is_visible.is_none()
            && self.timezone.is_none()
            && self.include_in_availability.is_none()
            && self.default_alerts_with_time.is_none()
            && self.default_alerts_without_time.is_none()
    }
}

impl crate::DavCollection {
    /// Puts what someone keeps for themselves over the owner's settings of a calendar shared with
    /// them.
    pub fn apply_prefs(&mut self, prefs: &CalendarPrefs) {
        if let Some(name) = &prefs.name {
            self.display_name = name.clone();
        }
        if let Some(color) = &prefs.color {
            self.color = Some(color.clone());
        }
        if let Some(timezone) = &prefs.timezone {
            self.timezone = Some(timezone.clone());
        }
        // Order and visibility are one's own from the start, not the owner's.
        self.sort_order = prefs.sort_order.unwrap_or(0);
        self.is_visible = prefs.is_visible.unwrap_or(true);
    }
}

/// One person's properties of one event, as JSON, and when they last changed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEventPrefs {
    pub data: String,
    pub updated_at: i64,
}

const PREFS_COLUMNS: &str = "name, color, sort_order, is_visible, timezone, include_in_availability, \
     default_alerts_with_time, default_alerts_without_time";

fn prefs_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<CalendarPrefs> {
    Ok(CalendarPrefs {
        name: row.get(offset)?,
        color: row.get(offset + 1)?,
        sort_order: row.get(offset + 2)?,
        is_visible: row.get(offset + 3)?,
        timezone: row.get(offset + 4)?,
        include_in_availability: row.get(offset + 5)?,
        default_alerts_with_time: row.get(offset + 6)?,
        default_alerts_without_time: row.get(offset + 7)?,
    })
}

impl Store {
    /// An account's own settings of the calendars it sees, by calendar id.
    pub async fn calendar_prefs(&self, account_id: i64) -> Result<HashMap<i64, CalendarPrefs>> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare(&format!("SELECT collection_id, {PREFS_COLUMNS} FROM calendar_prefs WHERE account_id = ?1"))?;
            let rows = stmt.query_map([account_id], |row| Ok((row.get(0)?, prefs_row(row, 1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Changes an account's own settings of a calendar it owns or that is shared with it. Only
    /// that account hears about it; the calendar's CalDAV state does not move.
    pub async fn set_calendar_prefs(
        &self,
        account_id: i64,
        collection_id: i64,
        update: CalendarPrefsUpdate,
    ) -> Result<()> {
        if update.is_empty() {
            return Ok(());
        }
        for alerts in [&update.default_alerts_with_time, &update.default_alerts_without_time] {
            if alerts.as_ref().and_then(Option::as_ref).is_some_and(|a| a.len() > CALENDAR_DEFAULT_ALERTS_MAX_BYTES) {
                return Err(StoreError::Invalid("the default alerts are too large".into()));
            }
        }
        let logged = self
            .write(move |tx| {
                let Some((collection, _)) = access(tx, account_id, collection_id)? else {
                    return Err(StoreError::NotFound(format!("calendar {collection_id}")));
                };
                if collection.kind != DavKind::Calendar {
                    return Err(StoreError::NotFound(format!("calendar {collection_id}")));
                }
                let mut current = tx
                    .query_row(
                        &format!(
                            "SELECT {PREFS_COLUMNS} FROM calendar_prefs WHERE collection_id = ?1 AND account_id = ?2"
                        ),
                        params![collection_id, account_id],
                        |row| prefs_row(row, 0),
                    )
                    .optional()?
                    .unwrap_or_default();
                let before = current.clone();
                macro_rules! apply {
                    ($($field:ident),*) => {$(
                        if let Some(value) = update.$field.clone() {
                            current.$field = value;
                        }
                    )*};
                }
                apply!(
                    name,
                    color,
                    sort_order,
                    is_visible,
                    timezone,
                    include_in_availability,
                    default_alerts_with_time,
                    default_alerts_without_time
                );
                if current == before {
                    return Ok(Default::default());
                }
                if current == CalendarPrefs::default() {
                    tx.execute(
                        "DELETE FROM calendar_prefs WHERE collection_id = ?1 AND account_id = ?2",
                        params![collection_id, account_id],
                    )?;
                } else {
                    tx.execute(
                        &format!(
                            "INSERT OR REPLACE INTO calendar_prefs (collection_id, account_id, {PREFS_COLUMNS})
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
                        ),
                        params![
                            collection_id,
                            account_id,
                            current.name,
                            current.color,
                            current.sort_order,
                            current.is_visible,
                            current.timezone,
                            current.include_in_availability,
                            current.default_alerts_with_time,
                            current.default_alerts_without_time
                        ],
                    )?;
                }
                if current.default_alerts_with_time != before.default_alerts_with_time
                    || current.default_alerts_without_time != before.default_alerts_without_time
                {
                    crate::calendar_alerts::mark_collection(tx, account_id, collection_id)?;
                }
                let mut log = ChangeLog::new(account_id);
                log.record(tx, "Calendar", collection_id, "updated")?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, logged);
        Ok(())
    }

    /// An account's own properties of events it sees, by event id.
    pub async fn calendar_event_prefs(
        &self,
        account_id: i64,
        ids: Vec<i64>,
    ) -> Result<HashMap<i64, CalendarEventPrefs>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into());
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT resource_id, data, updated_at FROM calendar_event_prefs
                 WHERE account_id = ?1 AND resource_id IN (SELECT value FROM json_each(?2))",
            )?;
            let rows = stmt.query_map(params![account_id, ids], |row| {
                Ok((row.get(0)?, CalendarEventPrefs { data: row.get(1)?, updated_at: row.get(2)? }))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Stores (or with `None` forgets) an account's own properties of an event in a calendar
    /// shared with it. Only that account hears about it.
    pub async fn set_calendar_event_prefs(&self, account_id: i64, id: i64, data: Option<String>) -> Result<()> {
        if data.as_ref().is_some_and(|data| data.len() > CALENDAR_EVENT_PREFS_MAX_BYTES) {
            return Err(StoreError::QuotaExceeded);
        }
        let logged = self
            .write(move |tx| {
                let owner: Option<i64> = tx
                    .query_row(
                        &format!(
                            "SELECT c.account_id FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                             WHERE r.id = ?2 AND {VISIBLE} AND c.kind = 'calendar' AND r.component = 'VEVENT'"
                        ),
                        params![account_id, id],
                        |row| row.get(0),
                    )
                    .optional()?;
                match owner {
                    None => return Err(StoreError::NotFound(format!("event {id}"))),
                    Some(owner) if owner == account_id => {
                        return Err(StoreError::Invalid("the owner's properties are part of the event".into()));
                    }
                    Some(_) => {}
                }
                match &data {
                    Some(data) => tx.execute(
                        "INSERT OR REPLACE INTO calendar_event_prefs (resource_id, account_id, data, updated_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![id, account_id, data, now()],
                    )?,
                    None => tx.execute(
                        "DELETE FROM calendar_event_prefs WHERE resource_id = ?1 AND account_id = ?2",
                        params![id, account_id],
                    )?,
                };
                crate::calendar_alerts::mark(tx, &[account_id], id)?;
                let mut log = ChangeLog::new(account_id);
                log.record(tx, "CalendarEvent", id, "updated")?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, logged);
        Ok(())
    }
}

/// Forgets what someone kept for themselves about a collection they no longer see.
pub(crate) fn forget(conn: &rusqlite::Connection, account_id: i64, collection_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM calendar_prefs WHERE collection_id = ?1 AND account_id = ?2",
        params![collection_id, account_id],
    )?;
    conn.execute(
        "DELETE FROM calendar_event_prefs WHERE account_id = ?1
             AND resource_id IN (SELECT id FROM dav_resources WHERE collection_id = ?2)",
        params![account_id, collection_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{DavPrecondition, DavWrite, NewAccount, NewDavCollection, Role, ShareRights};

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

    #[tokio::test]
    async fn own_settings_stay_with_their_person() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.org").await;
        let leni = account(&store, "leni@example.org").await;
        let calendar =
            store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap()[0]
                .clone();
        let write = DavWrite {
            name: "a.ics".into(),
            content: "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n".into(),
            uid: "a".into(),
            component: "VEVENT".into(),
            starts_at: None,
            ends_at: None,
        };
        store.dav_put(mini, calendar.id, write, DavPrecondition::default()).await.unwrap();
        let event = store.calendar_events(mini, None).await.unwrap()[0].id;
        let update = || CalendarPrefsUpdate { color: Some(Some("#00FF00FF".into())), ..Default::default() };
        assert!(matches!(store.set_calendar_prefs(leni, calendar.id, update()).await, Err(StoreError::NotFound(_))));
        store.dav_share(mini, calendar.id, "leni@example.org", ShareRights::Read).await.unwrap();

        let (mini_before, leni_before) =
            (store.account_modseq(mini).await.unwrap(), store.account_modseq(leni).await.unwrap());
        store.set_calendar_prefs(leni, calendar.id, update()).await.unwrap();
        store.set_calendar_event_prefs(leni, event, Some("{\"color\":\"red\"}".into())).await.unwrap();
        assert_eq!(store.calendar_prefs(leni).await.unwrap()[&calendar.id].color.as_deref(), Some("#00FF00FF"));
        assert!(store.calendar_prefs(mini).await.unwrap().is_empty());
        assert_eq!(store.calendar_event_prefs(leni, vec![event]).await.unwrap()[&event].data, "{\"color\":\"red\"}");
        assert_eq!(store.account_modseq(mini).await.unwrap(), mini_before, "the owner hears nothing");
        let changes = store.changes(leni, "CalendarEvent", leni_before, 0).await.unwrap();
        assert_eq!(changes.updated, vec![event]);
        assert!(store.set_calendar_event_prefs(mini, event, Some("{}".into())).await.is_err(), "not for the owner");

        // Leaving the calendar forgets them.
        store.dav_unshare(leni, calendar.id, leni).await.unwrap();
        assert!(store.calendar_prefs(leni).await.unwrap().is_empty());
        assert!(store.calendar_event_prefs(leni, vec![event]).await.unwrap().is_empty());
    }
}
