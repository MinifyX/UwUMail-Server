//! Mailbox/get, Mailbox/query and Mailbox/set (RFC 8621, section 2).

use serde_json::{Map, Value, json};
use uwumail_store::{Mailbox, MailboxRole, MailboxUpdate, StoreError};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;

const DEFAULTS: &[&str] = &[
    "id",
    "name",
    "parentId",
    "role",
    "sortOrder",
    "totalEmails",
    "unreadEmails",
    "totalThreads",
    "unreadThreads",
    "myRights",
    "isSubscribed",
];

fn rights(deletable: bool) -> Value {
    json!({
        "mayReadItems": true,
        "mayAddItems": true,
        "mayRemoveItems": true,
        "maySetSeen": true,
        "maySetKeywords": true,
        "mayCreateChild": true,
        "mayRename": deletable,
        "mayDelete": deletable,
        "maySubmit": true
    })
}

fn to_json(mailbox: &Mailbox) -> Map<String, Value> {
    let deletable = mailbox.role != Some(MailboxRole::Inbox);
    let Value::Object(map) = json!({
        "id": ids::mailbox(mailbox.id),
        "name": mailbox.name,
        "parentId": mailbox.parent_id.map(ids::mailbox),
        "role": mailbox.role.map(MailboxRole::as_str),
        "sortOrder": mailbox.sort_order,
        "totalEmails": mailbox.total_emails,
        "unreadEmails": mailbox.unread_emails,
        "totalThreads": mailbox.total_threads,
        "unreadThreads": mailbox.unread_threads,
        "myRights": rights(deletable),
        "isSubscribed": mailbox.subscribed,
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let mailboxes = ctx.jmap.store.mailboxes(ctx.account.id).await?;
    let (list, not_found) = match get_ids(args)? {
        None => (mailboxes.iter().map(|m| pick(to_json(m), &properties)).collect::<Vec<_>>(), Vec::new()),
        Some(requested) => {
            let mut list = Vec::new();
            let mut not_found = Vec::new();
            for id in requested {
                match ctx.parse_id('m', &id).and_then(|n| mailboxes.iter().find(|m| m.id == n)) {
                    Some(mailbox) => list.push(pick(to_json(mailbox), &properties)),
                    None => not_found.push(id),
                }
            }
            (list, not_found)
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let mut mailboxes = ctx.jmap.store.mailboxes(ctx.account.id).await?;
    if let Some(filter) = args.get("filter").filter(|f| !f.is_null()) {
        let filter = filter
            .as_object()
            .ok_or_else(|| MethodError::new("unsupportedFilter", "only simple filters are supported"))?;
        for (key, value) in filter {
            match key.as_str() {
                "parentId" => {
                    let parent = value.as_str().and_then(|v| ctx.parse_id('m', v));
                    mailboxes.retain(|m| m.parent_id == parent);
                }
                "name" => {
                    let needle = value.as_str().unwrap_or_default().to_lowercase();
                    mailboxes.retain(|m| m.name.to_lowercase().contains(&needle));
                }
                "role" => {
                    let role = value.as_str().and_then(MailboxRole::parse);
                    mailboxes.retain(|m| m.role == role);
                }
                "hasAnyRole" => {
                    let wanted = value.as_bool().unwrap_or(false);
                    mailboxes.retain(|m| m.role.is_some() == wanted);
                }
                "isSubscribed" => {
                    let wanted = value.as_bool().unwrap_or(false);
                    mailboxes.retain(|m| m.subscribed == wanted);
                }
                other => return Err(MethodError::new("unsupportedFilter", format!("unknown filter {other}"))),
            }
        }
    }
    if let Some(sort) = args.get("sort").and_then(Value::as_array) {
        for comparator in sort.iter().rev() {
            let ascending = comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true);
            match comparator.get("property").and_then(Value::as_str) {
                Some("name") => mailboxes.sort_by_key(|m| m.name.to_lowercase()),
                Some("sortOrder") => mailboxes.sort_by_key(|m| m.sort_order),
                _ => return Err(MethodError::kind("unsupportedSort")),
            }
            if !ascending {
                mailboxes.reverse();
            }
        }
    }
    let ids: Vec<String> = mailboxes.iter().map(|m| ids::mailbox(m.id)).collect();
    Ok(json!({
        "accountId": ctx.account_id(),
        "queryState": state,
        "canCalculateChanges": false,
        "position": 0,
        "total": ids.len(),
        "ids": ids,
    }))
}

fn parent(ctx: &Ctx<'_>, value: &Value) -> Result<Option<i64>, SetError> {
    match value {
        Value::Null => Ok(None),
        Value::String(id) => ctx
            .parse_id('m', id)
            .map(Some)
            .ok_or_else(|| SetError::invalid_properties(&["parentId"], "the parent mailbox does not exist")),
        _ => Err(SetError::invalid_properties(&["parentId"], "parentId must be a mailbox id or null")),
    }
}

fn role(value: &Value) -> Result<Option<MailboxRole>, SetError> {
    match value {
        Value::Null => Ok(None),
        Value::String(name) => MailboxRole::parse(&name.to_lowercase())
            .map(Some)
            .ok_or_else(|| SetError::invalid_properties(&["role"], format!("unknown role {name}"))),
        _ => Err(SetError::invalid_properties(&["role"], "role must be a string or null")),
    }
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_set_size(args)?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let store = ctx.jmap.store.clone();
    let account_id = ctx.account.id;
    let mut response = SetResponse::default();

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result: Result<i64, SetError> = async {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SetError::invalid_properties(&["name"], "name is required"))?;
                let parent = parent(ctx, object.get("parentId").unwrap_or(&Value::Null))?;
                let role = role(object.get("role").unwrap_or(&Value::Null))?;
                let sort_order = object.get("sortOrder").and_then(Value::as_i64).unwrap_or(0);
                let subscribed = object.get("isSubscribed").and_then(Value::as_bool).unwrap_or(true);
                Ok(store.create_mailbox(account_id, name, parent, role, sort_order, subscribed).await?)
            }
            .await;
            match result {
                Ok(id) => {
                    let jmap_id = ids::mailbox(id);
                    ctx.created_ids.insert(creation_id.clone(), jmap_id.clone());
                    response.created.insert(
                        creation_id.clone(),
                        json!({ "id": jmap_id, "totalEmails": 0, "unreadEmails": 0, "totalThreads": 0,
                                "unreadThreads": 0, "myRights": rights(true), "isSubscribed": true }),
                    );
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result: Result<(), SetError> = async {
                let mailbox_id = ctx.parse_id('m', id).ok_or_else(SetError::not_found)?;
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                let mut changes = MailboxUpdate::default();
                for (key, value) in patch {
                    match key.as_str() {
                        "name" => {
                            changes.name = Some(
                                value
                                    .as_str()
                                    .ok_or_else(|| SetError::invalid_properties(&["name"], "name must be a string"))?
                                    .to_owned(),
                            )
                        }
                        "parentId" => changes.parent_id = Some(parent(ctx, value)?),
                        "role" => changes.role = Some(role(value)?),
                        "sortOrder" => {
                            changes.sort_order = Some(value.as_i64().ok_or_else(|| {
                                SetError::invalid_properties(&["sortOrder"], "sortOrder must be a number")
                            })?)
                        }
                        "isSubscribed" => {
                            changes.subscribed = Some(value.as_bool().ok_or_else(|| {
                                SetError::invalid_properties(&["isSubscribed"], "isSubscribed must be true or false")
                            })?)
                        }
                        other => {
                            return Err(SetError::invalid_properties(&[other], format!("{other} cannot be changed")));
                        }
                    }
                }
                store.update_mailbox(account_id, mailbox_id, changes).await.map_err(SetError::from)
            }
            .await;
            match result {
                Ok(()) => {
                    response.updated.insert(id.clone(), Value::Null);
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        let remove_emails = args.get("onDestroyRemoveEmails").and_then(Value::as_bool).unwrap_or(false);
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('m', id) {
                Some(mailbox_id) => {
                    let is_inbox = store
                        .mailboxes(account_id)
                        .await?
                        .iter()
                        .any(|m| m.id == mailbox_id && m.role == Some(MailboxRole::Inbox));
                    if is_inbox {
                        Err(SetError::new("forbidden", "the inbox cannot be deleted"))
                    } else {
                        store.destroy_mailbox(account_id, mailbox_id, remove_emails).await.map_err(|err| match err {
                            StoreError::NotFound(_) => SetError::not_found(),
                            other => SetError::from(other),
                        })
                    }
                }
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

    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}
