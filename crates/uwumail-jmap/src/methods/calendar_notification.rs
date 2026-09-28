//! CalendarEventNotification/get, /set, /query and /changes (draft-ietf-jmap-calendars,
//! section 7): what others did to the account's events, from the store's notifications
//! (calendar_notifications.rs there). See docs/jmap-calendars.md.

use std::collections::HashSet;

use serde_json::{Map, Value, json};
use uwumail_store::{CalendarNotification, StoreError};

use super::calendar::check_enabled;
use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties, query_response};
use crate::error::{MethodError, MethodResult, SetError};
use crate::jscal;
use crate::sharing::principal_id;

const DEFAULTS: &[&str] =
    &["id", "created", "changedBy", "comment", "type", "calendarEventId", "isDraft", "event", "eventPatch"];

pub fn id(notification: i64) -> String {
    format!("n{notification}")
}

/// The event of a notification's iCalendar, as JSCalendar without what JMAP adds, and without the
/// per-user properties of the calendar's owner (alerts, colour, keywords and the like): those are
/// the owner's own, and the others who see the calendar hear of the notification too.
fn event_of(content: Option<&str>) -> Option<Map<String, Value>> {
    let parsed = jscal::from_icalendar(content?)?;
    let mut event = jscal::per_user_view(parsed.event(), None, std::collections::BTreeSet::new);
    event.remove("iCalendar");
    if let Some(Value::Object(overrides)) = event.get_mut("recurrenceOverrides") {
        for patch in overrides.values_mut().filter_map(Value::as_object_mut) {
            patch.remove("iCalendar");
        }
    }
    Some(event)
}

/// What changed between two versions of an event, as a PatchObject of its top-level properties.
fn patch_between(old: &Map<String, Value>, new: &Map<String, Value>) -> Map<String, Value> {
    let mut patch = Map::new();
    for key in old.keys().chain(new.keys()) {
        if old.get(key) != new.get(key) && !patch.contains_key(key) {
            patch.insert(jscal::escape_token(key), new.get(key).cloned().unwrap_or(Value::Null));
        }
    }
    patch
}

fn to_json(notification: &CalendarNotification) -> Map<String, Value> {
    let author = &notification.author;
    let old = event_of(notification.old_content.as_deref());
    let new = event_of(notification.new_content.as_deref());
    let (event, patch) = match notification.kind.as_str() {
        "created" => (new, None),
        "updated" => {
            let patch = old.as_ref().zip(new.as_ref()).map(|(old, new)| patch_between(old, new));
            (old, patch)
        }
        _ => (old, None),
    };
    let Value::Object(mut map) = json!({
        "id": id(notification.id),
        "created": jscal::format_utc(notification.created_at),
        "changedBy": {
            "name": author.name,
            "email": author.email,
            "principalId": author.account_id.map(principal_id),
            "calendarAddress": author.calendar_address,
        },
        "comment": author.comment,
        "type": notification.kind,
        "calendarEventId": crate::ids::calendar_event(notification.event_id),
        "event": event,
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    if notification.kind != "destroyed" {
        map.insert("isDraft".into(), json!(notification.is_draft));
    }
    if let Some(patch) = patch {
        map.insert("eventPatch".into(), Value::Object(patch));
    }
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let contents = properties.iter().any(|p| p == "event" || p == "eventPatch");
    let requested = get_ids(args)?;
    let numbers =
        requested.as_ref().map(|list| list.iter().filter_map(|text| ctx.parse_id('n', text)).collect::<Vec<_>>());
    let found = ctx.jmap.store.calendar_notifications(ctx.account.id, numbers, contents).await?;
    let list: Vec<Value> = found.iter().map(|n| pick(to_json(n), &properties)).collect();
    let seen: HashSet<String> = found.iter().map(|n| id(n.id)).collect();
    let not_found: Vec<String> = requested
        .unwrap_or_default()
        .into_iter()
        .filter(|text| !ctx.resolve(text).is_some_and(|id| seen.contains(id)))
        .collect();
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

/// Only destroying (dismissing) is possible; the server makes notifications.
pub async fn set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    check_set_size(args)?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();
    let forbidden =
        || SetError::new("forbidden", "notifications are made by the server; they can only be dismissed").to_json();
    for creation_id in args.get("create").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
        response.not_created.insert(creation_id.clone(), forbidden());
    }
    for id in args.get("update").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
        response.not_updated.insert(id.clone(), forbidden());
    }
    for text in args.get("destroy").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        let result = match ctx.parse_id('n', text) {
            Some(number) => {
                ctx.jmap.store.destroy_calendar_notification(ctx.account.id, number).await.map_err(|err| match err {
                    StoreError::NotFound(_) => SetError::not_found(),
                    other => SetError::from(other),
                })
            }
            None => Err(SetError::not_found()),
        };
        match result {
            Ok(()) => response.destroyed.push(text.to_owned()),
            Err(err) => {
                response.not_destroyed.insert(text.to_owned(), err.to_json());
            }
        }
    }
    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

