//! Default alerts of calendars (draft-ietf-jmap-calendars, section 6.1) and how CalDAV clients
//! get them.
//!
//! A calendar keeps, per person, the alerts events with and without a time get by default
//! (`defaultAlertsWithTime`, `defaultAlertsWithoutTime`), as JSCalendar Alert maps. CalDAV clients
//! see and set the same as `default-alarm-vevent-datetime` and `default-alarm-vevent-date`
//! (VALARMs, draft-daboo-valarm-extensions), as Apple's calendar does.
//!
//! An event with `useDefaultAlerts` carries the calendar's default alerts as its own VALARMs, so
//! phones ring them without knowing about defaults: they are written into the event whenever it is
//! stored over JMAP, and again into every such event of the calendar when the owner changes the
//! defaults. What an alert keeps for itself stays: the time it was acknowledged, and snoozes of it.

use calcard::icalendar::ICalendar;
use calcard::jscalendar::JSCalendar;
use serde_json::{Map, Value, json};
use uwumail_store::{Store, StoreError};

use crate::jscal;

/// The most default alerts one calendar has for events with or without a time.
pub const MAX_DEFAULT_ALERTS: usize = 20;
const MAX_ID_BYTES: usize = 64;
const MAX_VALARM_BYTES: usize = 64 * 1024;

/// Checks a `defaultAlertsWithTime` or `defaultAlertsWithoutTime` value: `null`, or a map of at
/// most [`MAX_DEFAULT_ALERTS`] alerts that trigger relative to the event.
pub fn check(value: &Value) -> Result<Option<Map<String, Value>>, String> {
    let alerts = match value {
        Value::Null => return Ok(None),
        Value::Object(alerts) => alerts,
        _ => return Err("must be a map of alerts or null".into()),
    };
    if alerts.len() > MAX_DEFAULT_ALERTS {
        return Err(format!("at most {MAX_DEFAULT_ALERTS} default alerts"));
    }
    for (id, alert) in alerts {
        if id.is_empty()
            || id.len() > MAX_ID_BYTES
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(format!("{id} is not a valid alert id"));
        }
        let Value::Object(alert) = alert else { return Err(format!("{id} must be an Alert")) };
        let trigger = alert.get("trigger").and_then(Value::as_object).ok_or(format!("{id} needs a trigger"))?;
        for (key, value) in alert {
            let ok = match key.as_str() {
                "@type" => value.as_str() == Some("Alert"),
                "action" => matches!(value.as_str(), Some("display" | "email")),
                "trigger" => true,
                // Vendor properties are kept as they are.
                other => other.contains(':'),
            };
            if !ok {
                return Err(format!("{key} is not valid in the default alert {id}"));
            }
        }
        for (key, value) in trigger {
            let ok = match key.as_str() {
                "@type" => value.as_str() == Some("OffsetTrigger"),
                "offset" => value.as_str().and_then(parse_offset).is_some(),
                "relativeTo" => matches!(value.as_str(), Some("start" | "end")),
                _ => false,
            };
            if !ok {
                return Err(format!("the trigger of {id} is not a valid OffsetTrigger"));
            }
        }
        if !trigger.contains_key("offset") {
            return Err(format!("the trigger of {id} needs an offset"));
        }
    }
    Ok(Some(alerts.clone()))
}

/// A SignedDuration: a Duration with an optional sign.
fn parse_offset(value: &str) -> Option<(i64, i64)> {
    jscal::parse_duration(value.strip_prefix(['-', '+']).unwrap_or(value))
}

