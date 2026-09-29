//! Calendar/get, Calendar/set and ParticipantIdentity (draft-ietf-jmap-calendars, sections 3
//! and 4) on the account's CalDAV calendars and those shared with it. See docs/jmap-calendars.md.
//!
//! Calendars others share with the account appear among its own, with `myRights` saying what it
//! may do; `shareWith` of an own calendar says who else sees it. Principals are the people of the
//! server with the ids Principal/get gives them (`p12`, see [`crate::sharing`]); an account id
//! (`a12`) or an address of the server may stand for one when writing.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use uwumail_store::{
    CalendarPrefs, CalendarPrefsUpdate, DavAccess, DavCollection, DavCollectionUpdate, DavKind, DavShare,
    NewDavCollection, ShareRights, StoreError,
};

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;
use crate::sharing::principal_id;

const DEFAULTS: &[&str] = &[
    "id",
    "name",
    "description",
    "color",
    "sortOrder",
    "isSubscribed",
    "isVisible",
    "isDefault",
    "includeInAvailability",
    "defaultAlertsWithTime",
    "defaultAlertsWithoutTime",
    "timeZone",
    "shareWith",
    "myRights",
    "uwuSharedBy",
    "uwuBirthdays",
];

/// The longest calendar name, in bytes, as the draft allows.
const MAX_NAME_BYTES: usize = 255;
const MAX_DESCRIPTION_BYTES: usize = 10_000;

/// A calendar as the account sees it: for one shared with it, with its own name, colour, order,
/// visibility and time zone in place of the owner's.
pub struct Listed {
    pub collection: DavCollection,
    pub access: DavAccess,
    /// For a calendar shared with the account: its owner's address and name.
    pub owner: Option<(String, String)>,
    /// What the account keeps for itself about the calendar.
    pub prefs: CalendarPrefs,
}

impl Listed {
    /// Which of its events make the account busy: `all`, `attending` or `none`. Its own calendars
    /// count by default, those shared with it and subscribed ones do not.
    pub fn include_in_availability(&self) -> &str {
        match self.prefs.include_in_availability.as_deref() {
            Some(value) => value,
            None if self.access.is_owner() && !self.collection.filled() => "all",
            None => "none",
        }
    }

    /// The default alerts for events with (`true`) or without a time, as an Alert map.
    pub fn default_alerts(&self, with_time: bool) -> Option<Map<String, Value>> {
        let text =
            if with_time { &self.prefs.default_alerts_with_time } else { &self.prefs.default_alerts_without_time };
        match serde_json::from_str(text.as_deref()?) {
            Ok(Value::Object(alerts)) => Some(alerts),
            _ => None,
        }
    }
}

/// The account's calendars, the default one made the first time like CalDAV does, then those
/// others share with it. Lists that only hold tasks, like the reminders of Apple's devices, are not
/// calendars of events and stay out.
pub async fn listed(ctx: &Ctx<'_>) -> MethodResult<Vec<Listed>> {
    let name = ctx.jmap.smtp.tone().language.collection_names().0;
    let own = ctx
        .jmap
        .store
        .dav_collections(ctx.account.id, DavKind::Calendar, NewDavCollection::default_calendar(name))
        .await?;
    let mut prefs = ctx.jmap.store.calendar_prefs(ctx.account.id).await?;
    let mut list: Vec<Listed> = own
        .into_iter()
        .map(|collection| {
            let prefs = prefs.remove(&collection.id).unwrap_or_default();
            Listed { collection, access: DavAccess::Owner, owner: None, prefs }
        })
        .collect();
    for shared in ctx.jmap.store.dav_shared_with(ctx.account.id, DavKind::Calendar).await? {
        let mut collection = shared.collection;
        let prefs = prefs.remove(&collection.id).unwrap_or_default();
        collection.apply_prefs(&prefs);
        list.push(Listed {
            collection,
            access: DavAccess::Shared(shared.rights),
            owner: Some((shared.owner_login, shared.owner_name)),
            prefs,
        });
    }
    Ok(list.into_iter().filter(|listed| holds_events(&listed.collection)).collect())
}

