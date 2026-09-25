//! Principal/get, Principal/query and Principal/changes (RFC 9670): the people on this server,
//! for choosing whom to share a mailbox with. Each person is an `individual` with the id
//! `p<account>`; their email address is their login.

use std::collections::HashSet;

use serde_json::{Map, Value, json};
use uwumail_store::SharePerson;

use super::{Ctx, get_ids, pick, properties, query_response};
use crate::error::{MethodError, MethodResult};
use crate::sharing::{PRINCIPALS_OWNER, principal_id, shared_accounts};
use crate::{ids, session};

const DEFAULTS: &[&str] = &["id", "type", "name", "description", "email", "timeZone", "capabilities", "accounts"];
const MAX_PRINCIPALS: usize = 1000;

/// Changes whenever someone joins, leaves or is renamed.
fn state(people: &[SharePerson]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for person in people {
        (person.id, &person.login, &person.display_name).hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn to_json(person: &SharePerson, accounts: &Map<String, Value>) -> Map<String, Value> {
    let name = if person.display_name.trim().is_empty() { &person.login } else { &person.display_name };
    let Value::Object(map) = json!({
        "id": principal_id(person.id),
        "type": "individual",
        "name": name,
        "description": null,
        "email": person.login,
        "timeZone": null,
        "capabilities": {},
        "accounts": if accounts.is_empty() { Value::Null } else { Value::Object(accounts.clone()) },
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    map
}

/// The accounts of a person the caller can open: their own, or the one they share from.
fn accounts_of(person: &SharePerson, me: i64, shared: &[(i64, String, bool)]) -> Map<String, Value> {
    let mut accounts = Map::new();
    let owner_capability = |id: i64| json!({ PRINCIPALS_OWNER: { "accountIdForPrincipal": ids::account(id), "principalId": principal_id(id) } });
    if person.id == me {
        accounts.insert(
            ids::account(me),
            json!({ "name": person.login, "isPersonal": true, "isReadOnly": false,
                    "accountCapabilities": owner_capability(me) }),
        );
    } else if let Some((owner, login, read_only)) = shared.iter().find(|(owner, _, _)| *owner == person.id) {
        let mut capabilities = owner_capability(*owner);
        capabilities[session::MAIL] = json!({});
        accounts.insert(
            ids::account(*owner),
            json!({ "name": login, "isPersonal": false, "isReadOnly": read_only, "accountCapabilities": capabilities }),
        );
    }
    accounts
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let people = ctx.jmap.store.share_people().await?;
    let shared = shared_accounts(&ctx.jmap.store, ctx.account.id).await;
    let properties = properties(args, "properties", DEFAULTS)?;
    let wanted: Option<HashSet<String>> = get_ids(args)?.map(|ids| ids.into_iter().collect());
    let mut list = Vec::new();
    let mut found = HashSet::new();
    for person in &people {
        let id = principal_id(person.id);
        if wanted.as_ref().is_some_and(|wanted| !wanted.contains(&id)) {
            continue;
        }
        found.insert(id);
        list.push(pick(to_json(person, &accounts_of(person, ctx.account.id, &shared)), &properties));
    }
    let not_found: Vec<String> =
        wanted.map(|wanted| wanted.into_iter().filter(|id| !found.contains(id)).collect()).unwrap_or_default();
    Ok(json!({ "accountId": ctx.account_id(), "state": state(&people), "list": list, "notFound": not_found }))
}

fn matches(person: &SharePerson, filter: &Map<String, Value>, sharing: &[i64]) -> MethodResult<bool> {
    for (key, value) in filter {
        let text = || {
            value
                .as_str()
                .map(str::to_lowercase)
                .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a string")))
        };
        let ok = match key.as_str() {
            "email" => person.login.to_lowercase().contains(&text()?),
            "name" => person.display_name.to_lowercase().contains(&text()?),
            "text" => {
                let needle = text()?;
                person.login.to_lowercase().contains(&needle) || person.display_name.to_lowercase().contains(&needle)
            }
            "type" => text()? == "individual",
            // Nobody here keeps a time zone on their principal.
            "timeZone" => false,
            "accountIds" => {
                let wanted: Vec<i64> = value
                    .as_array()
                    .ok_or_else(|| MethodError::new("unsupportedFilter", "accountIds must be a list"))?
                    .iter()
                    .filter_map(|id| id.as_str().and_then(|id| ids::parse('a', id)))
                    .collect();
                wanted.contains(&person.id) && sharing.contains(&person.id)
            }
            other => return Err(MethodError::new("unsupportedFilter", format!("unknown filter {other}"))),
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let people = ctx.jmap.store.share_people().await?;
    let mut sharing: Vec<i64> =
        shared_accounts(&ctx.jmap.store, ctx.account.id).await.into_iter().map(|a| a.0).collect();
    sharing.push(ctx.account.id);
    let filter = match args.get("filter") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(filter)) if !filter.contains_key("operator") => filter.clone(),
        Some(_) => return Err(MethodError::new("unsupportedFilter", "only simple filters are supported")),
    };
    let mut ids = Vec::new();
    for person in &people {
        if matches(person, &filter, &sharing)? {
            ids.push(principal_id(person.id));
        }
    }
    query_response(ctx, args, state(&people), ids, MAX_PRINCIPALS)
}

pub async fn changes(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let people = ctx.jmap.store.share_people().await?;
    let current = state(&people);
    let since = args
        .get("sinceState")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("sinceState is required"))?;
    if since != current {
        // No change log is kept for people; the client fetches them anew.
        return Err(MethodError::kind("cannotCalculateChanges"));
    }
    Ok(json!({
        "accountId": ctx.account_id(),
        "oldState": since,
        "newState": current,
        "hasMoreChanges": false,
        "created": [],
        "updated": [],
        "destroyed": [],
    }))
}
