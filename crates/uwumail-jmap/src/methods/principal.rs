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
    /// Whether it uses calendars.
    calendars: bool,
    /// Whether the caller may ask when it is busy: themselves, people in their own domains and
    /// people who share a calendar with them (security-audit-0.16.0 PROTOCOLS-L4).
    availability: bool,
}

/// Everyone the caller may see: all people and groups, and the shared mailboxes they use.
async fn principals(ctx: &Ctx<'_>) -> MethodResult<Vec<Principal>> {
    let store = &ctx.jmap.store;
    let visible = store.availability_visible(ctx.account.id).await?;
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
            calendars: person.calendars,
            availability: person.calendars && visible.contains(&person.id),
        })
        .collect();
    for group in store.groups(None).await? {
        list.push(Principal {
            id: format!("g{}", group.id),
            kind: "group",
            name: if group.name.trim().is_empty() { group.address.clone() } else { group.name },
            email: group.address,
            account: None,
            calendars: false,
            availability: false,
        });
    }
    for shared in store.shared_memberships(ctx.account.id).await? {
        list.push(Principal {
            id: principal_id(shared.id),
            kind: "other",
            name: if shared.name.trim().is_empty() { shared.address.clone() } else { shared.name },
            email: shared.address,
            account: Some(shared.id),
            calendars: false,
            availability: false,
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

fn to_json(principal: &Principal, accounts: &Map<String, Value>, me: Option<i64>) -> Map<String, Value> {
    // What calendars can do with it, for callers that use calendars (draft-ietf-jmap-calendars,
    // section 2.1): whose availability may be asked for, whom calendars may be shared with.
    let mut capabilities = Map::new();
    if let Some(me) = me {
        let person = principal.kind == "individual";
        capabilities.insert(
            session::CALENDARS.into(),
            json!({
                "accountId": (principal.account == Some(me)).then(|| ids::account(me)),
                "mayGetAvailability": person && principal.availability,
                "mayShareWith": person && principal.account != Some(me),
                "calendarAddress": format!("mailto:{}", principal.email),
            }),
        );
    }
    let Value::Object(map) = json!({
        "id": principal.id,
        "type": principal.kind,
        "name": principal.name,
        "description": null,
        "email": principal.email,
        "timeZone": null,
        "capabilities": capabilities,
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

/// The caller, when it uses calendars: then principals say what calendars can do with them.
fn calendar_user(ctx: &Ctx<'_>) -> Option<i64> {
    let me = ctx.shared.as_ref().map_or(&ctx.account, |view| &view.me);
    (me.protocols.caldav && ctx.may_use_dav).then_some(me.id)
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
        let accounts = accounts_of(principal, ctx.account.id, &shared);
        list.push(pick(to_json(principal, &accounts, calendar_user(ctx)), &properties));
    }
    let not_found: Vec<String> =
        wanted.map(|wanted| wanted.into_iter().filter(|id| !found.contains(id)).collect()).unwrap_or_default();
    Ok(json!({ "accountId": ctx.account_id(), "state": state(&principals), "list": list, "notFound": not_found }))
}

fn matches(
    principal: &Principal,
    filter: &Map<String, Value>,
    sharing: &[i64],
    addressed: Option<&Option<i64>>,
) -> MethodResult<bool> {
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
            // Found by any of their addresses, never by a masked one.
            "calendarAddress" => {
                let wanted = text()?;
                let wanted = wanted.strip_prefix("mailto:").unwrap_or(&wanted).to_owned();
                principal.email.to_lowercase() == wanted
                    || addressed.is_some_and(|account| account.is_some() && principal.account == *account)
            }
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
    // The account an address of the calendarAddress condition leads to.
    let addressed = match filter.get("calendarAddress").and_then(Value::as_str) {
        Some(address) => {
            let address = address.strip_prefix("mailto:").unwrap_or(address);
            let masked = ctx.jmap.store.masked_delivery(address).await?.is_some();
            Some(if masked { None } else { ctx.jmap.store.resolve_recipient(address).await? })
        }
        None => None,
    };
    let mut ids = Vec::new();
    // Shared mailboxes are only looked up, never shared with.
    for principal in principals.iter().filter(|principal| principal.kind != "other") {
        if matches(principal, &filter, &sharing, addressed.as_ref())? {
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

// ------------------------------------------------------------------------------------------------
// Principal/getAvailability (draft-ietf-jmap-calendars, section 2.2)

/// When a person of the server is busy. Everyone who uses calendars may ask about the people who
/// do in their own domains and those who share a calendar with them, as CalDAV's free-busy lookups
/// answer (security-audit-0.16.0 PROTOCOLS-L4); the events themselves come along only from
/// calendars the caller may read, and never for private ones.
pub async fn get_availability(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    if !ctx.may_use_dav {
        return Err(MethodError::new("forbidden", "this sign-in is not allowed calendars (scope dav)"));
    }
    super::calendar::check_enabled(ctx)?;
    let id = args.get("id").and_then(Value::as_str).ok_or_else(|| MethodError::invalid_arguments("id is required"))?;
    let time = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .and_then(crate::jscal::parse_utc)
            .ok_or_else(|| MethodError::invalid_arguments(format!("{key} must be a UTCDateTime")))
    };
    let (start, end) = (time("utcStart")?, time("utcEnd")?);
    if end <= start {
        return Err(MethodError::invalid_arguments("utcEnd must be after utcStart"));
    }
    if end - start > crate::availability::MAX_DAYS * 86_400 {
        return Err(MethodError::new(
            "tooLarge",
            format!("availability is worked out for at most {}", crate::availability::MAX_DURATION),
        ));
    }
    let show_details = args.get("showDetails").and_then(Value::as_bool).unwrap_or(false);
    let event_properties = match args.get("eventProperties") {
        None | Some(Value::Null) => None,
        Some(_) => Some(properties(args, "eventProperties", &[])?),
    };
    let principals = principals(ctx).await?;
    let principal = principals.iter().find(|p| p.id == id).ok_or_else(|| MethodError::kind("notFound"))?;
    let account = match (principal.kind, principal.account) {
        ("individual", Some(account)) if principal.availability => account,
        ("individual", Some(_)) if principal.calendars => {
            return Err(MethodError::new("forbidden", "you may not ask when this person is busy"));
        }
        _ => return Err(MethodError::new("forbidden", "this principal has no calendars to be busy in")),
    };
    let person = ctx.jmap.store.account_by_id(account).await?.ok_or_else(|| MethodError::kind("notFound"))?;
    let name = ctx.jmap.smtp.tone().language.collection_names().0;
    let default = uwumail_store::NewDavCollection::default_calendar(name);
    let deadline = super::request_deadline(ctx);
    // The events come along from calendars the caller may read, when they are public.
    let readable: Option<HashSet<i64>> = if show_details {
        Some(super::calendar::calendars(ctx).await?.into_iter().map(|calendar| calendar.id).collect())
    } else {
        None
    };
    let found = match crate::availability::busy(&ctx.jmap.store, &person, default, start, end, deadline, readable)
        .await?
    {
        Ok(found) => found,
        Err(crate::availability::OutOfTime) => {
            return Err(MethodError::new("rateLimit", "working out this availability takes too long; ask for less"));
        }
    };
    let me = ctx.account.id;
    let mut list = Vec::new();
    let mut hidden = Vec::new();
    for busy in found {
        let Some(event) = busy.event else {
            hidden.push(busy.period);
            continue;
        };
        // Someone else's per-user properties stay theirs.
        let mut event = if busy.record.owner_id == me {
            event
        } else {
            crate::jscal::per_user_view(&event, None, std::collections::BTreeSet::new)
        };
        let event_id = match &busy.recurrence_id {
            Some(rid) => ids::event_instance(busy.record.id, rid),
            None => ids::calendar_event(busy.record.id),
        };
        event.remove("iCalendar");
        event.insert("id".into(), json!(event_id));
        event.insert("calendarIds".into(), json!({ ids::calendar(busy.record.calendar_id): true }));
        event.insert("isDraft".into(), json!(busy.record.is_draft));
        event.insert(
            "baseEventId".into(),
            json!(busy.recurrence_id.as_ref().map(|_| ids::calendar_event(busy.record.id))),
        );
        let event = match &event_properties {
            Some(list) => pick(event, list),
            None => Value::Object(event),
        };
        list.push(json!({
            "utcStart": crate::jscal::format_utc(busy.period.start),
            "utcEnd": crate::jscal::format_utc(busy.period.end),
            "busyStatus": busy.period.status,
            "event": event,
            "accountId": ctx.account_id(),
        }));
    }
    for period in crate::availability::merge(&hidden) {
        list.push(json!({
            "utcStart": crate::jscal::format_utc(period.start),
            "utcEnd": crate::jscal::format_utc(period.end),
            "busyStatus": period.status,
            "event": null,
            "accountId": null,
        }));
    }
    list.sort_by(|a, b| a["utcStart"].as_str().cmp(&b["utcStart"].as_str()));
    Ok(json!({ "accountId": ctx.account_id(), "list": list }))
}
