//! Shipment/get, /changes, /query, /set and /rescan: the parcels found in shipping mail, and
//! what apps such as Home Assistant report about them. See docs/jmap-shipments.md.

use chrono::{NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Europe::Berlin;
use serde_json::{Map, Value, json};
use uwumail_labels::{Carrier, ShipmentStatus};
use uwumail_store::{
    NewShipmentEvent, SHIPMENT_MAX_EVENTS, SHIPMENT_RESCAN_DEFAULT_DAYS, SHIPMENT_RESCAN_MAX_DAYS, Shipment,
    ShipmentRescan, ShipmentUpdate, StoreError,
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
];

fn id(number: i64) -> String {
    format!("s{number}")
}

/// A wall-clock time in Germany as RFC 3339 with its offset, such as `2026-11-15T10:15:00+01:00`.
fn berlin(day: &str, time: &str) -> Option<String> {
    let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let time = NaiveTime::parse_from_str(time, "%H:%M").ok()?;
    let at = Berlin.from_local_datetime(&day.and_time(time)).earliest()?;
    Some(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

/// `{from, to, allDay}`. A day without a time window runs from its midnight to the next.
fn expected(shipment: &Shipment) -> Value {
    let Some(day) = shipment.expected_day.as_deref() else { return Value::Null };
    match (&shipment.expected_from, &shipment.expected_to) {
        (None, None) => {
            let next = NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.succ_opt())
                .map(|d| d.format("%Y-%m-%d").to_string());
            json!({
                "from": berlin(day, "00:00"),
                "to": next.and_then(|next| berlin(&next, "00:00")),
                "allDay": true,
            })
        }
        (from, to) => json!({
            "from": berlin(day, from.as_deref().unwrap_or("00:00")),
            "to": to.as_deref().and_then(|to| berlin(day, to)),
            "allDay": false,
        }),
    }
}

fn to_json(shipment: &Shipment) -> Map<String, Value> {
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
        "expected": expected(shipment),
        "deliveredAt": shipment.delivered_at.map(dates::format),
        "pickup": shipment.pickup,
        "hidden": shipment.hidden,
        "emailIds": shipment.email_ids().into_iter().map(ids::email).collect::<Vec<_>>(),
        "events": events,
        "created": dates::format(shipment.created_at),
        "updated": dates::format(shipment.updated_at),
    }) else {
        unreachable!("object literal")
    };
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    match get_ids(args)? {
        None => {
            let all = ctx.jmap.store.shipments(ctx.account.id, None).await?;
            list.extend(all.iter().map(|shipment| pick(to_json(shipment), &properties)));
        }
        Some(requested) => {
            if requested.len() > MAX_OBJECTS_IN_GET {
                return Err(MethodError::kind("requestTooLarge"));
            }
            let wanted: Vec<i64> = requested.iter().filter_map(|id| ctx.parse_id('s', id)).collect();
            let found = ctx.jmap.store.shipments(ctx.account.id, Some(wanted)).await?;
            for wanted in requested {
                match ctx.parse_id('s', &wanted).and_then(|n| found.iter().find(|shipment| shipment.id == n)) {
                    Some(shipment) => list.push(pick(to_json(shipment), &properties)),
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
fn expected_at(shipment: &Shipment) -> i64 {
    expected(shipment)["from"].as_str().and_then(dates::parse).unwrap_or(i64::MAX)
}

fn matches(shipment: &Shipment, filter: &Value) -> MethodResult<bool> {
    let Some(filter) = filter.as_object() else {
        return Err(MethodError::new("unsupportedFilter", "a filter is an object"));
    };
    if let Some(operator) = filter.get("operator") {
        let conditions = filter.get("conditions").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let mut results = Vec::with_capacity(conditions.len());
        for condition in conditions {
            results.push(matches(shipment, condition)?);
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
                expected_at(shipment) < before
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
    let mut shipments = Vec::new();
    for shipment in ctx.jmap.store.shipments(ctx.account.id, None).await? {
        if filter.map_or(Ok(true), |filter| matches(&shipment, filter))? {
            shipments.push(shipment);
        }
    }
    // Newest change first unless asked otherwise.
    shipments.sort_by_key(|shipment| std::cmp::Reverse((shipment.updated_at, shipment.id)));
    for comparator in sort.iter().rev() {
        let ascending = comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true);
        let key: fn(&Shipment) -> i64 = match comparator.get("property").and_then(Value::as_str) {
            Some("expected") => expected_at,
            Some("updated") => |shipment| shipment.updated_at,
            _ => return Err(MethodError::new("unsupportedSort", "sort by expected or updated")),
        };
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

/// The store's refusals as JMAP knows them.
fn refused(err: StoreError) -> SetError {
    match err {
        StoreError::Rule { code: "shipmentsOff", message } => SetError::new("forbidden", message),
        other => SetError::from(other),
    }
}

const UPDATABLE: &[&str] = &["title", "hidden", "addEvents"];
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
                        let full = to_json(shipment);
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
                };
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
                Some(number) => store.destroy_shipment(account_id, number).await.map_err(refused),
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
