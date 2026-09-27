//! PushSubscription/get and PushSubscription/set (RFC 8620, section 7.2): where the server pushes
//! a device's StateChanges over Web Push. Subscriptions belong to the login that made them, not to
//! an account, so neither method takes an `accountId` or has a state. See docs/jmap-push.md.

use serde_json::{Map, Value, json};
use uwumail_store::{
    NewPushSubscription, PUSH_SUBSCRIPTION_MAX_SECS, PushKeys, PushSubscription, PushSubscriptionUpdate, StoreError,
};

use super::{Ctx, SetResponse, check_set_size, get_ids, pick, properties, unix_now};
use crate::error::{MethodError, MethodResult, SetError};
use crate::{dates, ids, webpush};

const DEFAULTS: &[&str] = &["id", "deviceClientId", "verificationCode", "expires", "types"];
/// What never comes back: the address and the keys stay with the server (RFC 8620, 7.2.1).
const PRIVATE: &[&str] = &["url", "keys"];
const MAX_DEVICE_CLIENT_ID: usize = 255;
const MAX_URL: usize = 2048;
const MAX_TYPES: usize = 64;

fn to_json(subscription: &PushSubscription) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": ids::push_subscription(subscription.id),
        "deviceClientId": subscription.device_client_id,
        "verificationCode": subscription.verification_code,
        "expires": dates::format(subscription.expires),
        "types": subscription.types,
    }) else {
        unreachable!("object literal")
    };
    map
}

fn credential<'c>(ctx: &'c Ctx<'_>) -> MethodResult<&'c str> {
    ctx.credential.as_deref().ok_or_else(|| MethodError::new("forbidden", "push subscriptions need a login"))
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let credential = credential(ctx)?;
    let properties = properties(args, "properties", DEFAULTS)?;
    if properties.iter().any(|p| PRIVATE.contains(&p.as_str())) {
        return Err(MethodError::new("forbidden", "url and keys are never returned"));
    }
    let subscriptions = ctx.jmap.store.push_subscriptions(ctx.account.id, credential).await?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    match get_ids(args)? {
        None => list.extend(subscriptions.iter().map(|s| pick(to_json(s), &properties))),
        Some(requested) => {
            for id in requested {
                match ctx.parse_id('w', &id).and_then(|n| subscriptions.iter().find(|s| s.id == n)) {
                    Some(subscription) => list.push(pick(to_json(subscription), &properties)),
                    None => not_found.push(id),
                }
            }
        }
    }
    Ok(json!({ "list": list, "notFound": not_found }))
}

/// An `expires` the server accepts: at most [`PUSH_SUBSCRIPTION_MAX_SECS`] ahead, which is also
/// what `null` means.
fn expires(value: Option<&Value>) -> Result<i64, SetError> {
    let latest = unix_now() + PUSH_SUBSCRIPTION_MAX_SECS;
    match value {
        None | Some(Value::Null) => Ok(latest),
        Some(Value::String(date)) => match dates::parse(date) {
            Some(at) if at > unix_now() => Ok(at.min(latest)),
            Some(_) => Err(SetError::invalid_properties(&["expires"], "expires is in the past")),
            None => Err(SetError::invalid_properties(&["expires"], "expires is not a UTCDate")),
        },
        Some(_) => Err(SetError::invalid_properties(&["expires"], "expires is not a UTCDate")),
    }
}

fn types(value: Option<&Value>) -> Result<Option<Vec<String>>, SetError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) if items.len() <= MAX_TYPES => items
            .iter()
            .map(|item| match item.as_str() {
                Some(name) if !name.is_empty() && name.len() <= 64 => Ok(name.to_owned()),
                _ => Err(SetError::invalid_properties(&["types"], "types are names of data types")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(SetError::invalid_properties(&["types"], "types must be null or a list of type names")),
    }
}

fn keys(value: Option<&Value>) -> Result<Option<PushKeys>, SetError> {
    let object = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(object)) => object,
        Some(_) => return Err(SetError::invalid_properties(&["keys"], "keys must be null or an object")),
    };
    let (Some(p256dh), Some(auth)) =
        (object.get("p256dh").and_then(Value::as_str), object.get("auth").and_then(Value::as_str))
    else {
        return Err(SetError::invalid_properties(&["keys"], "keys need p256dh and auth"));
    };
    if object.len() != 2 || p256dh.len() > 256 || auth.len() > 64 || !webpush::check_keys(p256dh, auth) {
        return Err(SetError::invalid_properties(
            &["keys"],
            "p256dh must be an uncompressed P-256 key and auth 16 bytes, both in base64url (RFC 8291)",
        ));
    }
    Ok(Some(PushKeys { p256dh: p256dh.to_owned(), auth: auth.to_owned() }))
}

