//! Method implementations and the helpers they share.

mod address_book;
mod calendar;
mod calendar_event;
mod contact_card;
mod copy;
mod email;
mod identity;
mod mailbox;
mod query_changes;
mod senders;
mod settings;
mod sieve;
mod snippet;
mod submission;
mod suggest;
mod thread;
mod vacation;

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use uwumail_store::{Account, Changes};

use crate::api::requires;
use crate::error::{MethodError, MethodResult};
use crate::session::{
    CALENDARS, CONTACTS, CORE, MAIL, SENDERS, SETTINGS, SIEVE, SUBMISSION, SUGGEST, VACATION, WEBMAIL, WEBSOCKET,
};
use crate::{Inner, MAX_OBJECTS_IN_GET, MAX_OBJECTS_IN_SET, ids};

pub const KNOWN_CAPABILITIES: &[&str] =
    &[CORE, MAIL, SUBMISSION, VACATION, SENDERS, SETTINGS, SIEVE, WEBMAIL, CALENDARS, CONTACTS, WEBSOCKET, SUGGEST];

/// The most suggestions one `AddressSuggestion/query` returns.
pub const MAX_SUGGESTIONS: usize = suggest::MAX_LIMIT;

/// One or more `(method name, arguments)` responses for a call.
pub type Outputs = Vec<(String, Value)>;

pub struct Ctx<'a> {
    pub jmap: &'a Inner,
    pub account: Account,
    pub using: Vec<String>,
    pub created_ids: HashMap<String, String>,
    /// When the request began: work that has to be bounded per request (expanding calendar
    /// recurrences) counts from here, across all its method calls.
    pub started: std::time::Instant,
}

