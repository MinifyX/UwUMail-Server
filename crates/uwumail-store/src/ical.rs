//! Checks what goes into a calendar: one iCalendar object with one UID, and the time it covers, so
//! calendar queries can pick events by date. CalDAV and JMAP Calendars store through the same
//! check, so neither can write what the other would refuse.

use calcard::common::timezone::Tz;
use calcard::icalendar::{ICalendar, ICalendarComponentType};

/// Occurrences of a repeating event looked at to find where it ends. Beyond, it counts as open-ended.
const EXPANSION_LIMIT: usize = 3000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub uid: String,
    pub component: String,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
}

/// Why a calendar object was refused, as a CalDAV precondition name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Not iCalendar or vCard data at all.
    InvalidData(String),
    /// The component type is not allowed in this calendar.
    UnsupportedComponent(String),
    /// Several different UIDs, or none.
    InvalidObject(String),
}

pub fn check_calendar(content: &str, allowed: &[String]) -> Result<Checked, Refused> {
    let calendar = ICalendar::parse(content).map_err(|_| Refused::InvalidData("not an iCalendar object".into()))?;
    let mut uids: Vec<&str> = calendar.uids().collect();
    uids.sort_unstable();
    uids.dedup();
    let [uid] = uids[..] else {
        return Err(Refused::InvalidObject(format!("a calendar object needs exactly one UID, found {}", uids.len())));
    };
    let component = calendar
        .components
        .iter()
        .map(|component| &component.component_type)
        .find(|kind| {
            matches!(
                kind,
                ICalendarComponentType::VEvent | ICalendarComponentType::VTodo | ICalendarComponentType::VJournal
            )
        })
        .map(|kind| kind.as_str().to_owned())
        .ok_or_else(|| Refused::InvalidObject("no event, task or journal entry".into()))?;
    if !allowed.is_empty() && !allowed.iter().any(|kind| kind.eq_ignore_ascii_case(&component)) {
        return Err(Refused::UnsupportedComponent(component));
    }

    let expanded = calendar.expand_dates(Tz::Floating, EXPANSION_LIMIT);
    let open_ended = expanded.events.len() >= EXPANSION_LIMIT || !expanded.errors.is_empty();
    let starts_at = expanded.events.iter().map(|event| event.start.timestamp()).min();
    let ends_at = if open_ended {
        None
    } else {
        expanded
            .events
            .iter()
            .map(|event| match &event.end {
                calcard::icalendar::dates::TimeOrDelta::Time(end) => end.timestamp(),
                calcard::icalendar::dates::TimeOrDelta::Delta(delta) => event.start.timestamp() + delta.num_seconds(),
            })
            .max()
    };
    Ok(Checked { uid: uid.to_owned(), component, starts_at, ends_at })
}

/// The times an event keeps people busy within `[start, end)`: each occurrence, unless the event is
/// transparent (it does not block time) or cancelled. For free-busy lookups (RFC 6638).
pub fn busy_periods(content: &str, start: i64, end: i64) -> Vec<(i64, i64)> {
    let Ok(calendar) = ICalendar::parse(content) else { return Vec::new() };
    let unfolded = content.replace("\r\n ", "").replace("\n ", "").to_ascii_uppercase();
    if unfolded.contains("\nTRANSP:TRANSPARENT") || unfolded.contains("\nSTATUS:CANCELLED") {
        return Vec::new();
    }
    let expanded = calendar.expand_dates(Tz::Floating, EXPANSION_LIMIT);
    expanded
        .events
        .iter()
        .map(|event| {
            let from = event.start.timestamp();
            let to = match &event.end {
                calcard::icalendar::dates::TimeOrDelta::Time(end) => end.timestamp(),
                calcard::icalendar::dates::TimeOrDelta::Delta(delta) => from + delta.num_seconds(),
            };
            (from, to)
        })
        .filter(|(from, to)| *from < end && *to > start && to > from)
        .collect()
}

/// A VCALENDAR with the VTIMEZONE of an IANA time zone, the way CalDAV keeps a calendar's time
/// zone. `None` for names that are not in the time zone database.
pub fn timezone_calendar(tz_id: &str, now: i64) -> Option<String> {
    if !matches!(tz_id.parse::<Tz>(), Ok(Tz::Tz(tz)) if tz.name() == tz_id || tz_id == "Etc/UTC") {
        return None;
    }
    let mut calendar =
        ICalendar::parse("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//UwUMail//Server//EN\r\nEND:VCALENDAR\r\n")
            .ok()?;
    calendar.add_timezone(tz_id, now - 366 * 86_400, now + 366 * 86_400)?;
    Some(calendar.to_string())
}

/// The IANA name of the time zone in a calendar's VCALENDAR, if it maps to one.
pub fn timezone_id(vcalendar: &str) -> Option<String> {
    let calendar = ICalendar::parse(vcalendar).ok()?;
    calendar.timezones().find_map(|component| match component.timezone() {
        Some((_, tz @ Tz::Tz(_))) => tz.name().map(|name| name.into_owned()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_zones_round_trip() {
        let calendar = timezone_calendar("Europe/Berlin", 1_789_894_800).unwrap();
        assert!(calendar.contains("TZID:Europe/Berlin"), "{calendar}");
        assert_eq!(timezone_id(&calendar).as_deref(), Some("Europe/Berlin"));
        assert!(timezone_calendar("Etc/UTC", 0).is_some());
        assert!(timezone_calendar("Mars/Olympus_Mons", 0).is_none());
        assert!(timezone_calendar("(GMT+01:00) Amsterdam", 0).is_none());
        assert_eq!(timezone_id("nonsense"), None);
    }
}