/// Puts the default alerts into an event that uses them (`useDefaultAlerts: true`): the default
/// alerts by their ids, with the time an alert was acknowledged, and the snoozes of them. Other
/// alerts of such an event are ignored, as the draft says. Returns whether the alerts changed.
pub fn materialize(event: &mut Map<String, Value>, defaults: Option<&Map<String, Value>>) -> bool {
    if event.get("useDefaultAlerts") != Some(&Value::Bool(true)) {
        return false;
    }
    let current = event.get("alerts").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut alerts = Map::new();
    for (id, default) in defaults.into_iter().flatten() {
        let mut alert = default.clone();
        if let (Value::Object(alert), Some(acknowledged)) =
            (&mut alert, current.get(id).and_then(|a| a.get("acknowledged")))
        {
            alert.insert("acknowledged".into(), acknowledged.clone());
        }
        alerts.insert(id.clone(), alert);
    }
    for (id, alert) in &current {
        let snoozes_a_default = alert
            .get("relatedTo")
            .and_then(Value::as_object)
            .is_some_and(|related| related.keys().any(|parent| alerts.contains_key(parent)));
        if snoozes_a_default && !alerts.contains_key(id) {
            alerts.insert(id.clone(), alert.clone());
        }
    }
    let alerts = (!alerts.is_empty()).then_some(Value::Object(alerts));
    if event.get("alerts") == alerts.as_ref() {
        return false;
    }
    match alerts {
        Some(alerts) => event.insert("alerts".into(), alerts),
        None => event.remove("alerts"),
    };
    true
}

/// Which default alerts an event gets: those without a time for all-day events.
pub fn for_event<'a>(
    event: &Map<String, Value>,
    with_time: Option<&'a Map<String, Value>>,
    without_time: Option<&'a Map<String, Value>>,
) -> Option<&'a Map<String, Value>> {
    if event.get("showWithoutTime") == Some(&Value::Bool(true)) { without_time } else { with_time }
}

const WRAPPER_START: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//UwUMail//Server//EN\r\nBEGIN:VEVENT\r\n\
UID:default-alarms\r\nDTSTAMP:20000101T000000Z\r\nDTSTART:20000101T000000\r\n";
const WRAPPER_END: &str = "END:VEVENT\r\nEND:VCALENDAR\r\n";

/// Default alerts as the VALARMs of CalDAV's `default-alarm-vevent-datetime`.
pub fn to_valarms(alerts: &Map<String, Value>) -> String {
    let mut alerts = alerts.clone();
    for alert in alerts.values_mut().filter_map(Value::as_object_mut) {
        alert.entry("action").or_insert_with(|| json!("display"));
    }
    let group = json!({
        "@type": "Group",
        "entries": [{ "@type": "Event", "uid": "default-alarms", "start": "2000-01-01T00:00:00", "alerts": alerts }]
    });
    let Some(calendar) = JSCalendar::<String, String>::parse(&group.to_string()).ok().and_then(|c| c.into_icalendar())
    else {
        return String::new();
    };
    let text = calendar.to_string();
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("BEGIN:VALARM\r\n") {
        let Some(end) = rest[start..].find("END:VALARM\r\n") else { break };
        let end = start + end + "END:VALARM\r\n".len();
        out.push_str(&rest[start..end]);
        rest = &rest[end..];
    }
    out
}

/// The VALARMs a CalDAV client sets as default alarms, as default alerts. Alerts without an id of
/// their own get a new one, as ids have to be unique in the account.
pub fn from_valarms(text: &str) -> Result<Option<Map<String, Value>>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > MAX_VALARM_BYTES {
        return Err("the default alarms are too large".into());
    }
    let mut body = String::new();
    for line in text.lines() {
        let upper = line.trim_end().to_ascii_uppercase();
        // Only alarms: nothing may close the event they are read in.
        if (upper.starts_with("BEGIN:") || upper.starts_with("END:")) && !upper.ends_with(":VALARM") {
            return Err("only VALARM components are default alarms".into());
        }
        body.push_str(line.trim_end_matches('\r'));
        body.push_str("\r\n");
    }
    let calendar =
        ICalendar::parse(format!("{WRAPPER_START}{body}{WRAPPER_END}")).map_err(|_| "not valid iCalendar alarms")?;
    let Ok(group) = serde_json::to_value(calendar.into_jscalendar::<String, String>()) else {
        return Err("not valid iCalendar alarms".into());
    };
    let alerts = group["entries"][0].get("alerts").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut out = Map::new();
    for (id, mut alert) in alerts {
        let id = if text.contains(&format!("JSID:{id}")) { id } else { new_id() };
        if let Value::Object(alert) = &mut alert {
            alert.remove("iCalendar");
        }
        out.insert(id, alert);
    }
    check(&Value::Object(out)).map_err(|err| format!("these alarms cannot be default alerts: {err}"))
}