impl<'a> Ctx<'a> {
    pub fn new(jmap: &'a Inner, account: Account, using: Vec<String>, created_ids: HashMap<String, String>) -> Ctx<'a> {
        Ctx { jmap, account, using, created_ids, started: std::time::Instant::now() }
    }

    pub fn account_id(&self) -> String {
        ids::account(self.account.id)
    }

    /// Checks `accountId`; this server has exactly one account per login.
    pub fn check_account(&self, args: &Value) -> MethodResult<()> {
        match args.get("accountId").and_then(Value::as_str) {
            Some(id) if id == self.account_id() => Ok(()),
            Some(_) => Err(MethodError::kind("accountNotFound")),
            None => Err(MethodError::invalid_arguments("accountId is required")),
        }
    }

    /// Resolves `#creationId` references to ids created earlier in this request.
    pub fn resolve<'s>(&'s self, value: &'s str) -> Option<&'s str> {
        match value.strip_prefix('#') {
            Some(creation_id) => self.created_ids.get(creation_id).map(String::as_str),
            None => Some(value),
        }
    }

    /// The account behind another account id this login may read, for /copy. Only its own so far;
    /// shared accounts add theirs here.
    pub fn readable_account(&self, id: &str) -> Option<i64> {
        (id == self.account_id()).then_some(self.account.id)
    }

    pub fn parse_id(&self, prefix: char, value: &str) -> Option<i64> {
        self.resolve(value).and_then(|id| ids::parse(prefix, id))
    }

    pub async fn state(&self) -> MethodResult<String> {
        Ok(self.jmap.store.account_modseq(self.account.id).await?.to_string())
    }
}

pub async fn dispatch(ctx: &mut Ctx<'_>, name: &str, args: Value) -> MethodResult<Outputs> {
    let capability = match name.split('/').next().unwrap_or_default() {
        "Core" => CORE,
        "Mailbox" | "Email" | "Thread" | "SearchSnippet" => MAIL,
        "Identity" | "EmailSubmission" => SUBMISSION,
        "VacationResponse" => VACATION,
        "SenderList" => SENDERS,
        "UserSettings" => SETTINGS,
        "Calendar" | "CalendarEvent" | "ParticipantIdentity" => CALENDARS,
        "AddressBook" | "ContactCard" => CONTACTS,
        "SieveScript" => SIEVE,
        "AddressSuggestion" => SUGGEST,
        _ => return Err(MethodError::kind("unknownMethod")),
    };
    if !requires(capability, &ctx.using) {
        return Err(MethodError::new("unknownMethod", format!("add {capability} to `using` to call {name}")));
    }
    if name != "Core/echo" {
        ctx.check_account(&args)?;
    }
    let outputs = call(ctx, name, args).await?;
    // Every /query says whether its /queryChanges can answer.
    if name.ends_with("/query") {
        let can = query_changes::can_calculate(name);
        return Ok(outputs
            .into_iter()
            .map(|(method, mut output)| {
                if method == name && output.get("canCalculateChanges").is_some() {
                    output["canCalculateChanges"] = Value::Bool(can);
                }
                (method, output)
            })
            .collect());
    }
    Ok(outputs)
}

async fn call(ctx: &mut Ctx<'_>, name: &str, args: Value) -> MethodResult<Outputs> {
    let single = |value: Value| Ok(vec![(name.to_owned(), value)]);
    match name {
        "Core/echo" => single(args),
        "Mailbox/get" => single(mailbox::get(ctx, &args).await?),
        "Mailbox/changes" => single(changes(ctx, &args, "Mailbox", 'm').await?),
        "Mailbox/query" => single(mailbox::query(ctx, &args).await?),
        "Mailbox/queryChanges"
        | "Email/queryChanges"
        | "EmailSubmission/queryChanges"
        | "SieveScript/queryChanges"
        | "ContactCard/queryChanges" => single(query_changes::query_changes(ctx, name, &args).await?),
        "CalendarEvent/queryChanges" => Err(MethodError::kind("cannotCalculateChanges")),
        "Email/copy" => copy::copy(ctx, &args).await,
        "Mailbox/set" => single(mailbox::set(ctx, &args).await?),
        "Thread/get" => single(thread::get(ctx, &args).await?),
        "Thread/changes" => single(changes(ctx, &args, "Thread", 't').await?),
        "Email/get" => single(email::get(ctx, &args).await?),
        "Email/changes" => single(changes(ctx, &args, "Email", 'e').await?),
        "Email/query" => single(email::query(ctx, &args).await?),
        "Email/set" => single(email::set(ctx, &args).await?),
        "Email/import" => single(email::import(ctx, &args).await?),
        "Email/parse" => single(email::parse(ctx, &args).await?),
        "SearchSnippet/get" => single(snippet::get(ctx, &args).await?),
        "Identity/get" => single(identity::get(ctx, &args).await?),
        "Identity/changes" => single(changes(ctx, &args, "Identity", 'i').await?),
        "Identity/set" => single(identity::set(ctx, &args).await?),
        "EmailSubmission/get" => single(submission::get(ctx, &args).await?),
        "EmailSubmission/changes" => single(changes(ctx, &args, "EmailSubmission", 's').await?),
        "EmailSubmission/query" => single(submission::query(ctx, &args).await?),
        "EmailSubmission/set" => submission::set(ctx, &args).await,
        "VacationResponse/get" => single(vacation::get(ctx, &args).await?),
        "VacationResponse/set" => single(vacation::set(ctx, &args).await?),
        "SenderList/get" => single(senders::get(ctx, &args).await?),
        "SenderList/set" => single(senders::set(ctx, &args).await?),
        "UserSettings/get" => single(settings::get(ctx, &args).await?),
        "UserSettings/set" => single(settings::set(ctx, &args).await?),
        "Calendar/get" => single(calendar::get(ctx, &args).await?),
        "Calendar/changes" => {
            calendar::check_enabled(ctx)?;
            single(changes(ctx, &args, "Calendar", 'c').await?)
        }
        "Calendar/set" => single(calendar::set(ctx, &args).await?),
        "CalendarEvent/get" => single(calendar_event::get(ctx, &args).await?),
        "CalendarEvent/changes" => {
            calendar::check_enabled(ctx)?;
            single(changes(ctx, &args, "CalendarEvent", 'v').await?)
        }
        "CalendarEvent/set" => single(calendar_event::set(ctx, &args).await?),
        "CalendarEvent/query" => single(calendar_event::query(ctx, &args).await?),
        "ParticipantIdentity/get" => single(calendar::identities_get(ctx, &args).await?),
        "ParticipantIdentity/changes" => {
            calendar::check_enabled(ctx)?;
            single(changes(ctx, &args, "ParticipantIdentity", 'u').await?)
        }
        "ParticipantIdentity/set" => single(calendar::identities_set(ctx, &args).await?),
        "AddressBook/get" => single(address_book::get(ctx, &args).await?),
        "AddressBook/changes" => {
            address_book::check_enabled(ctx)?;
            single(changes(ctx, &args, "AddressBook", 'b').await?)
        }
        "AddressBook/set" => single(address_book::set(ctx, &args).await?),
        "ContactCard/get" => single(contact_card::get(ctx, &args).await?),
        "ContactCard/changes" => {
            address_book::check_enabled(ctx)?;
            single(changes(ctx, &args, "ContactCard", 'k').await?)
        }
        "ContactCard/set" => single(contact_card::set(ctx, &args).await?),
        "ContactCard/query" => single(contact_card::query(ctx, &args).await?),
        "SieveScript/get" => single(sieve::get(ctx, &args).await?),
        "SieveScript/changes" => single(changes(ctx, &args, "SieveScript", 'r').await?),
        "SieveScript/set" => single(sieve::set(ctx, &args).await?),
        "SieveScript/query" => single(sieve::query(ctx, &args).await?),
        "SieveScript/validate" => single(sieve::validate(ctx, &args).await?),
        "AddressSuggestion/query" => single(suggest::query(ctx, &args).await?),
        _ => Err(MethodError::kind("unknownMethod")),
    }
}

/// `ids` of a /get call: `None` means all, which the caller may refuse for large types.
pub fn get_ids(args: &Value) -> MethodResult<Option<Vec<String>>> {
    match args.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            if items.len() > MAX_OBJECTS_IN_GET {
                return Err(MethodError::kind("requestTooLarge"));
            }
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| MethodError::invalid_arguments("ids must be strings"))
                })
                .collect::<MethodResult<Vec<_>>>()
                .map(Some)
        }
        Some(_) => Err(MethodError::invalid_arguments("ids must be an array or null")),
    }
}