/// A filter condition, checked.
#[derive(Default)]
struct Condition {
    after: Option<i64>,
    before: Option<i64>,
    kind: Option<String>,
    events: Option<Vec<String>>,
}

fn condition(filter: Option<&Value>) -> MethodResult<Condition> {
    let mut condition = Condition::default();
    let Some(filter) = filter.filter(|f| !f.is_null()) else { return Ok(condition) };
    let Value::Object(filter) = filter else {
        return Err(MethodError::new("unsupportedFilter", "a filter is one condition object"));
    };
    let time = |value: &Value, key: &str| match value {
        Value::Null => Ok(None),
        value => value
            .as_str()
            .and_then(jscal::parse_utc)
            .map(Some)
            .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a UTCDateTime"))),
    };
    for (key, value) in filter {
        match key.as_str() {
            "after" => condition.after = time(value, key)?,
            "before" => condition.before = time(value, key)?,
            "type" => {
                condition.kind = Some(
                    value
                        .as_str()
                        .ok_or_else(|| MethodError::new("unsupportedFilter", "type must be a text"))?
                        .to_owned(),
                )
            }
            "calendarEventIds" if value.is_null() => {}
            "calendarEventIds" => {
                let ids = value
                    .as_array()
                    .filter(|ids| ids.len() <= crate::MAX_OBJECTS_IN_GET)
                    .ok_or_else(|| MethodError::new("unsupportedFilter", "calendarEventIds must be a list of ids"))?;
                condition.events = Some(ids.iter().filter_map(Value::as_str).map(str::to_owned).collect());
            }
            other => return Err(MethodError::new("unsupportedFilter", format!("{other} is not a filter here"))),
        }
    }
    Ok(condition)
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let condition = condition(args.get("filter"))?;
    let mut descending = false;
    if let Some(sort) = args.get("sort").filter(|s| !s.is_null()) {
        let sort = sort.as_array().ok_or_else(|| MethodError::invalid_arguments("sort must be a list"))?;
        for comparator in sort {
            if comparator.get("property").and_then(Value::as_str) != Some("created") || sort.len() > 1 {
                return Err(MethodError::new("unsupportedSort", "notifications sort by created"));
            }
            descending = comparator.get("isAscending") == Some(&Value::Bool(false));
        }
    }
    let all = ctx.jmap.store.calendar_notifications(ctx.account.id, None, false).await?;
    let mut ids: Vec<String> = all
        .iter()
        .filter(|n| condition.after.is_none_or(|after| n.created_at >= after))
        .filter(|n| condition.before.is_none_or(|before| n.created_at < before))
        .filter(|n| condition.kind.as_ref().is_none_or(|kind| *kind == n.kind))
        .filter(|n| {
            condition.events.as_ref().is_none_or(|events| events.contains(&crate::ids::calendar_event(n.event_id)))
        })
        .map(|n| id(n.id))
        .collect();
    // Notifications are numbered as they come, so their ids are in the order they were made.
    if descending {
        ids.reverse();
    }
    query_response(ctx, args, state, ids, uwumail_store::MAX_CALENDAR_NOTIFICATIONS as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_name_what_changed() {
        let old = json!({ "title": "Yoga", "a/b": 1, "gone": true }).as_object().unwrap().clone();
        let new = json!({ "title": "Pilates", "a/b": 1 }).as_object().unwrap().clone();
        assert_eq!(Value::Object(patch_between(&old, &new)), json!({ "title": "Pilates", "gone": null }));
    }
}
