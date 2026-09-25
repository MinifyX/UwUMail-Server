//! Identity/get and Identity/set (RFC 8621, section 6).

use serde_json::{Map, Value, json};
use uwumail_store::{EmailAddress, Identity, IdentityUpdate};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodResult, SetError};
use crate::ids;

const DEFAULTS: &[&str] = &["id", "name", "email", "replyTo", "bcc", "textSignature", "htmlSignature", "mayDelete"];

fn to_json(identity: &Identity) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": ids::identity(identity.id),
        "name": identity.name,
        "email": identity.email,
        "replyTo": identity.reply_to,
        "bcc": identity.bcc,
        "textSignature": identity.text_signature,
        "htmlSignature": identity.html_signature,
        "mayDelete": true,
    }) else {
        unreachable!("object literal")
    };
    map
}

fn addresses(value: &Value, property: &'static str) -> Result<Option<Vec<EmailAddress>>, SetError> {
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| SetError::invalid_properties(&[property], format!("{property} must be a list of addresses")))
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let identities = ctx.jmap.store.identities(ctx.account.id).await?;
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    match get_ids(args)? {
        None => list.extend(identities.iter().map(|i| pick(to_json(i), &properties))),
        Some(requested) => {
            for id in requested {
                match ctx.parse_id('i', &id).and_then(|n| identities.iter().find(|i| i.id == n)) {
                    Some(identity) => list.push(pick(to_json(identity), &properties)),
                    None => not_found.push(id),
                }
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_set_size(args)?;
    // Makes sure the default identities exist before the state is read.
    ctx.jmap.store.identities(ctx.account.id).await?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let store = ctx.jmap.store.clone();
    let account_id = ctx.account.id;
    let mut response = SetResponse::default();

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result: Result<i64, SetError> = async {
                let email = object
                    .get("email")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SetError::invalid_properties(&["email"], "email is required"))?;
                let name = object.get("name").and_then(Value::as_str).unwrap_or_default();
                // Checked before anything is created, so a refused identity leaves nothing behind.
                for property in ["textSignature", "htmlSignature"] {
                    if object
                        .get(property)
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.len() > uwumail_store::IDENTITY_SIGNATURE_MAX_BYTES)
                    {
                        return Err(SetError::invalid_properties(&[property], format!("{property} is too long")));
                    }
                }
                let id = store.create_identity(account_id, name, email).await?;
                let update = IdentityUpdate {
                    reply_to: object.get("replyTo").map(|v| addresses(v, "replyTo")).transpose()?,
                    bcc: object.get("bcc").map(|v| addresses(v, "bcc")).transpose()?,
                    text_signature: object.get("textSignature").and_then(Value::as_str).map(str::to_owned),
                    html_signature: object.get("htmlSignature").and_then(Value::as_str).map(str::to_owned),
                    ..IdentityUpdate::default()
                };
                store.update_identity(account_id, id, update).await?;
                Ok(id)
            }
            .await;
            match result {
                Ok(id) => {
                    ctx.created_ids.insert(creation_id.clone(), ids::identity(id));
                    response.created.insert(creation_id.clone(), json!({ "id": ids::identity(id), "mayDelete": true }));
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
                let number = ctx.parse_id('i', id).ok_or_else(SetError::not_found)?;
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                let mut changes = IdentityUpdate::default();
                for (key, value) in patch {
                    match key.as_str() {
                        "name" => changes.name = value.as_str().map(str::to_owned),
                        "replyTo" => changes.reply_to = Some(addresses(value, "replyTo")?),
                        "bcc" => changes.bcc = Some(addresses(value, "bcc")?),
                        "textSignature" => changes.text_signature = value.as_str().map(str::to_owned),
                        "htmlSignature" => changes.html_signature = value.as_str().map(str::to_owned),
                        other => {
                            return Err(SetError::invalid_properties(&[other], format!("{other} cannot be changed")));
                        }
                    }
                }
                Ok(store.update_identity(account_id, number, changes).await?)
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
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('i', id) {
                Some(number) => store.destroy_identity(account_id, number).await.map_err(SetError::from),
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
