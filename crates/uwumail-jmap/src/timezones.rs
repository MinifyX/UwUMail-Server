//! Custom time zones of events (RFC 8984, section 4.7.2): VTIMEZONEs that are no zone of the IANA
//! database, as JSCalendar `timeZones` with a `timeZone` like `/My Zone`, and back.
//!
//! calcard maps VTIMEZONEs it knows (IANA names, Windows names, `X-LIC-LOCATION`) to IANA zones
//! and leaves the others floating. Those are read here into TimeZone objects, times in them are
//! worked out from their rules, and a TimeZone written over JMAP becomes a VTIMEZONE again.
//! Rules are yearly (as every VTIMEZONE in use has them), with extra onsets (RDATE).

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::{Map, Value, json};

use crate::jscal::{format_local, parse_local};

/// Custom time zones one event may define, and rules in each.
const MAX_ZONES: usize = 10;
const MAX_RULES: usize = 20;
const MAX_ONSETS: usize = 100;
const MAX_ID_BYTES: usize = 255;

// ------------------------------------------------------------------------------------------------
// VTIMEZONE (as calcard keeps unknown components: jCal arrays) into TimeZone

fn jcal_properties(component: &Value) -> impl Iterator<Item = (String, &str)> {
    component.get(1).and_then(Value::as_array).into_iter().flatten().filter_map(|property| {
        let name = property.get(0)?.as_str()?.to_ascii_lowercase();
        Some((name, property.get(3)?.as_str()?))
    })
}

/// `16011028T030000` as a LocalDateTime.
fn ical_local(value: &str) -> Option<String> {
    let value = value.trim();
    let parsed = NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(value, "%Y%m%d").ok().map(|d| d.and_time(NaiveTime::MIN)))?;
    Some(format_local(parsed))
}

fn is_offset(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or("");
    (digits.len() == 4 || digits.len() == 6) && digits.bytes().all(|b| b.is_ascii_digit())
}

/// An RRULE of a time zone rule as a RecurrenceRule; `None` when it is not a yearly one.
fn rrule(value: &str) -> Option<Value> {
    let mut rule = Map::new();
    rule.insert("@type".into(), json!("RecurrenceRule"));
    for part in value.split(';') {
        let (key, value) = part.split_once('=')?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" if value.eq_ignore_ascii_case("YEARLY") => {
                rule.insert("frequency".into(), json!("yearly"));
            }
            "INTERVAL" => {
                rule.insert("interval".into(), json!(value.parse::<u64>().ok()?));
            }
            "COUNT" => {
                rule.insert("count".into(), json!(value.parse::<u64>().ok()?));
            }
            "UNTIL" => {
                rule.insert("until".into(), json!(ical_local(value)?));
            }
            "BYMONTH" => {
                let months: Option<Vec<Value>> =
                    value.split(',').map(|m| m.parse::<u8>().ok().map(|m| json!(m.to_string()))).collect();
                rule.insert("byMonth".into(), json!(months?));
            }
            "BYMONTHDAY" => {
                let days: Option<Vec<Value>> =
                    value.split(',').map(|d| d.parse::<i64>().ok().map(Value::from)).collect();
                rule.insert("byMonthDay".into(), json!(days?));
            }
            "BYDAY" => {
                let mut days = Vec::new();
                for day in value.split(',') {
                    let (nth, weekday) = day.split_at(day.len().checked_sub(2)?);
                    let mut nday = json!({ "@type": "NDay", "day": weekday.to_ascii_lowercase() });
                    if !nth.is_empty() {
                        nday["nthOfPeriod"] = json!(nth.parse::<i64>().ok()?);
                    }
                    days.push(nday);
                }
                rule.insert("byDay".into(), json!(days));
            }
            "WKST" => {}
            _ => return None,
        }
    }
    rule.contains_key("frequency").then_some(Value::Object(rule))
}

