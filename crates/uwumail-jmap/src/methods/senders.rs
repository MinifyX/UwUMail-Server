//! SenderList/get and SenderList/set: one's own allowed and blocked senders, the same list as under
//! My account → Spam filter in the portal. See docs/jmap-senders.md.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{Map, Value, json};
use uwumail_store::{ListOwner, ListScope, NewSenderListEntry, SenderKind, SenderList, SenderListEntry, StoreError};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodResult, SetError};
use crate::ids;

const DEFAULTS: &[&str] = &["id", "list", "kind", "value", "note", "createdAt"];

fn to_json(entry: &SenderListEntry) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": ids::sender(entry.id),
        "list": entry.list,
        "kind": entry.kind,
        "value": entry.value,
        "note": entry.note,
        "createdAt": entry.created_at,
    }) else {
        unreachable!("object literal")
    };
    map
}

/// The list changes rarely and has no change log; its state is a fingerprint of what is on it.
fn state(entries: &[SenderListEntry]) -> String {
    let mut hasher = DefaultHasher::new();
    for entry in entries {
        (entry.id, &entry.value, entry.list == SenderList::Block).hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

async fn entries(ctx: &Ctx<'_>) -> MethodResult<Vec<SenderListEntry>> {
    Ok(ctx.jmap.store.sender_list(ListScope::Account(ctx.account.id)).await?)
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let entries = entries(ctx).await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    match get_ids(args)? {
        None => list.extend(entries.iter().map(|entry| pick(to_json(entry), &properties))),
        Some(requested) => {
            for id in requested {
                match ctx.parse_id('l', &id).and_then(|n| entries.iter().find(|entry| entry.id == n)) {
                    Some(entry) => list.push(pick(to_json(entry), &properties)),
                    None => not_found.push(id),
                }
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state(&entries), "list": list, "notFound": not_found }))
}

fn parse<T: serde::de::DeserializeOwned>(
    object: &Map<String, Value>,
    property: &'static str,
) -> Result<Option<T>, SetError> {
    match object.get(property) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| SetError::invalid_properties(&[property], format!("{property} has an unknown value"))),
    }
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_set_size(args)?;
    let old_state = state(&entries(ctx).await?);
    if_in_state(args, &old_state)?;
    let store = ctx.jmap.store.clone();
    let account_id = ctx.account.id;
    let login = ctx.account.login.clone();
    let mut response = SetResponse::default();

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result: Result<SenderListEntry, SetError> = async {
                let object = object.as_object().ok_or_else(|| SetError::new("invalidProperties", "not an object"))?;
                let list: SenderList = parse(object, "list")?
                    .ok_or_else(|| SetError::invalid_properties(&["list"], "list is required"))?;
                let kind: Option<SenderKind> = parse(object, "kind")?;
                let value = object
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SetError::invalid_properties(&["value"], "value is required"))?;
                let note = object.get("note").and_then(Value::as_str).unwrap_or_default();
                let entry = NewSenderListEntry {
                    scope: ListScope::Account(account_id),
                    list,
                    kind,
                    value: value.to_owned(),
                    note: note.to_owned(),
                    created_by: login.clone(),
                };
                Ok(store.add_sender_list_entry(entry).await?)
            }
            .await;
            match result {
                Ok(entry) => {
                    let id = ids::sender(entry.id);
                    ctx.created_ids.insert(creation_id.clone(), id.clone());
                    // The server normalizes what it was given; the client learns the stored form.
                    let created =
                        json!({ "id": id, "kind": entry.kind, "value": entry.value, "createdAt": entry.created_at });
                    response.created.insert(creation_id.clone(), created);
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for id in update.keys() {
            let error = SetError::new("forbidden", "entries cannot be changed; destroy one and create another");
            response.not_updated.insert(id.clone(), error.to_json());
        }
    }

    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('l', id) {
                Some(number) => match store.remove_sender_list_entry(ListOwner::Account(account_id), number).await {
                    Ok(_) => Ok(()),
                    Err(StoreError::NotFound(_)) => Err(SetError::not_found()),
                    Err(err) => Err(SetError::from(err)),
                },
                None => Err(SetError::not_found()),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    response.not_destroyed.insert(id.to_owned(), err.to_json());
                }
            }
        }
    }

    let new_state = state(&entries(ctx).await?);
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}
