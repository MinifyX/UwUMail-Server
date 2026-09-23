//! JSCalendar for JMAP Calendars: the iCalendar objects CalDAV stores turned into CalendarEvent
//! JSON and back (with calcard), JMAP patches on that JSON, the rules an event has to follow to be
//! stored, local times in time zones, and recurrences expanded into instances.
//!
//! Events stay iCalendar on disk, so phones and Thunderbird see exactly what JMAP clients see.

use std::collections::BTreeSet;

use calcard::icalendar::{ICalendar, ICalendarComponentType, ICalendarProperty};
use calcard::jscalendar::JSCalendar;
use chrono::{DateTime, LocalResult, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde_json::{Map, Value};

/// The dates the server accepts in an event, as in the session's `minDateTime`/`maxDateTime`.
pub const MIN_DATE_TIME: &str = "1900-01-01T00:00:00Z";
pub const MAX_DATE_TIME: &str = "2200-01-01T00:00:00Z";
const MIN_TIMESTAMP: i64 = -2_208_988_800;
const MAX_TIMESTAMP: i64 = 7_258_118_400;
/// The longest window a query may expand recurrences over (`maxExpandedQueryDuration`).
pub const MAX_EXPANDED_DAYS: i64 = 400;
pub const MAX_EXPANDED_DURATION: &str = "P400D";
/// Occurrences of one recurring event looked at, from its first one on. An endless daily event
/// shows for about 27 years, which also bounds the work one event can cause.
pub const EXPANSION_LIMIT: usize = 10_000;
/// Longest title, in bytes. Everything else is bounded by the size of the whole object.
pub const MAX_TITLE_BYTES: usize = 1024;
/// Changed or excluded instances of one series that JMAP writes. Checking an event looks at each of
/// them against the series, so the number is bounded before anything else is done with them.
pub const MAX_OVERRIDES: usize = 1000;
const MAX_UID_BYTES: usize = 255;

/// Properties JMAP adds to an event; they never go into the iCalendar object.
pub const JMAP_PROPERTIES: &[&str] =
    &["id", "baseEventId", "calendarIds", "isDraft", "isOrigin", "utcStart", "utcEnd", "blobId"];

/// Properties an instance does not take over from its series when an override is written out whole.
const NOT_IN_OVERRIDES: &[&str] = &[
    "@type",
    "uid",
    "method",
    "prodId",
    "privacy",
    "organizerCalendarAddress",
    "sentBy",
    "recurrenceId",
    "recurrenceIdTimeZone",
    "recurrenceRule",
    "recurrenceOverrides",
    "excluded",
    "iCalendar",
];

/// A stored iCalendar object as JSCalendar: a Group whose entry `index` is the event.
#[derive(Debug, Clone)]
pub struct Parsed {
    group: Map<String, Value>,
    index: usize,
}

impl Parsed {
    pub fn event(&self) -> &Map<String, Value> {
        match &self.group["entries"][self.index] {
            Value::Object(event) => event,
            _ => unreachable!("checked when parsed"),
        }
    }

    /// The object with the event replaced, as iCalendar text.
    pub fn to_icalendar(&self, event: &Map<String, Value>) -> Result<String, String> {
        let mut event = event.clone();
        for property in JMAP_PROPERTIES {
            event.remove(*property);
        }
        fill_alert_actions(&mut event);
        materialize_overrides(&mut event);
        let mut group = self.group.clone();
        group["entries"][self.index] = Value::Object(event);
        let json = Value::Object(group).to_string();
        let calendar = JSCalendar::<String, String>::parse(&json).map_err(|err| format!("not JSCalendar: {err}"))?;
        let mut calendar = calendar.into_icalendar().ok_or("the event cannot be written as iCalendar")?;
        calendar.add_missing_timezones();
        Ok(calendar.to_string())
    }

    /// A new object holding just this event.
    pub fn new_event() -> Parsed {
        let mut group = Map::new();
        group.insert("@type".into(), "Group".into());
        group.insert("prodId".into(), "-//UwUMail//Server//EN".into());
        group.insert("entries".into(), Value::Array(vec![Value::Object(Map::new())]));
        Parsed { group, index: 0 }
    }
}

/// Reads a stored object. `None` when it holds no event calcard understands.
pub fn from_icalendar(content: &str) -> Option<Parsed> {
    let calendar = ICalendar::parse(content).ok()?;
    let Ok(Value::Object(mut group)) = serde_json::to_value(calendar.into_jscalendar::<String, String>()) else {
        return None;
    };
    let entries = group.get_mut("entries")?.as_array_mut()?;
    let index = entries.iter().position(|entry| entry.get("@type").and_then(Value::as_str) == Some("Event"))?;
    if let Value::Object(event) = &mut entries[index] {
        trim_overrides(event);
    }
    Some(Parsed { group, index })
}

/// iCalendar keeps whole instances; JSCalendar only what differs from the series. Drops what an
/// override repeats, so clients see the difference.
fn trim_overrides(event: &mut Map<String, Value>) {
    let base = event.clone();
    let Some(Value::Object(overrides)) = event.get_mut("recurrenceOverrides") else { return };
    for (rid, patch) in overrides.iter_mut() {
        let Value::Object(patch) = patch else { continue };
        if patch.get("excluded") == Some(&Value::Bool(true)) {
            continue;
        }
        patch.retain(|key, value| match key.as_str() {
            "iCalendar" => true,
            "start" => value.as_str() != Some(rid.as_str()),
            _ => base.get(key) != Some(value),
        });
    }
}

/// iCalendar alarms need an ACTION; JSCalendar alerts without one display.
fn fill_alert_actions(event: &mut Map<String, Value>) {
    let mut objects: Vec<&mut Map<String, Value>> = vec![event];
    while let Some(object) = objects.pop() {
        for (key, value) in object.iter_mut() {
            match (key.as_str(), value) {
                ("alerts", Value::Object(alerts)) => {
                    for alert in alerts.values_mut().filter_map(Value::as_object_mut) {
                        alert.entry("action").or_insert_with(|| Value::String("display".into()));
                    }
                }
                ("recurrenceOverrides", Value::Object(overrides)) => {
                    objects.extend(overrides.values_mut().filter_map(Value::as_object_mut));
                }
                _ => {}
            }
        }
    }
}

/// The opposite for writing: each override becomes the whole instance, so CalDAV clients find
/// title, time and place in it, as iCalendar has it.
fn materialize_overrides(event: &mut Map<String, Value>) {
    let base = event.clone();
    let Some(Value::Object(overrides)) = event.get_mut("recurrenceOverrides") else { return };
    for (rid, patch) in overrides.iter_mut() {
        let Value::Object(changes) = patch else { continue };
        if changes.get("excluded") == Some(&Value::Bool(true)) {
            *patch = serde_json::json!({ "excluded": true });
            continue;
        }
        let mut whole: Map<String, Value> = base
            .iter()
            .filter(|(key, _)| !NOT_IN_OVERRIDES.contains(&key.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        whole.insert("start".into(), Value::String(rid.clone()));
        if let Some(converted) = changes.get("iCalendar") {
            whole.insert("iCalendar".into(), converted.clone());
        }
        for (key, value) in changes.iter().filter(|(key, _)| key.as_str() != "iCalendar") {
            // A path that does not fit what the series has is left out rather than guessed at.
            let _ = apply_patch(&mut whole, key, value.clone());
        }
        *patch = Value::Object(whole);
    }
}

// ------------------------------------------------------------------------------------------------
// Patches (RFC 8620, section 5.3)

/// Splits a JSON pointer without leading slash into its tokens.
pub fn pointer_tokens(path: &str) -> Option<Vec<String>> {
    path.split('/')
        .map(|token| {
            let mut out = String::with_capacity(token.len());
            let mut chars = token.chars();
            while let Some(c) = chars.next() {
                match c {
                    '~' => match chars.next() {
                        Some('0') => out.push('~'),
                        Some('1') => out.push('/'),
                        _ => return None,
                    },
                    other => out.push(other),
                }
            }
            Some(out)
        })
        .collect()
}

pub fn escape_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Sets (or with `null` removes) the value a pointer names. Every parent has to exist and be an
/// object.
pub fn apply_patch(target: &mut Map<String, Value>, path: &str, value: Value) -> Result<(), String> {
    let tokens = pointer_tokens(path).ok_or_else(|| format!("{path} is not a valid pointer"))?;
    let (last, parents) = tokens.split_last().ok_or_else(|| "an empty pointer".to_string())?;
    let mut current = target;
    for token in parents {
        current = match current.get_mut(token) {
            Some(Value::Object(inner)) => inner,
            _ => return Err(format!("{path} points into something that is not an object")),
        };
    }
    if value.is_null() {
        current.remove(last);
    } else {
        current.insert(last.clone(), value);
    }
    Ok(())
}

/// Whether one path of a patch is a prefix of another, which RFC 8620 does not allow.
pub fn overlapping_paths(paths: &[&str]) -> bool {
    paths.iter().any(|a| paths.iter().any(|b| a != b && b.starts_with(a) && b.as_bytes().get(a.len()) == Some(&b'/')))
}

// ------------------------------------------------------------------------------------------------
// Times

/// `YYYY-MM-DDTHH:MM:SS`, a JSCalendar LocalDateTime.
pub fn parse_local(value: &str) -> Option<NaiveDateTime> {
    if value.len() != 19 {
        return None;
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").ok()
}

pub fn format_local(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// `YYYY-MM-DDTHH:MM:SSZ`, optionally with fractions of a second: a UTCDateTime.
pub fn parse_utc(value: &str) -> Option<i64> {
    let local = value.strip_suffix('Z').or_else(|| value.strip_suffix('z'))?;
    let whole = match local.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit()) => whole,
        Some(_) => return None,
        None => local,
    };
    Some(parse_local(whole)?.and_utc().timestamp())
}

pub fn format_utc(timestamp: i64) -> String {
    DateTime::from_timestamp(timestamp, 0).map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default()
}

/// A time zone from the IANA database, spelled exactly.
pub fn time_zone(name: &str) -> Option<Tz> {
    name.parse::<Tz>().ok()
}

/// A local time in a zone as a UTC timestamp. Times that do not exist (the hour clocks skip) move
/// forward by the gap; times that exist twice take the first.
pub fn to_utc(local: NaiveDateTime, zone: Tz) -> i64 {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(time) | LocalResult::Ambiguous(time, _) => time.timestamp(),
        LocalResult::None => zone
            .from_local_datetime(&(local + chrono::Duration::hours(1)))
            .earliest()
            .map_or_else(|| local.and_utc().timestamp(), |time| time.timestamp()),
    }
}

pub fn from_utc(timestamp: i64, zone: Tz) -> Option<NaiveDateTime> {
    Some(DateTime::from_timestamp(timestamp, 0)?.with_timezone(&zone).naive_local())
}

/// Seconds since 1970, now.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn in_range(timestamp: i64) -> bool {
    (MIN_TIMESTAMP..MAX_TIMESTAMP).contains(&timestamp)
}

/// A JSCalendar Duration: nominal days (weeks count 7) and exact seconds. No signs, no fractions.
pub fn parse_duration(value: &str) -> Option<(i64, i64)> {
    let mut rest = value.strip_prefix('P')?;
    let number = |rest: &mut &str, unit: char| -> Option<Option<i64>> {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || rest.as_bytes().get(digits) != Some(&(unit as u8)) {
            return Some(None);
        }
        if digits > 9 {
            return None;
        }
        let n = rest[..digits].parse().ok()?;
        *rest = &rest[digits + 1..];
        Some(Some(n))
    };
    if let Some(weeks) = number(&mut rest, 'W')? {
        return rest.is_empty().then_some((weeks * 7, 0));
    }
    let days = number(&mut rest, 'D')?;
    let mut seconds = None;
    if let Some(time) = rest.strip_prefix('T') {
        rest = time;
        let hours = number(&mut rest, 'H')?;
        let minutes = number(&mut rest, 'M')?;
        let secs = number(&mut rest, 'S')?;
        if hours.is_none() && minutes.is_none() && secs.is_none() {
            return None;
        }
        seconds = Some(hours.unwrap_or(0) * 3600 + minutes.unwrap_or(0) * 60 + secs.unwrap_or(0));
    }
    if !rest.is_empty() || (days.is_none() && seconds.is_none()) {
        return None;
    }
    Some((days.unwrap_or(0), seconds.unwrap_or(0)))
}

/// When an event (or instance) starts and ends in UTC. Floating times are read in `floating`.
pub fn span(event: &Map<String, Value>, floating: Tz) -> Option<(i64, i64)> {
    let start = parse_local(event.get("start")?.as_str()?)?;
    let zone = match event.get("timeZone") {
        Some(Value::String(name)) => time_zone(name).unwrap_or(chrono_tz::UTC),
        _ => floating,
    };
    let all_day = event.get("showWithoutTime").and_then(Value::as_bool).unwrap_or(false);
    let (days, seconds) = match event.get("duration").and_then(Value::as_str).and_then(parse_duration) {
        Some(duration) => duration,
        // iCalendar gives a day to a date without an end.
        None if all_day => (1, 0),
        None => (0, 0),
    };
    let end = start
        .checked_add_signed(chrono::Duration::days(days))?
        .checked_add_signed(chrono::Duration::seconds(seconds))?;
    Some((to_utc(start, zone), to_utc(end, zone)))
}

/// Whether `[start, end)` overlaps a query window: ends after `after` and starts before `before`.
/// An instant counts at its start.
pub fn overlaps(start: i64, end: i64, after: Option<i64>, before: Option<i64>) -> bool {
    let starts_before = before.is_none_or(|before| start < before);
    let ends_after = after.is_none_or(|after| end > after || (end == start && start >= after));
    starts_before && ends_after
}

// ------------------------------------------------------------------------------------------------
// Recurrences

pub fn is_recurring(event: &Map<String, Value>) -> bool {
    event.get("recurrenceRule").is_some_and(Value::is_object)
        || event.get("recurrenceOverrides").and_then(Value::as_object).is_some_and(|o| !o.is_empty())
}

/// The recurrence ids of a series' instances: what the rule makes (up to [`EXPANSION_LIMIT`]),
/// plus added instances, minus excluded ones.
pub fn recurrence_ids(content: &str, event: &Map<String, Value>) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if event.get("recurrenceRule").is_some_and(Value::is_object)
        && let Ok(calendar) = ICalendar::parse(content)
    {
        // The series itself, not an instance that was changed on its own.
        let master = calendar.components.iter().position(|component| {
            component.component_type == ICalendarComponentType::VEvent
                && component.entries.iter().any(|entry| entry.name == ICalendarProperty::Rrule)
                && !component.entries.iter().any(|entry| entry.name == ICalendarProperty::RecurrenceId)
        });
        if let Some(master) = master {
            let expanded = calendar.expand_dates(calcard::common::timezone::Tz::Floating, EXPANSION_LIMIT);
            for instance in expanded.events.iter().filter(|instance| instance.comp_id as usize == master) {
                ids.insert(format_local(instance.start.naive_local()));
            }
        }
    }
    if let Some(Value::Object(overrides)) = event.get("recurrenceOverrides") {
        for (rid, patch) in overrides {
            if patch.get("excluded") == Some(&Value::Bool(true)) {
                ids.remove(rid);
            } else if parse_local(rid).is_some() {
                ids.insert(rid.clone());
            }
        }
    }
    ids
}

