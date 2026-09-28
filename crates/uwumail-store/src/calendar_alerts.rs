//! Alerts the server fires itself (migration 0053): the next time each alert of an event goes off,
//! per account that sees the event, for the alert worker of the JMAP crate, which pushes a
//! CalendarAlert or sends a mail then (draft-ietf-jmap-calendars, section 6).
//!
//! Working out when an alert goes off needs the event as the account sees it (JSCalendar, its own
//! per-user alerts and default alerts), which this crate does not read; so writes only mark the
//! pairs of account and event whose alerts may have changed, and the worker plans them.

use rusqlite::{Connection, params};
use tokio::sync::broadcast;

use crate::{Result, Store};

/// An alert that went off, for push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarAlertFired {
    pub account_id: i64,
    pub event_id: i64,
    pub uid: String,
    pub recurrence_id: Option<String>,
    pub alert_id: String,
}

/// When one alert of an event goes off next for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAlert {
    pub alert_id: String,
    pub recurrence_id: Option<String>,
    pub fire_at: i64,
    /// `display` or `email`.
    pub action: String,
}

/// An alert that is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueAlert {
    pub account_id: i64,
    pub event_id: i64,
    pub alert: PlannedAlert,
}

/// Marks the alerts of an event to be worked out again for these accounts.
pub(crate) fn mark(conn: &Connection, accounts: &[i64], resource_id: i64) -> Result<()> {
    let mut stmt =
        conn.prepare_cached("INSERT OR IGNORE INTO calendar_alerts_dirty (account_id, resource_id) VALUES (?1, ?2)")?;
    for account in accounts {
        stmt.execute(params![account, resource_id])?;
    }
    Ok(())
}

/// Marks every event of a calendar for one account, as when its default alerts change.
pub(crate) fn mark_collection(conn: &Connection, account_id: i64, collection_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO calendar_alerts_dirty (account_id, resource_id)
         SELECT ?1, id FROM dav_resources WHERE collection_id = ?2 AND component = 'VEVENT'",
        params![account_id, collection_id],
    )?;
    Ok(())
}

impl Store {
    /// Follows the alerts that go off, for push.
    pub fn subscribe_calendar_alerts(&self) -> broadcast::Receiver<CalendarAlertFired> {
        self.inner.calendar_alerts.subscribe()
    }

    /// Tells push listeners about an alert that went off.
    pub fn announce_calendar_alert(&self, alert: CalendarAlertFired) {
        // Nobody listening is fine.
        let _ = self.inner.calendar_alerts.send(alert);
    }

    /// Pairs of account and event whose alerts are to be worked out again, at most `limit`.
    pub async fn calendar_alerts_to_plan(&self, limit: usize) -> Result<Vec<(i64, i64)>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare("SELECT account_id, resource_id FROM calendar_alerts_dirty LIMIT ?1")?;
            let rows = stmt.query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Replaces the planned alerts of an event for an account after `now`, and takes its mark.
    /// Those due by `now` stay: they go off in this turn of the worker.
    pub async fn plan_calendar_alerts(
        &self,
        account_id: i64,
        event_id: i64,
        now: i64,
        alerts: Vec<PlannedAlert>,
    ) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "DELETE FROM calendar_alerts WHERE account_id = ?1 AND resource_id = ?2 AND fire_at > ?3",
                params![account_id, event_id, now],
            )?;
            tx.execute(
                "DELETE FROM calendar_alerts_dirty WHERE account_id = ?1 AND resource_id = ?2",
                params![account_id, event_id],
            )?;
            let exists: bool =
                tx.query_row("SELECT EXISTS (SELECT 1 FROM dav_resources WHERE id = ?1)", [event_id], |row| row.get(0))?;
            if !exists {
                return Ok(());
            }
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO calendar_alerts (account_id, resource_id, alert_id, recurrence_id, fire_at, action)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for alert in alerts {
                stmt.execute(params![
                    account_id,
                    event_id,
                    alert.alert_id,
                    alert.recurrence_id,
                    alert.fire_at,
                    alert.action
                ])?;
            }
            Ok(())
        })
        .await
    }

    /// Alerts due at `now`, earliest first, at most `limit`.
    pub async fn due_calendar_alerts(&self, now: i64, limit: usize) -> Result<Vec<DueAlert>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT account_id, resource_id, alert_id, recurrence_id, fire_at, action FROM calendar_alerts
                 WHERE fire_at <= ?1 ORDER BY fire_at LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now, limit as i64], |row| {
                Ok(DueAlert {
                    account_id: row.get(0)?,
                    event_id: row.get(1)?,
                    alert: PlannedAlert {
                        alert_id: row.get(2)?,
                        recurrence_id: row.get(3)?,
                        fire_at: row.get(4)?,
                        action: row.get(5)?,
                    },
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Notes that an alert went off: it goes, and the event is planned again from now on.
    pub async fn calendar_alert_fired(&self, due: DueAlert) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "DELETE FROM calendar_alerts WHERE account_id = ?1 AND resource_id = ?2 AND alert_id = ?3 AND fire_at = ?4",
                params![due.account_id, due.event_id, due.alert.alert_id, due.alert.fire_at],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO calendar_alerts_dirty (account_id, resource_id)
                 SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM dav_resources WHERE id = ?2)",
                params![due.account_id, due.event_id],
            )?;
            Ok(())
        })
        .await
    }
}
