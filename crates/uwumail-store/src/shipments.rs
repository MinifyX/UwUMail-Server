//! Parcels on their way to a person: found in their mail when it arrives or by a look back through
//! earlier mail, added or followed by an app (docs/jmap-shipments.md). Kept per account, never
//! fetched from a carrier by the server.

use chrono::{DateTime, Datelike, Duration, NaiveDate};
use chrono_tz::Europe::Berlin;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uwumail_labels::{Carrier, Day, Pickup, ShipmentFacts, ShipmentStatus};

use crate::db::{next_modseq, record_change};
use crate::objects::{EMAIL_COLUMNS, EmailRecord, email_from_row};
use crate::{Result, Store, StoreError, now};

/// Delivered, returned and cancelled shipments go this long after their last status.
pub const SHIPMENT_DONE_SECS: i64 = 30 * 86_400;
/// Shipments that never arrived go this long after their last event.
pub const SHIPMENT_STALE_SECS: i64 = 60 * 86_400;
/// Events one shipment keeps; more are dropped.
pub const SHIPMENT_MAX_EVENTS: i64 = 200;
/// Shipments one account may have.
const MAX_PER_ACCOUNT: i64 = 2000;
/// A look back reaches this far at most, in days.
pub const SHIPMENT_RESCAN_MAX_DAYS: i64 = 90;
/// What a look back reaches when the person switches shipments on.
pub const SHIPMENT_RESCAN_DEFAULT_DAYS: i64 = 14;
const TEXT_MAX_CHARS: usize = 500;
const LOCATION_MAX_CHARS: usize = 200;
const TITLE_MAX_CHARS: usize = 200;
const NUMBER_MAX_CHARS: usize = 40;
const CLIENT_MAX_CHARS: usize = 100;
/// The preference that switches it (`"on"`, the default, or `"off"`).
const PREFERENCE: &str = "shipments";

/// Where a shipment waits to be picked up; `until` is a day like `2026-11-22`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShipmentPickup {
    pub kind: String,
    pub name: Option<String>,
    pub address: Option<String>,
    pub until: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipmentEvent {
    pub at: i64,
    pub status: ShipmentStatus,
    pub text: Option<String>,
    pub location: Option<String>,
    /// `mail`, `ai` or `client`.
    pub source: String,
    pub email_id: Option<i64>,
    pub client_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shipment {
    pub id: i64,
    /// A [`Carrier::id`].
    pub carrier: String,
    pub number: Option<String>,
    pub tracking_url: Option<String>,
    pub title: Option<String>,
    pub merchant: Option<String>,
    pub order_ref: Option<String>,
    pub status: ShipmentStatus,
    pub status_at: i64,
    /// The wall clock in Germany as the mail wrote it: `2026-11-15`, `10:15`, `12:15`.
    pub expected_day: Option<String>,
    pub expected_from: Option<String>,
    pub expected_to: Option<String>,
    pub delivered_at: Option<i64>,
    pub pickup: Option<ShipmentPickup>,
    pub hidden: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// Oldest first.
    pub events: Vec<ShipmentEvent>,
}

impl Shipment {
    /// The mails it was found in, oldest first, each once.
    pub fn email_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = Vec::new();
        for id in self.events.iter().filter_map(|event| event.email_id) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }
}

/// An event an app adds (`Shipment/set` `addEvents`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewShipmentEvent {
    pub at: i64,
    pub status: ShipmentStatus,
    pub text: Option<String>,
    pub location: Option<String>,
}

/// A change an app makes; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShipmentUpdate {
    pub title: Option<Option<String>>,
    pub hidden: Option<bool>,
    pub add_events: Vec<NewShipmentEvent>,
}

/// A look back through earlier mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipmentRescan {
    pub since: i64,
    pub started_at: i64,
    /// Shipments it found or moved on so far.
    pub found: i64,
    pub running: bool,
}

/// A look back waiting for the worker: where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShipmentRescanJob {
    pub account_id: i64,
    pub since: i64,
    pub after_id: i64,
}

fn limited(value: Option<&str>, max: usize, what: &str) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else { return Ok(None) };
    if value.chars().count() > max {
        return Err(StoreError::Invalid(format!("{what} may have at most {max} characters")));
    }
    if value.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!("{what} may not contain control characters")));
    }
    Ok(Some(value.to_owned()))
}

