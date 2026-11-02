//! Verification codes over JMAP (docs/jmap-verification.md): the `verification` property of an
//! Email and `VerificationCode/query`, the "codes" view of the inbox. The server found the code
//! when the mail came in (docs/verification-codes.md); here it is only read.

use serde_json::{Value, json};

use super::{Ctx, anchored, check_filter_size};
use crate::api::requires;
use crate::error::{MethodError, MethodResult};
use crate::ids;
use crate::session::VERIFICATION;

/// The most ids one `VerificationCode/query` returns: a person has a handful of live codes, and
/// the store keeps at most ten per sender and hour anyway.
const MAX_QUERY_LIMIT: usize = 256;

/// Fills `verification` into the emails of an `Email/get` that asked for it: the code object, or
/// `null` for a mail without one. Without the capability in `using` the property is not there at
/// all, as for any server that does not know it.
pub async fn fill(ctx: &Ctx<'_>, properties: &[String], list: &mut [Value]) -> MethodResult<()> {
    if !properties.iter().any(|p| p == "verification") {
        return Ok(());
    }
    if !requires(VERIFICATION, &ctx.using) {
        for email in list.iter_mut() {
            if let Some(object) = email.as_object_mut() {
                object.remove("verification");
            }
        }
        return Ok(());
    }
    let numbers: Vec<i64> =
        list.iter().filter_map(|email| email["id"].as_str().and_then(|id| ctx.parse_id('e', id))).collect();
    let mut found = ctx.jmap.store.verifications(ctx.account.id, numbers).await?;
    for email in list.iter_mut() {
        let code = email["id"].as_str().and_then(|id| ctx.parse_id('e', id)).and_then(|n| found.remove(&n));
        email["verification"] = code.map_or(Value::Null, |code| code.to_json());
    }
    Ok(())
}

/// `VerificationCode/query`: the ids of the emails with a code, newest first. Only the ones still
/// valid unless the filter says `expired: true`; `expired: null` asks for all. No sorting other
/// than newest first, and no queryChanges: the list is short and changes as codes expire. Paged in
/// the database, never loaded whole (security-audit-0.26.0 VC-07); otherwise answers like every
/// other /query.
pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_filter_size(args.get("filter"))?;
    let mut expired = Some(false);
    if let Some(filter) = args.get("filter").filter(|f| !f.is_null()) {
        let filter = filter.as_object().ok_or_else(|| MethodError::kind("unsupportedFilter"))?;
        for (key, value) in filter {
            match (key.as_str(), value) {
                ("expired", Value::Bool(wanted)) => expired = Some(*wanted),
                ("expired", Value::Null) => expired = None,
                ("expired", _) => return Err(MethodError::invalid_arguments("expired must be a boolean")),
                (other, _) => return Err(MethodError::new("unsupportedFilter", format!("unknown filter {other}"))),
            }
        }
    }
    if args.get("sort").is_some_and(|sort| sort.as_array().is_some_and(|list| !list.is_empty())) {
        return Err(MethodError::new("unsupportedSort", "codes are always newest first"));
    }
    let asked = match args.get("limit") {
        None | Some(Value::Null) => None,
        Some(value) => {
            Some(value.as_u64().ok_or_else(|| MethodError::invalid_arguments("limit must be a positive number"))?
                as usize)
        }
    };
    let limit = asked.unwrap_or(MAX_QUERY_LIMIT).min(MAX_QUERY_LIMIT);
    let state = ctx.state().await?;
    let store = &ctx.jmap.store;
    let account = ctx.account.id;
    let position = match args.get("anchor").and_then(Value::as_str) {
        Some(anchor) => {
            let email = ctx.parse_id('e', anchor).ok_or_else(|| MethodError::kind("anchorNotFound"))?;
            let index = store
                .verification_position(account, expired, email)
                .await?
                .ok_or_else(|| MethodError::kind("anchorNotFound"))?;
            anchored(index, args.get("anchorOffset").and_then(Value::as_i64).unwrap_or(0))
        }
        None => match args.get("position").and_then(Value::as_i64).unwrap_or(0) {
            p if p < 0 => {
                let (_, total) = store.query_verifications(account, expired, 0, 0).await?;
                total.saturating_sub(p.unsigned_abs() as usize)
            }
            p => p as usize,
        },
    };
    let (page, total) = store.query_verifications(account, expired, position, limit).await?;
    let position = position.min(total);
    let mut response = json!({
        "accountId": ctx.account_id(),
        "queryState": state,
        "canCalculateChanges": false,
        "position": position,
        "ids": page.into_iter().map(ids::email).collect::<Vec<_>>(),
    });
    if args.get("calculateTotal").and_then(Value::as_bool).unwrap_or(false) {
        response["total"] = json!(total);
    }
    let capped = match asked {
        Some(asked) => asked > MAX_QUERY_LIMIT,
        None => total - position > MAX_QUERY_LIMIT,
    };
    if capped {
        response["limit"] = json!(limit);
    }
    Ok(response)
}