/// One instance of a series: the series with the instance's changes applied, no rule of its own.
pub fn instance(event: &Map<String, Value>, rid: &str) -> Option<Map<String, Value>> {
    let patch = event.get("recurrenceOverrides").and_then(|overrides| overrides.get(rid));
    if patch.is_some_and(|patch| patch.get("excluded") == Some(&Value::Bool(true))) {
        return None;
    }
    // Not a whole clone: the overrides of a long series would be copied once per instance.
    let mut object: Map<String, Value> = event
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "iCalendar" | "recurrenceOverrides"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    object.insert("recurrenceRule".into(), Value::Null);
    object.insert("recurrenceOverrides".into(), Value::Null);
    object.insert("recurrenceId".into(), Value::String(rid.to_owned()));
    if let Some(Value::String(zone)) = event.get("timeZone") {
        object.insert("recurrenceIdTimeZone".into(), Value::String(zone.clone()));
    }
    object.insert("start".into(), Value::String(rid.to_owned()));
    if let Some(Value::Object(patch)) = patch {
        for (key, value) in patch.iter().filter(|(key, _)| key.as_str() != "iCalendar") {
            let _ = apply_patch(&mut object, key, value.clone());
        }
    }
    Some(object)
}

/// What an instance changes compared with its series, property by property, as the override to
/// store for it.
pub fn override_for(event: &Map<String, Value>, rid: &str, changed: &Map<String, Value>) -> Map<String, Value> {
    let plain = instance(&without_override(event, rid), rid).unwrap_or_default();
    let mut patch = Map::new();
    let keys: BTreeSet<&String> = plain.keys().chain(changed.keys()).collect();
    for key in keys {
        if JMAP_PROPERTIES.contains(&key.as_str()) || NOT_IN_OVERRIDES.contains(&key.as_str()) {
            continue;
        }
        match (plain.get(key), changed.get(key)) {
            (a, b) if a == b => {}
            (_, Some(value)) => {
                patch.insert(key.clone(), value.clone());
            }
            (Some(_), None) => {
                patch.insert(key.clone(), Value::Null);
            }
            (None, None) => {}
        }
    }
    patch
}