/// The requested `properties`, or the defaults. `id` is always included.
pub fn properties(args: &Value, key: &str, defaults: &[&str]) -> MethodResult<Vec<String>> {
    let mut list: Vec<String> = match args.get(key) {
        None | Some(Value::Null) => defaults.iter().map(|p| p.to_string()).collect(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|p| {
                p.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| MethodError::invalid_arguments(format!("{key} must be strings")))
            })
            .collect::<MethodResult<_>>()?,
        Some(_) => return Err(MethodError::invalid_arguments(format!("{key} must be an array"))),
    };
    if key == "properties" && !list.iter().any(|p| p == "id") {
        list.insert(0, "id".into());
    }
    Ok(list)
}

/// Keeps only the requested properties of an object.
pub fn pick(object: Map<String, Value>, properties: &[String]) -> Value {
    let mut out = Map::with_capacity(properties.len());
    for property in properties {
        if let Some(value) = object.get(property) {
            out.insert(property.clone(), value.clone());
        }
    }
    Value::Object(out)
}

pub fn if_in_state(args: &Value, state: &str) -> MethodResult<()> {
    match args.get("ifInState").and_then(Value::as_str) {
        Some(expected) if expected != state => Err(MethodError::kind("stateMismatch")),
        _ => Ok(()),
    }
}

/// Shared /changes implementation over the store's change log.
async fn changes(ctx: &Ctx<'_>, args: &Value, kind: &str, prefix: char) -> MethodResult<Value> {
    let since = args
        .get("sinceState")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("sinceState is required"))?;
    let since: i64 = since.parse().map_err(|_| MethodError::kind("cannotCalculateChanges"))?;
    let max_changes = match args.get("maxChanges") {
        None | Some(Value::Null) => 0,
        Some(value) => match value.as_u64() {
            Some(0) | None => return Err(MethodError::invalid_arguments("maxChanges must be a positive integer")),
            Some(max) => max as usize,
        },
    };
    let Changes { created, updated, destroyed, new_state, has_more } =
        match ctx.jmap.store.changes(ctx.account.id, kind, since, max_changes).await {
            Ok(changes) => changes,
            Err(uwumail_store::StoreError::Invalid(_)) => return Err(MethodError::kind("cannotCalculateChanges")),
            Err(err) => return Err(err.into()),
        };
    let format = |list: Vec<i64>| list.into_iter().map(|id| format!("{prefix}{id}")).collect::<Vec<_>>();
    let mut response = json!({
        "accountId": ctx.account_id(),
        "oldState": since.to_string(),
        "newState": new_state.to_string(),
        "hasMoreChanges": has_more,
        "created": format(created),
        "updated": format(updated),
        "destroyed": format(destroyed),
    });
    if kind == "Mailbox" {
        response["updatedProperties"] = Value::Null;
    }
    Ok(response)
}