/// A VTIMEZONE as a TimeZone object, when its rules are ones this server works with.
fn time_zone_of(component: &Value) -> Option<(String, Value)> {
    let mut zone = Map::new();
    zone.insert("@type".into(), json!("TimeZone"));
    let mut tzid = None;
    for (name, value) in jcal_properties(component) {
        match name.as_str() {
            "tzid" => tzid = Some(value.to_owned()),
            "tzurl" => {
                zone.insert("url".into(), json!(value));
            }
            "last-modified" => {
                if let Some(local) = ical_local(value) {
                    zone.insert("updated".into(), json!(format!("{local}Z")));
                }
            }
            _ => {}
        }
    }
    let tzid = tzid?;
    zone.insert("tzId".into(), json!(tzid));
    let mut kinds: [(&str, Vec<Value>); 2] = [("standard", Vec::new()), ("daylight", Vec::new())];
    for sub in component.get(2).and_then(Value::as_array).into_iter().flatten() {
        let kind = sub.get(0).and_then(Value::as_str)?.to_ascii_lowercase();
        let list = &mut kinds.iter_mut().find(|(k, _)| *k == kind)?.1;
        let mut rule = Map::new();
        rule.insert("@type".into(), json!("TimeZoneRule"));
        let (mut names, mut comments, mut onsets) = (Map::new(), Vec::new(), Map::new());
        for (name, value) in jcal_properties(sub) {
            match name.as_str() {
                "dtstart" => {
                    rule.insert("start".into(), json!(ical_local(value)?));
                }
                "tzoffsetfrom" if is_offset(value.trim()) => {
                    rule.insert("offsetFrom".into(), json!(value.trim()));
                }
                "tzoffsetto" if is_offset(value.trim()) => {
                    rule.insert("offsetTo".into(), json!(value.trim()));
                }
                "rrule" => {
                    rule.insert("recurrenceRules".into(), json!([rrule(value)?]));
                }
                "rdate" => {
                    for date in value.split(',') {
                        onsets.insert(ical_local(date)?, json!({}));
                    }
                }
                "tzname" => {
                    names.insert(value.to_owned(), json!(true));
                }
                "comment" => comments.push(json!(value)),
                _ => {}
            }
        }
        for key in ["start", "offsetFrom", "offsetTo"] {
            rule.get(key)?;
        }
        if !onsets.is_empty() {
            rule.insert("recurrenceOverrides".into(), Value::Object(onsets));
        }
        if !names.is_empty() {
            rule.insert("names".into(), Value::Object(names));
        }
        if !comments.is_empty() {
            rule.insert("comments".into(), Value::Array(comments));
        }
        list.push(Value::Object(rule));
    }
    for (kind, rules) in kinds {
        if !rules.is_empty() {
            zone.insert(kind.into(), Value::Array(rules));
        }
    }
    (zone.contains_key("standard") || zone.contains_key("daylight")).then_some((tzid, Value::Object(zone)))
}

/// The custom time zones a stored object defines: VTIMEZONEs calcard left floating, by TZID.
fn custom_zones(group: &Map<String, Value>) -> Map<String, Value> {
    let mut zones = Map::new();
    let components = group.get("iCalendar").and_then(|c| c.get("components")).and_then(Value::as_array);
    for component in components.into_iter().flatten() {
        if !component.get(0).and_then(Value::as_str).is_some_and(|n| n.eq_ignore_ascii_case("vtimezone")) {
            continue;
        }
        if let Some((tzid, zone)) = time_zone_of(component)
            && tzid.parse::<calcard::common::timezone::Tz>().is_err()
        {
            zones.insert(tzid, zone);
        }
    }
    zones
}

/// The TZID a JSCalendar object's start was written with, when calcard left it floating.
fn start_tzid(object: &Map<String, Value>) -> Option<&str> {
    object.get("iCalendar")?.get("convertedProperties")?.get("start")?.get("parameters")?.get("tzid")?.as_str()
}

