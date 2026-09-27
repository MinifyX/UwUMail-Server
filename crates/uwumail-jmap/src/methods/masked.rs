//! MaskedEmail/get and MaskedEmail/set, following Fastmail's masked email extension
//! (`https://www.fastmail.com/dev/maskedemail`): random addresses one makes per website, the same
//! as under My account → Masked addresses in the portal. See docs/jmap-masked-email.md.

use serde_json::{Map, Value, json};
use uwumail_store::{MaskedAddress, MaskedState, MaskedUpdate, NewMaskedAddress, StoreError};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodResult, SetError};
use crate::{dates, ids};

const DEFAULTS: &[&str] = &[
    "id",
    "email",
    "state",
    "forDomain",
    "description",
    "lastMessageAt",
    "createdAt",
    "createdBy",
    "url",
    "emailPrefix",
];

/// What the masked address was made with, when a JMAP client made it.
const CREATED_BY: &str = "JMAP";

fn to_json(masked: &MaskedAddress) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": ids::masked_email(masked.id),
        "email": masked.email,
        "state": masked.state,
        "forDomain": masked.for_domain,
        "description": masked.description,
        "lastMessageAt": masked.last_message_at.map(dates::format),
        "createdAt": dates::format(masked.created_at),
        "createdBy": masked.created_by,
        "url": masked.url,
        "emailPrefix": masked.email_prefix,
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
            let all = ctx.jmap.store.masked_addresses(ctx.account.id, None).await?;
            list.extend(all.iter().map(|masked| pick(to_json(masked), &properties)));
        }
        Some(requested) => {
            let wanted: Vec<i64> = requested.iter().filter_map(|id| ctx.parse_id('x', id)).collect();
            let found = ctx.jmap.store.masked_addresses(ctx.account.id, Some(wanted)).await?;
            for id in requested {
                match ctx.parse_id('x', &id).and_then(|n| found.iter().find(|masked| masked.id == n)) {
                    Some(masked) => list.push(pick(to_json(masked), &properties)),
                    None => not_found.push(id),
                }
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

fn text<'v>(object: &'v Map<String, Value>, property: &'static str) -> Result<Option<&'v str>, SetError> {
    match object.get(property) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(SetError::invalid_properties(&[property], format!("{property} must be a string"))),
    }
}

fn state(object: &Map<String, Value>) -> Result<Option<MaskedState>, SetError> {
    text(object, "state")?
        .map(|value| {
            MaskedState::parse(value).ok_or_else(|| {
                SetError::invalid_properties(&["state"], "state must be pending, enabled, disabled or deleted")
            })
        })
        .transpose()
}

/// The store's refusals as JMAP knows them.
fn refused(err: StoreError) -> SetError {
    match err {
        StoreError::Rule { code: "maskedState", message } => SetError::invalid_properties(&["state"], message),
        StoreError::Rule { code: "maskedPrefix", message } => SetError::invalid_properties(&["emailPrefix"], message),
        StoreError::Rule { code: "maskedDomain" | "maskedLimit", message } => SetError::new("forbidden", message),
        other => SetError::from(other),
    }
}

const SERVER_SET: &[&str] = &["id", "email", "lastMessageAt", "createdAt", "createdBy"];

fn check_known(object: &Map<String, Value>, creating: bool) -> Result<(), SetError> {
    for key in object.keys() {
        let known = DEFAULTS.contains(&key.as_str());
        let settable = !SERVER_SET.contains(&key.as_str()) && (creating || key != "emailPrefix");
        if !known || !settable {
            return Err(SetError::invalid_properties(&[key.as_str()], format!("{key} cannot be set")));
        }
    }
    Ok(())
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
            let result: Result<MaskedAddress, SetError> = async {
                let object = object.as_object().ok_or_else(|| SetError::new("invalidProperties", "not an object"))?;
                check_known(object, true)?;
                let new = NewMaskedAddress {
                    domain: None,
                    state: state(object)?,
                    for_domain: text(object, "forDomain")?.unwrap_or_default().to_owned(),
                    description: text(object, "description")?.unwrap_or_default().to_owned(),
                    url: text(object, "url")?.map(str::to_owned),
                    email_prefix: text(object, "emailPrefix")?.map(str::to_owned),
                    created_by: CREATED_BY.into(),
                };
                store.create_masked_address(account_id, new).await.map_err(refused)
            }
            .await;
            match result {
                Ok(masked) => {
                    let id = ids::masked_email(masked.id);
                    ctx.created_ids.insert(creation_id.clone(), id.clone());
                    let created = json!({
                        "id": id,
                        "email": masked.email,
                        "state": masked.state,
                        "createdAt": dates::format(masked.created_at),
                        "createdBy": masked.created_by,
                        "lastMessageAt": null,
                        "emailPrefix": masked.email_prefix,
                    });
                    response.created.insert(creation_id.clone(), created);
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
                let number = ctx.parse_id('x', id).ok_or_else(SetError::not_found)?;
                let patch = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "not an object"))?;
                check_known(patch, false)?;
                let change = MaskedUpdate {
                    state: state(patch)?,
                    for_domain: text(patch, "forDomain")?.map(str::to_owned),
                    description: text(patch, "description")?.map(str::to_owned),
                    url: patch
                        .contains_key("url")
                        .then(|| text(patch, "url"))
                        .transpose()?
                        .map(|url| url.map(str::to_owned)),
                };
                store.update_masked_address(account_id, number, change).await.map(|_| ()).map_err(refused)
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

    // A masked address is never handed out again, so destroying one deletes it the way setting
    // its state does: it stays, with state "deleted", and refuses mail.
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('x', id) {
                Some(number) => {
                    let change = MaskedUpdate { state: Some(MaskedState::Deleted), ..Default::default() };
                    store.update_masked_address(account_id, number, change).await.map(|_| ()).map_err(refused)
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
