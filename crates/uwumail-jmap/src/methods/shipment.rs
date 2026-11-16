//! Shipment/get, /changes, /query, /set and /rescan: the parcels found in shipping mail, and
//! what apps such as Home Assistant report about them. See docs/jmap-shipments.md.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Tz;
use serde_json::{Map, Value, json};
use uwumail_labels::{Carrier, ShipmentStatus};
use uwumail_store::{
    MailboxRole, NewShipmentEvent, SHIPMENT_MAX_EVENTS, SHIPMENT_RESCAN_DEFAULT_DAYS, SHIPMENT_RESCAN_MAX_DAYS,
    Shipment, ShipmentRescan, ShipmentUpdate, StoreError,
};

use super::{
    Ctx, SetResponse, check_filter_size, check_set_size, check_sort_size, get_ids, if_in_state, pick, properties,
    query_response,
};
use crate::error::{MethodError, MethodResult, SetError};
use crate::{MAX_OBJECTS_IN_GET, dates, ids};

const DEFAULTS: &[&str] = &[
    "id",
    "carrier",
    "number",
    "trackingUrl",
    "title",
    "merchant",
    "orderRef",
    "status",
    "statusAt",
    "expected",
    "deliveredAt",
    "pickup",
    "hidden",
    "emailIds",
    "events",
    "created",
    "updated",
    "lookedUpAt",
    "lookupError",
];

/// The most parcels one Shipment/refresh asks about; each may take up to ten seconds.
const MAX_REFRESH: usize = 20;

/// In someone else's account, the shipments the caller has the `needed` rights for: those of a
/// mailbox their mail lies in, as for the mail itself. One added by hand goes by the Inbox. In
/// the caller's own account, all of them.
pub(crate) async fn allowed(ctx: &Ctx<'_>, shipments: Vec<Shipment>, needed: &str) -> MethodResult<Vec<Shipment>> {
    let Some(view) = ctx.shared.as_ref() else { return Ok(shipments) };
    let store = &ctx.jmap.store;
    let mails: Vec<i64> = shipments.iter().flat_map(Shipment::email_ids).collect();
    let mailboxes: HashMap<i64, Vec<i64>> =
        store.emails_by_ids(ctx.account.id, mails).await?.into_iter().map(|mail| (mail.id, mail.mailbox_ids)).collect();
    let by_inbox = inbox_allows(ctx, needed).await?;
    Ok(shipments
        .into_iter()
        .filter(|shipment| {
            let mails = shipment.email_ids();
            if mails.is_empty() {
                return by_inbox;
            }
            mails.iter().filter_map(|mail| mailboxes.get(mail)).flatten().any(|mailbox| view.may(*mailbox, needed))
        })
        .map(|mut shipment| {
            // Only the events (and so the mail ids) of mail the caller may read, and the apps' own.
            let readable =
                |mail: &i64| mailboxes.get(mail).is_some_and(|boxes| boxes.iter().any(|b| view.may(*b, "r")));
            shipment.events.retain(|event| event.email_id.is_none_or(|mail| readable(&mail)));
            shipment
        })
        .collect())
}

/// Whether the caller has the `needed` rights on the Inbox of the account.
async fn inbox_allows(ctx: &Ctx<'_>, needed: &str) -> MethodResult<bool> {
    let Some(view) = ctx.shared.as_ref() else { return Ok(true) };
    let mailboxes = ctx.jmap.store.mailboxes(ctx.account.id).await?;
    Ok(mailboxes.iter().find(|m| m.role == Some(MailboxRole::Inbox)).is_some_and(|inbox| view.may(inbox.id, needed)))
}

/// Whether the caller may change shipment `number`: not found when they may not see it.
async fn may_change(ctx: &Ctx<'_>, number: i64) -> Result<(), SetError> {
    if ctx.shared.is_none() {
        return Ok(());
    }
    let found = ctx.jmap.store.shipments(ctx.account.id, Some(vec![number])).await.map_err(refused)?;
    if allowed(ctx, found.clone(), "r").await.map_err(|_| SetError::not_found())?.is_empty() {
        return Err(SetError::not_found());
    }
    if allowed(ctx, found, "w").await.map_err(|_| SetError::not_found())?.is_empty() {
        return Err(SetError::new("forbidden", "you may only read this account's shipments"));
    }
    Ok(())
}

fn id(number: i64) -> String {
    format!("s{number}")
}

