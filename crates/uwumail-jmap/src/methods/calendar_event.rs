//! CalendarEvent/get, /set and /query (draft-ietf-jmap-calendars, section 5) on the VEVENT entries
//! of the account's CalDAV calendars. See docs/jmap-calendars.md.
//!
//! Events are read from and written back to iCalendar each time; the JSON clients see is what
//! calcard makes of the stored object. Instances of a series that a query expands get ids of their
//! own (`v12_20261027T090000`), which /get and /set understand.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use chrono_tz::Tz;
use serde_json::{Map, Value, json};
use uwumail_store::{CalendarEventRecord, CalendarEventWrite, DAV_RESOURCE_MAX_BYTES, DavCollection, StoreError};

use super::calendar::{calendars, check_enabled};
use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick};
use crate::error::{MethodError, MethodResult, SetError};
use crate::jscal::{self, Parsed};
use crate::{MAX_OBJECTS_IN_GET, ids};

/// Always part of an event returned by /get, whatever `properties` says.
const ALWAYS: &[&str] = &["id", "calendarIds", "isDraft", "isOrigin", "baseEventId"];
/// Properties whose change is the user's own business and does not count as a new version.
const PER_USER: &[&str] = &[
    "calendarIds",
    "isDraft",
    "updated",
    "sequence",
    "keywords",
    "color",
    "freeBusyStatus",
    "useDefaultAlerts",
    "alerts",
];
const MAX_QUERY_LIMIT: usize = 5000;
/// How long one query may spend on expanding recurrences before it gives up.
const QUERY_TIME_LIMIT: Duration = Duration::from_secs(5);
/// Writes retried when CalDAV changed the event between reading and writing it.
const WRITE_ATTEMPTS: usize = 3;

/// An event id: a stored event, or one instance of a stored series.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EventId {
    Stored(i64),
    Instance(i64, String),
}

impl EventId {
    fn parse(ctx: &Ctx<'_>, value: &str) -> Option<EventId> {
        let value = ctx.resolve(value)?;
        match ids::parse('v', value) {
            Some(id) => Some(EventId::Stored(id)),
            None => ids::parse_event_instance(value)
                .filter(|(_, rid)| jscal::parse_local(rid).is_some())
                .map(|(id, rid)| EventId::Instance(id, rid)),
        }
    }

    fn base(&self) -> i64 {
        match self {
            EventId::Stored(id) | EventId::Instance(id, _) => *id,
        }
    }
}

/// The account's addresses as `mailto:` URIs, lowercase: who "we" are in an event.
async fn own_addresses(ctx: &Ctx<'_>) -> MethodResult<Vec<String>> {
    let mut addresses = ctx.jmap.store.addresses(&ctx.account.login).await.unwrap_or_default();
    addresses.push(ctx.account.login.clone());
    Ok(addresses.into_iter().map(|address| format!("mailto:{}", address.to_lowercase())).collect())
}

fn is_origin(event: &Map<String, Value>, own: &[String]) -> bool {
    match event.get("organizerCalendarAddress").and_then(Value::as_str) {
        None => true,
        Some(organizer) => own.contains(&organizer.to_lowercase()),
    }
}

/// Participants that would need a scheduling message, which this server does not send.
fn has_others(event: &Map<String, Value>, own: &[String]) -> bool {
    event.get("participants").and_then(Value::as_object).is_some_and(|participants| {
        participants.values().any(|participant| {
            participant
                .get("calendarAddress")
                .and_then(Value::as_str)
                .is_none_or(|address| !own.contains(&address.to_lowercase()))
        })
    })
}

fn decorate(object: &mut Map<String, Value>, id: String, calendar_id: i64, base: Option<i64>, own: &[String]) {
    let origin = is_origin(object, own);
    object.insert("id".into(), json!(id));
    object.insert("calendarIds".into(), json!({ ids::calendar(calendar_id): true }));
    object.insert("isDraft".into(), json!(false));
    object.insert("isOrigin".into(), json!(origin));
    object.insert("baseEventId".into(), json!(base.map(ids::calendar_event)));
}

/// The requested properties of an event; `None` asks for all stored ones.
fn output(mut object: Map<String, Value>, properties: &Option<Vec<String>>, floating: Tz) -> Value {
    match properties {
        None => {
            object.remove("iCalendar");
            Value::Object(object)
        }
        Some(list) => {
            if list.iter().any(|p| p == "utcStart" || p == "utcEnd")
                && let Some((start, end)) = jscal::span(&object, floating)
            {
                object.insert("utcStart".into(), json!(jscal::format_utc(start)));
                object.insert("utcEnd".into(), json!(jscal::format_utc(end)));
            }
            pick(object, list)
        }
    }
}

fn floating_zone(args: &Value) -> MethodResult<Tz> {
    match args.get("timeZone") {
        None | Some(Value::Null) => Ok(chrono_tz::UTC),
        Some(Value::String(name)) => jscal::time_zone(name)
            .ok_or_else(|| MethodError::invalid_arguments(format!("{name} is not a time zone of the IANA database"))),
        Some(_) => Err(MethodError::invalid_arguments("timeZone must be a time zone name")),
    }
}

/// A stored event, read.
struct Loaded {
    record: CalendarEventRecord,
    parsed: Parsed,
}

