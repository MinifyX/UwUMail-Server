//! AddressBook/get and AddressBook/set (RFC 9610, section 2) on the account's CardDAV address
//! books. See docs/jmap-contacts.md.

use serde_json::{Map, Value, json};
use uwumail_store::{DavCollection, DavCollectionUpdate, DavKind, NewDavCollection, StoreError};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;

const DEFAULTS: &[&str] =
    &["id", "name", "description", "sortOrder", "isDefault", "isSubscribed", "shareWith", "myRights"];

/// The longest address book name, in bytes, as for calendars.
const MAX_NAME_BYTES: usize = 255;
const MAX_DESCRIPTION_BYTES: usize = 10_000;

/// The account's address books, the default one made the first time like CardDAV does.
pub async fn address_books(ctx: &Ctx<'_>) -> MethodResult<Vec<DavCollection>> {
    let name = match ctx.jmap.smtp.tone().language {
        uwumail_smtp::Language::De => "Kontakte",
        _ => "Contacts",
    };
    Ok(ctx
        .jmap
        .store
        .dav_collections(ctx.account.id, DavKind::Addressbook, NewDavCollection::default_address_book(name))
        .await?)
}

/// The address books are switched off for the account the way CardDAV is.
pub fn check_enabled(ctx: &Ctx<'_>) -> MethodResult<()> {
    if ctx.account.protocols.carddav {
        Ok(())
    } else {
        Err(MethodError::new("accountNotSupportedByMethod", "contacts are switched off for this account"))
    }
}

fn to_json(book: &DavCollection, only_one: bool) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": ids::address_book(book.id),
        "name": book.display_name,
        "description": (!book.description.is_empty()).then_some(&book.description),
        "sortOrder": book.sort_order.max(0),
        "isDefault": book.is_default,
        "isSubscribed": true,
        "shareWith": null,
        "myRights": {
            "mayRead": true,
            "mayWrite": true,
            // Address books are not shared here, so nobody may change who they are shared with.
            "mayShare": false,
            "mayDelete": !only_one
        },
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let list = address_books(ctx).await?;
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let only_one = list.len() <= 1;
    let (found, not_found) = match get_ids(args)? {
        None => (list.iter().map(|b| pick(to_json(b, only_one), &properties)).collect::<Vec<_>>(), Vec::new()),
        Some(requested) => {
            let mut found = Vec::new();
            let mut not_found = Vec::new();
            for id in requested {
                match ctx.parse_id('b', &id).and_then(|n| list.iter().find(|b| b.id == n)) {
                    Some(book) => found.push(pick(to_json(book, only_one), &properties)),
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
            "sortOrder" => match value.as_u64() {
                Some(order) if order < (1 << 31) => update.sort_order = Some(order as i64),
                _ => bad.push("sortOrder"),
            },
            // What this server has only one answer to may be sent with that answer.
            "isSubscribed" if value == &Value::Bool(true) => {}
            "shareWith" if value.is_null() => {}
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
    let known = address_books(ctx).await?;
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
                let new = NewDavCollection {
                    slug: random_slug(),
                    display_name: update.display_name.clone().unwrap_or_default(),
                    description: update.description.clone().unwrap_or_default(),
                    ..Default::default()
                };
                store.create_address_book(account_id, new, update).await.map_err(|err| match err {
                    StoreError::Rule { code: "davCollectionsFull", message } => SetError::new("overQuota", message),
                    other => SetError::from(other),
                })
            }
            .await;
            match result {
                Ok(book) => {
                    let id = ids::address_book(book.id);
                    ctx.created_ids.insert(creation_id.clone(), id.clone());
                    // What the client did not send: the id, what the server set, the defaults.
                    let mut created = to_json(&book, false);
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
                let book_id = ctx.parse_id('b', id).ok_or_else(SetError::not_found)?;
                if !known.iter().any(|book| book.id == book_id) {
                    return Err(SetError::not_found());
                }
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                let changes = parse_properties(patch, false)?;
                store.dav_update_collection(account_id, book_id, changes).await.map_err(|err| match err {
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
        let with_contents = args.get("onDestroyRemoveContents").and_then(Value::as_bool).unwrap_or(false);
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('b', id) {
                Some(book_id) => {
                    store.destroy_address_book(account_id, book_id, with_contents).await.map_err(SetError::from)
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

    // Only when everything else worked; an id that does not resolve is ignored.
    if !failed && let Some(wanted) = args.get("onSuccessSetIsDefault").and_then(Value::as_str) {
        let before = address_books(ctx).await?;
        if let Some(book_id) = ctx.parse_id('b', wanted).filter(|n| before.iter().any(|b| b.id == *n && !b.is_default))
        {
            store.set_default_address_book(account_id, book_id).await?;
            for book in before.iter().filter(|b| b.is_default || b.id == book_id) {
                let id = ids::address_book(book.id);
                let is_default = book.id == book_id;
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

/// The last segment of a new address book's CardDAV URL.
fn random_slug() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("the operating system RNG failed");
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_are_checked() {
        let object = |value: Value| value.as_object().unwrap().clone();
        let update = parse_properties(&object(json!({ "name": " Familie ", "sortOrder": 2 })), true).unwrap();
        assert_eq!(update.display_name.as_deref(), Some("Familie"));
        assert_eq!(update.sort_order, Some(2));
        let err = parse_properties(&object(json!({ "isSubscribed": false, "shareWith": {} })), true).unwrap_err();
        assert_eq!(err.properties, Some(vec!["isSubscribed".into(), "shareWith".into(), "name".into()]));
        assert!(parse_properties(&object(json!({ "name": "x".repeat(256) })), false).is_err());
        assert!(parse_properties(&object(json!({ "color": "#ff0000" })), false).is_err());
    }
}