/// A wall-clock time in the person's time zone as RFC 3339 with its offset, such as
/// `2026-11-15T10:15:00+01:00`.
fn local(day: &str, time: &str, tz: Tz) -> Option<String> {
    let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let time = NaiveTime::parse_from_str(time, "%H:%M").ok()?;
    let at = tz.from_local_datetime(&day.and_time(time)).earliest()?;
    Some(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

/// `{from, to, allDay}`. A day without a time window runs from its midnight to the next.
fn expected(shipment: &Shipment, tz: Tz) -> Value {
    let Some(day) = shipment.expected_day.as_deref() else { return Value::Null };
    match (&shipment.expected_from, &shipment.expected_to) {
        (None, None) => {
            let next = NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.succ_opt())
                .map(|d| d.format("%Y-%m-%d").to_string());
            json!({
                "from": local(day, "00:00", tz),
                "to": next.and_then(|next| local(&next, "00:00", tz)),
                "allDay": true,
            })
        }
        (from, to) => json!({
            "from": local(day, from.as_deref().unwrap_or("00:00"), tz),
            "to": to.as_deref().and_then(|to| local(day, to, tz)),
            "allDay": false,
        }),
    }
}

fn to_json(shipment: &Shipment, tz: Tz) -> Map<String, Value> {
    let events: Vec<Value> = shipment
        .events
        .iter()
        .map(|event| {
            let mut object = json!({
                "at": dates::format(event.at),
                "status": event.status.id(),
                "text": event.text,
                "location": event.location,
                "source": event.source,
            });
            if let Some(email) = event.email_id {
                object["emailId"] = json!(ids::email(email));
            }
            if let Some(client) = &event.client_name {
                object["clientName"] = json!(client);
            }
            object
        })
        .collect();
    let Value::Object(map) = json!({
        "id": id(shipment.id),
        "carrier": shipment.carrier,
        "number": shipment.number,
        "trackingUrl": shipment.tracking_url,
        "title": shipment.title,
        "merchant": shipment.merchant,
        "orderRef": shipment.order_ref,
        "status": shipment.status.id(),
        "statusAt": dates::format(shipment.status_at),
        "expected": expected(shipment, tz),
        "deliveredAt": shipment.delivered_at.map(dates::format),
        "pickup": shipment.pickup,
        "hidden": shipment.hidden,
        "emailIds": shipment.email_ids().into_iter().map(ids::email).collect::<Vec<_>>(),
        "events": events,
        "created": dates::format(shipment.created_at),
        "updated": dates::format(shipment.updated_at),
        "lookedUpAt": shipment.looked_up_at.map(dates::format),
        "lookupError": shipment.lookup_error,
    }) else {
        unreachable!("object literal")
    };
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let tz = ctx.jmap.store.shipment_timezone(ctx.account.id).await?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    match get_ids(args)? {
        None => {
            let all = allowed(ctx, ctx.jmap.store.shipments(ctx.account.id, None).await?, "r").await?;
            list.extend(all.iter().map(|shipment| pick(to_json(shipment, tz), &properties)));
        }
        Some(requested) => {
            if requested.len() > MAX_OBJECTS_IN_GET {
                return Err(MethodError::kind("requestTooLarge"));
            }
            let wanted: Vec<i64> = requested.iter().filter_map(|id| ctx.parse_id('s', id)).collect();
            let found = allowed(ctx, ctx.jmap.store.shipments(ctx.account.id, Some(wanted)).await?, "r").await?;
            for wanted in requested {
                match ctx.parse_id('s', &wanted).and_then(|n| found.iter().find(|shipment| shipment.id == n)) {
                    Some(shipment) => list.push(pick(to_json(shipment, tz), &properties)),
                    None => not_found.push(wanted),
                }
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

/// Delivered, returned and cancelled shipments are done; everything else is on its way.
fn active(status: ShipmentStatus) -> bool {
    !matches!(status, ShipmentStatus::Delivered | ShipmentStatus::Returned | ShipmentStatus::Cancelled)
}

/// When it is expected, as a sort key: its start, or the end of time when nobody knows.
fn expected_at(shipment: &Shipment, tz: Tz) -> i64 {
    expected(shipment, tz)["from"].as_str().and_then(dates::parse).unwrap_or(i64::MAX)
}

fn matches(shipment: &Shipment, filter: &Value, tz: Tz) -> MethodResult<bool> {
    let Some(filter) = filter.as_object() else {
        return Err(MethodError::new("unsupportedFilter", "a filter is an object"));
    };
    if let Some(operator) = filter.get("operator") {
        let conditions = filter.get("conditions").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let mut results = Vec::with_capacity(conditions.len());
        for condition in conditions {
            results.push(matches(shipment, condition, tz)?);
        }
        return match operator.as_str() {
            Some("AND") => Ok(results.iter().all(|&r| r)),
            Some("OR") => Ok(results.iter().any(|&r| r)),
            Some("NOT") => Ok(!results.iter().any(|&r| r)),
            _ => Err(MethodError::new("unsupportedFilter", "operator must be AND, OR or NOT")),
        };
    }
    let unsupported = |what: &str| MethodError::new("unsupportedFilter", what.to_owned());
    for (key, value) in filter {
        let ok = match key.as_str() {
            "status" => {
                let list = value.as_array().ok_or_else(|| unsupported("status is a list"))?;
                list.iter().any(|status| status.as_str() == Some(shipment.status.id()))
            }
            "active" => {
                value.as_bool().ok_or_else(|| unsupported("active is true or false"))? == active(shipment.status)
            }
            "hidden" => value.as_bool().ok_or_else(|| unsupported("hidden is true or false"))? == shipment.hidden,
            "carrier" => Some(shipment.carrier.as_str()) == value.as_str(),
            "deliveredAfter" => {
                let after =
                    value.as_str().and_then(dates::parse).ok_or_else(|| unsupported("deliveredAfter is a date"))?;
                shipment.delivered_at.is_some_and(|at| at > after)
            }
            "expectedBefore" => {
                let before =
                    value.as_str().and_then(dates::parse).ok_or_else(|| unsupported("expectedBefore is a date"))?;
                expected_at(shipment, tz) < before
            }
            other => return Err(unsupported(&format!("{other} is not a Shipment filter"))),
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let filter = args.get("filter").filter(|filter| !filter.is_null());
    check_filter_size(filter)?;
    let sort = args.get("sort").and_then(Value::as_array).cloned().unwrap_or_default();
    check_sort_size(&sort)?;
    let state = ctx.state().await?;
    let tz = ctx.jmap.store.shipment_timezone(ctx.account.id).await?;
    let mut shipments = Vec::new();
    for shipment in allowed(ctx, ctx.jmap.store.shipments(ctx.account.id, None).await?, "r").await? {
        if filter.map_or(Ok(true), |filter| matches(&shipment, filter, tz))? {
            shipments.push(shipment);
        }
    }
    // Newest change first unless asked otherwise.
    shipments.sort_by_key(|shipment| std::cmp::Reverse((shipment.updated_at, shipment.id)));
    for comparator in sort.iter().rev() {
        let ascending = comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true);
        let by_expected = match comparator.get("property").and_then(Value::as_str) {
            Some("expected") => true,
            Some("updated") => false,
            _ => return Err(MethodError::new("unsupportedSort", "sort by expected or updated")),
        };
        let key = |shipment: &Shipment| if by_expected { expected_at(shipment, tz) } else { shipment.updated_at };
        shipments.sort_by(|a, b| if ascending { key(a).cmp(&key(b)) } else { key(b).cmp(&key(a)) });
    }
    let ids = shipments.iter().map(|shipment| id(shipment.id)).collect();
    query_response(ctx, args, state, ids, MAX_OBJECTS_IN_GET)
}

/// The name an app reports events under: its app password's or its OAuth app's.
async fn client_name(ctx: &Ctx<'_>) -> Option<String> {
    let credential = ctx.credential.as_deref()?;
    let store = &ctx.jmap.store;
    let result = if let Some(grant) = credential.strip_prefix("oauth:").and_then(|g| g.parse::<i64>().ok()) {
        store.oauth_grant_client_name(grant).await
    } else {
        let app = credential.strip_prefix("app:")?.parse::<i64>().ok()?;
        store
            .app_passwords(ctx.account.id)
            .await
            .map(|apps| apps.into_iter().find(|password| password.id == app).map(|password| password.name))
    };
    result.unwrap_or_else(|err| {
        tracing::warn!(%err, "reading the name of an app failed");
        None
    })
}

fn text<'v>(object: &'v Map<String, Value>, property: &'static str) -> Result<Option<&'v str>, SetError> {
    match object.get(property) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(SetError::invalid_properties(&[property], format!("{property} must be a string"))),
    }
}

/// How far ahead an event's `at` may be; the store counts one ahead of now as now.
const MAX_AHEAD_SECS: i64 = 365 * 86_400;

fn events(value: &Value) -> Result<Vec<NewShipmentEvent>, SetError> {
    let invalid = |message: &str| SetError::invalid_properties(&["addEvents"], message.to_owned());
    let list = value.as_array().ok_or_else(|| invalid("addEvents is a list"))?;
    if list.len() > SHIPMENT_MAX_EVENTS as usize {
        return Err(invalid(&format!("at most {SHIPMENT_MAX_EVENTS} events at once")));
    }
    let mut events = Vec::with_capacity(list.len());
    for event in list {
        let event = event.as_object().ok_or_else(|| invalid("an event is an object"))?;
        if let Some(key) = event.keys().find(|key| !matches!(key.as_str(), "at" | "status" | "text" | "location")) {
            return Err(invalid(&format!("an event has no {key}")));
        }
        let at =
            event.get("at").and_then(Value::as_str).and_then(dates::parse).ok_or_else(|| invalid("at is a date"))?;
        if at > chrono::Utc::now().timestamp() + MAX_AHEAD_SECS {
            return Err(invalid("at is more than a year ahead"));
        }
        let status = event
            .get("status")
            .and_then(Value::as_str)
            .and_then(ShipmentStatus::from_id)
            .ok_or_else(|| invalid("status is not a shipment status"))?;
        let field = |key: &'static str| match event.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(invalid(&format!("{key} must be a string"))),
        };
        events.push(NewShipmentEvent { at, status, text: field("text")?, location: field("location")? });
    }
    Ok(events)
}

/// `removeEvents`: events an app added before, each by `at` and `status`.
fn removals(value: &Value) -> Result<Vec<(i64, ShipmentStatus)>, SetError> {
    let invalid = |message: &str| SetError::invalid_properties(&["removeEvents"], message.to_owned());
    let list = value.as_array().ok_or_else(|| invalid("removeEvents is a list"))?;
    if list.len() > SHIPMENT_MAX_EVENTS as usize {
        return Err(invalid(&format!("at most {SHIPMENT_MAX_EVENTS} events at once")));
    }
    list.iter()
        .map(|event| {
            let event = event.as_object().ok_or_else(|| invalid("an event is an object"))?;
            if let Some(key) = event.keys().find(|key| !matches!(key.as_str(), "at" | "status")) {
                return Err(invalid(&format!("an event to remove has no {key}")));
            }
            let at = event
                .get("at")
                .and_then(Value::as_str)
                .and_then(dates::parse)
                .ok_or_else(|| invalid("at is a date"))?;
            let status = event
                .get("status")
                .and_then(Value::as_str)
                .and_then(ShipmentStatus::from_id)
                .ok_or_else(|| invalid("status is not a shipment status"))?;
            Ok((at, status))
        })
        .collect()
}

/// The store's refusals as JMAP knows them.
fn refused(err: StoreError) -> SetError {
    match err {
        StoreError::Rule { code: "shipmentsOff", message } => SetError::new("forbidden", message),
        other => SetError::from(other),
    }
}

const UPDATABLE: &[&str] = &["title", "hidden", "addEvents", "removeEvents"];
const CREATABLE: &[&str] = &["carrier", "number", "title"];

fn check_known(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), SetError> {
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(SetError::invalid_properties(&[key.as_str()], format!("{key} cannot be set"))),
        None => Ok(()),
    }
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_set_size(args)?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let store = ctx.jmap.store.clone();
    let account_id = ctx.account.id;
    let tz = store.shipment_timezone(account_id).await?;
    let mut response = SetResponse::default();
    let wants_client = args.get("create").is_some_and(|c| c.as_object().is_some_and(|c| !c.is_empty()))
        || args.get("update").is_some();
    let client = if wants_client { client_name(ctx).await } else { None };

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result: Result<i64, SetError> = async {
                let object = object.as_object().ok_or_else(|| SetError::new("invalidProperties", "not an object"))?;
                check_known(object, CREATABLE)?;
                let carrier = text(object, "carrier")?
                    .and_then(Carrier::from_id)
                    .ok_or_else(|| SetError::invalid_properties(&["carrier"], "carrier is not a known carrier"))?;
                let number = text(object, "number")?
                    .ok_or_else(|| SetError::invalid_properties(&["number"], "number is required"))?
                    .to_owned();
                let title = text(object, "title")?.map(str::to_owned);
                if !inbox_allows(ctx, "w").await.map_err(|_| SetError::not_found())? {
                    return Err(SetError::new("forbidden", "adding a shipment needs the right to change the Inbox"));
                }
                store.create_shipment(account_id, carrier, number, title, client.clone()).await.map_err(refused)
            }
            .await;
            match result {
                Ok(number) => {
                    let created = id(number);
                    ctx.created_ids.insert(creation_id.clone(), created.clone());
                    let shipment = store.shipments(account_id, Some(vec![number])).await?;
                    let mut object = json!({ "id": created });
                    if let Some(shipment) = shipment.first() {
                        let full = to_json(shipment, tz);
                        for key in ["trackingUrl", "status", "statusAt", "created", "updated"] {
                            object[key] = full[key].clone();
                        }
                    }
                    response.created.insert(creation_id.clone(), object);
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (wanted, patch) in update {
            let result: Result<(), SetError> = async {
                let number = ctx.parse_id('s', wanted).ok_or_else(SetError::not_found)?;
                let patch = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "not an object"))?;
                check_known(patch, UPDATABLE)?;
                let hidden = match patch.get("hidden") {
                    None => None,
                    Some(Value::Bool(hidden)) => Some(*hidden),
                    Some(_) => return Err(SetError::invalid_properties(&["hidden"], "hidden is true or false")),
                };
                let change = ShipmentUpdate {
                    title: patch
                        .contains_key("title")
                        .then(|| text(patch, "title"))
                        .transpose()?
                        .map(|t| t.map(str::to_owned)),
                    hidden,
                    add_events: patch.get("addEvents").map(events).transpose()?.unwrap_or_default(),
                    remove_events: patch.get("removeEvents").map(removals).transpose()?.unwrap_or_default(),
                };
                may_change(ctx, number).await?;
                store.update_shipment(account_id, number, change, client.clone()).await.map_err(refused)
            }
            .await;
            match result {
                Ok(()) => {
                    response.updated.insert(wanted.clone(), Value::Null);
                }
                Err(err) => {
                    response.not_updated.insert(wanted.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for wanted in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('s', wanted) {
                Some(number) => match may_change(ctx, number).await {
                    Ok(()) => store.destroy_shipment(account_id, number).await.map_err(refused),
                    Err(err) => Err(err),
                },
                None => Err(SetError::not_found()),
            };
            match result {
                Ok(()) => response.destroyed.push(wanted.to_owned()),
                Err(err) => {
                    response.not_destroyed.insert(wanted.to_owned(), err.to_json());
                }
            }
        }
    }

    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

fn rescan_json(rescan: ShipmentRescan) -> Value {
    json!({
        "since": dates::format(rescan.since),
        "startedAt": dates::format(rescan.started_at),
        "found": rescan.found,
        "running": rescan.running,
    })
}

/// Starts a look back through the last `days` of mail and answers with how it stands.
pub async fn rescan(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let days = match args.get("days") {
        None | Some(Value::Null) => SHIPMENT_RESCAN_DEFAULT_DAYS,
        Some(value) => match value.as_i64() {
            Some(days) if (1..=SHIPMENT_RESCAN_MAX_DAYS).contains(&days) => days,
            _ => {
                return Err(MethodError::invalid_arguments(format!(
                    "days is a number from 1 to {SHIPMENT_RESCAN_MAX_DAYS}"
                )));
            }
        },
    };
    let rescan = match ctx.jmap.store.start_shipment_rescan(ctx.account.id, days).await {
        Ok(rescan) => rescan,
        Err(StoreError::Rule { code: "shipmentsOff", message }) => return Err(MethodError::new("forbidden", message)),
        Err(err) => return Err(err.into()),
    };
    Ok(json!({ "accountId": ctx.account_id(), "rescan": rescan_json(rescan) }))
}

/// Asks the carriers about the parcels now, at most once in five minutes each. What they say
/// reaches clients the usual way, through the Shipment state.
pub async fn refresh(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let Some(requested) = get_ids(args)? else {
        return Err(MethodError::invalid_arguments("ids is a list of shipment ids"));
    };
    if requested.len() > MAX_REFRESH {
        return Err(MethodError::kind("requestTooLarge"));
    }
    let store = &ctx.jmap.store;
    let on = store.shipment_lookup_enabled(ctx.account.id).await?;
    let wanted: Vec<i64> = requested.iter().filter_map(|id| ctx.parse_id('s', id)).collect();
    let visible = allowed(ctx, store.shipments(ctx.account.id, Some(wanted)).await?, "r").await?;
    let mut refreshed = Vec::new();
    let mut not_refreshed = Map::new();
    for wanted in requested {
        let number = ctx.parse_id('s', &wanted).filter(|n| visible.iter().any(|shipment| shipment.id == *n));
        let outcome = match number {
            None => Err("notFound"),
            Some(_) if !on => Err("notSupported"),
            Some(number) => {
                match ctx.jmap.smtp.parcels().refresh(store, ctx.account.id, number, crate::jscal::now()).await {
                    Ok(outcome) => outcome,
                    Err(StoreError::NotFound(_)) => Err("notFound"),
                    Err(err) => return Err(err.into()),
                }
            }
        };
        match outcome {
            Ok(()) => refreshed.push(json!(wanted)),
            Err(kind) => {
                not_refreshed.insert(wanted, json!({ "type": kind }));
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "refreshed": refreshed, "notRefreshed": not_refreshed }))
}