/// Gives an event read from iCalendar its custom time zone: `timeZone: "/<TZID>"` and the zone in
/// `timeZones`, for its start and those of its changed instances.
pub fn read(group: &Map<String, Value>, event: &mut Map<String, Value>) {
    let zones = custom_zones(group);
    if zones.is_empty() {
        return;
    }
    let mut used = Map::new();
    let adopt = |object: &mut Map<String, Value>, used: &mut Map<String, Value>| {
        if object.get("timeZone").is_some_and(|zone| !zone.is_null()) {
            return;
        }
        let Some(tzid) = start_tzid(object).map(str::to_owned) else { return };
        let Some(zone) = zones.get(&tzid) else { return };
        let key = format!("/{tzid}");
        object.insert("timeZone".into(), json!(key));
        used.insert(key, zone.clone());
    };
    adopt(event, &mut used);
    let base_zone = event.get("timeZone").cloned();
    if let Some(Value::Object(overrides)) = event.get_mut("recurrenceOverrides") {
        for patch in overrides.values_mut().filter_map(Value::as_object_mut) {
            if patch.contains_key("start") {
                adopt(patch, &mut used);
                // The same zone as the series is no difference.
                if patch.get("timeZone") == base_zone.as_ref() {
                    patch.remove("timeZone");
                }
            }
        }
    }
    if !used.is_empty() {
        event.insert("timeZones".into(), Value::Object(used));
    }
}

// ------------------------------------------------------------------------------------------------
// TimeZone into VTIMEZONE

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n").replace('\r', "")
}

fn ical_date_time(local: &str) -> Option<String> {
    Some(parse_local(local)?.format("%Y%m%dT%H%M%S").to_string())
}

fn rrule_text(rule: &Map<String, Value>) -> Option<String> {
    let mut parts = vec!["FREQ=YEARLY".to_owned()];
    if let Some(interval) = rule.get("interval").and_then(Value::as_u64) {
        parts.push(format!("INTERVAL={interval}"));
    }
    if let Some(count) = rule.get("count").and_then(Value::as_u64) {
        parts.push(format!("COUNT={count}"));
    }
    if let Some(until) = rule.get("until").and_then(Value::as_str) {
        parts.push(format!("UNTIL={}Z", ical_date_time(until)?));
    }
    if let Some(months) = rule.get("byMonth").and_then(Value::as_array) {
        let months: Vec<&str> = months.iter().filter_map(Value::as_str).collect();
        parts.push(format!("BYMONTH={}", months.join(",")));
    }
    if let Some(days) = rule.get("byMonthDay").and_then(Value::as_array) {
        let days: Vec<String> = days.iter().filter_map(Value::as_i64).map(|d| d.to_string()).collect();
        parts.push(format!("BYMONTHDAY={}", days.join(",")));
    }
    if let Some(days) = rule.get("byDay").and_then(Value::as_array) {
        let days: Vec<String> = days
            .iter()
            .filter_map(|day| {
                let weekday = day.get("day")?.as_str()?.to_ascii_uppercase();
                Some(match day.get("nthOfPeriod").and_then(Value::as_i64) {
                    Some(nth) => format!("{nth}{weekday}"),
                    None => weekday,
                })
            })
            .collect();
        parts.push(format!("BYDAY={}", days.join(",")));
    }
    Some(parts.join(";"))
}