fn without_override(event: &Map<String, Value>, rid: &str) -> Map<String, Value> {
    let mut event = event.clone();
    if let Some(Value::Object(overrides)) = event.get_mut("recurrenceOverrides") {
        overrides.remove(rid);
    }
    event
}

// ------------------------------------------------------------------------------------------------
// Rules for stored events

/// Why an event cannot be stored: the properties at fault and a description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub properties: Vec<String>,
    pub description: String,
}

fn invalid(property: &str, description: impl Into<String>) -> Invalid {
    Invalid { properties: vec![property.to_owned()], description: description.into() }
}

fn check_local(value: Option<&Value>, property: &str) -> Result<Option<NaiveDateTime>, Invalid> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let local = parse_local(text).ok_or_else(|| invalid(property, "must be YYYY-MM-DDTHH:MM:SS"))?;
            if !in_range(local.and_utc().timestamp()) {
                return Err(invalid(property, format!("must be between {MIN_DATE_TIME} and {MAX_DATE_TIME}")));
            }
            Ok(Some(local))
        }
        Some(_) => Err(invalid(property, "must be a local date and time")),
    }
}

fn check_zone(value: Option<&Value>, property: &str) -> Result<(), Invalid> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(name)) if time_zone(name).is_some() => Ok(()),
        Some(_) => Err(invalid(property, "must be a time zone of the IANA database, like Europe/Berlin")),
    }
}