async fn load(ctx: &Ctx<'_>, ids: Option<Vec<i64>>) -> MethodResult<Vec<Loaded>> {
    let records = ctx.jmap.store.calendar_events(ctx.account.id, ids).await?;
    run_blocking(move || {
        records
            .into_iter()
            .filter_map(|record| Some(Loaded { parsed: jscal::from_icalendar(&record.content)?, record }))
            .collect()
    })
    .await
}

async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> MethodResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|_| MethodError::server_fail("the calendar work failed"))
}

// ------------------------------------------------------------------------------------------------
// CalendarEvent/get

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let floating = floating_zone(args)?;
    let properties = match args.get("properties") {
        None | Some(Value::Null) => None,
        Some(_) => {
            let mut list = super::properties(args, "properties", &[])?;
            for always in ALWAYS {
                if !list.iter().any(|p| p == always) {
                    list.push((*always).to_owned());
                }
            }
            Some(list)
        }
    };
    if let Some(list) = &properties
        && list.iter().any(|p| p == "utcStart" || p == "utcEnd")
        && list.iter().any(|p| p == "recurrenceOverrides")
    {
        return Err(MethodError::invalid_arguments("utcStart and utcEnd cannot be asked for with recurrenceOverrides"));
    }
    let requested = get_ids(args)?;
    let wanted: Option<Vec<(String, Option<EventId>)>> = requested.map(|list| {
        list.into_iter()
            .map(|id| {
                let parsed = EventId::parse(ctx, &id);
                (id, parsed)
            })
            .collect()
    });
    let loaded = match &wanted {
        None => load(ctx, None).await?,
        Some(list) => {
            let bases: BTreeSet<i64> = list.iter().filter_map(|(_, id)| id.as_ref().map(EventId::base)).collect();
            if bases.is_empty() { Vec::new() } else { load(ctx, Some(bases.into_iter().collect())).await? }
        }
    };
    if wanted.is_none() && loaded.len() > MAX_OBJECTS_IN_GET {
        return Err(MethodError::new("requestTooLarge", "too many events to fetch at once; ask for ids"));
    }
    let own = own_addresses(ctx).await?;
    let (list, not_found) = run_blocking(move || {
        let by_id: HashMap<i64, &Loaded> = loaded.iter().map(|l| (l.record.id, l)).collect();
        let stored = |loaded: &Loaded| {
            let mut object = loaded.parsed.event().clone();
            decorate(&mut object, ids::calendar_event(loaded.record.id), loaded.record.calendar_id, None, &own);
            output(object, &properties, floating)
        };
        let Some(wanted) = wanted else {
            return (loaded.iter().map(stored).collect::<Vec<_>>(), Vec::new());
        };
        let mut series: HashMap<i64, BTreeSet<String>> = HashMap::new();
        let mut list = Vec::new();
        let mut not_found = Vec::new();
        for (text, id) in wanted {
            let found = match &id {
                Some(EventId::Stored(n)) => by_id.get(n).map(|loaded| stored(loaded)),
                Some(EventId::Instance(n, rid)) => by_id.get(n).and_then(|loaded| {
                    let event = loaded.parsed.event();
                    let rids = series.entry(*n).or_insert_with(|| jscal::recurrence_ids(&loaded.record.content, event));
                    if !rids.contains(rid) {
                        return None;
                    }
                    let mut object = jscal::instance(event, rid)?;
                    decorate(&mut object, text.clone(), loaded.record.calendar_id, Some(*n), &own);
                    Some(output(object, &properties, floating))
                }),
                None => None,
            };
            match found {
                Some(object) => list.push(object),
                None => not_found.push(text),
            }
        }
        (list, not_found)
    })
    .await?;
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

// ------------------------------------------------------------------------------------------------
// CalendarEvent/set

fn invalid(error: jscal::Invalid) -> SetError {
    let properties: Vec<&str> = error.properties.iter().map(String::as_str).collect();
    SetError::invalid_properties(&properties, error.description)
}

/// The one calendar of `calendarIds`, which has to be one of the account's.
fn calendar_of(ctx: &Ctx<'_>, value: &Value, calendars: &[DavCollection]) -> Result<i64, SetError> {
    let error = || SetError::invalid_properties(&["calendarIds"], "an event belongs to exactly one of your calendars");
    let Value::Object(map) = value else { return Err(error()) };
    let mut chosen = map.iter().filter(|(_, v)| v.as_bool() == Some(true));
    let (Some((id, _)), None) = (chosen.next(), chosen.next()) else { return Err(error()) };
    if map.values().any(|v| v.as_bool() != Some(true)) {
        return Err(error());
    }
    let id = ctx.parse_id('c', id).ok_or_else(error)?;
    calendars.iter().any(|c| c.id == id).then_some(id).ok_or_else(error)
}

