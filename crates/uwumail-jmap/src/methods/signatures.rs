//! SignatureSettings/get and SignatureSettings/set: signatures per domain, for every domain and per
//! address, and the company signatures of the domains (docs/jmap-signatures.md,
//! `urn:uwumail:jmap:signatures`). `Identity/get` keeps handing out the effective signature.

use serde_json::{Map, Value, json};
use uwumail_store::{
    IDENTITY_SIGNATURE_MAX_BYTES, MAX_SIGNATURE_CHANGES, SIGNATURE_ALL_DOMAINS, SIGNATURE_PLACEHOLDERS,
    SignatureChanges, SignatureOverview, SignatureText, StoreError,
};

use super::Ctx;
use crate::error::{MethodError, MethodResult};
use crate::ids;

/// What the capability says.
pub fn capability() -> Value {
    json!({
        "maxSize": IDENTITY_SIGNATURE_MAX_BYTES,
        "maxChanges": MAX_SIGNATURE_CHANGES,
        "placeholders": SIGNATURE_PLACEHOLDERS,
        "allDomains": SIGNATURE_ALL_DOMAINS,
    })
}

/// The overview as JMAP shows it: identity ids as JMAP ids.
pub fn overview_json(overview: &SignatureOverview, identity_id: impl Fn(i64) -> Value) -> Value {
    let mut value = serde_json::to_value(overview).unwrap_or(Value::Null);
    if let Some(list) = value.get_mut("identities").and_then(Value::as_array_mut) {
        for identity in list {
            if let Some(id) = identity.get("id").and_then(Value::as_i64) {
                identity["id"] = identity_id(id);
            }
        }
    }
    value
}

/// A signature in a change: `null` removes it, else `{text, html}`, both strings.
pub fn parse_signature(value: &Value) -> Result<Option<SignatureText>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Object(object) => {
            let mut signature = SignatureText::default();
            for (key, value) in object {
                let text = value.as_str().ok_or_else(|| format!("{key} must be a string"))?.to_owned();
                match key.as_str() {
                    "text" => signature.text = text,
                    "html" => signature.html = text,
                    other => return Err(format!("unknown property {other}")),
                }
            }
            Ok(Some(signature))
        }
        _ => Err("a signature is null or an object with text and html".into()),
    }
}

pub async fn get(ctx: &Ctx<'_>, _args: &Value) -> MethodResult<Value> {
    let overview = ctx.jmap.store.signature_overview(ctx.account.id).await?;
    let mut result = overview_json(&overview, |id| json!(ids::identity(id)));
    result["accountId"] = json!(ctx.account_id());
    Ok(result)
}

fn invalid(message: impl Into<String>) -> MethodError {
    MethodError::new("invalidArguments", message.into())
}

pub async fn set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let store = &ctx.jmap.store;
    let old_state = store.signature_overview(ctx.account.id).await?.state;
    if let Some(expected) = args.get("ifInState").and_then(Value::as_str)
        && expected != old_state
    {
        return Err(MethodError::kind("stateMismatch"));
    }
    let empty = Map::new();
    let object = |name: &str| -> MethodResult<&Map<String, Value>> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(&empty),
            Some(Value::Object(map)) => Ok(map),
            Some(_) => Err(invalid(format!("{name} must be an object"))),
        }
    };
    let mut changes = SignatureChanges::default();
    for (domain, value) in object("domains")? {
        let signature = parse_signature(value).map_err(|err| invalid(format!("domains/{domain}: {err}")))?;
        changes.domains.push((domain.clone(), signature));
    }
    for (id, value) in object("identities")? {
        let number = ctx.parse_id('i', id).ok_or_else(|| invalid(format!("identities/{id}: no such identity")))?;
        let signature = parse_signature(value).map_err(|err| invalid(format!("identities/{id}: {err}")))?;
        changes.identities.push((number, signature));
    }
    match store.set_signatures(ctx.account.id, changes).await {
        Ok(()) => {}
        Err(StoreError::Invalid(message)) => return Err(invalid(message)),
        Err(StoreError::NotFound(message)) => return Err(invalid(format!("{message} does not exist"))),
        Err(err) => return Err(err.into()),
    }
    let new_state = store.signature_overview(ctx.account.id).await?.state;
    Ok(json!({ "accountId": ctx.account_id(), "oldState": old_state, "newState": new_state }))
}
