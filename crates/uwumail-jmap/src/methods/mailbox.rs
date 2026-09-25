//! Mailbox/get, Mailbox/query and Mailbox/set (RFC 8621, section 2).

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use uwumail_store::{Mailbox, MailboxRole, MailboxUpdate, StoreError};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};
use crate::{ids, sharing};

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
    "shareWith",
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
        "maySubmit": true,
        "mayAdmin": true
    })
}

/// A mailbox as the caller sees it. `share_with` is its `shareWith` for someone who may
/// administer it (RFC 9670), `null` for everyone else.
fn to_json(ctx: &Ctx<'_>, mailbox: &Mailbox, share_with: &HashMap<i64, Map<String, Value>>) -> Map<String, Value> {
    let is_inbox = mailbox.role == Some(MailboxRole::Inbox);
    let (my_rights, admin, parent) = match &ctx.shared {
        None => (rights(!is_inbox), true, mailbox.parent_id),
        Some(view) => (
            sharing::rights_json(view.rights_of(mailbox.id), is_inbox, false),
            view.may(mailbox.id, "a"),
            // A parent that is not shared is not there for the caller.
            mailbox.parent_id.filter(|parent| view.visible(*parent)),
        ),
    };
    let share_with =
        if admin { Value::Object(share_with.get(&mailbox.id).cloned().unwrap_or_default()) } else { Value::Null };
    let Value::Object(map) = json!({
        "id": ids::mailbox(mailbox.id),
        "name": mailbox.name,
        "parentId": parent.map(ids::mailbox),
        "role": mailbox.role.map(MailboxRole::as_str),
        "sortOrder": mailbox.sort_order,
        "totalEmails": mailbox.total_emails,
        "unreadEmails": mailbox.unread_emails,
        "totalThreads": mailbox.total_threads,
        "unreadThreads": mailbox.unread_threads,
        "myRights": my_rights,
        // Someone else's folders are always subscribed: their flag is the owner's.
        "isSubscribed": mailbox.subscribed || ctx.shared.is_some(),
        "shareWith": share_with,
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    map
}

/// The account's mailboxes, those shared with the caller when it is someone else's.
async fn visible_mailboxes(ctx: &Ctx<'_>) -> MethodResult<Vec<Mailbox>> {
    let mut mailboxes = ctx.jmap.store.mailboxes(ctx.account.id).await?;
    if let Some(view) = &ctx.shared {
        mailboxes.retain(|mailbox| view.visible(mailbox.id));
    }
    Ok(mailboxes)
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let mailboxes = visible_mailboxes(ctx).await?;
    let share_with = if properties.iter().any(|p| p == "shareWith") {
        sharing::share_with(&ctx.jmap.store, ctx.account.id).await?
    } else {
        HashMap::new()
    };
    let (list, not_found) = match get_ids(args)? {
        None => {
            (mailboxes.iter().map(|m| pick(to_json(ctx, m, &share_with), &properties)).collect::<Vec<_>>(), Vec::new())
        }
        Some(requested) => {
            let mut list = Vec::new();
            let mut not_found = Vec::new();
            for id in requested {
                match ctx.parse_id('m', &id).and_then(|n| mailboxes.iter().find(|m| m.id == n)) {
                    Some(mailbox) => list.push(pick(to_json(ctx, mailbox, &share_with), &properties)),
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
    let mut mailboxes = visible_mailboxes(ctx).await?;
    if let Some(filter) = args.get("filter").filter(|f| !f.is_null()) {
        let filter = filter
            .as_object()
            .ok_or_else(|| MethodError::new("unsupportedFilter", "only simple filters are supported"))?;
        for (key, value) in filter {
            match key.as_str() {
                "parentId" => {
                    let parent = value.as_str().and_then(|v| ctx.parse_id('m', v));
                    let visible = |id: i64| ctx.shared.as_ref().is_none_or(|view| view.visible(id));
                    mailboxes.retain(|m| m.parent_id.filter(|p| visible(*p)) == parent);
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
        // Parents first: a create may point at another create of this call ("parentId": "#k"),
        // and JSON objects carry no order.
        let mut pending: Vec<(&String, &Value)> = create.iter().collect();
        let mut ordered = Vec::with_capacity(pending.len());
        while !pending.is_empty() {
            let waiting_on = |object: &Value, pending: &[(&String, &Value)]| {
                object
                    .get("parentId")
                    .and_then(Value::as_str)
                    .and_then(|p| p.strip_prefix('#'))
                    .is_some_and(|parent| pending.iter().any(|(id, _)| id.as_str() == parent))
            };
            let ready = pending.iter().position(|(_, object)| !waiting_on(object, &pending)).unwrap_or(0);
            ordered.push(pending.remove(ready));
        }
        for (creation_id, object) in ordered {
            let result: Result<i64, SetError> = async {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SetError::invalid_properties(&["name"], "name is required"))?;
                let parent = parent(ctx, object.get("parentId").unwrap_or(&Value::Null))?;
                let role = role(object.get("role").unwrap_or(&Value::Null))?;
                let sort_order = object.get("sortOrder").and_then(Value::as_i64).unwrap_or(0);
                let subscribed = object.get("isSubscribed").and_then(Value::as_bool).unwrap_or(true);
                if let Some(view) = &ctx.shared {
                    // In someone else's account: only inside a folder that allows it (`k`).
                    if !parent.is_some_and(|parent| view.may(parent, "k")) {
                        return Err(SetError::new("forbidden", "you may not create a mailbox there"));
                    }
                    if role.is_some() {
                        return Err(SetError::new("forbidden", "only the owner gives mailboxes a role"));
                    }
                }
                let id = store.create_mailbox(account_id, name, parent, role, sort_order, subscribed).await?;
                if let Some(share_with) = object.get("shareWith").filter(|value| !value.is_null()) {
                    // The new folder already has its parent's sharing; this replaces it.
                    set_share_with(ctx, id, share_with).await?;
                }
                Ok(id)
            }
            .await;
            match result {
                Ok(id) => {
                    let jmap_id = ids::mailbox(id);
                    ctx.created_ids.insert(creation_id.clone(), jmap_id.clone());
                    let my_rights = match &ctx.shared {
                        None => rights(true),
                        // A new folder is shared like its parent.
                        Some(view) => {
                            let parent =
                                object.get("parentId").and_then(Value::as_str).and_then(|p| ctx.parse_id('m', p));
                            sharing::rights_json(parent.map_or("", |parent| view.rights_of(parent)), false, false)
                        }
                    };
                    response.created.insert(
                        creation_id.clone(),
                        json!({ "id": jmap_id, "totalEmails": 0, "unreadEmails": 0, "totalThreads": 0,
                                "unreadThreads": 0, "myRights": my_rights, "isSubscribed": true }),
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
                if ctx.shared.as_ref().is_some_and(|view| !view.visible(mailbox_id)) {
                    return Err(SetError::not_found());
                }
                let mut changes = MailboxUpdate::default();
                let mut share_with: Option<Value> = None;
                let mut share_patch: Vec<(String, Value)> = Vec::new();
                for (key, value) in patch {
                    if key == "shareWith" {
                        share_with = Some(value.clone());
                        continue;
                    }
                    if let Some(principal) = key.strip_prefix("shareWith/") {
                        share_patch.push((principal.to_owned(), value.clone()));
                        continue;
                    }
                    if let Some(view) = &ctx.shared {
                        match key.as_str() {
                            // The owner's flag; someone else's folders are always subscribed.
                            "isSubscribed" => continue,
                            "parentId" => {
                                let target = parent(ctx, value)?;
                                if !target.is_some_and(|target| view.may(target, "k")) {
                                    return Err(SetError::new("forbidden", "you may not move the mailbox there"));
                                }
                            }
                            "role" => return Err(SetError::new("forbidden", "only the owner gives mailboxes a role")),
                            _ => {}
                        }
                        if !view.may(mailbox_id, "x") {
                            return Err(SetError::new("forbidden", "you may not change this mailbox"));
                        }
                    }
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
                if changes != MailboxUpdate::default() {
                    store.update_mailbox(account_id, mailbox_id, changes).await.map_err(SetError::from)?;
                }
                if let Some(share_with) = share_with {
                    set_share_with(ctx, mailbox_id, &share_with).await?;
                }
                for (principal, rights) in share_patch {
                    share_one(ctx, mailbox_id, &principal, &rights).await?;
                }
                Ok(())
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
                Some(mailbox_id) if ctx.shared.as_ref().is_some_and(|view| !view.visible(mailbox_id)) => {
                    Err(SetError::not_found())
                }
                Some(mailbox_id) if ctx.shared.as_ref().is_some_and(|view| !view.may(mailbox_id, "x")) => {
                    Err(SetError::new("forbidden", "you may not delete this mailbox"))
                }
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

/// Whether the caller may share this mailbox on: the owner, or someone it is shared with `a`.
fn may_administer(ctx: &Ctx<'_>, mailbox_id: i64) -> bool {
    ctx.shared.as_ref().is_none_or(|view| view.may(mailbox_id, "a"))
}

/// Shares a mailbox with one principal (`p<account>`), or stops when `rights` is null or empty.
async fn share_one(ctx: &Ctx<'_>, mailbox_id: i64, principal: &str, rights: &Value) -> Result<(), SetError> {
    if !may_administer(ctx, mailbox_id) {
        return Err(SetError::new("forbidden", "you may not share this mailbox"));
    }
    let invalid = |description: String| SetError::invalid_properties(&["shareWith"], description);
    let grantee = ids::parse('p', principal).ok_or_else(|| invalid(format!("unknown principal {principal}")))?;
    let letters = sharing::rights_from_json(rights).map_err(invalid)?;
    match ctx.jmap.store.set_mailbox_acl_for(ctx.account.id, mailbox_id, grantee, &letters).await {
        Ok(_) => Ok(()),
        Err(StoreError::NotFound(_)) => Err(invalid(format!("unknown principal {principal}"))),
        Err(StoreError::Rule { message, .. }) => Err(invalid(message)),
        Err(err) => Err(SetError::from(err)),
    }
}

/// Replaces who a mailbox is shared with by a whole `shareWith` map.
async fn set_share_with(ctx: &Ctx<'_>, mailbox_id: i64, value: &Value) -> Result<(), SetError> {
    if !may_administer(ctx, mailbox_id) {
        return Err(SetError::new("forbidden", "you may not share this mailbox"));
    }
    let wanted = match value {
        Value::Null => Map::new(),
        Value::Object(map) => map.clone(),
        _ => return Err(SetError::invalid_properties(&["shareWith"], "shareWith must be an object or null")),
    };
    let current = ctx.jmap.store.mailbox_acl(ctx.account.id, mailbox_id).await.map_err(SetError::from)?;
    for entry in current {
        let principal = sharing::principal_id(entry.grantee_id);
        if !wanted.contains_key(&principal) {
            share_one(ctx, mailbox_id, &principal, &Value::Null).await?;
        }
    }
    for (principal, rights) in &wanted {
        share_one(ctx, mailbox_id, principal, rights).await?;
    }
    Ok(())
}