async fn create(
    ctx: &Ctx<'_>,
    credential: &str,
    object: &Value,
) -> Result<(PushSubscription, Map<String, Value>), SetError> {
    let object = object.as_object().ok_or_else(|| SetError::new("invalidProperties", "not an object"))?;
    const KNOWN: &[&str] = &["deviceClientId", "url", "keys", "verificationCode", "expires", "types"];
    if let Some(unknown) = object.keys().find(|key| !KNOWN.contains(&key.as_str())) {
        return Err(SetError::invalid_properties(&[unknown.as_str()], format!("{unknown} cannot be set")));
    }
    let device_client_id = match object.get("deviceClientId").and_then(Value::as_str) {
        Some(id) if !id.is_empty() && id.len() <= MAX_DEVICE_CLIENT_ID => id.to_owned(),
        _ => return Err(SetError::invalid_properties(&["deviceClientId"], "deviceClientId is required")),
    };
    let url = match object.get("url").and_then(Value::as_str) {
        Some(url) if url.len() <= MAX_URL => url.trim().to_owned(),
        _ => return Err(SetError::invalid_properties(&["url"], "url is required")),
    };
    ctx.jmap.push.check_url(&url).map_err(|reason| SetError::invalid_properties(&["url"], reason))?;
    if !object.get("verificationCode").is_none_or(Value::is_null) {
        return Err(SetError::invalid_properties(&["verificationCode"], "the server sends the verification code"));
    }
    let keys = keys(object.get("keys"))?;
    let expires = expires(object.get("expires"))?;
    let types = types(object.get("types"))?;
    if !ctx.jmap.push.may_create(ctx.account.id) {
        return Err(SetError::new("rateLimit", "too many push subscriptions were made in the last hour"));
    }
    let (subscription, target) = ctx
        .jmap
        .store
        .create_push_subscription(NewPushSubscription {
            account_id: ctx.account.id,
            credential: credential.to_owned(),
            device_client_id,
            url,
            keys,
            expires,
            types,
        })
        .await?;
    ctx.jmap.push.send_verification(target);
    let created = json!({
        "id": ids::push_subscription(subscription.id),
        "expires": dates::format(subscription.expires),
    });
    let Value::Object(created) = created else { unreachable!("object literal") };
    Ok((subscription, created))
}

async fn update(ctx: &Ctx<'_>, credential: &str, id: &str, patch: &Value) -> Result<Option<Value>, SetError> {
    let number = ctx.parse_id('w', id).ok_or_else(SetError::not_found)?;
    let patch = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "not an object"))?;
    let mut change = PushSubscriptionUpdate::default();
    for (key, value) in patch {
        match key.as_str() {
            "verificationCode" => match value.as_str() {
                Some(code) if code.len() <= 256 => change.verification_code = Some(code.to_owned()),
                _ => {
                    return Err(SetError::invalid_properties(&["verificationCode"], "the verification code is text"));
                }
            },
            "expires" => change.expires = Some(expires(Some(value))?),
            "types" => change.types = Some(types(Some(value))?),
            "id" | "deviceClientId" | "url" | "keys" => {
                return Err(SetError::invalid_properties(
                    &[key.as_str()],
                    format!("{key} cannot be changed; destroy the subscription and create another"),
                ));
            }
            other => return Err(SetError::invalid_properties(&[other], format!("{other} cannot be set"))),
        }
    }
    let asked_expires = patch.get("expires").and_then(Value::as_str).and_then(dates::parse);
    let updated =
        match ctx.jmap.store.update_push_subscription(ctx.account.id, credential, number, change.clone()).await {
            Ok(updated) => updated,
            Err(StoreError::Rule { code: "invalidVerificationCode", message }) => {
                return Err(SetError::invalid_properties(&["verificationCode"], message));
            }
            Err(err) => return Err(err.into()),
        };
    // The server's own choice of expiry comes back when it is not what the client asked for.
    Ok(change
        .expires
        .filter(|at| Some(*at) != asked_expires)
        .map(|_| json!({ "expires": dates::format(updated.expires) })))
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_set_size(args)?;
    let credential = credential(ctx)?.to_owned();
    let mut response = SetResponse::default();

    if let Some(create_map) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create_map {
            match create(ctx, &credential, object).await {
                Ok((subscription, created)) => {
                    ctx.created_ids.insert(creation_id.clone(), ids::push_subscription(subscription.id));
                    response.created.insert(creation_id.clone(), Value::Object(created));
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update_map) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update_map {
            match update(ctx, &credential, id, patch).await {
                Ok(changed) => {
                    response.updated.insert(id.clone(), changed.unwrap_or(Value::Null));
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('w', id) {
                Some(number) => ctx.jmap.store.destroy_push_subscription(ctx.account.id, &credential, number).await,
                None => Err(StoreError::NotFound(id.to_owned())),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    response.not_destroyed.insert(id.to_owned(), SetError::from(err).to_json());
                }
            }
        }
    }

    // Like any /set, without the account and the states push subscriptions do not have.
    let mut out = response.finish(String::new(), String::new(), String::new());
    if let Value::Object(map) = &mut out {
        for key in ["accountId", "oldState", "newState"] {
            map.remove(key);
        }
    }
    Ok(out)
}