/// The calendars events may be in: the account's own and those shared with it.
pub async fn calendars(ctx: &Ctx<'_>) -> MethodResult<Vec<DavCollection>> {
    Ok(listed(ctx).await?.into_iter().map(|listed| listed.collection).collect())
}

/// Whether the account owns a calendar, which is where scheduling messages are sent from.
pub async fn owns(ctx: &Ctx<'_>, calendar_id: i64) -> bool {
    matches!(ctx.jmap.store.dav_access(ctx.account.id, calendar_id).await, Ok(Some((_, DavAccess::Owner))))
}

fn holds_events(calendar: &DavCollection) -> bool {
    calendar.components.is_empty() || calendar.components.iter().any(|kind| kind.eq_ignore_ascii_case("VEVENT"))
}

/// The calendars are switched off for the account the way CalDAV is.
pub fn check_enabled(ctx: &Ctx<'_>) -> MethodResult<()> {
    if ctx.account.protocols.caldav {
        Ok(())
    } else {
        Err(MethodError::new("accountNotSupportedByMethod", "calendars are switched off for this account"))
    }
}

/// `#RRGGBB` or `#RRGGBBAA` as CalDAV clients store it, as CSS `#rrggbb`.
pub fn css_color(stored: Option<&str>) -> Option<String> {
    let hex = stored?.strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", hex[..6].to_ascii_lowercase()))
}

/// A CSS `#rgb` or `#rrggbb` as the `#RRGGBBAA` Apple's calendar-color uses.
fn stored_color(css: &str) -> Option<String> {
    let hex = css.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let full = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_owned(),
        _ => return None,
    };
    Some(format!("#{}FF", full.to_ascii_uppercase()))
}

/// The CalendarRights object of a level of sharing. Everyone may change their own per-user
/// properties, which never reach the owner's data.
fn share_rights(rights: ShareRights) -> Value {
    let write = rights >= ShareRights::Write;
    json!({
        "mayReadFreeBusy": true,
        "mayReadItems": true,
        "mayWriteAll": write,
        "mayWriteOwn": write,
        "mayUpdatePrivate": true,
        "mayRSVP": write,
        "mayShare": rights == ShareRights::All,
        "mayDelete": false
    })
}

/// What the account may do with one of its calendars or one shared with it.
fn rights(access: DavAccess, may_delete: bool) -> Value {
    match access {
        DavAccess::Owner => json!({
            "mayReadFreeBusy": true,
            "mayReadItems": true,
            "mayWriteAll": true,
            "mayWriteOwn": true,
            "mayUpdatePrivate": true,
            "mayRSVP": true,
            "mayShare": true,
            "mayDelete": may_delete
        }),
        // Leaving a shared calendar is its destruction for this account.
        DavAccess::Shared(level) => {
            let mut rights = share_rights(level);
            rights["mayDelete"] = json!(true);
            rights
        }
    }
}

/// Rights without writing entries, for a subscribed calendar (its feed fills it) and the birthdays
/// calendar (the address books fill it).
fn read_only_if(mut rights: Value, filled: bool) -> Value {
    if filled {
        for flag in ["mayWriteAll", "mayWriteOwn", "mayUpdatePrivate", "mayRSVP"] {
            rights[flag] = json!(false);
        }
    }
    rights
}

/// The level a CalendarRights object asks for: `None` when it grants nothing.
pub(super) fn level_of(rights: &Value) -> Result<Option<ShareRights>, ()> {
    let Value::Object(map) = rights else { return Err(()) };
    let flag = |name: &str| map.get(name).and_then(Value::as_bool).unwrap_or(false);
    if map.values().any(|v| !v.is_boolean()) {
        return Err(());
    }
    Ok(if flag("mayShare") {
        Some(ShareRights::All)
    } else if ["mayWriteAll", "mayWriteOwn", "mayRSVP", "mayWrite"].iter().any(|f| flag(f)) {
        Some(ShareRights::Write)
    } else if flag("mayReadItems") || flag("mayReadFreeBusy") || flag("mayRead") {
        Some(ShareRights::Read)
    } else {
        None
    })
}