/// Cut to `max` characters, for what comes from mail and must not refuse the mail.
fn clipped(value: &str, max: usize) -> Option<String> {
    let value: String = value.chars().filter(|c| !c.is_control()).take(max).collect();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Whether shipments are switched on for an account (they are unless the person said no).
pub(crate) fn enabled(conn: &Connection, account_id: i64) -> Result<bool> {
    let raw: Option<String> =
        conn.query_row("SELECT preferences FROM accounts WHERE id = ?1", [account_id], |row| row.get(0)).optional()?;
    let preferences: Value = raw.and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default();
    Ok(preferences.get(PREFERENCE).and_then(Value::as_str) != Some("off"))
}

/// After the preferences changed: switched off deletes every shipment and the look back, switched
/// on again looks back over the last two weeks. Returns the last modseq it used, if any.
pub(crate) fn follow_setting(tx: &Transaction<'_>, account_id: i64, was_on: bool) -> Result<Option<i64>> {
    let on = enabled(tx, account_id)?;
    if on == was_on {
        return Ok(None);
    }
    if on {
        queue_rescan(tx, account_id, SHIPMENT_RESCAN_DEFAULT_DAYS)?;
        return Ok(None);
    }
    tx.execute("DELETE FROM shipment_rescans WHERE account_id = ?1", [account_id])?;
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM shipments WHERE account_id = ?1")?;
        stmt.query_map([account_id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let mut last = None;
    for id in ids {
        last = Some(destroy(tx, account_id, id)?);
    }
    Ok(last)
}

fn queue_rescan(conn: &Connection, account_id: i64, days: i64) -> Result<ShipmentRescan> {
    let now = now();
    let since = now - days.clamp(1, SHIPMENT_RESCAN_MAX_DAYS) * 86_400;
    conn.execute(
        "INSERT INTO shipment_rescans (account_id, since, after_id, found, started_at) VALUES (?1, ?2, 0, 0, ?3)
         ON CONFLICT (account_id) DO UPDATE SET since = excluded.since, after_id = 0, found = 0,
                                                started_at = excluded.started_at",
        params![account_id, since, now],
    )?;
    Ok(ShipmentRescan { since, started_at: now, found: 0, running: true })
}

/// Records a change of a shipment for the owner's JMAP state.
fn changed(conn: &Connection, account_id: i64, id: i64, change: &str) -> Result<i64> {
    let modseq = next_modseq(conn, account_id)?;
    let column = if change == "created" { "created_modseq = ?1, updated_modseq = ?1" } else { "updated_modseq = ?1" };
    conn.execute(&format!("UPDATE shipments SET {column} WHERE id = ?2"), params![modseq, id])?;
    record_change(conn, account_id, modseq, "Shipment", id, change)?;
    Ok(modseq)
}

fn destroy(conn: &Connection, account_id: i64, id: i64) -> Result<i64> {
    let modseq = next_modseq(conn, account_id)?;
    conn.execute("DELETE FROM shipments WHERE id = ?1", [id])?;
    record_change(conn, account_id, modseq, "Shipment", id, "destroyed")?;
    Ok(modseq)
}

/// The German day a mail meant, from the day it came.
// shortcut: every day and time is read as German time, upgrade with a time zone per account.
fn resolve(day: Day, received_at: i64) -> Option<NaiveDate> {
    let base = DateTime::from_timestamp(received_at, 0)?.with_timezone(&Berlin).date_naive();
    match day {
        Day::InDays(days) if days <= 60 => base.checked_add_signed(Duration::days(days.into())),
        Day::InDays(_) => None,
        Day::On { year: Some(year), month, day } => NaiveDate::from_ymd_opt(year, month, day),
        Day::On { year: None, month, day } => {
            let date = NaiveDate::from_ymd_opt(base.year(), month, day)?;
            // "am 3. Januar" in a mail from late December is next year.
            if date < base - Duration::days(60) {
                NaiveDate::from_ymd_opt(base.year() + 1, month, day)
            } else {
                Some(date)
            }
        }
    }
}

fn pickup_of(pickup: &Pickup, received_at: i64) -> ShipmentPickup {
    ShipmentPickup {
        kind: pickup.kind.id().to_owned(),
        name: pickup.name.as_deref().and_then(|name| clipped(name, TITLE_MAX_CHARS)),
        address: pickup.address.as_deref().and_then(|address| clipped(address, TITLE_MAX_CHARS)),
        until: pickup.until.and_then(|day| resolve(day, received_at)).map(|date| date.to_string()),
    }
}

fn status_of(value: &str) -> rusqlite::Result<ShipmentStatus> {
    ShipmentStatus::from_id(value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, format!("status {value}").into())
    })
}

const COLUMNS: &str = "id, carrier, number, tracking_url, title, merchant, order_ref, status, status_at, expected_day,
     expected_from, expected_to, delivered_at, pickup_json, hidden, created_at, updated_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Shipment> {
    let pickup: Option<String> = row.get(13)?;
    Ok(Shipment {
        id: row.get(0)?,
        carrier: row.get(1)?,
        number: row.get(2)?,
        tracking_url: row.get(3)?,
        title: row.get(4)?,
        merchant: row.get(5)?,
        order_ref: row.get(6)?,
        status: status_of(&row.get::<_, String>(7)?)?,
        status_at: row.get(8)?,
        expected_day: row.get(9)?,
        expected_from: row.get(10)?,
        expected_to: row.get(11)?,
        delivered_at: row.get(12)?,
        pickup: pickup.and_then(|raw| serde_json::from_str(&raw).ok()),
        hidden: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        events: Vec::new(),
    })
}

/// Adds an event and moves the status on if it may: only forward (see [`ShipmentStatus::moves_to`])
/// and never by an event older than the current status. The same event twice, or one past
/// [`SHIPMENT_MAX_EVENTS`], is dropped. Returns whether anything changed.
fn add_event(tx: &Connection, id: i64, event: &ShipmentEvent) -> Result<bool> {
    let (count, known): (i64, bool) = tx.query_row(
        "SELECT count(*),
                coalesce(sum(CASE WHEN (?2 IS NOT NULL AND email_id = ?2)
                                    OR (at = ?3 AND status = ?4 AND source = ?5) THEN 1 ELSE 0 END), 0) > 0
         FROM shipment_events WHERE shipment_id = ?1",
        params![id, event.email_id, event.at, event.status.id(), event.source],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if known || count >= SHIPMENT_MAX_EVENTS {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO shipment_events (shipment_id, at, status, text, location, source, email_id, client_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            event.at,
            event.status.id(),
            event.text,
            event.location,
            event.source,
            event.email_id,
            event.client_name
        ],
    )?;
    let (status, status_at): (String, i64) =
        tx.query_row("SELECT status, status_at FROM shipments WHERE id = ?1", [id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let current = status_of(&status)?;
    if event.at >= status_at && current.moves_to(event.status) {
        let delivered = (event.status == ShipmentStatus::Delivered).then_some(event.at);
        tx.execute(
            "UPDATE shipments SET status = ?2, status_at = ?3, delivered_at = coalesce(?4, delivered_at) WHERE id = ?1",
            params![id, event.status.id(), event.at, delivered],
        )?;
    }
    tx.execute("UPDATE shipments SET updated_at = ?2 WHERE id = ?1", params![id, now()])?;
    Ok(true)
}

fn count(tx: &Connection, account_id: i64) -> Result<i64> {
    Ok(tx.query_row("SELECT count(*) FROM shipments WHERE account_id = ?1", [account_id], |row| row.get(0))?)
}

#[allow(clippy::too_many_arguments)]
fn insert(
    tx: &Connection,
    account_id: i64,
    carrier: &str,
    number: Option<&str>,
    tracking_url: Option<&str>,
    merchant: Option<&str>,
    order_ref: Option<&str>,
    status: ShipmentStatus,
    at: i64,
    title: Option<&str>,
) -> Result<i64> {
    let now = now();
    tx.execute(
        "INSERT INTO shipments (account_id, carrier, number, tracking_url, merchant, order_ref, status, status_at,
                                title, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![account_id, carrier, number, tracking_url, merchant, order_ref, status.id(), at, title, now],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Puts what one mail tells into the account's shipments. Returns the last modseq it used, if any.
fn record_mail(
    tx: &Transaction<'_>,
    account_id: i64,
    email_id: i64,
    received_at: i64,
    subject: &str,
    facts: &ShipmentFacts,
) -> Result<Option<i64>> {
    if !enabled(tx, account_id)? {
        return Ok(None);
    }
    let carrier = facts.carrier.as_ref().map(Carrier::id);
    let merchant = facts.merchant.as_deref().and_then(|merchant| clipped(merchant, TITLE_MAX_CHARS));
    let order_ref = facts.order_ref.as_deref().and_then(|order| clipped(order, NUMBER_MAX_CHARS));
    let numbers: Vec<&str> =
        facts.numbers.iter().map(String::as_str).filter(|number| number.chars().count() <= NUMBER_MAX_CHARS).collect();
    if numbers.is_empty() && order_ref.is_none() {
        return Ok(None);
    }
    let status =
        facts.status.unwrap_or(if numbers.is_empty() { ShipmentStatus::Ordered } else { ShipmentStatus::Shipped });
    let event = ShipmentEvent {
        at: received_at,
        status,
        text: clipped(subject, TEXT_MAX_CHARS),
        location: None,
        source: "mail".into(),
        email_id: Some(email_id),
        client_name: None,
    };
    let order_of = |tx: &Connection, number_is_null: bool| -> Result<Vec<i64>> {
        let Some(order_ref) = &order_ref else { return Ok(Vec::new()) };
        let mut stmt = tx.prepare_cached(
            "SELECT id FROM shipments WHERE account_id = ?1 AND order_ref = ?2
               AND (?3 IS NULL OR merchant IS NULL OR merchant = ?3) AND (?4 = 0 OR number IS NULL)
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![account_id, order_ref, merchant, number_is_null], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    };

    // Which shipments the mail is about, and whether each is new.
    let mut targets: Vec<(i64, bool)> = Vec::new();
    if numbers.is_empty() {
        targets.extend(order_of(tx, false)?.into_iter().map(|id| (id, false)));
    }
    for number in &numbers {
        // Rule 1: the same number of the same carrier, or of any carrier when the mail names none.
        let known: Option<i64> = tx
            .query_row(
                "SELECT id FROM shipments WHERE account_id = ?1 AND number = ?2 AND (?3 IS NULL OR carrier = ?3)
                 ORDER BY id LIMIT 1",
                params![account_id, number, carrier],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = known {
            targets.push((id, false));
            continue;
        }
        // Rules 2 and 3: the order confirmation becomes the shipment once its number comes.
        if let Some(id) = order_of(tx, true)?.into_iter().next() {
            tx.execute(
                "UPDATE shipments SET number = ?2, carrier = coalesce(?3, carrier) WHERE id = ?1",
                params![id, number, carrier],
            )?;
            targets.push((id, false));
            continue;
        }
        if count(tx, account_id)? >= MAX_PER_ACCOUNT {
            break;
        }
        let tracking = carrier_tracking(facts, number, numbers.len());
        let id = insert(
            tx,
            account_id,
            carrier.unwrap_or("other"),
            Some(number),
            tracking.as_deref(),
            merchant.as_deref(),
            order_ref.as_deref(),
            status,
            received_at,
            None,
        )?;
        targets.push((id, true));
    }
    if numbers.is_empty() && targets.is_empty() && count(tx, account_id)? < MAX_PER_ACCOUNT {
        let id = insert(
            tx,
            account_id,
            carrier.unwrap_or("other"),
            None,
            None,
            merchant.as_deref(),
            order_ref.as_deref(),
            status,
            received_at,
            None,
        )?;
        targets.push((id, true));
    }

    let expected = facts.expected.and_then(|window| {
        let day = resolve(window.day, received_at)?;
        let time = |time: Option<(u32, u32)>| time.map(|(hour, minute)| format!("{hour:02}:{minute:02}"));
        Some((day.to_string(), time(window.from), time(window.to)))
    });
    let pickup = facts.pickup.as_ref().map(|pickup| pickup_of(pickup, received_at));
    let mut last = None;
    for (id, created) in targets {
        let added = add_event(tx, id, &event)?;
        if !added && !created {
            continue;
        }
        let number: Option<String> =
            tx.query_row("SELECT number FROM shipments WHERE id = ?1", [id], |row| row.get(0))?;
        let tracking = number.as_deref().and_then(|number| carrier_tracking(facts, number, numbers.len()));
        // What the newest mail says wins; an older one only fills in what is missing.
        let newest: bool =
            tx.query_row("SELECT status_at <= ?2 FROM shipments WHERE id = ?1", params![id, received_at], |row| {
                row.get(0)
            })?;
        let (day, from, to) = match &expected {
            Some((day, from, to)) => (Some(day.clone()), from.clone(), to.clone()),
            None => (None, None, None),
        };
        let pickup_json = pickup.as_ref().map(|pickup| serde_json::to_string(pickup).unwrap_or_default());
        tx.execute(
            "UPDATE shipments SET
                carrier = CASE WHEN carrier = 'other' AND ?2 IS NOT NULL THEN ?2 ELSE carrier END,
                tracking_url = coalesce(tracking_url, ?3),
                merchant = coalesce(merchant, ?4),
                order_ref = coalesce(order_ref, ?5),
                expected_day = CASE WHEN ?6 IS NOT NULL AND (?9 OR expected_day IS NULL) THEN ?6 ELSE expected_day END,
                expected_from = CASE WHEN ?6 IS NOT NULL AND (?9 OR expected_day IS NULL) THEN ?7 ELSE expected_from END,
                expected_to = CASE WHEN ?6 IS NOT NULL AND (?9 OR expected_day IS NULL) THEN ?8 ELSE expected_to END,
                pickup_json = CASE WHEN ?10 IS NOT NULL AND (?9 OR pickup_json IS NULL) THEN ?10 ELSE pickup_json END
             WHERE id = ?1",
            params![id, carrier, tracking, merchant, order_ref, day, from, to, newest, pickup_json],
        )?;
        last = Some(changed(tx, account_id, id, if created { "created" } else { "updated" })?);
    }
    Ok(last)
}

/// The link to follow a shipment: the one in the mail when it names one shipment, else the carrier's.
fn carrier_tracking(facts: &ShipmentFacts, number: &str, numbers: usize) -> Option<String> {
    let own = facts.carrier.as_ref().and_then(|carrier| carrier.tracking_url(number));
    if numbers > 1 { own } else { facts.tracking_url.clone().or(own) }
}

fn load(conn: &Connection, account_id: i64, ids: Option<&[i64]>) -> Result<Vec<Shipment>> {
    let ids_json = ids.map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".into()));
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM shipments
         WHERE account_id = ?1 AND (?2 IS NULL OR id IN (SELECT value FROM json_each(?2)))
         ORDER BY id"
    ))?;
    let mut shipments: Vec<Shipment> =
        stmt.query_map(params![account_id, ids_json], from_row)?.collect::<rusqlite::Result<_>>()?;
    let mut events = conn.prepare_cached(
        "SELECT at, status, text, location, source, email_id, client_name FROM shipment_events
         WHERE shipment_id = ?1 ORDER BY at, id",
    )?;
    for shipment in &mut shipments {
        shipment.events = events
            .query_map([shipment.id], |row| {
                Ok(ShipmentEvent {
                    at: row.get(0)?,
                    status: status_of(&row.get::<_, String>(1)?)?,
                    text: row.get(2)?,
                    location: row.get(3)?,
                    source: row.get(4)?,
                    email_id: row.get(5)?,
                    client_name: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(shipments)
}

fn rescan_of(conn: &Connection, account_id: i64) -> Result<Option<ShipmentRescan>> {
    Ok(conn
        .query_row("SELECT since, started_at, found FROM shipment_rescans WHERE account_id = ?1", [account_id], |row| {
            Ok(ShipmentRescan { since: row.get(0)?, started_at: row.get(1)?, found: row.get(2)?, running: true })
        })
        .optional()?)
}

impl Store {
    /// Whether shipments are switched on for an account.
    pub async fn shipments_enabled(&self, account_id: i64) -> Result<bool> {
        self.read(move |conn| enabled(conn, account_id)).await
    }

    /// An account's shipments with their events, oldest first; `ids` narrows the list.
    pub async fn shipments(&self, account_id: i64, ids: Option<Vec<i64>>) -> Result<Vec<Shipment>> {
        self.read(move |conn| load(conn, account_id, ids.as_deref())).await
    }

    /// Puts what a mail tells about a shipment into the account's shipments, unless the person
    /// switched them off. `subject` becomes the event's text.
    pub async fn record_shipment_mail(
        &self,
        account_id: i64,
        email_id: i64,
        received_at: i64,
        subject: String,
        facts: ShipmentFacts,
    ) -> Result<bool> {
        let modseq = self.write(move |tx| record_mail(tx, account_id, email_id, received_at, &subject, &facts)).await?;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        Ok(modseq.is_some())
    }

    /// A shipment an app adds by hand from its carrier and number.
    pub async fn create_shipment(
        &self,
        account_id: i64,
        carrier: Carrier,
        number: String,
        title: Option<String>,
        client_name: Option<String>,
    ) -> Result<i64> {
        let number: String = number.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
        if number.is_empty()
            || number.chars().count() > NUMBER_MAX_CHARS
            || !number.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(StoreError::Invalid("a number has up to 40 letters, digits and -".into()));
        }
        let title = limited(title.as_deref(), TITLE_MAX_CHARS, "a title")?;
        let client_name = limited(client_name.as_deref(), CLIENT_MAX_CHARS, "the app")?;
        let (id, modseq) = self
            .write(move |tx| {
                if !enabled(tx, account_id)? {
                    return Err(StoreError::Rule {
                        code: "shipmentsOff",
                        message: "shipments are switched off for this account".into(),
                    });
                }
                let known: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM shipments WHERE account_id = ?1 AND carrier = ?2 AND number = ?3",
                        params![account_id, carrier.id(), number],
                        |row| row.get(0),
                    )
                    .optional()?;
                if known.is_some() {
                    return Err(StoreError::Rule {
                        code: "alreadyExists",
                        message: "this shipment is already there".into(),
                    });
                }
                if count(tx, account_id)? >= MAX_PER_ACCOUNT {
                    return Err(StoreError::Rule {
                        code: "overQuota",
                        message: format!("at most {MAX_PER_ACCOUNT} shipments"),
                    });
                }
                let at = now();
                let tracking = carrier.tracking_url(&number);
                let id = insert(
                    tx,
                    account_id,
                    carrier.id(),
                    Some(&number),
                    tracking.as_deref(),
                    None,
                    None,
                    ShipmentStatus::Shipped,
                    at,
                    title.as_deref(),
                )?;
                let event = ShipmentEvent {
                    at,
                    status: ShipmentStatus::Shipped,
                    text: None,
                    location: None,
                    source: "client".into(),
                    email_id: None,
                    client_name,
                };
                add_event(tx, id, &event)?;
                Ok((id, changed(tx, account_id, id, "created")?))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(id)
    }

    /// Changes the title or hidden flag of a shipment and adds the events an app reports.
    pub async fn update_shipment(
        &self,
        account_id: i64,
        id: i64,
        update: ShipmentUpdate,
        client_name: Option<String>,
    ) -> Result<()> {
        if update.add_events.len() > SHIPMENT_MAX_EVENTS as usize {
            return Err(StoreError::Invalid(format!("at most {SHIPMENT_MAX_EVENTS} events at once")));
        }
        let title = match &update.title {
            Some(title) => Some(limited(title.as_deref(), TITLE_MAX_CHARS, "a title")?),
            None => None,
        };
        let client_name = limited(client_name.as_deref(), CLIENT_MAX_CHARS, "the app")?;
        let mut events = Vec::with_capacity(update.add_events.len());
        for event in &update.add_events {
            events.push(ShipmentEvent {
                at: event.at,
                status: event.status,
                text: limited(event.text.as_deref(), TEXT_MAX_CHARS, "an event's text")?,
                location: limited(event.location.as_deref(), LOCATION_MAX_CHARS, "a location")?,
                source: "client".into(),
                email_id: None,
                client_name: client_name.clone(),
            });
        }
        let hidden = update.hidden;
        let modseq = self
            .write(move |tx| {
                let found: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM shipments WHERE id = ?1 AND account_id = ?2)",
                    params![id, account_id],
                    |row| row.get(0),
                )?;
                if !found {
                    return Err(StoreError::NotFound(format!("shipment {id}")));
                }
                if let Some(title) = &title {
                    tx.execute("UPDATE shipments SET title = ?2 WHERE id = ?1", params![id, title])?;
                }
                if let Some(hidden) = hidden {
                    tx.execute("UPDATE shipments SET hidden = ?2 WHERE id = ?1", params![id, hidden])?;
                }
                // Oldest first, so the status moves on in order.
                let mut events = events;
                events.sort_by_key(|event| event.at);
                for event in &events {
                    add_event(tx, id, event)?;
                }
                tx.execute("UPDATE shipments SET updated_at = ?2 WHERE id = ?1", params![id, now()])?;
                changed(tx, account_id, id, "updated")
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    pub async fn destroy_shipment(&self, account_id: i64, id: i64) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let found: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM shipments WHERE id = ?1 AND account_id = ?2)",
                    params![id, account_id],
                    |row| row.get(0),
                )?;
                if !found {
                    return Err(StoreError::NotFound(format!("shipment {id}")));
                }
                destroy(tx, account_id, id)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    /// Deletes delivered, returned and cancelled shipments after [`SHIPMENT_DONE_SECS`] and the
    /// others after [`SHIPMENT_STALE_SECS`] without news. Returns how many went.
    pub async fn prune_shipments(&self) -> Result<usize> {
        let now = now();
        let gone = self
            .write(move |tx| {
                let stale: Vec<(i64, i64)> = {
                    let mut stmt = tx.prepare(
                        "SELECT s.id, s.account_id FROM shipments s
                         WHERE (s.status IN ('delivered', 'returned', 'cancelled') AND s.status_at < ?1)
                            OR coalesce((SELECT max(e.at) FROM shipment_events e WHERE e.shipment_id = s.id),
                                        s.updated_at) < ?2",
                    )?;
                    let rows = stmt.query_map(params![now - SHIPMENT_DONE_SECS, now - SHIPMENT_STALE_SECS], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                let mut gone = Vec::with_capacity(stale.len());
                for (id, account_id) in stale {
                    gone.push((account_id, destroy(tx, account_id, id)?));
                }
                Ok(gone)
            })
            .await?;
        for (account_id, modseq) in &gone {
            self.notify_change(*account_id, *modseq);
        }
        Ok(gone.len())
    }

    /// Starts a look back over the last `days` (1 to [`SHIPMENT_RESCAN_MAX_DAYS`]), from the start
    /// again if one is running.
    pub async fn start_shipment_rescan(&self, account_id: i64, days: i64) -> Result<ShipmentRescan> {
        self.write(move |tx| {
            if !enabled(tx, account_id)? {
                return Err(StoreError::Rule {
                    code: "shipmentsOff",
                    message: "shipments are switched off for this account".into(),
                });
            }
            queue_rescan(tx, account_id, days)
        })
        .await
    }

    /// The look back of an account that is still running, if any.
    pub async fn shipment_rescan(&self, account_id: i64) -> Result<Option<ShipmentRescan>> {
        self.read(move |conn| rescan_of(conn, account_id)).await
    }

    /// Every look back waiting for the worker.
    pub async fn shipment_rescan_jobs(&self) -> Result<Vec<ShipmentRescanJob>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare_cached("SELECT account_id, since, after_id FROM shipment_rescans ORDER BY started_at")?;
            let rows = stmt.query_map([], |row| {
                Ok(ShipmentRescanJob { account_id: row.get(0)?, since: row.get(1)?, after_id: row.get(2)? })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Up to `limit` mails of a look back after `after_id`, oldest first, in neither Junk nor Trash.
    pub async fn shipment_rescan_candidates(
        &self,
        account_id: i64,
        since: i64,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<EmailRecord>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {EMAIL_COLUMNS} FROM emails e
                 WHERE e.account_id = ?1 AND e.received_at >= ?2 AND e.id > ?3
                   AND NOT EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                                   WHERE em.email_id = e.id AND m.role IN ('junk', 'trash'))
                 ORDER BY e.id LIMIT ?4"
            ))?;
            let rows = stmt.query_map(params![account_id, since, after_id, limit], email_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Notes how far a look back got; `done` ends it.
    pub async fn shipment_rescan_progress(&self, account_id: i64, after_id: i64, found: i64, done: bool) -> Result<()> {
        self.write(move |tx| {
            if done {
                tx.execute("DELETE FROM shipment_rescans WHERE account_id = ?1", [account_id])?;
            } else {
                tx.execute(
                    "UPDATE shipment_rescans SET after_id = max(after_id, ?2), found = found + ?3 WHERE account_id = ?1",
                    params![account_id, after_id, found],
                )?;
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_read_in_german_time() {
        // 2026-11-14 23:30 UTC is already the 15th in Germany.
        let at = 1_794_699_000;
        assert_eq!(resolve(Day::InDays(0), at), NaiveDate::from_ymd_opt(2026, 11, 15));
        assert_eq!(resolve(Day::InDays(1), at), NaiveDate::from_ymd_opt(2026, 11, 16));
        assert_eq!(resolve(Day::InDays(61), at), None);
        assert_eq!(resolve(Day::On { year: None, month: 1, day: 3 }, at), NaiveDate::from_ymd_opt(2027, 1, 3));
        assert_eq!(resolve(Day::On { year: None, month: 11, day: 2 }, at), NaiveDate::from_ymd_opt(2026, 11, 2));
        assert_eq!(resolve(Day::On { year: None, month: 2, day: 30 }, at), None);
    }

    use crate::test_support::store;
    use crate::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role};
    use serde_json::{Map, json};
    use uwumail_labels::DateWindow;

    async fn setup() -> (Store, tempfile::TempDir, i64) {
        let (store, dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let account = store
            .create_account(NewAccount {
                address: "mini@example.org".into(),
                display_name: "Mini".into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        (store, dir, account)
    }

    async fn mail(store: &Store, account: i64) -> i64 {
        let raw = "From: shop@shop.example\r\nTo: mini@example.org\r\nSubject: Paket\r\n\r\nUnterwegs\r\n";
        let request = IngestRequest {
            account_id: account,
            raw: raw.as_bytes().to_vec(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        };
        store.ingest(request).await.unwrap().id
    }

    fn facts(numbers: &[&str], status: Option<ShipmentStatus>, order: Option<&str>) -> ShipmentFacts {
        ShipmentFacts {
            carrier: (!numbers.is_empty()).then_some(Carrier::Dhl),
            numbers: numbers.iter().map(|number| number.to_string()).collect(),
            tracking_url: None,
            status,
            expected: None,
            merchant: Some("Beispielshop".into()),
            order_ref: order.map(str::to_owned),
            pickup: None,
            confidence: 0.9,
        }
    }

    #[tokio::test]
    async fn mails_make_one_shipment_that_only_moves_forward() {
        let (store, _dir, account) = setup().await;
        let t = 1_794_699_000;
        // The order confirmation, then the shipping mail with two numbers.
        let order = mail(&store, account).await;
        assert!(
            store
                .record_shipment_mail(account, order, t, "Bestellung".into(), facts(&[], None, Some("A-1")))
                .await
                .unwrap()
        );
        let shipped = mail(&store, account).await;
        let two = facts(&["00340434000000000001", "00340434000000000002"], None, Some("A-1"));
        store.record_shipment_mail(account, shipped, t + 60, "Versandt".into(), two.clone()).await.unwrap();
        let all = store.shipments(account, None).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, all.iter().map(|s| s.id).min().unwrap());
        assert_eq!(all[0].email_ids(), vec![order, shipped]);
        assert_eq!(all[0].carrier, "dhl");
        assert!(all.iter().all(|s| s.status == ShipmentStatus::Shipped && s.order_ref.as_deref() == Some("A-1")));
        assert!(all[0].tracking_url.as_deref().unwrap().contains("00340434000000000001"));

        // The same mail again changes nothing.
        assert!(!store.record_shipment_mail(account, shipped, t + 60, "Versandt".into(), two).await.unwrap());

        // Out for delivery today between 10:15 and 12:15, then an older "in transit" mail.
        let today = mail(&store, account).await;
        let mut out = facts(&["00340434000000000001"], Some(ShipmentStatus::OutForDelivery), None);
        out.expected = Some(DateWindow { day: Day::InDays(0), from: Some((10, 15)), to: Some((12, 15)) });
        store.record_shipment_mail(account, today, t + 3600, "Heute".into(), out).await.unwrap();
        let late = mail(&store, account).await;
        let transit = facts(&["00340434000000000001"], Some(ShipmentStatus::InTransit), None);
        store.record_shipment_mail(account, late, t + 1800, "Unterwegs".into(), transit).await.unwrap();
        let first = &store.shipments(account, Some(vec![all[0].id])).await.unwrap()[0];
        assert_eq!(first.status, ShipmentStatus::OutForDelivery);
        assert_eq!(first.events.len(), 4);
        assert_eq!(first.expected_day.as_deref(), Some("2026-11-15"));
        assert_eq!(first.expected_from.as_deref(), Some("10:15"));
        assert_eq!(first.expected_to.as_deref(), Some("12:15"));

        // An app reports the delivery; its text is limited and too many events are refused.
        let delivered = NewShipmentEvent {
            at: t + 7200,
            status: ShipmentStatus::Delivered,
            text: Some("Zugestellt".into()),
            location: Some("Musterstadt".into()),
        };
        let update = ShipmentUpdate { add_events: vec![delivered.clone()], ..Default::default() };
        store.update_shipment(account, first.id, update, Some("Home Assistant".into())).await.unwrap();
        let first = &store.shipments(account, Some(vec![first.id])).await.unwrap()[0];
        assert_eq!(first.status, ShipmentStatus::Delivered);
        assert_eq!(first.delivered_at, Some(t + 7200));
        assert_eq!(first.events.last().unwrap().client_name.as_deref(), Some("Home Assistant"));
        let long = NewShipmentEvent { text: Some("x".repeat(501)), ..delivered.clone() };
        let update = ShipmentUpdate { add_events: vec![long], ..Default::default() };
        assert!(store.update_shipment(account, first.id, update, None).await.is_err());
        let many = ShipmentUpdate { add_events: vec![delivered; 201], ..Default::default() };
        assert!(store.update_shipment(account, first.id, many, None).await.is_err());
    }

    #[tokio::test]
    async fn switching_off_deletes_and_on_looks_back() {
        let (store, _dir, account) = setup().await;
        // The migration queued none: the account came after it.
        store.shipment_rescan_progress(account, 0, 0, true).await.unwrap();
        let id =
            store.create_shipment(account, Carrier::Ups, "1z 999 aa1 0123456784".into(), None, None).await.unwrap();
        assert_eq!(store.shipments(account, None).await.unwrap()[0].number.as_deref(), Some("1Z999AA10123456784"));
        assert!(store.create_shipment(account, Carrier::Ups, "1Z999AA10123456784".into(), None, None).await.is_err());

        let mut off = Map::new();
        off.insert("shipments".into(), json!("off"));
        store.update_preferences(account, off).await.unwrap();
        assert!(store.shipments(account, None).await.unwrap().is_empty());
        assert!(store.shipment_rescan(account).await.unwrap().is_none());
        assert!(store.destroy_shipment(account, id).await.is_err());
        let email = mail(&store, account).await;
        let found = facts(&["00340434000000000001"], None, None);
        assert!(!store.record_shipment_mail(account, email, now(), "Versandt".into(), found).await.unwrap());

        let mut on = Map::new();
        on.insert("shipments".into(), json!("on"));
        store.update_preferences(account, on).await.unwrap();
        let rescan = store.shipment_rescan(account).await.unwrap().unwrap();
        assert!(rescan.since <= now() - 13 * 86_400);
        let jobs = store.shipment_rescan_jobs().await.unwrap();
        assert_eq!(jobs.len(), 1);
        let candidates = store.shipment_rescan_candidates(account, jobs[0].since, 0, 10).await.unwrap();
        assert_eq!(candidates.iter().map(|record| record.id).collect::<Vec<_>>(), vec![email]);
    }

    #[tokio::test]
    async fn old_shipments_are_pruned() {
        let (store, _dir, account) = setup().await;
        let old = now() - SHIPMENT_DONE_SECS - 60;
        let email = mail(&store, account).await;
        let done = facts(&["00340434000000000001"], Some(ShipmentStatus::Delivered), None);
        store.record_shipment_mail(account, email, old, "Zugestellt".into(), done).await.unwrap();
        let fresh = facts(&["00340434000000000002"], None, None);
        store.record_shipment_mail(account, email, now(), "Versandt".into(), fresh).await.unwrap();
        assert_eq!(store.prune_shipments().await.unwrap(), 1);
        let left = store.shipments(account, None).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].number.as_deref(), Some("00340434000000000002"));
    }
}