/// [`to_valarms`] for default alerts as the store keeps them, JSON; empty for none.
pub fn stored_to_valarms(stored: Option<&str>) -> String {
    match stored.map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Object(alerts))) => to_valarms(&alerts),
        _ => String::new(),
    }
}

/// [`from_valarms`] into the JSON the store keeps.
pub fn valarms_to_stored(text: &str) -> Result<Option<String>, String> {
    Ok(from_valarms(text)?.map(|alerts| Value::Object(alerts).to_string()))
}

fn new_id() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("the operating system RNG failed");
    hex::encode(bytes)
}

/// Writes a calendar's current default alerts into every event of it that uses them, after its
/// owner changed them. Events CalDAV changes meanwhile are read again; one that keeps changing is
/// left for the next write.
pub async fn apply(store: &Store, owner: i64, calendar_id: i64) -> Result<(), StoreError> {
    let prefs = store.calendar_prefs(owner).await?.remove(&calendar_id).unwrap_or_default();
    let parse = |text: &Option<String>| match text.as_deref().map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Object(alerts))) => Some(alerts),
        _ => None,
    };
    let (with_time, without_time) = (parse(&prefs.default_alerts_with_time), parse(&prefs.default_alerts_without_time));
    let events = store.calendar_events_between(owner, Some(calendar_id), None, None).await?;
    for record in events.into_iter().filter(|e| e.owner_id == owner && e.content.contains("useDefaultAlerts")) {
        let (with_time, without_time) = (with_time.clone(), without_time.clone());
        let content = record.content.clone();
        let rewritten = tokio::task::spawn_blocking(move || {
            let parsed = jscal::from_icalendar(&content)?;
            let mut event = parsed.event().clone();
            let defaults = for_event(&event, with_time.as_ref(), without_time.as_ref());
            if !materialize(&mut event, defaults) {
                return None;
            }
            parsed.to_icalendar(&event).ok()
        })
        .await
        .ok()
        .flatten();
        let Some(content) = rewritten else { continue };
        let Ok(checked) = uwumail_store::ical::check_calendar(&content, &[]) else { continue };
        let write = uwumail_store::CalendarEventWrite {
            id: Some(record.id),
            calendar_id,
            content,
            uid: checked.uid,
            starts_at: checked.starts_at,
            ends_at: checked.ends_at,
            if_etag: Some(record.etag),
            keep_schedule_tag: true,
            draft: None,
            author: uwumail_store::Author::Nobody,
        };
        match store.put_calendar_event(owner, write).await {
            Ok(_) | Err(StoreError::Conflict(_)) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Alerts the server fires (draft-ietf-jmap-calendars, section 6.4)

/// How often the worker looks for alerts to plan and to fire.
const TICK: std::time::Duration = std::time::Duration::from_secs(20);
/// An alert that should have gone off longer ago than this (the server was down) is dropped.
const MAX_LATE_SECS: i64 = 3600;
/// Events planned and alerts fired per turn; the rest comes in the next one.
const BATCH: usize = 500;

/// The SignedDuration of an OffsetTrigger in seconds; days count 24 hours.
fn offset_seconds(value: &str) -> Option<i64> {
    let (sign, rest) = match value.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, value.strip_prefix('+').unwrap_or(value)),
    };
    let (days, seconds) = jscal::parse_duration(rest)?;
    Some(sign * (days * 86_400 + seconds))
}

type Planned = std::collections::BTreeMap<String, uwumail_store::PlannedAlert>;