/// Turns `utcStart`/`utcEnd` into `start`/`duration`. Returns the properties the server set.
fn apply_utc(
    event: &mut Map<String, Value>,
    utc_start: Option<Value>,
    utc_end: Option<Value>,
    calendar: &DavCollection,
    start_given: bool,
    duration_given: bool,
) -> Result<Vec<&'static str>, SetError> {
    let mut set = Vec::new();
    let parse = |value: &Value, property: &str| {
        value
            .as_str()
            .and_then(jscal::parse_utc)
            .filter(|t| jscal::in_range(*t))
            .ok_or_else(|| SetError::invalid_properties(&[property], "must be a UTC date and time"))
    };
    if utc_start.is_none() && utc_end.is_none() {
        return Ok(set);
    }
    if (utc_start.is_some() && start_given) || (utc_end.is_some() && duration_given) {
        return Err(SetError::invalid_properties(
            &["utcStart", "utcEnd"],
            "give either utcStart/utcEnd or start/duration",
        ));
    }
    // The event keeps its zone; one without gets the calendar's, or UTC.
    let zone_name = match event.get("timeZone").and_then(Value::as_str) {
        Some(zone) => zone.to_owned(),
        None => {
            let zone = calendar
                .timezone
                .as_deref()
                .and_then(uwumail_store::ical::timezone_id)
                .unwrap_or_else(|| "Etc/UTC".into());
            event.insert("timeZone".into(), json!(zone));
            set.push("timeZone");
            zone
        }
    };
    let zone = jscal::time_zone(&zone_name).unwrap_or(chrono_tz::UTC);
    if let Some(value) = utc_start {
        let start = jscal::from_utc(parse(&value, "utcStart")?, zone)
            .ok_or_else(|| SetError::invalid_properties(&["utcStart"], "out of range"))?;
        event.insert("start".into(), json!(jscal::format_local(start)));
        set.push("start");
    }
    if let Some(value) = utc_end {
        let end = parse(&value, "utcEnd")?;
        let (start, _) = jscal::span(event, zone)
            .ok_or_else(|| SetError::invalid_properties(&["utcEnd"], "the event has no start"))?;
        if end < start {
            return Err(SetError::invalid_properties(&["utcEnd"], "the event cannot end before it starts"));
        }
        event.insert("duration".into(), json!(format!("PT{}S", end - start)));
        set.push("duration");
    }
    Ok(set)
}

/// A new uid: a random UUID.
fn new_uid() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system RNG failed");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Everything a write needs from around it.
struct Writer<'a> {
    calendars: Vec<DavCollection>,
    own: Vec<String>,
    scheduling: bool,
    ctx: &'a Ctx<'a>,
}