/// A TimeZone as a VTIMEZONE, lines ending in CRLF (long ones are folded by the reader's rules
/// only when they need to be; these stay short).
pub fn vtimezone(tzid: &str, zone: &Map<String, Value>) -> Option<String> {
    let mut out = format!("BEGIN:VTIMEZONE\r\nTZID:{}\r\n", escape(tzid));
    if let Some(url) = zone.get("url").and_then(Value::as_str) {
        out.push_str(&format!("TZURL:{}\r\n", escape(url)));
    }
    if let Some(updated) = zone.get("updated").and_then(Value::as_str) {
        out.push_str(&format!("LAST-MODIFIED:{}Z\r\n", ical_date_time(updated.trim_end_matches('Z'))?));
    }
    for (kind, name) in [("standard", "STANDARD"), ("daylight", "DAYLIGHT")] {
        for rule in zone.get(kind).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object) {
            out.push_str(&format!("BEGIN:{name}\r\n"));
            out.push_str(&format!("DTSTART:{}\r\n", ical_date_time(rule.get("start")?.as_str()?)?));
            out.push_str(&format!("TZOFFSETFROM:{}\r\n", rule.get("offsetFrom")?.as_str()?));
            out.push_str(&format!("TZOFFSETTO:{}\r\n", rule.get("offsetTo")?.as_str()?));
            if let Some(rule) = rule.get("recurrenceRules").and_then(|r| r.get(0)).and_then(Value::as_object) {
                out.push_str(&format!("RRULE:{}\r\n", rrule_text(rule)?));
            }
            for onset in rule.get("recurrenceOverrides").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
                out.push_str(&format!("RDATE:{}\r\n", ical_date_time(onset)?));
            }
            for tzname in rule.get("names").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
                out.push_str(&format!("TZNAME:{}\r\n", escape(tzname)));
            }
            for comment in
                rule.get("comments").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)
            {
                out.push_str(&format!("COMMENT:{}\r\n", escape(comment)));
            }
            out.push_str(&format!("END:{name}\r\n"));
        }
    }
    out.push_str("END:VTIMEZONE\r\n");
    Some(out)
}

/// Sets the TZID calcard writes for a floating time of `object` (its `start`, and in an instance
/// its `recurrenceId` too).
fn set_tzid(object: &mut Map<String, Value>, tzid: &str, properties: &[&str]) {
    let hints = object.entry("iCalendar").or_insert_with(|| json!({}));
    if !hints.is_object() {
        *hints = json!({});
    }
    let converted = hints.as_object_mut().expect("an object").entry("convertedProperties").or_insert_with(|| json!({}));
    if !converted.is_object() {
        *converted = json!({});
    }
    for property in properties {
        converted[*property] = json!({ "parameters": { "tzid": tzid } });
    }
}

/// Prepares an event with custom time zones for calcard, which knows only IANA zones: its times
/// in such a zone become floating times that carry the zone's TZID, and the zones are returned
/// by TZID, to be written as VTIMEZONEs. Runs after the overrides were written out whole.
pub fn write(event: &mut Map<String, Value>) -> Vec<(String, Map<String, Value>)> {
    let Some(Value::Object(zones)) = event.remove("timeZones") else { return Vec::new() };
    let tzid_of = |key: &str| -> Option<String> {
        let zone = zones.get(key)?;
        Some(zone.get("tzId").and_then(Value::as_str).unwrap_or(key.trim_start_matches('/')).to_owned())
    };
    let mut used: Vec<(String, Map<String, Value>)> = Vec::new();
    let mut note = |key: &str| {
        if let (Some(tzid), Some(Value::Object(zone))) = (tzid_of(key), zones.get(key))
            && !used.iter().any(|(id, _)| *id == tzid)
        {
            used.push((tzid, zone.clone()));
        }
    };
    let custom =
        |value: Option<&Value>| value.and_then(Value::as_str).filter(|zone| zone.starts_with('/')).map(str::to_owned);
    let base = custom(event.get("timeZone"));
    if let Some(key) = &base
        && let Some(tzid) = tzid_of(key)
    {
        note(key);
        event.remove("timeZone");
        set_tzid(event, &tzid, &["start"]);
    }
    if custom(event.get("recurrenceIdTimeZone")).is_some() {
        event.remove("recurrenceIdTimeZone");
    }
    if let Some(Value::Object(overrides)) = event.get_mut("recurrenceOverrides") {
        for patch in overrides.values_mut().filter_map(Value::as_object_mut) {
            if patch.get("excluded") == Some(&Value::Bool(true)) {
                continue;
            }
            let own = custom(patch.get("timeZone"));
            if let Some(key) = own.as_ref().or(base.as_ref())
                && let Some(tzid) = tzid_of(key)
            {
                note(key);
                patch.remove("timeZone");
                let mut properties = vec!["start"];
                if base.is_some() {
                    properties.push("recurrenceId");
                }
                set_tzid(patch, &tzid, &properties);
            }
        }
    }
    used
}