fn check_duration(value: Option<&Value>, property: &str) -> Result<Option<(i64, i64)>, Invalid> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => match parse_duration(text) {
            Some((days, seconds)) if days <= 366 * 100 && seconds <= 366 * 100 * 86_400 => Ok(Some((days, seconds))),
            _ => Err(invalid(property, "must be a duration like PT1H or P1D")),
        },
        Some(_) => Err(invalid(property, "must be a duration")),
    }
}

fn check_utc(value: Option<&Value>, property: &str) -> Result<(), Invalid> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(text)) if parse_utc(text).is_some_and(in_range) => Ok(()),
        Some(_) => Err(invalid(property, "must be a UTC date and time like 2026-01-31T12:00:00Z")),
    }
}

fn check_all_day(object: &Map<String, Value>, start: Option<NaiveDateTime>, property: &str) -> Result<(), Invalid> {
    if object.get("showWithoutTime").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    if start.is_some_and(|start| start.time() != chrono::NaiveTime::MIN) {
        return Err(invalid(&format!("{property}start"), "an event without time starts at T00:00:00"));
    }
    if let Some((_, seconds)) = check_duration(object.get("duration"), "duration")?
        && seconds != 0
    {
        return Err(invalid(&format!("{property}duration"), "an event without time lasts whole days"));
    }
    Ok(())
}

