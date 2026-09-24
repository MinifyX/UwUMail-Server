//! Calendar/get, Calendar/set and ParticipantIdentity (draft-ietf-jmap-calendars, sections 3
//! and 4) on the account's CalDAV calendars. See docs/jmap-calendars.md.

use serde_json::{Map, Value, json};
use uwumail_store::{DavCollection, DavCollectionUpdate, DavKind, NewDavCollection, StoreError};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;

const DEFAULTS: &[&str] = &[
    "id",
    "name",
    "description",
    "color",
    "sortOrder",
    "isSubscribed",
    "isVisible",
    "isDefault",
    "includeInAvailability",
    "defaultAlertsWithTime",
    "defaultAlertsWithoutTime",
    "timeZone",
    "shareWith",
    "myRights",
];

/// The longest calendar name, in bytes, as the draft allows.
const MAX_NAME_BYTES: usize = 255;
const MAX_DESCRIPTION_BYTES: usize = 10_000;

/// The account's calendars, the default one made the first time like CalDAV does. Lists that only
/// hold tasks, like the reminders of Apple's devices, are not calendars of events and stay out.
pub async fn calendars(ctx: &Ctx<'_>) -> MethodResult<Vec<DavCollection>> {
    let name = ctx.jmap.smtp.tone().language.collection_names().0;
    let all = ctx
        .jmap
        .store
        .dav_collections(ctx.account.id, DavKind::Calendar, NewDavCollection::default_calendar(name))
        .await?;
    Ok(all.into_iter().filter(holds_events).collect())
}

fn holds_events(calendar: &DavCollection) -> bool {
    calendar.components.is_empty() || calendar.components.iter().any(|kind| kind.eq_ignore_ascii_case("VEVENT"))
}

/// The calendars are switched off for the account the way CalDAV is.
pub fn check_enabled(ctx: &Ctx<'_>) -> MethodResult<()> {
    if ctx.account.protocols.caldav {
        Ok(())
    } else {
        Err(MethodError::new("accountNotSupportedByMethod", "calendars are switched off for this account"))
    }
}

/// `#RRGGBB` or `#RRGGBBAA` as CalDAV clients store it, as CSS `#rrggbb`.
pub fn css_color(stored: Option<&str>) -> Option<String> {
    let hex = stored?.strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", hex[..6].to_ascii_lowercase()))
}

/// A CSS `#rgb` or `#rrggbb` as the `#RRGGBBAA` Apple's calendar-color uses.
fn stored_color(css: &str) -> Option<String> {
    let hex = css.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let full = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_owned(),
        _ => return None,
    };
    Some(format!("#{}FF", full.to_ascii_uppercase()))
}

fn rights(may_delete: bool) -> Value {
    json!({
        "mayReadFreeBusy": true,
        "mayReadItems": true,
        "mayWriteAll": true,
        "mayWriteOwn": true,
        "mayUpdatePrivate": true,
        "mayRSVP": true,
        // Calendars are not shared here, so nobody may change who they are shared with.
        "mayShare": false,
        "mayDelete": may_delete
    })
}