/// The page of a sorted /query result that `position` or `anchor`, `anchorOffset` and `limit`
/// ask for (RFC 8620, section 5.5), as the response.
pub fn query_response(
    ctx: &Ctx<'_>,
    args: &Value,
    state: String,
    ids: Vec<String>,
    max_limit: usize,
) -> MethodResult<Value> {
    let total = ids.len();
    let mut position = match args.get("anchor").and_then(Value::as_str) {
        Some(anchor) => {
            let index = ids.iter().position(|id| id == anchor).ok_or_else(|| MethodError::kind("anchorNotFound"))?;
            let offset = args.get("anchorOffset").and_then(Value::as_i64).unwrap_or(0);
            (index as i64 + offset).max(0) as usize
        }
        None => match args.get("position").and_then(Value::as_i64).unwrap_or(0) {
            p if p < 0 => total.saturating_sub(p.unsigned_abs() as usize),
            p => p as usize,
        },
    };
    position = position.min(total);
    let asked = match args.get("limit") {
        None | Some(Value::Null) => None,
        Some(value) => {
            Some(value.as_u64().ok_or_else(|| MethodError::invalid_arguments("limit must be a positive number"))?
                as usize)
        }
    };
    let limit = asked.unwrap_or(max_limit).min(max_limit);
    let page: Vec<String> = ids.into_iter().skip(position).take(limit).collect();
    let mut response = json!({
        "accountId": ctx.account_id(),
        "queryState": state,
        "canCalculateChanges": false,
        "position": position,
        "ids": page,
    });
    if args.get("calculateTotal").and_then(Value::as_bool).unwrap_or(false) {
        response["total"] = json!(total);
    }
    let capped = match asked {
        Some(asked) => asked > max_limit,
        None => total - position > max_limit,
    };
    if capped {
        response["limit"] = json!(limit);
    }
    Ok(response)
}

/// Counts the objects of a /set call against the limit.
/// How long one request may spend on calendar events and contact cards in all its method calls
/// together: without it, each of the calls in a request would get a query's limit anew, and /get
/// and /set none (security-audit-0.7.0 S-45, 0.8.0 C-1).
const REQUEST_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(15);

/// When the request's time for calendar and contact work is up.
pub fn request_deadline(ctx: &Ctx<'_>) -> std::time::Instant {
    ctx.started + REQUEST_TIME_LIMIT
}

/// The most a query filter may hold, operators and conditions together. Each condition is checked
/// against every object, and a text condition reads the object's whole text, so a request-sized
/// filter against one large object ran for hours without looking at the clock
/// (security-audit-0.8.0 C-2). Search forms and apps use a handful.
const MAX_FILTER_CONDITIONS: usize = 100;

/// Refuses a filter with more than [`MAX_FILTER_CONDITIONS`] operators and conditions.
pub fn check_filter_size(filter: Option<&Value>) -> MethodResult<()> {
    fn count(value: &Value, total: &mut usize) -> bool {
        *total += 1;
        if *total > MAX_FILTER_CONDITIONS {
            return false;
        }
        let conditions = value.get("conditions").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        conditions.iter().all(|condition| count(condition, total))
    }
    match filter {
        Some(filter) if !filter.is_null() && !count(filter, &mut 0) => Err(MethodError::new(
            "unsupportedFilter",
            format!("a filter may have at most {MAX_FILTER_CONDITIONS} operators and conditions"),
        )),
        _ => Ok(()),
    }
}

pub fn check_set_size(args: &Value) -> MethodResult<()> {
    let count = args.get("create").and_then(Value::as_object).map_or(0, Map::len)
        + args.get("update").and_then(Value::as_object).map_or(0, Map::len)
        + args.get("destroy").and_then(Value::as_array).map_or(0, Vec::len);
    if count > MAX_OBJECTS_IN_SET {
        return Err(MethodError::kind("requestTooLarge"));
    }
    Ok(())
}

/// Collects /set results into the response shape.
#[derive(Default)]
pub struct SetResponse {
    pub created: Map<String, Value>,
    pub not_created: Map<String, Value>,
    pub updated: Map<String, Value>,
    pub not_updated: Map<String, Value>,
    pub destroyed: Vec<String>,
    pub not_destroyed: Map<String, Value>,
}

impl SetResponse {
    pub fn finish(self, account_id: String, old_state: String, new_state: String) -> Value {
        let map_or_null = |map: Map<String, Value>| if map.is_empty() { Value::Null } else { Value::Object(map) };
        json!({
            "accountId": account_id,
            "oldState": old_state,
            "newState": new_state,
            "created": map_or_null(self.created),
            "notCreated": map_or_null(self.not_created),
            "updated": map_or_null(self.updated),
            "notUpdated": map_or_null(self.not_updated),
            "destroyed": if self.destroyed.is_empty() { Value::Null } else { json!(self.destroyed) },
            "notDestroyed": map_or_null(self.not_destroyed),
        })
    }
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