fn check_rule(rule: &Value) -> Result<(), Invalid> {
    let property = "recurrenceRule";
    let Value::Object(rule) = rule else {
        return Err(invalid(property, "must be a RecurrenceRule object or null"));
    };
    let frequencies = ["yearly", "monthly", "weekly", "daily", "hourly", "minutely", "secondly"];
    if !rule.get("frequency").and_then(Value::as_str).is_some_and(|f| frequencies.contains(&f)) {
        return Err(invalid(
            property,
            "frequency must be yearly, monthly, weekly, daily, hourly, minutely or secondly",
        ));
    }
    for (key, value) in rule {
        let ok = match key.as_str() {
            "@type" => value.as_str() == Some("RecurrenceRule"),
            "frequency" => true,
            "interval" => value.as_u64().is_some_and(|n| (1..=10_000).contains(&n)),
            "count" => value.as_u64().is_some_and(|n| (1..=1_000_000).contains(&n)),
            "until" => check_local(Some(value), property).is_ok(),
            "rscale" => value.as_str() == Some("gregorian"),
            "skip" => matches!(value.as_str(), Some("omit" | "backward" | "forward")),
            "firstDayOfWeek" => value.as_str().is_some_and(is_weekday),
            "byDay" => value.as_array().is_some_and(|days| {
                days.len() <= 64
                    && days.iter().all(|day| {
                        day.get("day").and_then(Value::as_str).is_some_and(is_weekday)
                            && day.as_object().is_some_and(|day| {
                                day.iter().all(|(k, v)| match k.as_str() {
                                    "@type" => v.as_str() == Some("NDay"),
                                    "day" => true,
                                    "nthOfPeriod" => v.as_i64().is_some_and(|n| n != 0 && (-53..=53).contains(&n)),
                                    _ => false,
                                })
                            })
                    })
            }),
            "byMonthDay" | "byYearDay" | "byWeekNo" | "byHour" | "byMinute" | "bySecond" | "bySetPosition" => {
                value.as_array().is_some_and(|list| {
                    list.len() <= 400 && list.iter().all(|n| n.as_i64().is_some_and(|n| (-400..=400).contains(&n)))
                })
            }
            "byMonth" => value.as_array().is_some_and(|list| {
                list.len() <= 24
                    && list.iter().all(|month| {
                        month.as_str().is_some_and(|m| {
                            let number = m.strip_suffix('L').unwrap_or(m);
                            number.parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))
                        })
                    })
            }),
            _ => false,
        };
        if !ok {
            return Err(invalid(property, format!("{key} is not allowed or not valid in a recurrence rule")));
        }
    }
    if rule.contains_key("count") && rule.contains_key("until") {
        return Err(invalid(property, "count and until cannot both be set"));
    }
    Ok(())
}

fn is_weekday(day: &str) -> bool {
    ["mo", "tu", "we", "th", "fr", "sa", "su"].contains(&day)
}

/// Properties an override must not change (they belong to the series).
const FORBIDDEN_IN_OVERRIDES: &[&str] = &[
    "@type",
    "uid",
    "method",
    "prodId",
    "privacy",
    "organizerCalendarAddress",
    "sentBy",
    "recurrenceId",
    "recurrenceIdTimeZone",
    "recurrenceRule",
    "recurrenceRules",
    "excludedRecurrenceRules",
    "recurrenceOverrides",
    "calendarIds",
    "isDraft",
    "id",
    "baseEventId",
    "utcStart",
    "utcEnd",
    "mayInviteSelf",
    "mayInviteOthers",
    "hideAttendees",
];

