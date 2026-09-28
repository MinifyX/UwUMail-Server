//! When someone is busy (draft-ietf-jmap-calendars, section 2.2), for `Principal/getAvailability`
//! and CalDAV's free-busy lookups alike.
//!
//! A person's busy times come from the calendars that count for them (`includeInAvailability`,
//! their own ones by default, see [`crate::methods`]), from events that make them busy: not
//! secret, not cancelled, `freeBusyStatus` busy (their own one for an event of a calendar shared
//! with them), and in an `attending` calendar only those they accepted or may attend. Recurring
//! events are expanded over the window. Only addresses, never masked ones, lead to a person.

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use serde_json::{Map, Value};
use uwumail_store::{Account, CalendarEventRecord, DavKind, NewDavCollection, Store, StoreError};

use crate::jscal;

/// The longest window availability is worked out for, as `maxAvailabilityDuration`.
pub const MAX_DAYS: i64 = 400;
pub const MAX_DURATION: &str = "P400D";

/// One stretch of time someone is busy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Period {
    pub start: i64,
    pub end: i64,
    /// `confirmed`, `tentative` or `unavailable`.
    pub status: &'static str,
}

/// An event (or one instance of it) that makes someone busy.
#[derive(Debug, Clone)]
pub struct Busy {
    pub period: Period,
    pub record: CalendarEventRecord,
    /// The instance of a recurring event, or the other single instance of an object without its
    /// series.
    pub recurrence_id: Option<String>,
    /// The event or instance as its calendar's owner keeps it.
    pub event: Map<String, Value>,
}

/// The work ran out of time.
#[derive(Debug)]
pub struct OutOfTime;

/// Whether an address belongs to someone of this server whose calendars count and whom `viewer`
/// may ask about, and who: only by an address of their own ([`Store::free_busy_owner`]), never a
/// masked address, which would tie it to its person, nor a group, forwarding address or catch-all,
/// and only in the viewer's own domains or when they share a calendar with the viewer
/// (security-audit-0.16.0 PROTOCOLS-L4).
pub async fn person_of(store: &Store, viewer: i64, address: &str) -> Option<Account> {
    let address = address.strip_prefix("mailto:").unwrap_or(address);
    let id = store.free_busy_owner(viewer, address).await.ok()??;
    let id = store.delivery_target(id).await.ok()??;
    let account = store.account_by_id(id).await.ok()??;
    (account.protocols.caldav && account.deleted_at.is_none()).then_some(account)
}

fn calendar_zone(timezone: Option<&str>) -> chrono_tz::Tz {
    timezone
        .and_then(uwumail_store::ical::timezone_id)
        .and_then(|name| jscal::time_zone(&name))
        .unwrap_or(chrono_tz::UTC)
}

/// The events and instances that make `account` busy between `start` and `end` (UTC seconds).
pub async fn busy(
    store: &Store,
    account: &Account,
    default_calendar: NewDavCollection,
    start: i64,
    end: i64,
    deadline: Instant,
) -> Result<Result<Vec<Busy>, OutOfTime>, StoreError> {
    // Which calendars count for the person, and how; floating times are read in their time zone.
    let mut prefs = store.calendar_prefs(account.id).await?;
    let mut counting: HashMap<i64, (bool, chrono_tz::Tz)> = HashMap::new();
    let own = store.dav_collections(account.id, DavKind::Calendar, default_calendar).await?;
    for calendar in own {
        let prefs = prefs.remove(&calendar.id).unwrap_or_default();
        let default = if calendar.subscribed { "none" } else { "all" };
        let include = prefs.include_in_availability.as_deref().unwrap_or(default);
        if include != "none" {
            counting.insert(calendar.id, (include == "attending", calendar_zone(calendar.timezone.as_deref())));
        }
    }
    for shared in store.dav_shared_with(account.id, DavKind::Calendar).await? {
        let prefs = prefs.remove(&shared.collection.id).unwrap_or_default();
        let include = prefs.include_in_availability.as_deref().unwrap_or("none");
        if include != "none" {
            let zone = prefs.timezone.as_deref().or(shared.collection.timezone.as_deref());
            counting.insert(shared.collection.id, (include == "attending", calendar_zone(zone)));
        }
    }
    if counting.is_empty() {
        return Ok(Ok(Vec::new()));
    }
    // Floating times may be up to a day off UTC; the exact check comes below.
    let margin = 2 * 86_400;
    let records: Vec<CalendarEventRecord> = store
        .calendar_events_between(account.id, None, Some(start - margin), Some(end + margin))
        .await?
        .into_iter()
        .filter(|record| counting.contains_key(&record.calendar_id))
        .collect();
    // For calendars shared with the person, their own freeBusyStatus.
    let shared: Vec<i64> = records.iter().filter(|r| r.owner_id != account.id).map(|r| r.id).collect();
    let own_status: HashMap<i64, String> = store
        .calendar_event_prefs(account.id, shared)
        .await?
        .into_iter()
        .filter_map(|(id, prefs)| {
            let data: Value = serde_json::from_str(&prefs.data).ok()?;
            Some((id, data.get("freeBusyStatus")?.as_str()?.to_owned()))
        })
        .collect();
    let mut addresses = store.addresses(&account.login).await.unwrap_or_default();
    addresses.push(account.login.clone());
    let me: Vec<String> = addresses.into_iter().map(|a| format!("mailto:{}", a.to_lowercase())).collect();
    let me_id = account.id;
    let found = tokio::task::spawn_blocking(move || {
        let mut found = Vec::new();
        for record in records {
            if Instant::now() > deadline {
                return Err(OutOfTime);
            }
            let Some(parsed) = jscal::from_icalendar(&record.content) else { continue };
            let (attending_only, floating) = counting[&record.calendar_id];
            let status_override = (record.owner_id != me_id).then(|| own_status.get(&record.id).cloned()).flatten();
            let mut candidates: Vec<(Option<String>, Map<String, Value>)> = Vec::new();
            let event = parsed.event();
            if jscal::is_recurring(event) {
                for rid in jscal::recurrence_ids(&record.content, event) {
                    if Instant::now() > deadline {
                        return Err(OutOfTime);
                    }
                    let Some(instance) = jscal::instance(event, &rid) else { continue };
                    candidates.push((Some(rid), instance));
                }
            } else {
                candidates.push((None, event.clone()));
                for (rid, index) in parsed.other_instances() {
                    if let Some(other) = parsed.at(index) {
                        candidates.push((Some(rid), other.event().clone()));
                    }
                }
            }
            for (recurrence_id, object) in candidates {
                let Some(status) = busy_status(&object, attending_only, status_override.as_deref(), &me) else {
                    continue;
                };
                let Some((from, to)) = jscal::span(&object, floating) else { continue };
                if !jscal::overlaps(from, to, Some(start), Some(end)) {
                    continue;
                }
                let period = Period { start: from.max(start), end: to.min(end).max(from.max(start)), status };
                found.push(Busy { period, record: record.clone(), recurrence_id, event: object });
            }
        }
        Ok(found)
    })
    .await
    .map_err(|_| StoreError::Internal("working out availability failed".into()))?;
    Ok(found)
}