fn share_with(shares: &[DavShare]) -> Value {
    if shares.is_empty() {
        return Value::Null;
    }
    let map: Map<String, Value> =
        shares.iter().map(|share| (principal_id(share.account_id), share_rights(share.rights))).collect();
    Value::Object(map)
}

fn to_json(listed: &Listed, only_one: bool, shares: &[DavShare]) -> Map<String, Value> {
    let calendar = &listed.collection;
    let owner = listed.access.is_owner();
    let Value::Object(map) = json!({
        "id": ids::calendar(calendar.id),
        "name": calendar.display_name,
        "description": (!calendar.description.is_empty()).then_some(&calendar.description),
        "color": css_color(calendar.color.as_deref()),
        "sortOrder": calendar.sort_order.max(0),
        "isSubscribed": true,
        "isVisible": calendar.is_visible,
        "isDefault": owner && calendar.is_default,
        "includeInAvailability": listed.include_in_availability(),
        "defaultAlertsWithTime": listed.default_alerts(true),
        "defaultAlertsWithoutTime": listed.default_alerts(false),
        "timeZone": calendar.timezone.as_deref().and_then(uwumail_store::ical::timezone_id),
        "shareWith": if listed.access.may_admin() { share_with(shares) } else { Value::Null },
        "myRights": read_only_if(
            rights(listed.access, !(owner && (only_one || calendar.birthdays))),
            calendar.filled()
        ),
        "uwuSharedBy": listed.owner.as_ref().map(|(address, name)| {
            json!({ "email": address, "name": name, "principalId": principal_id(calendar.account_id) })
        }),
        "uwuBirthdays": calendar.birthdays,
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    map
}

/// Who each calendar the account may share is shared with, by calendar id.
async fn shares_of(ctx: &Ctx<'_>, list: &[Listed]) -> MethodResult<BTreeMap<i64, Vec<DavShare>>> {
    let mut shares: BTreeMap<i64, Vec<DavShare>> = BTreeMap::new();
    for share in ctx.jmap.store.dav_shares(ctx.account.id, None).await? {
        shares.entry(share.collection_id).or_default().push(share);
    }
    // Calendars shared with the account with all rights: it sees who else has them.
    for listed in list.iter().filter(|l| !l.access.is_owner() && l.access.may_admin()) {
        let all = ctx.jmap.store.dav_shares(listed.collection.account_id, Some(listed.collection.id)).await?;
        shares.insert(listed.collection.id, all);
    }
    Ok(shares)
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let list = listed(ctx).await?;
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    // The birthdays calendar is not one of one's own that could stay alone.
    let only_one = list.iter().filter(|l| l.access.is_owner() && !l.collection.birthdays).count() <= 1;
    let shares = shares_of(ctx, &list).await?;
    let json_of = |listed: &Listed| {
        let shares = shares.get(&listed.collection.id).map(Vec::as_slice).unwrap_or_default();
        pick(to_json(listed, only_one, shares), &properties)
    };
    let (found, not_found) = match get_ids(args)? {
        None => (list.iter().map(json_of).collect::<Vec<_>>(), Vec::new()),
        Some(requested) => {
            let mut found = Vec::new();
            let mut not_found = Vec::new();
            for id in requested {
                match ctx.parse_id('c', &id).and_then(|n| list.iter().find(|l| l.collection.id == n)) {
                    Some(listed) => found.push(json_of(listed)),
                    None => not_found.push(id),
                }
            }
            (found, not_found)
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": found, "notFound": not_found }))
}

/// Who a calendar is to be shared with, and how: `None` takes someone off.
pub(super) type Sharing = Vec<(String, Option<ShareRights>)>;

/// The account a principal of `shareWith` stands for when it is given by id: a principal id
/// (`p12`) or, as older clients of this server sent, an account id (`a12`).
pub(super) fn principal_account(principal: &str) -> Option<i64> {
    ids::parse('p', principal).or_else(|| ids::parse('a', principal))
}

/// Reads `shareWith` or a `shareWith/<principal>` patch. Keys are principal ids (`p12`) or, as
/// this server's own help for clients without principals, account ids and addresses of people of
/// the server.
pub(super) fn parse_share_with(key: &str, value: &Value, sharing: &mut Option<(bool, Sharing)>) -> Result<(), ()> {
    let principal_ok = |principal: &str| principal_account(principal).is_some() || principal.contains('@');
    let entry = |value: &Value| -> Result<Option<ShareRights>, ()> {
        match value {
            Value::Null => Ok(None),
            rights => level_of(rights),
        }
    };
    match key.strip_prefix("shareWith/") {
        None => {
            let mut all = Vec::new();
            match value {
                Value::Null => {}
                Value::Object(map) => {
                    for (principal, rights) in map {
                        if !principal_ok(principal) {
                            return Err(());
                        }
                        all.push((principal.clone(), entry(rights)?));
                    }
                }
                _ => return Err(()),
            }
            *sharing = Some((true, all));
        }
        Some(principal) => {
            let principal = crate::jscal::pointer_tokens(principal).and_then(|t| t.into_iter().next()).ok_or(())?;
            if !principal_ok(&principal) {
                return Err(());
            }
            let (_, list) = sharing.get_or_insert_with(|| (false, Vec::new()));
            list.push((principal, entry(value)?));
        }
    }
    Ok(())
}

/// The properties of a create or update, checked. `creating` insists on a name.
#[cfg(test)]
fn parse_properties(object: &Map<String, Value>, creating: bool) -> Result<DavCollectionUpdate, SetError> {
    parse_all(object, creating).map(|parsed| parsed.update)
}

/// What a create or update asks for.
struct Parsed {
    /// The calendar's own properties (the owner's, or with [`Parsed::own_only`] one's own).
    update: DavCollectionUpdate,
    /// What is always kept per person.
    prefs: CalendarPrefsUpdate,
    /// Whether it replaces all shares, and the principals with their new rights.
    sharing: Option<(bool, Sharing)>,
}

impl Parsed {
    /// For a calendar shared with the account: name, colour, order, visibility and time zone are
    /// its own, the description stays the owner's.
    fn own_only(&mut self) {
        let update = std::mem::take(&mut self.update);
        self.update.description = update.description;
        self.prefs.name = update.display_name.map(Some);
        self.prefs.color = update.color;
        self.prefs.sort_order = update.sort_order.map(Some);
        self.prefs.is_visible = update.is_visible.map(Some);
        self.prefs.timezone = update.timezone;
    }
}

fn is_empty(update: &DavCollectionUpdate) -> bool {
    update.display_name.is_none()
        && update.description.is_none()
        && update.color.is_none()
        && update.sort_order.is_none()
        && update.timezone.is_none()
        && update.is_visible.is_none()
}

/// The properties of a create or update, checked. `creating` insists on a name.
fn parse_all(object: &Map<String, Value>, creating: bool) -> Result<Parsed, SetError> {
    let mut update = DavCollectionUpdate::default();
    let mut prefs = CalendarPrefsUpdate::default();
    let mut sharing: Option<(bool, Sharing)> = None;
    let mut bad: Vec<&str> = Vec::new();
    for (key, value) in object {
        if key == "shareWith" || key.starts_with("shareWith/") {
            if parse_share_with(key, value, &mut sharing).is_err() {
                bad.push("shareWith");
            }
            continue;
        }
        match key.as_str() {
            "name" => match value.as_str().map(str::trim) {
                Some(name)
                    if !name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control) =>
                {
                    update.display_name = Some(name.to_owned())
                }
                _ => bad.push("name"),
            },
            "description" => match value {
                Value::Null => update.description = Some(String::new()),
                Value::String(text) if text.len() <= MAX_DESCRIPTION_BYTES => update.description = Some(text.clone()),
                _ => bad.push("description"),
            },
            "color" => match value {
                Value::Null => update.color = Some(None),
                Value::String(css) => match stored_color(css) {
                    Some(color) => update.color = Some(Some(color)),
                    None => bad.push("color"),
                },
                _ => bad.push("color"),
            },
            "sortOrder" => match value.as_u64() {
                Some(order) if order < (1 << 31) => update.sort_order = Some(order as i64),
                _ => bad.push("sortOrder"),
            },
            "isVisible" => match value.as_bool() {
                Some(visible) => update.is_visible = Some(visible),
                None => bad.push("isVisible"),
            },
            "timeZone" => match value {
                Value::Null => update.timezone = Some(None),
                Value::String(name) if crate::jscal::time_zone(name).is_some() => {
                    match uwumail_store::ical::timezone_calendar(name, crate::jscal::now()) {
                        Some(calendar) => update.timezone = Some(Some(calendar)),
                        None => bad.push("timeZone"),
                    }
                }
                _ => bad.push("timeZone"),
            },
            // What this server has only one answer to may be sent with that answer.
            "isSubscribed" if value == &Value::Bool(true) => {}
            // A new calendar is never the birthdays calendar; there is one, made by the server.
            "uwuBirthdays" if creating && value == &Value::Bool(false) => {}
            "includeInAvailability" => match value.as_str() {
                Some(which @ ("all" | "attending" | "none")) => {
                    prefs.include_in_availability = Some(Some(which.to_owned()))
                }
                _ => bad.push("includeInAvailability"),
            },
            "defaultAlertsWithTime" | "defaultAlertsWithoutTime" => match crate::calendar_alerts::check(value) {
                Ok(alerts) => {
                    let alerts = Some(alerts.map(|alerts| Value::Object(alerts).to_string()));
                    if key == "defaultAlertsWithTime" {
                        prefs.default_alerts_with_time = alerts;
                    } else {
                        prefs.default_alerts_without_time = alerts;
                    }
                }
                Err(_) => bad.push(key.as_str()),
            },
            _ => bad.push(key.as_str()),
        }
    }
    if creating && update.display_name.is_none() && !bad.contains(&"name") {
        bad.push("name");
    }
    if bad.is_empty() {
        Ok(Parsed { update, prefs, sharing })
    } else {
        Err(SetError::invalid_properties(&bad, "these properties are missing, not valid or cannot be set here"))
    }
}

/// Default alert ids have to be unique in the account, across its calendars.
fn check_alert_ids(known: &[Listed], calendar_id: Option<i64>, prefs: &CalendarPrefsUpdate) -> Result<(), SetError> {
    let ids = |text: Option<&String>| -> Vec<String> {
        text.and_then(|text| serde_json::from_str::<Map<String, Value>>(text).ok())
            .map(|alerts| alerts.keys().cloned().collect())
            .unwrap_or_default()
    };
    let wanted: Vec<String> = [&prefs.default_alerts_with_time, &prefs.default_alerts_without_time]
        .into_iter()
        .flat_map(|update| ids(update.as_ref().and_then(Option::as_ref)))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let others = known.iter().filter(|listed| Some(listed.collection.id) != calendar_id).flat_map(|listed| {
        ids(listed.prefs.default_alerts_with_time.as_ref())
            .into_iter()
            .chain(ids(listed.prefs.default_alerts_without_time.as_ref()))
    });
    let taken: std::collections::HashSet<String> = others.collect();
    if wanted.iter().any(|id| taken.contains(id) || !seen.insert(id)) {
        return Err(SetError::invalid_properties(
            &["defaultAlertsWithTime", "defaultAlertsWithoutTime"],
            "default alert ids have to be unique across your calendars",
        ));
    }
    Ok(())
}

/// Whether a change touches the default alerts, which events using them then take over.
fn changes_defaults(prefs: &CalendarPrefsUpdate) -> bool {
    prefs.default_alerts_with_time.is_some() || prefs.default_alerts_without_time.is_some()
}

/// Applies `shareWith` to a calendar: all of it (`replace`), or single principals.
pub(super) async fn apply_sharing(
    ctx: &Ctx<'_>,
    calendar: &DavCollection,
    replace: bool,
    wanted: Sharing,
) -> Result<(), SetError> {
    let store = &ctx.jmap.store;
    let invalid = |message: &str| SetError::invalid_properties(&["shareWith"], message);
    // Principal ids and addresses, as account ids.
    let mut resolved: Vec<(i64, Option<ShareRights>)> = Vec::new();
    for (principal, rights) in wanted {
        let account = match principal_account(&principal) {
            Some(id) => Some(id),
            None if principal.contains('@') => store.resolve_recipient(&principal).await.map_err(SetError::from)?,
            None => None,
        };
        let account = account.ok_or_else(|| invalid(&format!("{principal} is nobody on this server")))?;
        if account == calendar.account_id {
            return Err(invalid("the owner is not in shareWith"));
        }
        resolved.push((account, rights));
    }
    let current = store.dav_shares(calendar.account_id, Some(calendar.id)).await.map_err(SetError::from)?;
    if replace {
        for share in current.iter().filter(|s| !resolved.iter().any(|(id, _)| *id == s.account_id)) {
            store.dav_unshare(ctx.account.id, calendar.id, share.account_id).await.map_err(SetError::from)?;
        }
    }
    for (account, rights) in resolved {
        match rights {
            Some(rights) => {
                store.dav_share_with(ctx.account.id, calendar.id, account, rights).await.map_err(|err| match err {
                    StoreError::Rule { code: "unknownPerson" | "ownShare", message } => invalid(&message),
                    other => SetError::from(other),
                })?;
            }
            None if current.iter().any(|s| s.account_id == account) => {
                store.dav_unshare(ctx.account.id, calendar.id, account).await.map_err(SetError::from)?;
            }
            None => {}
        }
    }
    Ok(())
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    check_set_size(args)?;
    let known = listed(ctx).await?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let store = ctx.jmap.store.clone();
    let account_id = ctx.account.id;
    let mut response = SetResponse::default();
    let mut failed = false;

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result: Result<DavCollection, SetError> = async {
                let object =
                    object.as_object().ok_or_else(|| SetError::new("invalidProperties", "must be an object"))?;
                let Parsed { update, prefs, sharing } = parse_all(object, true)?;
                check_alert_ids(&known, None, &prefs)?;
                let slug = random_slug();
                let new = NewDavCollection {
                    slug,
                    display_name: update.display_name.clone().unwrap_or_default(),
                    description: update.description.clone().unwrap_or_default(),
                    color: update.color.clone().flatten(),
                    components: vec!["VEVENT".into(), "VTODO".into()],
                };
                let calendar = store.create_calendar(account_id, new, update).await.map_err(|err| match err {
                    StoreError::Rule { code: "davCollectionsFull", message } => SetError::new("overQuota", message),
                    other => SetError::from(other),
                })?;
                let shared = match sharing {
                    Some((replace, wanted)) => apply_sharing(ctx, &calendar, replace, wanted).await,
                    None => Ok(()),
                };
                let kept = match shared {
                    Ok(()) => store.set_calendar_prefs(account_id, calendar.id, prefs).await.map_err(SetError::from),
                    Err(err) => Err(err),
                };
                if let Err(err) = kept {
                    // All or nothing: the calendar goes again.
                    let _ = store.destroy_calendar(account_id, calendar.id, true).await;
                    return Err(err);
                }
                Ok(calendar)
            }
            .await;
            match result {
                Ok(calendar) => {
                    let id = ids::calendar(calendar.id);
                    ctx.created_ids.insert(creation_id.clone(), id.clone());
                    // What the client did not send: the id, what the server set, the defaults.
                    let prefs = store.calendar_prefs(account_id).await?.remove(&calendar.id).unwrap_or_default();
                    let listed = Listed { collection: calendar, access: DavAccess::Owner, owner: None, prefs };
                    let mut created = to_json(&listed, false, &[]);
                    for key in object.as_object().into_iter().flat_map(Map::keys) {
                        created.remove(key);
                    }
                    created.insert("id".into(), json!(id));
                    response.created.insert(creation_id.clone(), Value::Object(created));
                }
                Err(err) => {
                    failed = true;
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result: Result<(), SetError> = async {
                let calendar_id = ctx.parse_id('c', id).ok_or_else(SetError::not_found)?;
                let listed = known.iter().find(|l| l.collection.id == calendar_id).ok_or_else(SetError::not_found)?;
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                let mut parsed = parse_all(patch, false)?;
                if !listed.access.is_owner() {
                    // Someone else's calendar: name, colour and the like are one's own; its
                    // description and who else sees it only change with all rights.
                    parsed.own_only();
                    if (!is_empty(&parsed.update) || parsed.sharing.is_some()) && !listed.access.may_admin() {
                        return Err(SetError::new("forbidden", "this calendar is someone else's"));
                    }
                }
                if !is_empty(&parsed.update) {
                    let owner = listed.collection.account_id;
                    store.dav_update_collection(owner, calendar_id, parsed.update).await.map_err(|err| match err {
                        StoreError::NotFound(_) => SetError::not_found(),
                        other => SetError::from(other),
                    })?;
                }
                check_alert_ids(&known, Some(calendar_id), &parsed.prefs)?;
                let defaults = changes_defaults(&parsed.prefs);
                store.set_calendar_prefs(account_id, calendar_id, parsed.prefs).await.map_err(SetError::from)?;
                // Events of one's own calendar that use the defaults carry them for CalDAV clients.
                if defaults && listed.access.is_owner() {
                    crate::calendar_alerts::apply(&store, account_id, calendar_id).await.map_err(SetError::from)?;
                }
                if let Some((replace, wanted)) = parsed.sharing {
                    apply_sharing(ctx, &listed.collection, replace, wanted).await?;
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    response.updated.insert(id.clone(), Value::Null);
                }
                Err(err) => {
                    failed = true;
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        let with_events = args.get("onDestroyRemoveEvents").and_then(Value::as_bool).unwrap_or(false);
        for id in destroy.iter().filter_map(Value::as_str) {
            let listed = ctx.parse_id('c', id).and_then(|n| known.iter().find(|l| l.collection.id == n));
            let result = match listed {
                // A calendar shared with the account is left, not deleted: it stays its owner's.
                Some(listed) if !listed.access.is_owner() => {
                    store.dav_unshare(account_id, listed.collection.id, account_id).await.map_err(SetError::from)
                }
                Some(listed) => {
                    store.destroy_calendar(account_id, listed.collection.id, with_events).await.map_err(SetError::from)
                }
                None => Err(SetError::not_found()),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    failed = true;
                    response.not_destroyed.insert(id.to_owned(), err.to_json());
                }
            }
        }
    }

    // Only when everything else worked; an id that does not resolve is ignored, as the draft says.
    // Only the account's own calendars can be its default.
    if !failed && let Some(wanted) = args.get("onSuccessSetIsDefault").and_then(Value::as_str) {
        let before: Vec<DavCollection> =
            listed(ctx).await?.into_iter().filter(|l| l.access.is_owner()).map(|l| l.collection).collect();
        if let Some(calendar_id) =
            ctx.parse_id('c', wanted).filter(|n| before.iter().any(|c| c.id == *n && !c.is_default && !c.filled()))
        {
            store.set_default_calendar(account_id, calendar_id).await?;
            for calendar in before.iter().filter(|c| c.is_default || c.id == calendar_id) {
                let id = ids::calendar(calendar.id);
                let is_default = calendar.id == calendar_id;
                match response.created.values_mut().find(|created| created["id"] == id) {
                    Some(created) => created["isDefault"] = json!(is_default),
                    None => {
                        response.updated.insert(id, json!({ "isDefault": is_default }));
                    }
                }
            }
        }
    }

    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

/// The last segment of a new calendar's CalDAV URL.
fn random_slug() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("the operating system RNG failed");
    hex::encode(bytes)
}

// ------------------------------------------------------------------------------------------------
// ParticipantIdentity: the account itself, the one address events know it by.

pub async fn identities_get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let properties = properties(args, "properties", &["id", "name", "calendarAddress", "isDefault"])?;
    let id = ids::participant(ctx.account.id);
    let Value::Object(identity) = json!({
        "id": id,
        "name": ctx.account.display_name,
        "calendarAddress": format!("mailto:{}", ctx.account.login),
        "isDefault": true,
    }) else {
        unreachable!("object literal")
    };
    let (list, not_found): (Vec<Value>, Vec<String>) = match get_ids(args)? {
        None => (vec![pick(identity, &properties)], Vec::new()),
        Some(requested) => {
            let found = requested.contains(&id);
            (
                if found { vec![pick(identity, &properties)] } else { Vec::new() },
                requested.into_iter().filter(|r| *r != id).collect(),
            )
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

pub async fn identities_set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    check_set_size(args)?;
    let state = ctx.state().await?;
    if_in_state(args, &state)?;
    let mut response = SetResponse::default();
    let forbidden = || SetError::new("forbidden", "the participant identity is the account itself").to_json();
    for creation_id in args.get("create").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
        response.not_created.insert(creation_id.clone(), forbidden());
    }
    for id in args.get("update").and_then(Value::as_object).into_iter().flat_map(Map::keys) {
        response.not_updated.insert(id.clone(), forbidden());
    }
    for id in args.get("destroy").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        response.not_destroyed.insert(id.to_owned(), forbidden());
    }
    Ok(response.finish(ctx.account_id(), state.clone(), state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_between_css_and_caldav() {
        assert_eq!(css_color(Some("#FF4D8DFF")).as_deref(), Some("#ff4d8d"));
        assert_eq!(css_color(Some("#FF4D8D")).as_deref(), Some("#ff4d8d"));
        assert_eq!(css_color(Some("pink")), None);
        assert_eq!(stored_color("#abc").as_deref(), Some("#AABBCCFF"));
        assert_eq!(stored_color("#12ab9f").as_deref(), Some("#12AB9FFF"));
        assert_eq!(stored_color("#12ab9fzz"), None);
        assert_eq!(stored_color("red"), None);
    }

    #[test]
    fn properties_are_checked() {
        let object = |value: Value| value.as_object().unwrap().clone();
        let update =
            parse_properties(&object(json!({ "name": " Arbeit ", "color": "#00ff00", "isVisible": false })), true)
                .unwrap();
        assert_eq!(update.display_name.as_deref(), Some("Arbeit"));
        assert_eq!(update.color, Some(Some("#00FF00FF".into())));
        let err = parse_properties(&object(json!({ "color": "green", "shareWith": 5 })), true).unwrap_err();
        assert_eq!(err.properties, Some(vec!["color".into(), "shareWith".into(), "name".into()]));
        let err = parse_properties(&object(json!({ "timeZone": "Europe/Atlantis" })), false).unwrap_err();
        assert_eq!(err.properties, Some(vec!["timeZone".into()]));
        assert!(parse_properties(&object(json!({ "name": "x".repeat(256) })), false).is_err());
    }
}