/// When each alert of an event (as the account sees it) next goes off after `after`: per alert
/// id, the first instance whose trigger lies after it and that was not acknowledged.
pub fn next_alerts(
    event: &Map<String, Value>,
    content: &str,
    floating: chrono_tz::Tz,
    after: i64,
) -> Vec<uwumail_store::PlannedAlert> {
    let mut next: Planned = Default::default();
    let consider = |next: &mut Planned, recurrence_id: Option<&str>, object: &Map<String, Value>| {
        let Some(Value::Object(alerts)) = object.get("alerts") else { return };
        let Some((start, end)) = jscal::span(object, floating) else { return };
        for (id, alert) in alerts {
            let trigger = alert.get("trigger");
            let fire_at = match trigger.and_then(|t| t.get("@type")).and_then(Value::as_str) {
                Some("AbsoluteTrigger") => {
                    trigger.and_then(|t| t.get("when")).and_then(Value::as_str).and_then(jscal::parse_utc)
                }
                Some("UnknownTrigger") => None,
                _ => {
                    let offset = trigger.and_then(|t| t.get("offset")).and_then(Value::as_str).and_then(offset_seconds);
                    let base = if trigger.and_then(|t| t.get("relativeTo")).and_then(Value::as_str) == Some("end") {
                        end
                    } else {
                        start
                    };
                    offset.map(|offset| base + offset)
                }
            };
            let Some(fire_at) = fire_at.filter(|fire_at| *fire_at > after) else { continue };
            let acknowledged = alert.get("acknowledged").and_then(Value::as_str).and_then(jscal::parse_utc);
            if acknowledged.is_some_and(|acknowledged| acknowledged >= fire_at) {
                continue;
            }
            let action = match alert.get("action").and_then(Value::as_str) {
                Some("email") => "email",
                _ => "display",
            };
            if next.get(id).is_none_or(|planned| fire_at < planned.fire_at) {
                next.insert(
                    id.clone(),
                    uwumail_store::PlannedAlert {
                        alert_id: id.clone(),
                        recurrence_id: recurrence_id.map(str::to_owned),
                        fire_at,
                        action: action.to_owned(),
                    },
                );
            }
        }
    };
    if !jscal::is_recurring(event) {
        consider(&mut next, None, event);
    } else {
        // Instances come in order; once every alert of the series has its next time and the
        // instances start well after the latest of them, nothing earlier can come.
        let ids: usize = event.get("alerts").and_then(Value::as_object).map_or(0, Map::len);
        for rid in jscal::recurrence_ids(content, event) {
            let Some(instance) = jscal::instance(event, &rid) else { continue };
            if next.len() >= ids
                && let (Some(latest), Some((start, _))) =
                    (next.values().map(|p| p.fire_at).max(), jscal::span(&instance, floating))
                && start > latest + 400 * 86_400
            {
                break;
            }
            consider(&mut next, Some(&rid), &instance);
        }
    }
    next.into_values().collect()
}