/// The rules an event has to follow before it is turned into iCalendar. Everything else in it is
/// kept as data.
pub fn validate(event: &Map<String, Value>) -> Result<(), Invalid> {
    if event.get("@type").and_then(Value::as_str) != Some("Event") {
        return Err(invalid("@type", "must be Event"));
    }
    match event.get("uid") {
        Some(Value::String(uid))
            if !uid.trim().is_empty() && uid.len() <= MAX_UID_BYTES && !uid.chars().any(char::is_control) => {}
        _ => return Err(invalid("uid", format!("must be a text of 1 to {MAX_UID_BYTES} bytes"))),
    }
    if event.contains_key("method") {
        return Err(invalid("method", "a stored event has no method"));
    }
    for property in ["recurrenceRules", "excludedRecurrenceRules"] {
        if event.contains_key(property) {
            return Err(invalid(property, "not supported here; use recurrenceRule"));
        }
    }
    if event.get("recurrenceId").is_some_and(|rid| !rid.is_null()) {
        return Err(invalid("recurrenceId", "single instances are changed through their series"));
    }
    match event.get("title") {
        None | Some(Value::Null) => {}
        Some(Value::String(title)) if title.len() <= MAX_TITLE_BYTES => {}
        Some(_) => return Err(invalid("title", format!("must be a text of at most {MAX_TITLE_BYTES} bytes"))),
    }
    for property in ["description", "descriptionContentType", "color", "status", "freeBusyStatus", "locale"] {
        if event.get(property).is_some_and(|value| !value.is_string() && !value.is_null()) {
            return Err(invalid(property, "must be a text"));
        }
    }
    if event.get("privacy").is_some_and(|value| !matches!(value.as_str(), Some("public" | "private" | "secret"))) {
        return Err(invalid("privacy", "must be public, private or secret"));
    }
    if event.get("showWithoutTime").is_some_and(|value| !value.is_boolean()) {
        return Err(invalid("showWithoutTime", "must be true or false"));
    }
    if event.get("sequence").is_some_and(|value| value.as_u64().is_none_or(|n| n > u32::MAX as u64)) {
        return Err(invalid("sequence", "must be a whole number"));
    }
    for property in ["locations", "virtualLocations", "participants", "alerts", "links", "relatedTo", "keywords"] {
        if event.get(property).is_some_and(|value| !value.is_object() && !value.is_null()) {
            return Err(invalid(property, "must be an object"));
        }
    }
    let start = check_local(event.get("start"), "start")?.ok_or_else(|| invalid("start", "an event needs a start"))?;
    check_zone(event.get("timeZone"), "timeZone")?;
    let duration = check_duration(event.get("duration"), "duration")?.unwrap_or((0, 0));
    let end = start + chrono::Duration::days(duration.0) + chrono::Duration::seconds(duration.1);
    if !in_range(end.and_utc().timestamp()) {
        return Err(invalid("duration", format!("the event has to end before {MAX_DATE_TIME}")));
    }
    check_all_day(event, Some(start), "")?;
    check_utc(event.get("created"), "created")?;
    check_utc(event.get("updated"), "updated")?;
    match event.get("recurrenceRule") {
        None | Some(Value::Null) => {}
        Some(rule) => check_rule(rule)?,
    }
    match event.get("recurrenceOverrides") {
        None | Some(Value::Null) => {}
        Some(Value::Object(overrides)) => {
            if overrides.len() > MAX_OVERRIDES {
                return Err(invalid(
                    "recurrenceOverrides",
                    format!("a series may have at most {MAX_OVERRIDES} changed or excluded instances"),
                ));
            }
            for (rid, patch) in overrides {
                let property = format!("recurrenceOverrides/{}", escape_token(rid));
                check_local(Some(&Value::String(rid.clone())), &property)?;
                let Value::Object(patch) = patch else {
                    return Err(invalid(&property, "must be a patch object"));
                };
                for (key, value) in patch {
                    let top = key.split('/').next().unwrap_or_default();
                    if FORBIDDEN_IN_OVERRIDES.contains(&top) {
                        return Err(invalid(&property, format!("{key} cannot differ for one instance")));
                    }
                    match key.as_str() {
                        "excluded" if !value.is_boolean() => {
                            return Err(invalid(&property, "excluded must be true or false"));
                        }
                        "start" => {
                            check_local(Some(value), &property)?;
                        }
                        "timeZone" => check_zone(Some(value), &property)?,
                        "duration" => {
                            check_duration(Some(value), &property)?;
                        }
                        "title" if value.as_str().is_some_and(|title| title.len() > MAX_TITLE_BYTES) => {
                            return Err(invalid(&property, "the title is too long"));
                        }
                        _ => {}
                    }
                }
                let merged = instance(event, rid).unwrap_or_default();
                check_all_day(
                    &merged,
                    merged.get("start").and_then(Value::as_str).and_then(parse_local),
                    &format!("{property}/"),
                )?;
            }
        }
        Some(_) => return Err(invalid("recurrenceOverrides", "must be an object")),
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Text search

/// The words and "quoted phrases" of a search; all have to be found.
pub fn search_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let mut term = String::new();
        if c == '"' || c == '\'' {
            while let Some(next) = chars.next() {
                match next {
                    '\\' => term.extend(chars.next()),
                    quote if quote == c => break,
                    other => term.push(other),
                }
            }
        } else {
            term.push(c);
            while let Some(next) = chars.peek().copied().filter(|n| !n.is_whitespace()) {
                term.push(next);
                chars.next();
            }
        }
        if !term.is_empty() {
            terms.push(term.to_lowercase());
        }
    }
    terms
}