// ------------------------------------------------------------------------------------------------
// Checks

fn is_paramtext(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_ID_BYTES && !text.chars().any(|c| c.is_control() || "\";:,".contains(c))
}

/// Checks `timeZones` and returns its keys. Every rule is yearly, as VTIMEZONEs have them.
pub fn check(value: Option<&Value>) -> Result<Vec<String>, String> {
    let zones = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(zones)) => zones,
        Some(_) => return Err("must be a map of time zones".into()),
    };
    if zones.len() > MAX_ZONES {
        return Err(format!("at most {MAX_ZONES} custom time zones"));
    }
    for (key, zone) in zones {
        if !key.starts_with('/') || !is_paramtext(key) {
            return Err(format!("{key} must start with / and be a valid TZID"));
        }
        let Value::Object(zone) = zone else { return Err(format!("{key} must be a TimeZone")) };
        let mut rules = 0;
        for (property, value) in zone {
            let ok = match property.as_str() {
                "@type" => value.as_str() == Some("TimeZone"),
                "tzId" => value.as_str().is_some_and(is_paramtext),
                "updated" => value.as_str().is_some_and(|t| crate::jscal::parse_utc(t).is_some()),
                "url" => value.as_str().is_some_and(|u| u.len() <= 2048 && !u.chars().any(char::is_control)),
                "validUntil" => value.as_str().is_some_and(|t| crate::jscal::parse_utc(t).is_some()),
                "aliases" => value.as_object().is_some_and(|a| a.len() <= 10),
                "standard" | "daylight" => {
                    let Some(list) = value.as_array() else { return Err(format!("{key}: {property} must be a list")) };
                    rules += list.len();
                    list.iter().all(|rule| check_rule(rule).is_ok())
                }
                _ => false,
            };
            if !ok {
                return Err(format!("{key}: {property} is not valid"));
            }
        }
        if rules == 0 || rules > MAX_RULES {
            return Err(format!("{key} needs between 1 and {MAX_RULES} rules"));
        }
    }
    Ok(zones.keys().cloned().collect())
}