impl crate::Jmap {
    /// Runs until `shutdown`: plans the alerts of changed events and fires those that are due, as
    /// a CalendarAlert push or a mail.
    pub async fn run_calendar_alerts(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        loop {
            self.calendar_alerts_tick(crate::methods::unix_now()).await;
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = shutdown.changed() => return,
            }
        }
    }

    /// One turn of the alert worker at time `now`.
    pub async fn calendar_alerts_tick(&self, now: i64) {
        let store = &self.inner.store;
        match store.calendar_alerts_to_plan(BATCH).await {
            Ok(pairs) => {
                for (account_id, event_id) in pairs {
                    let planned = self.plan_alerts(account_id, event_id, now).await;
                    if let Err(err) = store.plan_calendar_alerts(account_id, event_id, now, planned).await {
                        tracing::warn!(%err, account_id, event_id, "planning calendar alerts failed");
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "reading the calendar alerts to plan failed"),
        }
        let due = match store.due_calendar_alerts(now, BATCH).await {
            Ok(due) => due,
            Err(err) => {
                tracing::warn!(%err, "reading due calendar alerts failed");
                return;
            }
        };
        for alert in due {
            if now - alert.alert.fire_at <= MAX_LATE_SECS {
                self.fire_alert(&alert).await;
            }
            if let Err(err) = store.calendar_alert_fired(alert).await {
                tracing::warn!(%err, "noting a calendar alert failed");
            }
        }
    }

    /// The next alerts of an event for an account: none for a draft, for an account without
    /// calendars, or for an event it does not see.
    async fn plan_alerts(&self, account_id: i64, event_id: i64, now: i64) -> Vec<uwumail_store::PlannedAlert> {
        let Ok(Some(account)) = self.inner.store.account_by_id(account_id).await else { return Vec::new() };
        if !account.protocols.caldav || account.deleted_at.is_some() {
            return Vec::new();
        }
        let ctx = crate::methods::Ctx::new(&self.inner, account, Vec::new(), Default::default());
        let Ok(Some((record, event, floating))) = crate::methods::event_for_alerts(&ctx, event_id).await else {
            return Vec::new();
        };
        if record.is_draft {
            return Vec::new();
        }
        tokio::task::spawn_blocking(move || next_alerts(&event, &record.content, floating, now))
            .await
            .unwrap_or_default()
    }

    /// Pushes a CalendarAlert, or sends the mail of an `email` alert.
    async fn fire_alert(&self, due: &uwumail_store::DueAlert) {
        let store = &self.inner.store;
        let Ok(Some(record)) =
            store.calendar_events(due.account_id, Some(vec![due.event_id])).await.map(|mut r| r.pop())
        else {
            return;
        };
        if due.alert.action == "email" {
            let Ok(Some(account)) = store.account_by_id(due.account_id).await else { return };
            let calendar = uwumail_store::itip::Component::parse(&record.content);
            let summary = calendar.as_ref().map(uwumail_store::itip::summary).unwrap_or_default();
            // For an instance of a series, the instance's time.
            let when = match &due.alert.recurrence_id {
                Some(rid) => rid.replacen('T', " ", 1).get(..16).unwrap_or(rid).to_owned(),
                None => summary.when.clone(),
            };
            if let Err(err) = self.inner.smtp.send_reminder(&account, &summary.title, &when, &summary.location).await {
                tracing::warn!(%err, login = %account.login, "the mail of a calendar alert could not be sent");
            }
            return;
        }
        store.announce_calendar_alert(uwumail_store::CalendarAlertFired {
            account_id: due.account_id,
            event_id: due.event_id,
            uid: record.uid,
            recurrence_id: due.alert.recurrence_id.clone(),
            alert_id: due.alert.alert_id.clone(),
        });
    }
}

/// The CalendarAlert object a push carries.
pub fn alert_json(alert: &uwumail_store::CalendarAlertFired) -> Value {
    json!({
        "@type": "CalendarAlert",
        "accountId": crate::ids::account(alert.account_id),
        "calendarEventId": crate::ids::calendar_event(alert.event_id),
        "uid": alert.uid,
        "recurrenceId": alert.recurrence_id,
        "alertId": alert.alert_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn default_alerts_are_checked() {
        let good = json!({ "d1": { "@type": "Alert", "trigger": { "@type": "OffsetTrigger", "offset": "-PT15M" } } });
        assert!(check(&good).unwrap().is_some());
        assert_eq!(check(&Value::Null), Ok(None));
        let absolute = json!({ "d1": { "trigger": { "@type": "AbsoluteTrigger", "when": "2026-01-01T00:00:00Z" } } });
        assert!(check(&absolute).is_err());
        assert!(check(&json!({ "d/1": { "trigger": { "offset": "PT0S" } } })).is_err());
        assert!(check(&json!({ "d1": { "trigger": { "offset": "soon" } } })).is_err());
    }

    #[test]
    fn events_using_defaults_get_them_and_keep_acknowledgements() {
        let defaults = object(json!({ "d1": { "trigger": { "offset": "-PT15M" } } }));
        let mut event = object(json!({
            "useDefaultAlerts": true,
            "alerts": {
                "d1": { "trigger": { "offset": "-PT1M" }, "acknowledged": "2026-10-20T08:45:00Z" },
                "mine": { "trigger": { "offset": "-PT2H" } },
                "snooze": { "trigger": { "@type": "AbsoluteTrigger", "when": "2026-10-20T08:50:00Z" }, "relatedTo": { "d1": { "relation": { "snooze": true } } } }
            }
        }));
        assert!(materialize(&mut event, Some(&defaults)));
        let alerts = event["alerts"].as_object().unwrap();
        assert_eq!(alerts["d1"]["trigger"]["offset"], "-PT15M");
        assert_eq!(alerts["d1"]["acknowledged"], "2026-10-20T08:45:00Z");
        assert!(alerts.contains_key("snooze") && !alerts.contains_key("mine"));
        assert!(!materialize(&mut event, Some(&defaults)), "nothing more to do");
        let mut own = object(json!({ "alerts": { "mine": {} } }));
        assert!(!materialize(&mut own, Some(&defaults)), "only for events that use them");
    }

    #[test]
    fn alerts_are_planned_for_the_next_instance() {
        let utc = chrono_tz::UTC;
        let event = object(json!({
            "start": "2026-10-20T09:00:00", "timeZone": "Etc/UTC", "duration": "PT1H",
            "recurrenceRule": { "frequency": "daily", "count": 3 },
            "alerts": {
                "a": { "trigger": { "offset": "-PT15M" } },
                "b": { "trigger": { "offset": "PT0S", "relativeTo": "end" }, "action": "email",
                       "acknowledged": "2026-10-21T10:00:00Z" },
                "c": { "trigger": { "@type": "AbsoluteTrigger", "when": "2026-10-19T12:00:00Z" } }
            }
        }));
        let content = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261020T090000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let after = jscal::parse_utc("2026-10-20T09:00:00Z").unwrap();
        let planned = next_alerts(&event, content, utc, after);
        let at = |id: &str| {
            planned
                .iter()
                .find(|p| p.alert_id == id)
                .map(|p| (jscal::format_utc(p.fire_at), p.recurrence_id.clone(), p.action.clone()))
        };
        assert_eq!(
            at("a"),
            Some(("2026-10-21T08:45:00Z".into(), Some("2026-10-21T09:00:00".into()), "display".into()))
        );
        // Acknowledged on the 21st after it went off: the next is the 22nd's.
        assert_eq!(at("b"), Some(("2026-10-22T10:00:00Z".into(), Some("2026-10-22T09:00:00".into()), "email".into())));
        assert_eq!(at("c"), None, "absolute and in the past");
        let later = jscal::parse_utc("2026-10-23T00:00:00Z").unwrap();
        assert!(next_alerts(&event, content, utc, later).is_empty(), "the series is over");
    }

    #[test]
    fn default_alerts_round_trip_through_valarms() {
        let alerts = object(json!({
            "d1": { "@type": "Alert", "trigger": { "@type": "OffsetTrigger", "offset": "-PT15M" }, "action": "display" }
        }));
        let text = to_valarms(&alerts);
        assert!(text.starts_with("BEGIN:VALARM\r\n") && text.contains("TRIGGER:-PT15M"), "{text}");
        let back = from_valarms(&text).unwrap().unwrap();
        assert_eq!(back["d1"]["trigger"]["offset"], "-PT15M", "{back:?}");
        let apple = "BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT30M\nEND:VALARM\n";
        let read = from_valarms(apple).unwrap().unwrap();
        assert_eq!(read.len(), 1);
        assert!(read.keys().all(|id| id.len() == 24), "a new id: {read:?}");
        assert!(from_valarms("END:VEVENT\nBEGIN:VEVENT\nUID:x\n").is_err());
        assert_eq!(from_valarms(""), Ok(None));
    }
}