fn to_json(calendar: &DavCollection, only_one: bool) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": ids::calendar(calendar.id),
        "name": calendar.display_name,
        "description": (!calendar.description.is_empty()).then_some(&calendar.description),
        "color": css_color(calendar.color.as_deref()),
        "sortOrder": calendar.sort_order.max(0),
        "isSubscribed": true,
        "isVisible": calendar.is_visible,
        "isDefault": calendar.is_default,
        "includeInAvailability": "all",
        "defaultAlertsWithTime": null,
        "defaultAlertsWithoutTime": null,
        "timeZone": calendar.timezone.as_deref().and_then(uwumail_store::ical::timezone_id),
        "shareWith": null,
        "myRights": rights(!only_one),
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let list = calendars(ctx).await?;
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let only_one = list.len() <= 1;
    let (found, not_found) = match get_ids(args)? {
        None => (list.iter().map(|c| pick(to_json(c, only_one), &properties)).collect::<Vec<_>>(), Vec::new()),
        Some(requested) => {
            let mut found = Vec::new();
            let mut not_found = Vec::new();
            for id in requested {
                match ctx.parse_id('c', &id).and_then(|n| list.iter().find(|c| c.id == n)) {
                    Some(calendar) => found.push(pick(to_json(calendar, only_one), &properties)),
                    None => not_found.push(id),
                }
            }
            (found, not_found)
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": found, "notFound": not_found }))
}

/// The properties of a create or update, checked. `creating` insists on a name.
fn parse_properties(object: &Map<String, Value>, creating: bool) -> Result<DavCollectionUpdate, SetError> {
    let mut update = DavCollectionUpdate::default();
    let mut bad: Vec<&str> = Vec::new();
    for (key, value) in object {
        match key.as_str() {
            "name" => match value.as_str().map(str::trim) {
                Some(name)
                    if !name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control) =>
                {
                    update.display_name = Some(name.to_owned())
                }
                _ => bad.push("name"),
            },
            "description" => match value {
                Value::Null => update.description = Some(String::new()),
                Value::String(text) if text.len() <= MAX_DESCRIPTION_BYTES => update.description = Some(text.clone()),
                _ => bad.push("description"),
            },
            "color" => match value {
                Value::Null => update.color = Some(None),
                Value::String(css) => match stored_color(css) {
                    Some(color) => update.color = Some(Some(color)),
                    None => bad.push("color"),
                },
                _ => bad.push("color"),
            },
            "sortOrder" => match value.as_u64() {
                Some(order) if order < (1 << 31) => update.sort_order = Some(order as i64),
                _ => bad.push("sortOrder"),
            },
            "isVisible" => match value.as_bool() {
                Some(visible) => update.is_visible = Some(visible),
                None => bad.push("isVisible"),
            },
            "timeZone" => match value {
                Value::Null => update.timezone = Some(None),
                Value::String(name) if crate::jscal::time_zone(name).is_some() => {
                    match uwumail_store::ical::timezone_calendar(name, crate::jscal::now()) {
                        Some(calendar) => update.timezone = Some(Some(calendar)),
                        None => bad.push("timeZone"),
                    }
                }
                _ => bad.push("timeZone"),
            },
            // What this server has only one answer to may be sent with that answer.
            "isSubscribed" if value == &Value::Bool(true) => {}
            "includeInAvailability" if value.as_str() == Some("all") => {}
            "defaultAlertsWithTime" | "defaultAlertsWithoutTime" | "shareWith" if value.is_null() => {}
            _ => bad.push(key.as_str()),
        }
    }
    if creating && update.display_name.is_none() && !bad.contains(&"name") {
        bad.push("name");
    }
    if bad.is_empty() {
        Ok(update)
    } else {
        Err(SetError::invalid_properties(&bad, "these properties are missing, not valid or cannot be set here"))
    }
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    check_set_size(args)?;
    let known = calendars(ctx).await?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let store = ctx.jmap.store.clone();
    let account_id = ctx.account.id;
    let mut response = SetResponse::default();
    let mut failed = false;

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result: Result<DavCollection, SetError> = async {
                let object =
                    object.as_object().ok_or_else(|| SetError::new("invalidProperties", "must be an object"))?;
                let update = parse_properties(object, true)?;
                let slug = random_slug();
                let new = NewDavCollection {
                    slug,
                    display_name: update.display_name.clone().unwrap_or_default(),
                    description: update.description.clone().unwrap_or_default(),
                    color: update.color.clone().flatten(),
                    components: vec!["VEVENT".into(), "VTODO".into()],
                };
                store.create_calendar(account_id, new, update).await.map_err(|err| match err {
                    StoreError::Rule { code: "davCollectionsFull", message } => SetError::new("overQuota", message),
                    other => SetError::from(other),
                })
            }
            .await;
            match result {
                Ok(calendar) => {
                    let id = ids::calendar(calendar.id);
                    ctx.created_ids.insert(creation_id.clone(), id.clone());
                    // What the client did not send: the id, what the server set, the defaults.
                    let mut created = to_json(&calendar, false);
                    for key in object.as_object().into_iter().flat_map(Map::keys) {
                        created.remove(key);
                    }
                    created.insert("id".into(), json!(id));
                    response.created.insert(creation_id.clone(), Value::Object(created));
                }
                Err(err) => {
                    failed = true;
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result: Result<(), SetError> = async {
                let calendar_id = ctx.parse_id('c', id).ok_or_else(SetError::not_found)?;
                if !known.iter().any(|calendar| calendar.id == calendar_id) {
                    return Err(SetError::not_found());
                }
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                let changes = parse_properties(patch, false)?;
                store.dav_update_collection(account_id, calendar_id, changes).await.map_err(|err| match err {
                    StoreError::NotFound(_) => SetError::not_found(),
                    other => SetError::from(other),
                })?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    response.updated.insert(id.clone(), Value::Null);
                }
                Err(err) => {
                    failed = true;
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        let with_events = args.get("onDestroyRemoveEvents").and_then(Value::as_bool).unwrap_or(false);
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('c', id) {
                Some(calendar_id) => {
                    store.destroy_calendar(account_id, calendar_id, with_events).await.map_err(SetError::from)
                }
                None => Err(SetError::not_found()),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    failed = true;
                    response.not_destroyed.insert(id.to_owned(), err.to_json());
                }
            }
        }
    }

    // Only when everything else worked; an id that does not resolve is ignored, as the draft says.
    if !failed && let Some(wanted) = args.get("onSuccessSetIsDefault").and_then(Value::as_str) {
        let before = calendars(ctx).await?;
        if let Some(calendar_id) =
            ctx.parse_id('c', wanted).filter(|n| before.iter().any(|c| c.id == *n && !c.is_default))
        {
            store.set_default_calendar(account_id, calendar_id).await?;
            for calendar in before.iter().filter(|c| c.is_default || c.id == calendar_id) {
                let id = ids::calendar(calendar.id);
                let is_default = calendar.id == calendar_id;
                match response.created.values_mut().find(|created| created["id"] == id) {
                    Some(created) => created["isDefault"] = json!(is_default),
                    None => {
                        response.updated.insert(id, json!({ "isDefault": is_default }));
                    }
                }
            }
        }
    }

    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

/// The last segment of a new calendar's CalDAV URL.
fn random_slug() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("the operating system RNG failed");
    hex::encode(bytes)
}

