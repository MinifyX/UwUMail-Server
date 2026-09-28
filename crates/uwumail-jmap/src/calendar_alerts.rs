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
        };
        match store.put_calendar_event(owner, write).await {
            Ok(_) | Err(StoreError::Conflict(_)) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
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
