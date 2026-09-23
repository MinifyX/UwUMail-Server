//! SieveScript/get, /changes, /set, /query and /validate: a person's mail rules as Sieve scripts
//! (RFC 9661). The content goes up and down as a blob; see docs/sieve.md.

use serde_json::{Map, Value, json};
use uwumail_store::{SIEVE_MAX_SCRIPT_SIZE, SieveError, SieveScript};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;

const DEFAULTS: &[&str] = &["id", "name", "blobId", "isActive"];
const PREFIX: char = 'r';

fn to_json(script: &SieveScript) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": ids::sieve_script(script.id),
        "name": script.name,
        "blobId": ids::blob(&script.blob),
        "isActive": script.is_active,
    }) else {
        unreachable!("object literal")
    };
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let scripts = ctx.jmap.store.sieve_scripts(ctx.account.id).await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    match get_ids(args)? {
        None => list.extend(scripts.iter().map(|script| pick(to_json(script), &properties))),
        Some(requested) => {
            for id in requested {
                match ctx.parse_id(PREFIX, &id).and_then(|n| scripts.iter().find(|script| script.id == n)) {
                    Some(script) => list.push(pick(to_json(script), &properties)),
                    None => not_found.push(id),
                }
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

/// The script behind a blob id: an upload of this account, or the content of one of its scripts.
async fn content(ctx: &Ctx<'_>, blob_id: &str) -> Result<Vec<u8>, SetError> {
    let missing = || SetError::new("blobNotFound", format!("there is no blob {blob_id}"));
    let Some(ids::BlobRef::Whole(hash)) = ids::parse_blob(ctx.resolve(blob_id).unwrap_or(blob_id)) else {
        return Err(missing());
    };
    let store = &ctx.jmap.store;
    if let Some(script) = store.sieve_script_blob(ctx.account.id, &hash).await? {
        return Ok(script.into_bytes());
    }
    if !store.blob_accessible(ctx.account.id, &hash).await? {
        return Err(missing());
    }
    let bytes = store.blob(&hash).await.map_err(|_| missing())?;
    if bytes.len() > SIEVE_MAX_SCRIPT_SIZE {
        return Err(SetError::new("tooLarge", format!("a script may have at most {SIEVE_MAX_SCRIPT_SIZE} bytes")));
    }
    Ok(bytes)
}

fn invalid_sieve(description: impl Into<String>) -> SetError {
    SetError::new("invalidSieve", description)
}

/// Checks a script the way delivery will run it.
fn check(content: &[u8]) -> Result<(), SetError> {
    uwumail_smtp::sieve::validate(content).map_err(invalid_sieve)
}

impl From<SieveError> for SetError {
    fn from(err: SieveError) -> SetError {
        match err {
            SieveError::AlreadyExists(id) => SetError {
                existing_id: Some(ids::sieve_script(id)),
                ..SetError::new("alreadyExists", "a script with this name exists already")
            },
            SieveError::TooMany => SetError::new("overQuota", err.to_string()),
            SieveError::TooLarge => SetError::new("tooLarge", err.to_string()),
            SieveError::InvalidName(message) => SetError::invalid_properties(&["name"], message),
            SieveError::InvalidContent(message) => invalid_sieve(message),
            SieveError::NotFound => SetError::not_found(),
            SieveError::Active => SetError::new("sieveIsActive", "switch the script off before destroying it"),
            SieveError::Store(err) => SetError::from(err),
        }
    }
}

/// `name` of a create or update: `Some(None)` for null.
fn name(object: &Map<String, Value>) -> Result<Option<Option<String>>, SetError> {
    match object.get("name") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(name)) => Ok(Some(Some(name.clone()))),
        Some(_) => Err(SetError::invalid_properties(&["name"], "name must be a string or null")),
    }
}

/// Refuses properties a client may not set. `isActive` is server-set; its current value is fine.
fn check_properties(object: &Map<String, Value>, is_active: bool) -> Result<(), SetError> {
    for (property, value) in object {
        match property.as_str() {
            "name" | "blobId" => {}
            "isActive" if value.as_bool() == Some(is_active) => {}
            "isActive" => {
                return Err(SetError::invalid_properties(
                    &["isActive"],
                    "use onSuccessActivateScript or onSuccessDeactivateScript",
                ));
            }
            "id" => return Err(SetError::invalid_properties(&["id"], "id is set by the server")),
            other => return Err(SetError::invalid_properties(&[other], format!("unknown property {other}"))),
        }
    }
    Ok(())
}

async fn create(ctx: &Ctx<'_>, object: &Value) -> Result<SieveScript, SetError> {
    let object = object.as_object().ok_or_else(|| SetError::new("invalidProperties", "not an object"))?;
    check_properties(object, false)?;
    let name = name(object)?.flatten();
    let blob_id = object
        .get("blobId")
        .and_then(Value::as_str)
        .ok_or_else(|| SetError::invalid_properties(&["blobId"], "blobId is required"))?;
    let content = content(ctx, blob_id).await?;
    check(&content)?;
    Ok(ctx.jmap.store.create_sieve_script(ctx.account.id, name.as_deref(), &content).await?)
}

async fn update(ctx: &Ctx<'_>, id: i64, patch: &Value) -> Result<(SieveScript, bool), SetError> {
    let patch = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "not an object"))?;
    let scripts = ctx.jmap.store.sieve_scripts(ctx.account.id).await?;
    let current = scripts.iter().find(|script| script.id == id).ok_or_else(SetError::not_found)?;
    check_properties(patch, current.is_active)?;
    let name = match name(patch)? {
        Some(None) => return Err(SetError::invalid_properties(&["name"], "a script keeps its name")),
        Some(Some(name)) => Some(name),
        None => None,
    };
    let content = match patch.get("blobId") {
        None => None,
        Some(Value::String(blob_id)) => {
            let content = content(ctx, blob_id).await?;
            check(&content)?;
            Some(content)
        }
        Some(_) => return Err(SetError::invalid_properties(&["blobId"], "blobId must be a string")),
    };
    let updated = ctx.jmap.store.update_sieve_script(ctx.account.id, id, name.as_deref(), content.as_deref()).await?;
    let blob_changed = updated.blob != current.blob;
    Ok((updated, blob_changed))
}