impl Writer<'_> {
    fn calendar(&self, id: i64) -> Result<&DavCollection, SetError> {
        self.calendars.iter().find(|c| c.id == id).ok_or_else(|| {
            SetError::invalid_properties(&["calendarIds"], "an event belongs to exactly one of your calendars")
        })
    }

    /// Checks an event, turns it into iCalendar and stores it.
    async fn store(
        &self,
        parsed: &Parsed,
        event: &Map<String, Value>,
        id: Option<i64>,
        calendar_id: i64,
        if_etag: Option<String>,
    ) -> Result<Result<i64, SetError>, ()> {
        let calendar = match self.calendar(calendar_id) {
            Ok(calendar) => calendar,
            Err(err) => return Ok(Err(err)),
        };
        if let Err(err) = jscal::validate(event) {
            return Ok(Err(invalid(err)));
        }
        if self.scheduling && has_others(event, &self.own) {
            return Ok(Err(SetError::new(
                "noSupportedScheduleMethods",
                "this server does not send invitations; store the event without sendSchedulingMessages",
            )));
        }
        let content = match parsed.to_icalendar(event) {
            Ok(content) => content,
            Err(message) => return Ok(Err(SetError::new("invalidProperties", message))),
        };
        if content.len() > DAV_RESOURCE_MAX_BYTES {
            return Ok(Err(SetError::new(
                "tooLarge",
                format!("an event may take up to {DAV_RESOURCE_MAX_BYTES} bytes"),
            )));
        }
        // The same check a CalDAV PUT goes through: what JMAP writes, phones can read.
        let checked = match uwumail_store::ical::check_calendar(&content, &calendar.components) {
            Ok(checked) if checked.component == "VEVENT" => checked,
            Ok(_) | Err(uwumail_store::ical::Refused::UnsupportedComponent(_)) => {
                return Ok(Err(SetError::invalid_properties(&["calendarIds"], "this calendar does not hold events")));
            }
            Err(_) => {
                return Ok(Err(SetError::new("invalidProperties", "the event cannot be stored as iCalendar")));
            }
        };
        if jscal::from_icalendar(&content).is_none() {
            return Ok(Err(SetError::new("invalidProperties", "the event cannot be stored as iCalendar")));
        }
        let write = CalendarEventWrite {
            id,
            calendar_id,
            content,
            uid: checked.uid,
            starts_at: checked.starts_at,
            ends_at: checked.ends_at,
            if_etag,
        };
        match self.ctx.jmap.store.put_calendar_event(self.ctx.account.id, write).await {
            Ok((id, _)) => Ok(Ok(id)),
            // Changed over CalDAV meanwhile: read it again.
            Err(StoreError::Conflict(_)) => Err(()),
            Err(StoreError::QuotaExceeded) => Ok(Err(SetError::new("overQuota", "the calendar is full"))),
            Err(StoreError::NotFound(_)) if id.is_some() => Ok(Err(SetError::not_found())),
            Err(StoreError::NotFound(_)) => Ok(Err(SetError::invalid_properties(&["calendarIds"], "no such calendar"))),
            Err(err) => Ok(Err(SetError::from(err))),
        }
    }

    async fn create(&self, object: &Value) -> Result<(i64, Map<String, Value>), SetError> {
        let Value::Object(object) = object else {
            return Err(SetError::new("invalidProperties", "an event must be an object"));
        };
        // A null is the same as leaving the property out, at the top.
        let mut event: Map<String, Value> =
            object.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect();
        for server_set in ["id", "baseEventId", "isOrigin"] {
            if event.contains_key(server_set) {
                return Err(SetError::invalid_properties(&[server_set], "set by the server"));
            }
        }
        let calendar_id = calendar_of(self.ctx, &event.remove("calendarIds").unwrap_or(Value::Null), &self.calendars)?;
        if event.remove("isDraft").is_some_and(|draft| draft != Value::Bool(false)) {
            return Err(SetError::invalid_properties(&["isDraft"], "drafts are not supported"));
        }
        let mut server_set: Map<String, Value> = Map::new();
        let (utc_start, utc_end) = (event.remove("utcStart"), event.remove("utcEnd"));
        let (start_given, duration_given) = (event.contains_key("start"), event.contains_key("duration"));
        for property in
            apply_utc(&mut event, utc_start, utc_end, self.calendar(calendar_id)?, start_given, duration_given)?
        {
            server_set.insert(property.into(), event[property].clone());
        }
        let now = jscal::format_utc(jscal::now());
        for (property, value) in [("@type", json!("Event")), ("uid", json!(new_uid())), ("sequence", json!(0))] {
            if !event.contains_key(property) {
                event.insert(property.into(), value.clone());
                server_set.insert(property.into(), value);
            }
        }
        // The server is the origin of what is made here, so it keeps the times.
        let created = match event.get("created").and_then(Value::as_str).and_then(jscal::parse_utc) {
            Some(created) if created <= jscal::now() => event["created"].clone(),
            _ => json!(now),
        };
        event.insert("created".into(), created.clone());
        event.insert("updated".into(), json!(now));
        server_set.insert("created".into(), created);
        server_set.insert("updated".into(), json!(now));
        let parsed = Parsed::new_event();
        let id = match self.store(&parsed, &event, None, calendar_id, None).await {
            Ok(result) => result?,
            Err(()) => return Err(SetError::new("serverFail", "the event could not be stored")),
        };
        server_set.insert("id".into(), json!(ids::calendar_event(id)));
        server_set.insert("isDraft".into(), json!(false));
        server_set.insert("isOrigin".into(), json!(is_origin(&event, &self.own)));
        server_set.insert("baseEventId".into(), Value::Null);
        Ok((id, server_set))
    }

    /// Applies a patch to a stored event. Returns what the server set.
    async fn update_stored(&self, id: i64, patch: &Map<String, Value>) -> Result<Value, SetError> {
        let paths: Vec<&str> = patch.keys().map(String::as_str).collect();
        if jscal::overlapping_paths(&paths) {
            return Err(SetError::new("invalidPatch", "one path of the patch lies inside another"));
        }
        for _ in 0..WRITE_ATTEMPTS {
            let loaded = self.load_one(id).await?;
            let base = loaded.parsed.event();
            let mut event = base.clone();
            let mut calendars: BTreeSet<i64> = BTreeSet::from([loaded.record.calendar_id]);
            let (mut utc_start, mut utc_end) = (None, None);
            let mut new_version = false;
            for (path, value) in patch {
                let tokens = jscal::pointer_tokens(path)
                    .ok_or_else(|| SetError::new("invalidPatch", format!("{path} is not a pointer")))?;
                let top = tokens[0].as_str();
                match top {
                    "calendarIds" if tokens.len() == 1 => {
                        calendars = BTreeSet::from([calendar_of(self.ctx, value, &self.calendars)?]);
                    }
                    "calendarIds" if tokens.len() == 2 => {
                        let calendar = self
                            .ctx
                            .parse_id('c', &tokens[1])
                            .ok_or_else(|| SetError::invalid_properties(&["calendarIds"], "no such calendar"))?;
                        match value {
                            Value::Bool(true) => {
                                calendars.insert(calendar);
                            }
                            Value::Null | Value::Bool(false) => {
                                calendars.remove(&calendar);
                            }
                            _ => return Err(SetError::invalid_properties(&["calendarIds"], "values must be true")),
                        }
                    }
                    "isDraft" if tokens.len() == 1 && matches!(value, Value::Bool(false) | Value::Null) => {}
                    "uid" | "@type" if tokens.len() == 1 && base.get(top) == Some(value) => {}
                    "utcStart" if tokens.len() == 1 => utc_start = Some(value.clone()),
                    "utcEnd" if tokens.len() == 1 => utc_end = Some(value.clone()),
                    "id" | "baseEventId" | "isOrigin" | "isDraft" | "uid" | "@type" | "calendarIds" | "utcStart"
                    | "utcEnd" => {
                        return Err(SetError::invalid_properties(&[top], "cannot be changed"));
                    }
                    _ => {
                        jscal::apply_patch(&mut event, path, value.clone())
                            .map_err(|message| SetError::new("invalidPatch", message))?;
                        new_version |= !PER_USER.contains(&top);
                    }
                }
            }
            let [calendar_id] = calendars.iter().copied().collect::<Vec<_>>()[..] else {
                return Err(SetError::invalid_properties(&["calendarIds"], "an event belongs to exactly one calendar"));
            };
            let (start_given, duration_given) = (patch.contains_key("start"), patch.contains_key("duration"));
            let mut server_set = Map::new();
            new_version |= utc_start.is_some() || utc_end.is_some();
            for property in
                apply_utc(&mut event, utc_start, utc_end, self.calendar(calendar_id)?, start_given, duration_given)?
            {
                server_set.insert(property.into(), event[property].clone());
            }
            let current = base.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            let asked = patch.get("sequence").and_then(Value::as_u64);
            if new_version && asked.is_none_or(|asked| asked <= current) {
                event.insert("sequence".into(), json!(current + 1));
                server_set.insert("sequence".into(), json!(current + 1));
            }
            if is_origin(&event, &self.own) {
                let now = json!(jscal::format_utc(jscal::now()));
                event.insert("updated".into(), now.clone());
                server_set.insert("updated".into(), now);
            }
            match self.store(&loaded.parsed, &event, Some(id), calendar_id, Some(loaded.record.etag.clone())).await {
                Ok(result) => {
                    result?;
                    return Ok(Value::Object(server_set));
                }
                Err(()) => continue,
            }
        }
        Err(SetError::new("serverFail", "the event keeps changing; try again"))
    }

    /// Changes one instance of a series: the change becomes an override of the series.
    async fn update_instance(&self, id: i64, rid: &str, patch: &Map<String, Value>) -> Result<(), SetError> {
        for (path, value) in patch {
            let top = path.split('/').next().unwrap_or_default();
            let allowed = match top {
                "isDraft" => matches!(value, Value::Bool(false) | Value::Null),
                "id"
                | "baseEventId"
                | "isOrigin"
                | "calendarIds"
                | "uid"
                | "@type"
                | "method"
                | "recurrenceRule"
                | "recurrenceRules"
                | "recurrenceOverrides"
                | "recurrenceId"
                | "recurrenceIdTimeZone"
                | "utcStart"
                | "utcEnd"
                | "excluded"
                | "privacy"
                | "prodId" => false,
                _ => true,
            };
            if !allowed {
                return Err(SetError::invalid_properties(&[top], "cannot differ for one instance"));
            }
        }
        let paths: Vec<&str> = patch.keys().map(String::as_str).collect();
        if jscal::overlapping_paths(&paths) {
            return Err(SetError::new("invalidPatch", "one path of the patch lies inside another"));
        }
        self.change_series(id, rid, |event, instance| {
            let mut changed = instance.clone();
            for (path, value) in patch.iter().filter(|(path, _)| path.as_str() != "isDraft") {
                jscal::apply_patch(&mut changed, path, value.clone())
                    .map_err(|message| SetError::new("invalidPatch", message))?;
            }
            let own = jscal::override_for(event, rid, &changed);
            let new_version = own.keys().any(|key| !PER_USER.contains(&key.as_str()))
                || patch.keys().any(|path| !PER_USER.contains(&path.split('/').next().unwrap_or_default()));
            Ok((Value::Object(own), new_version))
        })
        .await
    }

    /// Takes one instance out of a series (an EXDATE for CalDAV clients).
    async fn destroy_instance(&self, id: i64, rid: &str) -> Result<(), SetError> {
        self.change_series(id, rid, |_, _| Ok((json!({ "excluded": true }), true))).await
    }

    /// Replaces the override of one existing instance with what `change` makes of it.
    async fn change_series(
        &self,
        id: i64,
        rid: &str,
        change: impl Fn(&Map<String, Value>, &Map<String, Value>) -> Result<(Value, bool), SetError>,
    ) -> Result<(), SetError> {
        for _ in 0..WRITE_ATTEMPTS {
            let loaded = self.load_one(id).await?;
            let base = loaded.parsed.event();
            let content = loaded.record.content.clone();
            let series = base.clone();
            let rids = run_blocking(move || jscal::recurrence_ids(&content, &series))
                .await
                .map_err(|_| SetError::new("serverFail", "expansion failed"))?;
            if !rids.contains(rid) {
                return Err(SetError::not_found());
            }
            let instance = jscal::instance(base, rid).ok_or_else(SetError::not_found)?;
            let (patch, new_version) = change(base, &instance)?;
            let mut event = base.clone();
            let overrides = event.entry("recurrenceOverrides").or_insert_with(|| json!({}));
            if !overrides.is_object() {
                *overrides = json!({});
            }
            overrides[rid] = patch;
            let current = base.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            if new_version {
                event.insert("sequence".into(), json!(current + 1));
            }
            if is_origin(&event, &self.own) {
                event.insert("updated".into(), json!(jscal::format_utc(jscal::now())));
            }
            let etag = Some(loaded.record.etag.clone());
            match self.store(&loaded.parsed, &event, Some(id), loaded.record.calendar_id, etag).await {
                Ok(result) => return result.map(|_| ()),
                Err(()) => continue,
            }
        }
        Err(SetError::new("serverFail", "the event keeps changing; try again"))
    }

    async fn load_one(&self, id: i64) -> Result<Loaded, SetError> {
        let mut loaded = load(self.ctx, Some(vec![id]))
            .await
            .map_err(|_| SetError::new("serverFail", "the event could not be read"))?;
        loaded.pop().ok_or_else(SetError::not_found)
    }
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    check_set_size(args)?;
    let calendars = calendars(ctx).await?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let own = own_addresses(ctx).await?;
    let scheduling = args.get("sendSchedulingMessages").and_then(Value::as_bool).unwrap_or(false);
    let mut response = SetResponse::default();
    let mut created_ids: Vec<(String, String)> = Vec::new();

    {
        let writer = Writer { calendars, own, scheduling, ctx };
        if let Some(create) = args.get("create").and_then(Value::as_object) {
            for (creation_id, object) in create {
                match writer.create(object).await {
                    Ok((id, server_set)) => {
                        created_ids.push((creation_id.clone(), ids::calendar_event(id)));
                        response.created.insert(creation_id.clone(), Value::Object(server_set));
                    }
                    Err(err) => {
                        response.not_created.insert(creation_id.clone(), err.to_json());
                    }
                }
            }
        }
    }
    // Later updates in the same call may name the new events by their creation ids.
    ctx.created_ids.extend(created_ids);
    let calendars = super::calendar::calendars(ctx).await?;
    let own = own_addresses(ctx).await?;
    let writer = Writer { calendars, own, scheduling, ctx };

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result = async {
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                match EventId::parse(ctx, id) {
                    Some(EventId::Stored(n)) => writer.update_stored(n, patch).await,
                    Some(EventId::Instance(n, rid)) => {
                        writer.update_instance(n, &rid, patch).await.map(|()| Value::Null)
                    }
                    None => Err(SetError::not_found()),
                }
            }
            .await;
            match result {
                Ok(server_set) => {
                    let server_set =
                        if server_set.as_object().is_some_and(Map::is_empty) { Value::Null } else { server_set };
                    response.updated.insert(id.clone(), server_set);
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match EventId::parse(ctx, id) {
                Some(EventId::Stored(n)) => {
                    if scheduling
                        && let Ok(loaded) = writer.load_one(n).await
                        && has_others(loaded.parsed.event(), &writer.own)
                    {
                        Err(SetError::new("noSupportedScheduleMethods", "this server does not send cancellations"))
                    } else {
                        ctx.jmap.store.destroy_calendar_event(ctx.account.id, n, None).await.map_err(SetError::from)
                    }
                }
                Some(EventId::Instance(n, rid)) => writer.destroy_instance(n, &rid).await,
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

// ------------------------------------------------------------------------------------------------
// CalendarEvent/query

const CONDITIONS: &[&str] =
    &["inCalendar", "after", "before", "text", "title", "description", "location", "owner", "attendee", "uid"];

/// A filter, checked once, with its times in UTC.
#[derive(Debug, Clone)]
enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Vec<Filter>),
    Condition(Condition),
}

#[derive(Debug, Clone, Default)]
struct Condition {
    /// `None` inside: a calendar that is not the account's, which nothing is in.
    calendar: Option<Option<i64>>,
    after: Option<i64>,
    before: Option<i64>,
    text: Option<Vec<String>>,
    title: Option<Vec<String>>,
    description: Option<Vec<String>>,
    location: Option<Vec<String>>,
    owner: Option<Vec<String>>,
    attendee: Option<Vec<String>>,
    uid: Option<String>,
}

fn parse_filter(ctx: &Ctx<'_>, value: &Value, zone: Tz) -> MethodResult<Filter> {
    let Value::Object(map) = value else {
        return Err(MethodError::new("unsupportedFilter", "a filter is an object"));
    };
    if let Some(operator) = map.get("operator") {
        let conditions = map
            .get("conditions")
            .and_then(Value::as_array)
            .ok_or_else(|| MethodError::new("unsupportedFilter", "an operator needs conditions"))?
            .iter()
            .map(|c| parse_filter(ctx, c, zone))
            .collect::<MethodResult<Vec<_>>>()?;
        return match operator.as_str() {
            Some("AND") => Ok(Filter::And(conditions)),
            Some("OR") => Ok(Filter::Or(conditions)),
            Some("NOT") => Ok(Filter::Not(conditions)),
            _ => Err(MethodError::new("unsupportedFilter", "operator must be AND, OR or NOT")),
        };
    }
    let mut condition = Condition::default();
    for (key, value) in map {
        if !CONDITIONS.contains(&key.as_str()) {
            return Err(MethodError::new("unsupportedFilter", format!("{key} is not a filter of CalendarEvent/query")));
        }
        let text =
            value.as_str().ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a text")))?;
        let time = || {
            jscal::parse_local(text)
                .map(|local| jscal::to_utc(local, zone))
                .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be YYYY-MM-DDTHH:MM:SS")))
        };
        match key.as_str() {
            "inCalendar" => condition.calendar = Some(ctx.parse_id('c', text)),
            "after" => condition.after = Some(time()?),
            "before" => condition.before = Some(time()?),
            "text" => condition.text = Some(jscal::search_terms(text)),
            "title" => condition.title = Some(jscal::search_terms(text)),
            "description" => condition.description = Some(jscal::search_terms(text)),
            "location" => condition.location = Some(jscal::search_terms(text)),
            "owner" => condition.owner = Some(jscal::search_terms(text)),
            "attendee" => condition.attendee = Some(jscal::search_terms(text)),
            _ => condition.uid = Some(text.to_owned()),
        }
    }
    Ok(Filter::Condition(condition))
}

fn participants_with_role(event: &Map<String, Value>, role: &str) -> String {
    let mut out = String::new();
    if let Some(Value::Object(participants)) = event.get("participants") {
        for participant in
            participants.values().filter(|p| p.get("roles").and_then(|r| r.get(role)) == Some(&Value::Bool(true)))
        {
            for field in ["name", "email", "calendarAddress"] {
                if let Some(text) = participant.get(field).and_then(Value::as_str) {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Whether the text conditions hold for one object (an event or an instance).
fn text_matches(condition: &Condition, object: &Map<String, Value>) -> bool {
    let field = |name: &str| object.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
    let locations = || jscal::texts_of(object, "locations", &["name", "description"]);
    let all = || {
        [
            field("title"),
            field("description"),
            locations(),
            jscal::texts_of(object, "virtualLocations", &["name", "description", "uri"]),
            jscal::texts_of(object, "participants", &["name", "email", "calendarAddress"]),
        ]
        .join("\n")
    };
    let holds = |terms: &Option<Vec<String>>, haystack: &dyn Fn() -> String| {
        terms.as_ref().is_none_or(|terms| jscal::matches_terms(&haystack(), terms))
    };
    holds(&condition.title, &|| field("title"))
        && holds(&condition.description, &|| field("description"))
        && holds(&condition.location, &locations)
        && holds(&condition.owner, &|| participants_with_role(object, "owner"))
        && holds(&condition.attendee, &|| participants_with_role(object, "attendee"))
        && holds(&condition.text, &all)
}

/// When an instance starts and ends, without building it when it is not changed.
fn instance_span(event: &Map<String, Value>, rid: &str, floating: Tz) -> Option<(i64, i64)> {
    let changed = event
        .get("recurrenceOverrides")
        .and_then(|o| o.get(rid))
        .is_some_and(|p| p.as_object().is_some_and(|p| !p.is_empty()));
    if changed {
        return jscal::span(&jscal::instance(event, rid)?, floating);
    }
    let mut times = Map::new();
    for key in ["timeZone", "duration", "showWithoutTime"] {
        if let Some(value) = event.get(key) {
            times.insert(key.into(), value.clone());
        }
    }
    times.insert("start".into(), json!(rid));
    jscal::span(&times, floating)
}

/// One line of a query's result.
struct Hit {
    id: String,
    start: i64,
    uid: String,
    recurrence_id: String,
    created: String,
    updated: String,
}

struct Evaluator {
    floating: Tz,
    deadline: Instant,
}

impl Evaluator {
    fn check_time(&self) -> MethodResult<()> {
        if Instant::now() > self.deadline {
            return Err(MethodError::new("cannotCalculateOccurrences", "expanding the recurrences takes too long"));
        }
        Ok(())
    }

    /// Whether a stored event matches (any instance may satisfy any condition).
    fn matches(&self, filter: &Filter, loaded: &Loaded, rids: &mut Option<BTreeSet<String>>) -> MethodResult<bool> {
        Ok(match filter {
            Filter::And(all) => {
                for f in all {
                    if !self.matches(f, loaded, rids)? {
                        return Ok(false);
                    }
                }
                true
            }
            Filter::Or(any) => {
                for f in any {
                    if self.matches(f, loaded, rids)? {
                        return Ok(true);
                    }
                }
                false
            }
            Filter::Not(none) => {
                for f in none {
                    if self.matches(f, loaded, rids)? {
                        return Ok(false);
                    }
                }
                true
            }
            Filter::Condition(condition) => self.condition_matches(condition, loaded, rids)?,
        })
    }

    fn condition_matches(
        &self,
        condition: &Condition,
        loaded: &Loaded,
        rids: &mut Option<BTreeSet<String>>,
    ) -> MethodResult<bool> {
        let event = loaded.parsed.event();
        if let Some(calendar) = condition.calendar
            && calendar != Some(loaded.record.calendar_id)
        {
            return Ok(false);
        }
        if condition.uid.as_ref().is_some_and(|uid| event.get("uid").and_then(Value::as_str) != Some(uid.as_str())) {
            return Ok(false);
        }
        let recurring = jscal::is_recurring(event);
        if condition.after.is_some() || condition.before.is_some() {
            let hit = if recurring {
                self.check_time()?;
                let rids = rids.get_or_insert_with(|| jscal::recurrence_ids(&loaded.record.content, event));
                rids.iter().any(|rid| {
                    instance_span(event, rid, self.floating)
                        .is_some_and(|(s, e)| jscal::overlaps(s, e, condition.after, condition.before))
                })
            } else {
                jscal::span(event, self.floating)
                    .is_some_and(|(s, e)| jscal::overlaps(s, e, condition.after, condition.before))
            };
            if !hit {
                return Ok(false);
            }
        }
        if text_matches(condition, event) {
            return Ok(true);
        }
        // Changed instances have texts of their own.
        let overrides = event.get("recurrenceOverrides").and_then(Value::as_object);
        Ok(overrides.is_some_and(|overrides| {
            overrides
                .keys()
                .any(|rid| jscal::instance(event, rid).is_some_and(|instance| text_matches(condition, &instance)))
        }))
    }

    fn hit(&self, id: String, object: &Map<String, Value>, recurrence_id: Option<&str>, start: i64) -> Hit {
        let text = |name: &str| object.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
        Hit {
            id,
            start,
            uid: text("uid"),
            recurrence_id: recurrence_id.unwrap_or_default().to_owned(),
            created: text("created"),
            updated: text("updated"),
        }
    }

    /// The hits of one event for a plain query.
    fn stored_hits(&self, filter: &Option<Filter>, loaded: &Loaded) -> MethodResult<Vec<Hit>> {
        let mut rids = None;
        if let Some(filter) = filter
            && !self.matches(filter, loaded, &mut rids)?
        {
            return Ok(Vec::new());
        }
        let event = loaded.parsed.event();
        let start = jscal::span(event, self.floating).map_or(i64::MIN, |(s, _)| s);
        Ok(vec![self.hit(ids::calendar_event(loaded.record.id), event, None, start)])
    }

    /// The hits of one event with its recurrences expanded: one per instance in the window.
    fn expanded_hits(&self, condition: &Condition, loaded: &Loaded) -> MethodResult<Vec<Hit>> {
        let event = loaded.parsed.event();
        if !jscal::is_recurring(event) {
            return self.stored_hits(&Some(Filter::Condition(condition.clone())), loaded);
        }
        if let Some(calendar) = condition.calendar
            && calendar != Some(loaded.record.calendar_id)
        {
            return Ok(Vec::new());
        }
        if condition.uid.as_ref().is_some_and(|uid| event.get("uid").and_then(Value::as_str) != Some(uid.as_str())) {
            return Ok(Vec::new());
        }
        self.check_time()?;
        let mut hits = Vec::new();
        for rid in jscal::recurrence_ids(&loaded.record.content, event) {
            let Some((start, end)) = instance_span(event, &rid, self.floating) else { continue };
            if !jscal::overlaps(start, end, condition.after, condition.before) {
                continue;
            }
            let Some(instance) = jscal::instance(event, &rid) else { continue };
            if text_matches(condition, &instance) {
                hits.push(self.hit(ids::event_instance(loaded.record.id, &rid), &instance, Some(&rid), start));
            }
        }
        Ok(hits)
    }
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let floating = floating_zone(args)?;
    let expand = args.get("expandRecurrences").and_then(Value::as_bool).unwrap_or(false);
    let filter = match args.get("filter") {
        None | Some(Value::Null) => None,
        Some(value) => Some(parse_filter(ctx, value, floating)?),
    };
    // The window the storage can narrow down to, with room for floating times in any zone.
    let mut window = (None, None, None);
    if let Some(Filter::Condition(condition)) = &filter {
        let margin = 2 * 86_400;
        window =
            (condition.calendar.flatten(), condition.after.map(|t| t - margin), condition.before.map(|t| t + margin));
        if condition.calendar == Some(None) {
            window.0 = Some(-1);
        }
    }
    let expanded = if expand {
        let Some(Filter::Condition(condition)) = &filter else {
            return Err(MethodError::invalid_arguments(
                "expandRecurrences needs a filter condition with after and before",
            ));
        };
        let (Some(after), Some(before)) = (condition.after, condition.before) else {
            return Err(MethodError::invalid_arguments(
                "expandRecurrences needs a filter condition with after and before",
            ));
        };
        if before - after > jscal::MAX_EXPANDED_DAYS * 86_400 + 2 * 3600 {
            return Err(MethodError::new(
                "expandDurationTooLarge",
                format!("recurrences are expanded over at most {}", jscal::MAX_EXPANDED_DURATION),
            ));
        }
        Some(condition.clone())
    } else {
        None
    };
    let mut sort: Vec<(String, bool)> = Vec::new();
    if let Some(list) = args.get("sort").filter(|s| !s.is_null()) {
        for comparator in list.as_array().ok_or_else(|| MethodError::invalid_arguments("sort must be a list"))? {
            let property = comparator.get("property").and_then(Value::as_str).unwrap_or_default();
            if !["start", "uid", "recurrenceId", "created", "updated"].contains(&property) {
                return Err(MethodError::new("unsupportedSort", format!("cannot sort by {property}")));
            }
            sort.push((property.to_owned(), comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true)));
        }
    }
    let records = ctx.jmap.store.calendar_events_between(ctx.account.id, window.0, window.1, window.2).await?;
    let evaluator = Evaluator { floating, deadline: Instant::now() + QUERY_TIME_LIMIT };
    let mut hits = run_blocking(move || -> MethodResult<Vec<Hit>> {
        let mut hits = Vec::new();
        for record in records {
            evaluator.check_time()?;
            let Some(parsed) = jscal::from_icalendar(&record.content) else { continue };
            let loaded = Loaded { record, parsed };
            match &expanded {
                Some(condition) => hits.extend(evaluator.expanded_hits(condition, &loaded)?),
                None => hits.extend(evaluator.stored_hits(&filter, &loaded)?),
            }
        }
        Ok(hits)
    })
    .await??;

    hits.sort_by(|a, b| {
        for (property, ascending) in &sort {
            let order = match property.as_str() {
                "start" => a.start.cmp(&b.start),
                "uid" => a.uid.cmp(&b.uid),
                "recurrenceId" => a.recurrence_id.cmp(&b.recurrence_id),
                "created" => a.created.cmp(&b.created),
                _ => a.updated.cmp(&b.updated),
            };
            let order = if *ascending { order } else { order.reverse() };
            if order.is_ne() {
                return order;
            }
        }
        a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id))
    });
    let total = hits.len();
    let mut position = match args.get("anchor").and_then(Value::as_str) {
        Some(anchor) => {
            let index =
                hits.iter().position(|hit| hit.id == anchor).ok_or_else(|| MethodError::kind("anchorNotFound"))?;
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
    let limit = asked.unwrap_or(MAX_QUERY_LIMIT).min(MAX_QUERY_LIMIT);
    let ids: Vec<String> = hits.into_iter().skip(position).take(limit).map(|hit| hit.id).collect();
    let mut response = json!({
        "accountId": ctx.account_id(),
        "queryState": state,
        "canCalculateChanges": false,
        "position": position,
        "ids": ids,
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