// ------------------------------------------------------------------------------------------------
// ParticipantIdentity: the account itself, the one address events know it by.

pub async fn identities_get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let properties = properties(args, "properties", &["id", "name", "calendarAddress", "isDefault"])?;
    let id = ids::participant(ctx.account.id);
    let Value::Object(identity) = json!({
        "id": id,
        "name": ctx.account.display_name,
        "calendarAddress": format!("mailto:{}", ctx.account.login),
        "isDefault": true,
    }) else {
        unreachable!("object literal")
    };
    let (list, not_found): (Vec<Value>, Vec<String>) = match get_ids(args)? {
        None => (vec![pick(identity, &properties)], Vec::new()),
        Some(requested) => {
            let found = requested.contains(&id);
            (
                if found { vec![pick(identity, &properties)] } else { Vec::new() },
                requested.into_iter().filter(|r| *r != id).collect(),
            )
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

pub async fn identities_set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    check_set_size(args)?;
    let state = ctx.state().await?;
    if_in_state(args, &state)?;
    let mut response = SetResponse::default();
    let forbidden = || SetError::new("forbidden", "the participant identity is the account itself").to_json();
    for creation_id in args.get("create").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
        response.not_created.insert(creation_id.clone(), forbidden());
    }
    for id in args.get("update").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
        response.not_updated.insert(id.clone(), forbidden());
    }
    for id in args.get("destroy").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        response.not_destroyed.insert(id.to_owned(), forbidden());
    }
    Ok(response.finish(ctx.account_id(), state.clone(), state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_between_css_and_caldav() {
        assert_eq!(css_color(Some("#FF4D8DFF")).as_deref(), Some("#ff4d8d"));
        assert_eq!(css_color(Some("#FF4D8D")).as_deref(), Some("#ff4d8d"));
        assert_eq!(css_color(Some("pink")), None);
        assert_eq!(stored_color("#abc").as_deref(), Some("#AABBCCFF"));
        assert_eq!(stored_color("#12ab9f").as_deref(), Some("#12AB9FFF"));
        assert_eq!(stored_color("#12ab9fzz"), None);
        assert_eq!(stored_color("red"), None);
    }

    #[test]
    fn properties_are_checked() {
        let object = |value: Value| value.as_object().unwrap().clone();
        let update =
            parse_properties(&object(json!({ "name": " Arbeit ", "color": "#00ff00", "isVisible": false })), true)
                .unwrap();
        assert_eq!(update.display_name.as_deref(), Some("Arbeit"));
        assert_eq!(update.color, Some(Some("#00FF00FF".into())));
        let err = parse_properties(&object(json!({ "color": "green", "shareWith": {} })), true).unwrap_err();
        assert_eq!(err.properties, Some(vec!["color".into(), "shareWith".into(), "name".into()]));
        let err = parse_properties(&object(json!({ "timeZone": "Europe/Atlantis" })), false).unwrap_err();
        assert_eq!(err.properties, Some(vec!["timeZone".into()]));
        assert!(parse_properties(&object(json!({ "name": "x".repeat(256) })), false).is_err());
    }
}