/// Merges server-set changes into an `updated` or `created` entry.
fn merge(entries: &mut Map<String, Value>, key: &str, changes: Value) {
    let entry = entries.entry(key.to_owned()).or_insert(Value::Null);
    if entry.is_null() {
        *entry = json!({});
    }
    if let (Some(entry), Value::Object(changes)) = (entry.as_object_mut(), changes) {
        entry.extend(changes);
    }
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_set_size(args)?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let activate = match args.get("onSuccessActivateScript") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.clone()),
        Some(_) => return Err(MethodError::invalid_arguments("onSuccessActivateScript must be an id")),
    };
    let deactivate = match args.get("onSuccessDeactivateScript") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(deactivate)) => *deactivate,
        Some(_) => return Err(MethodError::invalid_arguments("onSuccessDeactivateScript must be a boolean")),
    };
    let account_id = ctx.account.id;
    let mut response = SetResponse::default();
    let mut all_succeeded = true;
    // Script id -> creation id, to report an activation in `created`.
    let mut created_here: Vec<(i64, String)> = Vec::new();

    if let Some(creates) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in creates {
            match create(ctx, object).await {
                Ok(script) => {
                    let id = ids::sieve_script(script.id);
                    ctx.created_ids.insert(creation_id.clone(), id.clone());
                    created_here.push((script.id, creation_id.clone()));
                    response.created.insert(creation_id.clone(), Value::Object(to_json(&script)));
                }
                Err(err) => {
                    all_succeeded = false;
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(updates) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in updates {
            let result = match ctx.parse_id(PREFIX, id) {
                Some(number) => update(ctx, number, patch).await,
                None => Err(SetError::not_found()),
            };
            match result {
                // The blob id follows the content; the client learns the new one.
                Ok((script, true)) => {
                    response.updated.insert(id.clone(), json!({ "blobId": ids::blob(&script.blob) }));
                }
                Ok((_, false)) => {
                    response.updated.insert(id.clone(), Value::Null);
                }
                Err(err) => {
                    all_succeeded = false;
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(destroys) = args.get("destroy").and_then(Value::as_array) {
        for id in destroys.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id(PREFIX, id) {
                Some(number) => ctx.jmap.store.destroy_sieve_script(account_id, number).await.map_err(SetError::from),
                None => Err(SetError::not_found()),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    all_succeeded = false;
                    response.not_destroyed.insert(id.to_owned(), err.to_json());
                }
            }
        }
    }

    // Only when everything above worked; deactivating first (RFC 9661 section 2.4).
    if all_succeeded {
        let report = |activation: uwumail_store::SieveActivation, response: &mut SetResponse| {
            if let Some(off) = activation.deactivated {
                merge(&mut response.updated, &ids::sieve_script(off), json!({ "isActive": false }));
            }
            if let Some(on) = activation.activated {
                match created_here.iter().find(|(id, _)| *id == on) {
                    Some((_, creation_id)) => merge(&mut response.created, creation_id, json!({ "isActive": true })),
                    None => merge(&mut response.updated, &ids::sieve_script(on), json!({ "isActive": true })),
                }
            }
        };
        if deactivate {
            let activation = ctx.jmap.store.activate_sieve_script(account_id, None).await.map_err(set_failure)?;
            report(activation, &mut response);
        }
        // An id that is invalid or does not exist is ignored.
        if let Some(number) = activate.as_deref().and_then(|id| ctx.parse_id(PREFIX, id)) {
            match ctx.jmap.store.activate_sieve_script(account_id, Some(number)).await {
                Ok(activation) => report(activation, &mut response),
                Err(SieveError::NotFound) => {}
                Err(err) => return Err(set_failure(err)),
            }
        }
    }

    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

fn set_failure(err: SieveError) -> MethodError {
    match err {
        SieveError::Store(err) => err.into(),
        other => MethodError::server_fail(other.to_string()),
    }
}

/// Whether a script matches a filter condition (`name` contained, `isActive`) or operator.
fn matches(script: &SieveScript, filter: &Value) -> MethodResult<bool> {
    let filter = filter.as_object().ok_or_else(|| MethodError::invalid_arguments("filter must be an object"))?;
    if let Some(operator) = filter.get("operator") {
        let conditions = filter
            .get("conditions")
            .and_then(Value::as_array)
            .ok_or_else(|| MethodError::invalid_arguments("an operator needs conditions"))?;
        let results =
            conditions.iter().map(|condition| matches(script, condition)).collect::<MethodResult<Vec<_>>>()?;
        return match operator.as_str() {
            Some("AND") => Ok(results.iter().all(|r| *r)),
            Some("OR") => Ok(results.iter().any(|r| *r)),
            Some("NOT") => Ok(!results.iter().any(|r| *r)),
            _ => Err(MethodError::new("unsupportedFilter", "operator must be AND, OR or NOT")),
        };
    }
    for (key, value) in filter {
        let hit = match (key.as_str(), value) {
            ("name", Value::String(needle)) => script.name.to_lowercase().contains(&needle.to_lowercase()),
            ("isActive", Value::Bool(active)) => script.is_active == *active,
            (other, _) => return Err(MethodError::new("unsupportedFilter", format!("cannot filter by {other}"))),
        };
        if !hit {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let mut scripts = ctx.jmap.store.sieve_scripts(ctx.account.id).await?;
    if let Some(filter) = args.get("filter").filter(|filter| !filter.is_null()) {
        let mut kept = Vec::with_capacity(scripts.len());
        for script in scripts {
            if matches(&script, filter)? {
                kept.push(script);
            }
        }
        scripts = kept;
    }
    if let Some(sort) = args.get("sort").and_then(Value::as_array) {
        for comparator in sort.iter().rev() {
            let ascending = comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true);
            match comparator.get("property").and_then(Value::as_str) {
                Some("name") => scripts.sort_by_key(|script| script.name.to_lowercase()),
                Some("isActive") => scripts.sort_by_key(|script| script.is_active),
                _ => return Err(MethodError::kind("unsupportedSort")),
            }
            if !ascending {
                scripts.reverse();
            }
        }
    }
    let ids: Vec<String> = scripts.iter().map(|script| ids::sieve_script(script.id)).collect();
    let total = ids.len();
    let position = match args.get("position").and_then(Value::as_i64).unwrap_or(0) {
        negative if negative < 0 => total.saturating_sub(negative.unsigned_abs() as usize),
        position => (position as usize).min(total),
    };
    let limit = args.get("limit").and_then(Value::as_u64).map_or(total, |limit| limit as usize);
    let page: Vec<String> = ids.into_iter().skip(position).take(limit).collect();
    Ok(json!({
        "accountId": ctx.account_id(),
        "queryState": state,
        "canCalculateChanges": false,
        "position": position,
        "total": total,
        "ids": page,
    }))
}

pub async fn validate(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let blob_id = args
        .get("blobId")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("blobId is required"))?;
    let error = match content(ctx, blob_id).await {
        Ok(content) if content.len() > SIEVE_MAX_SCRIPT_SIZE => {
            Some(SetError::new("tooLarge", "the script is too large"))
        }
        Ok(content) => check(&content).err(),
        Err(err) => Some(err),
    };
    Ok(json!({ "accountId": ctx.account_id(), "error": error.map(|err| err.to_json()) }))
}