pub fn matches_terms(haystack: &str, terms: &[String]) -> bool {
    let haystack = haystack.to_lowercase();
    terms.iter().all(|term| haystack.contains(term.as_str()))
}

/// The texts of named fields of the objects in a map, like the names of all locations.
pub fn texts_of(event: &Map<String, Value>, property: &str, fields: &[&str]) -> String {
    let mut out = String::new();
    if let Some(Value::Object(items)) = event.get(property) {
        for item in items.values() {
            for field in fields {
                if let Some(text) = item.get(*field).and_then(Value::as_str) {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn limits_match_their_names() {
        assert_eq!(parse_utc(MIN_DATE_TIME), Some(MIN_TIMESTAMP));
        assert_eq!(parse_utc(MAX_DATE_TIME), Some(MAX_TIMESTAMP));
        assert_eq!(parse_utc("2026-01-31T12:00:00.250Z"), Some(1_769_860_800));
        assert_eq!(parse_utc("2026-01-31T12:00:00"), None);
        assert_eq!(parse_local("2026-02-30T00:00:00"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H"), Some((0, 3600)));
        assert_eq!(parse_duration("P2D"), Some((2, 0)));
        assert_eq!(parse_duration("P1DT2H30M"), Some((1, 9000)));
        assert_eq!(parse_duration("PT3H0M0S"), Some((0, 10_800)));
        assert_eq!(parse_duration("P2W"), Some((14, 0)));
        for bad in ["P", "PT", "1H", "P1H", "-PT1H", "P1W2D", "PT1.5S", "P99999999999D", "PT1M1H"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn local_times_become_utc() {
        let berlin = time_zone("Europe/Berlin").unwrap();
        let summer = parse_local("2026-07-01T10:00:00").unwrap();
        assert_eq!(format_utc(to_utc(summer, berlin)), "2026-07-01T08:00:00Z");
        // The hour that does not exist on the last Sunday of March.
        let gap = parse_local("2026-03-29T02:30:00").unwrap();
        assert_eq!(format_utc(to_utc(gap, berlin)), "2026-03-29T01:30:00Z");
        assert!(time_zone("europe/berlin").is_none());
        assert!(time_zone("Mars/Olympus_Mons").is_none());
    }

    #[test]
    fn patches_follow_rfc_8620() {
        let mut target = object(json!({ "a": { "b": 1 }, "list": [1] }));
        apply_patch(&mut target, "a/b", json!(2)).unwrap();
        apply_patch(&mut target, "a/c~1d", json!(3)).unwrap();
        apply_patch(&mut target, "list", Value::Null).unwrap();
        assert_eq!(Value::Object(target.clone()), json!({ "a": { "b": 2, "c/d": 3 } }));
        assert!(apply_patch(&mut target, "missing/x", json!(1)).is_err());
        assert!(apply_patch(&mut target, "a/b/c", json!(1)).is_err());
        assert!(overlapping_paths(&["a", "a/b"]));
        assert!(!overlapping_paths(&["a", "ab/c"]));
    }

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nBEGIN:VEVENT\r\nUID:yoga@example.org\r\n\
DTSTAMP:20260901T080000Z\r\nDTSTART;TZID=Europe/Berlin:20261020T090000\r\nDURATION:PT1H\r\nSUMMARY:Yoga\r\n\
LOCATION:Studio\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE;TZID=Europe/Berlin:20261103T090000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:yoga@example.org\r\nDTSTAMP:20260901T080000Z\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261027T090000\r\n\
DTSTART;TZID=Europe/Berlin:20261027T100000\r\nDURATION:PT1H\r\nSUMMARY:Yoga\r\nLOCATION:Park\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn series_expand_into_instances() {
        let parsed = from_icalendar(SERIES).unwrap();
        let event = parsed.event();
        assert_eq!(event["title"], "Yoga");
        assert_eq!(event["timeZone"], "Europe/Berlin");
        // The override keeps what differs from the series, and nothing else.
        let changed = &event["recurrenceOverrides"]["2026-10-27T09:00:00"];
        assert_eq!(changed["start"], "2026-10-27T10:00:00");
        assert!(changed.get("title").is_none(), "{changed}");
        let ids: Vec<String> = recurrence_ids(SERIES, event).into_iter().collect();
        assert_eq!(ids, ["2026-10-20T09:00:00", "2026-10-27T09:00:00", "2026-11-10T09:00:00"]);

        let moved = instance(event, "2026-10-27T09:00:00").unwrap();
        assert_eq!(moved["start"], "2026-10-27T10:00:00");
        assert_eq!(moved["recurrenceId"], "2026-10-27T09:00:00");
        assert_eq!(moved["recurrenceRule"], Value::Null);
        let berlin = time_zone("Europe/Berlin").unwrap();
        assert_eq!(span(&moved, berlin).map(|(s, _)| format_utc(s)).unwrap(), "2026-10-27T09:00:00Z");
        assert!(instance(event, "2026-11-03T09:00:00").is_none(), "excluded");

        // Written back, the moved instance is whole again for CalDAV clients.
        let text = parsed.to_icalendar(event).unwrap();
        let written = uwumail_store::ical::check_calendar(&text, &[]).unwrap();
        assert_eq!(written.uid, "yoga@example.org");
        let override_part = &text[text.find("RECURRENCE-ID").unwrap_or(0).saturating_sub(300)..];
        assert!(text.matches("SUMMARY:Yoga").count() == 2, "{text}");
        assert!(override_part.contains("LOCATION:Park"), "{text}");
        let again = from_icalendar(&text).unwrap();
        assert_eq!(again.event()["recurrenceOverrides"], event["recurrenceOverrides"]);
        // Written again, the time zone it now carries is not added a second time.
        let twice = again.to_icalendar(again.event()).unwrap();
        assert_eq!(twice.matches("BEGIN:VTIMEZONE").count(), 1, "{twice}");
    }

    #[test]
    fn overrides_record_only_what_changed() {
        let parsed = from_icalendar(SERIES).unwrap();
        let event = parsed.event();
        let mut changed = instance(event, "2026-11-10T09:00:00").unwrap();
        changed.insert("title".into(), json!("Yoga im Park"));
        changed.remove("locations");
        let patch = override_for(event, "2026-11-10T09:00:00", &changed);
        assert_eq!(Value::Object(patch), json!({ "title": "Yoga im Park", "locations": null }));
    }

    #[test]
    fn validation() {
        let good = object(json!({
            "@type": "Event", "uid": "a@example.org", "title": "Tee", "start": "2026-10-20T09:00:00",
            "timeZone": "Europe/Berlin", "duration": "PT1H",
            "recurrenceRule": { "@type": "RecurrenceRule", "frequency": "weekly", "byDay": [{ "@type": "NDay", "day": "tu" }] },
            "recurrenceOverrides": { "2026-10-27T09:00:00": { "excluded": true } }
        }));
        assert_eq!(validate(&good), Ok(()));
        let bad = |change: Value| {
            let mut event = good.clone();
            for (key, value) in object(change) {
                if value.is_null() {
                    event.remove(&key);
                } else {
                    event.insert(key, value);
                }
            }
            validate(&event).unwrap_err().properties
        };
        assert_eq!(bad(json!({ "timeZone": "Mars/Olympus_Mons" })), ["timeZone"]);
        assert_eq!(bad(json!({ "start": "1850-01-01T00:00:00" })), ["start"]);
        assert_eq!(bad(json!({ "start": null })), ["start"]);
        assert_eq!(bad(json!({ "method": "request" })), ["method"]);
        assert_eq!(bad(json!({ "title": "x".repeat(MAX_TITLE_BYTES + 1) })), ["title"]);
        assert_eq!(bad(json!({ "duration": "forever" })), ["duration"]);
        assert_eq!(bad(json!({ "recurrenceRule": { "frequency": "fortnightly" } })), ["recurrenceRule"]);
        assert_eq!(
            bad(json!({ "recurrenceRule": { "frequency": "daily", "until": "2300-01-01T00:00:00" } })),
            ["recurrenceRule"]
        );
        assert_eq!(bad(json!({ "showWithoutTime": true })), ["start"]);
        assert_eq!(
            bad(json!({ "recurrenceOverrides": { "2026-10-27T09:00:00": { "uid": "other" } } })),
            ["recurrenceOverrides/2026-10-27T09:00:00"]
        );
        assert_eq!(bad(json!({ "uid": "" })), ["uid"]);
    }

    #[test]
    fn search_terms_and_phrases() {
        assert_eq!(search_terms(r#"Yoga "im Park" 'a\'b'"#), ["yoga", "im park", "a'b"]);
        assert!(matches_terms("Yoga im Park", &search_terms("park YOGA")));
        assert!(!matches_terms("Yoga im Park", &search_terms("\"yoga park\"")));
    }
}