fn check_rule(rule: &Value) -> Result<(), ()> {
    let Value::Object(rule) = rule else { return Err(()) };
    for key in ["start", "offsetFrom", "offsetTo"] {
        if !rule.contains_key(key) {
            return Err(());
        }
    }
    for (key, value) in rule {
        let ok = match key.as_str() {
            "@type" => value.as_str() == Some("TimeZoneRule"),
            "start" => value.as_str().and_then(parse_local).is_some(),
            "offsetFrom" | "offsetTo" => value.as_str().is_some_and(is_offset),
            "recurrenceRules" => value.as_array().is_some_and(|rules| {
                rules.len() <= 1
                    && rules.iter().all(|rule| {
                        rule.get("frequency").and_then(Value::as_str) == Some("yearly")
                            && rule.as_object().is_some_and(|rule| {
                                rule.keys().all(|k| {
                                    matches!(
                                        k.as_str(),
                                        "@type"
                                            | "frequency"
                                            | "interval"
                                            | "count"
                                            | "until"
                                            | "byMonth"
                                            | "byMonthDay"
                                            | "byDay"
                                    )
                                })
                            })
                            && rrule_text(rule.as_object().expect("checked")).is_some()
                    })
            }),
            "recurrenceOverrides" => value.as_object().is_some_and(|onsets| {
                onsets.len() <= MAX_ONSETS && onsets.keys().all(|onset| parse_local(onset).is_some())
            }),
            "names" => value.as_object().is_some_and(|names| names.len() <= 10 && names.keys().all(|n| n.len() <= 64)),
            "comments" => value
                .as_array()
                .is_some_and(|c| c.len() <= 10 && c.iter().all(|c| c.as_str().is_some_and(|c| c.len() <= 1024))),
            _ => false,
        };
        if !ok {
            return Err(());
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Times in a custom zone

fn offset_seconds(value: &str) -> Option<i64> {
    let (sign, digits) = match value.split_at_checked(1)? {
        ("+", digits) => (1, digits),
        ("-", digits) => (-1, digits),
        _ => return None,
    };
    let number = |range: std::ops::Range<usize>| digits.get(range)?.parse::<i64>().ok();
    let seconds = number(0..2)? * 3600 + number(2..4)? * 60 + if digits.len() == 6 { number(4..6)? } else { 0 };
    Some(sign * seconds)
}

/// The date a yearly rule falls on in `year`.
fn onset_in(year: i32, rule: &Map<String, Value>, start: NaiveDateTime) -> Option<NaiveDateTime> {
    let month = match rule.get("byMonth").and_then(|m| m.get(0)).and_then(Value::as_str) {
        Some(month) => month.parse::<u32>().ok()?,
        None => start.month(),
    };
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let last = NaiveDate::from_ymd_opt(if month == 12 { year + 1 } else { year }, month % 12 + 1, 1)?.pred_opt()?;
    let date = if let Some(day) = rule.get("byDay").and_then(|d| d.get(0)) {
        let weekday = match day.get("day")?.as_str()? {
            "mo" => chrono::Weekday::Mon,
            "tu" => chrono::Weekday::Tue,
            "we" => chrono::Weekday::Wed,
            "th" => chrono::Weekday::Thu,
            "fr" => chrono::Weekday::Fri,
            "sa" => chrono::Weekday::Sat,
            _ => chrono::Weekday::Sun,
        };
        let nth = day.get("nthOfPeriod").and_then(Value::as_i64).unwrap_or(1);
        // Among the month's days with this weekday; with byMonthDay too, the one on those days.
        let mut days: Vec<NaiveDate> =
            first.iter_days().take_while(|d| *d <= last).filter(|d| d.weekday() == weekday).collect();
        if let Some(Value::Array(month_days)) = rule.get("byMonthDay") {
            let wanted: Vec<u32> =
                month_days.iter().filter_map(Value::as_i64).filter(|d| *d > 0).map(|d| d as u32).collect();
            days.retain(|d| wanted.contains(&d.day()));
            *days.first()?
        } else if nth > 0 {
            *days.get(nth as usize - 1)?
        } else {
            *days.get(days.len().checked_sub(nth.unsigned_abs() as usize)?)?
        }
    } else if let Some(day) = rule.get("byMonthDay").and_then(|d| d.get(0)).and_then(Value::as_i64) {
        if day > 0 {
            NaiveDate::from_ymd_opt(year, month, day as u32)?
        } else {
            last - chrono::Duration::days(-day - 1)
        }
    } else {
        NaiveDate::from_ymd_opt(year, month, start.day())?
    };
    Some(date.and_time(start.time()))
}

/// The latest onset of a rule at or before `local`.
fn latest_onset(rule: &Map<String, Value>, local: NaiveDateTime) -> Option<NaiveDateTime> {
    let start = parse_local(rule.get("start")?.as_str()?)?;
    let mut best = (start <= local).then_some(start);
    if let Some(yearly) = rule.get("recurrenceRules").and_then(|r| r.get(0)).and_then(Value::as_object) {
        let until = yearly.get("until").and_then(Value::as_str).and_then(parse_local);
        let count = yearly.get("count").and_then(Value::as_i64);
        let interval = yearly.get("interval").and_then(Value::as_i64).unwrap_or(1).max(1);
        for year in [local.year() - 1, local.year()] {
            let since = i64::from(year - start.year());
            if since < 0 || since % interval != 0 || count.is_some_and(|count| since / interval >= count) {
                continue;
            }
            if let Some(onset) = onset_in(year, yearly, start)
                && onset >= start
                && onset <= local
                && until.is_none_or(|until| onset <= until)
            {
                best = best.max(Some(onset));
            }
        }
    }
    for onset in rule.get("recurrenceOverrides").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
        if let Some(onset) = parse_local(onset).filter(|onset| *onset <= local) {
            best = best.max(Some(onset));
        }
    }
    best
}

/// The offset from UTC in a custom zone at a local time, in seconds.
pub fn offset_at(zone: &Map<String, Value>, local: NaiveDateTime) -> Option<i64> {
    let rules: Vec<&Map<String, Value>> = ["standard", "daylight"]
        .iter()
        .flat_map(|kind| zone.get(*kind).and_then(Value::as_array).into_iter().flatten())
        .filter_map(Value::as_object)
        .collect();
    let latest =
        rules.iter().filter_map(|rule| Some((latest_onset(rule, local)?, *rule))).max_by_key(|(onset, _)| *onset);
    match latest {
        Some((_, rule)) => offset_seconds(rule.get("offsetTo")?.as_str()?),
        // Before its first rule, a zone keeps the offset that rule starts from.
        None => {
            let first =
                rules.iter().min_by_key(|rule| rule.get("start").and_then(Value::as_str).and_then(parse_local))?;
            offset_seconds(first.get("offsetFrom")?.as_str()?)
        }
    }
}

/// A local time in a custom zone as a UTC timestamp.
pub fn to_utc(zone: &Map<String, Value>, local: NaiveDateTime) -> i64 {
    local.and_utc().timestamp() - offset_at(zone, local).unwrap_or(0)
}

/// A UTC timestamp as the local time in a custom zone.
pub fn from_utc(zone: &Map<String, Value>, timestamp: i64) -> Option<NaiveDateTime> {
    let utc = chrono::DateTime::from_timestamp(timestamp, 0)?.naive_utc();
    // The offset at the local time depends on the local time: two rounds settle it.
    let mut local = utc + chrono::Duration::seconds(offset_at(zone, utc).unwrap_or(0));
    local = utc + chrono::Duration::seconds(offset_at(zone, local).unwrap_or(0));
    Some(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin_like() -> Map<String, Value> {
        json!({
            "@type": "TimeZone", "tzId": "My Zone",
            "standard": [{ "@type": "TimeZoneRule", "start": "1601-10-28T03:00:00", "offsetFrom": "+0200", "offsetTo": "+0100",
                "recurrenceRules": [{ "@type": "RecurrenceRule", "frequency": "yearly", "byMonth": ["10"], "byDay": [{ "@type": "NDay", "day": "su", "nthOfPeriod": -1 }] }] }],
            "daylight": [{ "@type": "TimeZoneRule", "start": "1601-03-25T02:00:00", "offsetFrom": "+0100", "offsetTo": "+0200",
                "recurrenceRules": [{ "@type": "RecurrenceRule", "frequency": "yearly", "byMonth": ["3"], "byDay": [{ "@type": "NDay", "day": "su", "nthOfPeriod": -1 }] }] }]
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn times_follow_the_rules_of_a_custom_zone() {
        let zone = berlin_like();
        let at = |local: &str| offset_at(&zone, parse_local(local).unwrap()).unwrap();
        assert_eq!(at("2026-07-01T12:00:00"), 7200);
        assert_eq!(at("2026-12-01T12:00:00"), 3600);
        assert_eq!(at("2026-10-25T02:59:59"), 7200, "until the last Sunday of October at three");
        assert_eq!(at("2026-10-25T03:00:00"), 3600);
        assert_eq!(at("2026-03-29T02:00:00"), 7200);
        let local = parse_local("2026-10-27T10:00:00").unwrap();
        assert_eq!(from_utc(&zone, to_utc(&zone, local)), Some(local));
        assert_eq!(check(Some(&json!({ "/My Zone": zone }))), Ok(vec!["/My Zone".to_owned()]));
        assert!(check(Some(&json!({ "My Zone": berlin_like() }))).is_err());
        let text = vtimezone("My Zone", &berlin_like()).unwrap();
        assert!(text.contains("RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\n"), "{text}");
    }
}