/// How an event makes someone busy, or `None` when it does not.
fn busy_status(
    event: &Map<String, Value>,
    attending_only: bool,
    own_free_busy: Option<&str>,
    me: &[String],
) -> Option<&'static str> {
    let text = |key: &str| event.get(key).and_then(Value::as_str);
    if text("privacy") == Some("secret") || text("status") == Some("cancelled") {
        return None;
    }
    if own_free_busy.or(text("freeBusyStatus")).unwrap_or("busy") != "busy" {
        return None;
    }
    let mine = event.get("participants").and_then(Value::as_object).and_then(|participants| {
        participants
            .values()
            .find(|p| p.get("calendarAddress").and_then(Value::as_str).is_some_and(|a| me.contains(&a.to_lowercase())))
    });
    let answer = mine.and_then(|p| p.get("participationStatus")).and_then(Value::as_str);
    if attending_only && !matches!(answer, Some("accepted" | "tentative")) {
        return None;
    }
    if text("status") == Some("tentative") || answer == Some("tentative") {
        return Some("tentative");
    }
    Some("confirmed")
}

fn priority(status: &str) -> u8 {
    match status {
        "confirmed" => 3,
        "unavailable" => 2,
        _ => 1,
    }
}

/// Periods merged and split so that none overlap and neighbours with the same status are one:
/// where they overlap, confirmed wins over unavailable, which wins over tentative.
pub fn merge(periods: &[Period]) -> Vec<Period> {
    let bounds: BTreeSet<i64> = periods.iter().flat_map(|p| [p.start, p.end]).collect();
    let bounds: Vec<i64> = bounds.into_iter().collect();
    let mut merged: Vec<Period> = Vec::new();
    for pair in bounds.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let status = periods
            .iter()
            .filter(|p| p.start <= from && p.end >= to && p.end > p.start)
            .map(|p| p.status)
            .max_by_key(|status| priority(status));
        let Some(status) = status else { continue };
        match merged.last_mut() {
            Some(last) if last.end == from && last.status == status => last.end = to,
            _ => merged.push(Period { start: from, end: to, status }),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn periods_merge_by_priority() {
        let period = |start, end, status| Period { start, end, status };
        let merged = merge(&[period(0, 10, "tentative"), period(5, 15, "confirmed"), period(15, 20, "confirmed")]);
        assert_eq!(merged, vec![period(0, 5, "tentative"), period(5, 20, "confirmed")]);
        assert_eq!(merge(&[period(0, 5, "confirmed"), period(7, 9, "confirmed")]).len(), 2, "a gap stays");
    }

    #[test]
    fn what_makes_someone_busy() {
        let me = vec!["mailto:mini@example.org".to_owned()];
        let event = |value: Value| value.as_object().unwrap().clone();
        assert_eq!(busy_status(&event(json!({})), false, None, &me), Some("confirmed"));
        assert_eq!(busy_status(&event(json!({ "freeBusyStatus": "free" })), false, None, &me), None);
        assert_eq!(
            busy_status(&event(json!({ "freeBusyStatus": "free" })), false, Some("busy"), &me),
            Some("confirmed")
        );
        assert_eq!(busy_status(&event(json!({ "privacy": "secret" })), false, None, &me), None);
        assert_eq!(busy_status(&event(json!({ "status": "cancelled" })), false, None, &me), None);
        assert_eq!(busy_status(&event(json!({ "status": "tentative" })), false, None, &me), Some("tentative"));
        let invited = |status: &str| {
            event(
                json!({ "participants": { "m": { "calendarAddress": "mailto:MINI@example.org", "participationStatus": status } } }),
            )
        };
        assert_eq!(busy_status(&invited("needs-action"), true, None, &me), None);
        assert_eq!(busy_status(&invited("accepted"), true, None, &me), Some("confirmed"));
        assert_eq!(busy_status(&invited("tentative"), true, None, &me), Some("tentative"));
        assert_eq!(busy_status(&event(json!({})), true, None, &me), None, "not a participant");
    }
}
