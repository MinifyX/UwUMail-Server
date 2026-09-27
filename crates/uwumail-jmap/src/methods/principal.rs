//! Principal/get, Principal/query and Principal/changes (RFC 9670): the people on this server,
//! for choosing whom to share a mailbox with, and the groups (docs/groups.md). Each person is an
//! `individual` with the id `p<account>`; their email address is their login. A group is a `group`
//! with the id `g<group>` and its address. A shared mailbox one uses is an `other` with the id
//! `p<account>`, so the shared account's principal can be looked up; it is not listed by
//! Principal/query, as nothing can be shared with it.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::{Ctx, get_ids, pick, properties, query_response};
use crate::error::{MethodError, MethodResult};
use crate::sharing::{PRINCIPALS_OWNER, principal_id, shared_accounts};
use crate::{ids, session};

const DEFAULTS: &[&str] = &["id", "type", "name", "description", "email", "timeZone", "capabilities", "accounts"];
const MAX_PRINCIPALS: usize = 1000;

/// A person, a group or a shared mailbox.
struct Principal {
    id: String,
    kind: &'static str,
    name: String,
    email: String,
    /// The account behind it, for people and shared mailboxes.
    account: Option<i64>,
}

/// Everyone the caller may see: all people and groups, and the shared mailboxes they use.
async fn principals(ctx: &Ctx<'_>) -> MethodResult<Vec<Principal>> {
    let store = &ctx.jmap.store;
    let mut list: Vec<Principal> = store
        .share_people()
        .await?
        .into_iter()
        .map(|person| Principal {
            id: principal_id(person.id),
            kind: "individual",
            name: if person.display_name.trim().is_empty() { person.login.clone() } else { person.display_name },
            email: person.login,
            account: Some(person.id),
        })
        .collect();
    for group in store.groups(None).await? {
        list.push(Principal {
            id: format!("g{}", group.id),
            kind: "group",
            name: if group.name.trim().is_empty() { group.address.clone() } else { group.name },
            email: group.address,
            account: None,
        });
    }
    for shared in store.shared_memberships(ctx.account.id).await? {
        list.push(Principal {
            id: principal_id(shared.id),
            kind: "other",
            name: if shared.name.trim().is_empty() { shared.address.clone() } else { shared.name },
            email: shared.address,
            account: Some(shared.id),
        });
    }
    Ok(list)
}

/// Changes whenever someone joins, leaves or is renamed.
fn state(principals: &[Principal]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for principal in principals {
        (&principal.id, &principal.email, &principal.name).hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn to_json(principal: &Principal, accounts: &Map<String, Value>) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "id": principal.id,
        "type": principal.kind,
        "name": principal.name,
        "description": null,
        "email": principal.email,
        "timeZone": null,
        "capabilities": {},
        "accounts": if accounts.is_empty() { Value::Null } else { Value::Object(accounts.clone()) },
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    map
}

/// The accounts of a principal the caller can open: their own, or the one they share from.
fn accounts_of(principal: &Principal, me: i64, shared: &[(i64, String, bool)]) -> Map<String, Value> {
    let mut accounts = Map::new();
    let Some(account) = principal.account else { return accounts };
    let owner_capability = |id: i64| json!({ PRINCIPALS_OWNER: { "accountIdForPrincipal": ids::account(id), "principalId": principal_id(id) } });
    if account == me {
        accounts.insert(
            ids::account(me),
            json!({ "name": principal.email, "isPersonal": true, "isReadOnly": false,
                    "accountCapabilities": owner_capability(me) }),
        );
    } else if let Some((owner, login, read_only)) = shared.iter().find(|(owner, _, _)| *owner == account) {
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
    let principals = principals(ctx).await?;
    let shared = shared_accounts(&ctx.jmap.store, ctx.account.id).await;
    let properties = properties(args, "properties", DEFAULTS)?;
    let wanted: Option<HashSet<String>> = get_ids(args)?.map(|ids| ids.into_iter().collect());
    let mut list = Vec::new();
    let mut found = HashSet::new();
    for principal in &principals {
        if wanted.as_ref().is_some_and(|wanted| !wanted.contains(&principal.id)) || found.contains(&principal.id) {
            continue;
        }
        found.insert(principal.id.clone());
        list.push(pick(to_json(principal, &accounts_of(principal, ctx.account.id, &shared)), &properties));
    }
    let not_found: Vec<String> =
        wanted.map(|wanted| wanted.into_iter().filter(|id| !found.contains(id)).collect()).unwrap_or_default();
    Ok(json!({ "accountId": ctx.account_id(), "state": state(&principals), "list": list, "notFound": not_found }))
}

fn matches(principal: &Principal, filter: &Map<String, Value>, sharing: &[i64]) -> MethodResult<bool> {
    for (key, value) in filter {
        let text = || {
            value
                .as_str()
                .map(str::to_lowercase)
                .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a string")))
        };
        let ok = match key.as_str() {
            "email" => principal.email.to_lowercase().contains(&text()?),
            "name" => principal.name.to_lowercase().contains(&text()?),
            "text" => {
                let needle = text()?;
                principal.email.to_lowercase().contains(&needle) || principal.name.to_lowercase().contains(&needle)
            }
            "type" => text()? == principal.kind,
            // Nobody here keeps a time zone on their principal.
            "timeZone" => false,
            "accountIds" => {
                let wanted: Vec<i64> = value
                    .as_array()
                    .ok_or_else(|| MethodError::new("unsupportedFilter", "accountIds must be a list"))?
                    .iter()
                    .filter_map(|id| id.as_str().and_then(|id| ids::parse('a', id)))
                    .collect();
                principal.account.is_some_and(|account| wanted.contains(&account) && sharing.contains(&account))
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
    let principals = principals(ctx).await?;
    let mut sharing: Vec<i64> =
        shared_accounts(&ctx.jmap.store, ctx.account.id).await.into_iter().map(|a| a.0).collect();
    sharing.push(ctx.account.id);
    let filter = match args.get("filter") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(filter)) if !filter.contains_key("operator") => filter.clone(),
        Some(_) => return Err(MethodError::new("unsupportedFilter", "only simple filters are supported")),
    };
    let mut ids = Vec::new();
    // Shared mailboxes are only looked up, never shared with.
    for principal in principals.iter().filter(|principal| principal.kind != "other") {
        if matches(principal, &filter, &sharing)? {
            ids.push(principal.id.clone());
        }
    }
    query_response(ctx, args, state(&principals), ids, MAX_PRINCIPALS)
}

pub async fn changes(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let current = state(&principals(ctx).await?);
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
