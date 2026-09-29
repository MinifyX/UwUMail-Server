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

use super::calendar::{calendars, check_enabled, owns};
use super::{
    Ctx, SetResponse, check_filter_size, check_set_size, get_ids, if_in_state, pick, query_response, request_deadline,
};
use crate::calendar_alerts;
use crate::error::{MethodError, MethodResult, SetError};
use crate::jscal::{self, Parsed};
use crate::{MAX_OBJECTS_IN_GET, ids};

/// Always part of an event returned by /get, whatever `properties` says.
const ALWAYS: &[&str] = &["id", "calendarIds", "isDraft", "isOrigin", "baseEventId"];
/// Properties whose change does not count as a new version: the per-user ones and what only
/// says where and whether the event is kept.
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
const SORTS: &[&str] = &["start", "uid", "recurrenceId", "created", "updated"];

fn out_of_time() -> MethodError {
    MethodError::new(
        "serverUnavailable",
        "this request has used up its time for calendar events; send the rest in a new request",
    )
}

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

fn decorate(
    object: &mut Map<String, Value>,
    id: String,
    record: &CalendarEventRecord,
    base: Option<i64>,
    own: &[String],
) {
    let origin = is_origin(object, own);
    object.insert("id".into(), json!(id));
    object.insert("calendarIds".into(), json!({ ids::calendar(record.calendar_id): true }));
    object.insert("isDraft".into(), json!(record.is_draft));
    object.insert("isOrigin".into(), json!(origin));
    object.insert("baseEventId".into(), json!(base.map(ids::calendar_event)));
    decorate_birthday(object, &record.content);
}

/// An event of the birthdays calendar says whose date it is (`uwuBirthday`, docs/birthdays.md),
/// and an instance of one has the age of its year in its title: "Max Muster (30)".
fn decorate_birthday(object: &mut Map<String, Value>, content: &str) {
    let Some(event) = uwumail_store::birthdays::birthday_event(content) else { return };
    let year = object
        .get("recurrenceId")
        .and_then(Value::as_str)
        .and_then(|rid| rid.get(..4))
        .and_then(|year| year.parse::<i32>().ok());
    if let Some(year) = year {
        object.insert("title".into(), json!(event.title_in(year)));
    }
    object.insert(
        "uwuBirthday".into(),
        json!({
            "contactId": ids::contact_card(event.card_id),
            "kind": event.kind.as_str(),
            "label": event.label,
            "name": event.name,
            "year": event.year,
        }),
    );
}

/// The requested properties of an event; `None` asks for all stored ones.
fn output(mut object: Map<String, Value>, properties: &Option<Vec<String>>, floating: Tz) -> Value {
    // The iCalendar conversion hints only when asked for, in the overrides too.
    if !properties.as_ref().is_some_and(|list| list.iter().any(|p| p == "iCalendar")) {
        object.remove("iCalendar");
        if let Some(Value::Object(overrides)) = object.get_mut("recurrenceOverrides") {
            for patch in overrides.values_mut().filter_map(Value::as_object_mut) {
                patch.remove("iCalendar");
            }
        }
    }
    match properties {
        None => Value::Object(object),
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

/// What `CalendarEvent/get` leaves out on request (draft section 5.7): overrides outside
/// `recurrenceOverridesAfter`/`Before`, and with `reduceParticipants`, or for someone who is no
/// owner of an event that hides its attendees, every participant but the owners and oneself.
struct Shaping {
    after: Option<i64>,
    before: Option<i64>,
    reduce: bool,
    own: Vec<String>,
}

impl Shaping {
    fn from(args: &Value, own: Vec<String>) -> MethodResult<Shaping> {
        let time = |key: &str| match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_str()
                .and_then(jscal::parse_utc)
                .map(Some)
                .ok_or_else(|| MethodError::invalid_arguments(format!("{key} must be a UTCDateTime"))),
        };
        Ok(Shaping {
            after: time("recurrenceOverridesAfter")?,
            before: time("recurrenceOverridesBefore")?,
            reduce: args.get("reduceParticipants").and_then(Value::as_bool).unwrap_or(false),
            own,
        })
    }

    fn is_own(&self, participant: &Value) -> bool {
        participant
            .get("calendarAddress")
            .and_then(Value::as_str)
            .is_some_and(|address| self.own.contains(&address.to_lowercase()))
    }

    fn keeps(&self, participant: &Value) -> bool {
        participant.get("roles").and_then(|roles| roles.get("owner")) == Some(&Value::Bool(true))
            || self.is_own(participant)
    }

    fn apply(&self, object: &mut Map<String, Value>, floating: Tz) {
        if self.after.is_some() || self.before.is_some() {
            let base = object.clone();
            if let Some(Value::Object(overrides)) = object.get_mut("recurrenceOverrides") {
                overrides.retain(|rid, _| {
                    let Some(local) = jscal::parse_local(rid) else { return true };
                    let at = jscal::local_to_utc(&base, local, floating);
                    self.after.is_none_or(|after| at >= after) && self.before.is_none_or(|before| at < before)
                });
            }
        }
        let owner = object.get("participants").and_then(Value::as_object).is_some_and(|all| {
            all.values()
                .any(|p| self.is_own(p) && p.get("roles").and_then(|r| r.get("owner")) == Some(&Value::Bool(true)))
        });
        let hidden = object.get("hideAttendees") == Some(&Value::Bool(true)) && !owner;
        if self.reduce || hidden {
            let reduce = |participants: &mut Value| {
                if let Value::Object(participants) = participants {
                    participants.retain(|_, participant| self.keeps(participant));
                }
            };
            if let Some(participants) = object.get_mut("participants") {
                reduce(participants);
            }
            if let Some(Value::Object(overrides)) = object.get_mut("recurrenceOverrides") {
                for patch in overrides.values_mut().filter_map(Value::as_object_mut) {
                    if let Some(participants) = patch.get_mut("participants") {
                        reduce(participants);
                    }
                    // Single participants a patch names are left to the whole list above.
                    patch.retain(|key, value| {
                        !key.starts_with("participants/") || key.matches('/').count() != 1 || self.keeps(value)
                    });
                }
            }
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

/// What someone a calendar is shared with sees of an event its owner marked `private`
/// (RFC 8984, section 4.4.3), besides what JMAP adds.
const PRIVATE_PROPERTIES: &[&str] = &[
    "@type",
    "created",
    "duration",
    "excluded",
    "freeBusyStatus",
    "privacy",
    "recurrenceId",
    "recurrenceIdTimeZone",
    "recurrenceOverrides",
    "recurrenceRule",
    "sequence",
    "showWithoutTime",
    "start",
    "timeZone",
    "timeZones",
    "uid",
    "updated",
];

fn privacy(event: &Map<String, Value>) -> &str {
    event.get("privacy").and_then(Value::as_str).unwrap_or("public")
}

/// An event reduced to what others may see of a private one.
fn reduce_private(event: &mut Map<String, Value>) {
    event.retain(|key, _| PRIVATE_PROPERTIES.contains(&key.as_str()) || jscal::JMAP_PROPERTIES.contains(&key.as_str()));
    if let Some(Value::Object(overrides)) = event.get_mut("recurrenceOverrides") {
        for patch in overrides.values_mut().filter_map(Value::as_object_mut) {
            patch.retain(|key, _| PRIVATE_PROPERTIES.contains(&key.split('/').next().unwrap_or_default()));
        }
    }
}

type Alerts = Map<String, Value>;

/// Default alerts as kept by the store.
fn alerts_of(text: &Option<String>) -> Option<Alerts> {
    match serde_json::from_str(text.as_deref()?) {
        Ok(Value::Object(alerts)) => Some(alerts),
        _ => None,
    }
}

/// A stored event, read.
struct Loaded {
    record: CalendarEventRecord,
    parsed: Parsed,
    /// The event is in a calendar someone else shares with the account.
    shared: bool,
    /// Then the account's own per-user properties of it, and when it last changed them.
    prefs: Option<(Map<String, Value>, i64)>,
    /// And its own default alerts of the calendar, for events with and without a time.
    defaults: (Option<Alerts>, Option<Alerts>),
}

impl Loaded {
    /// The event as the account sees it: in a calendar shared with it, with its own per-user
    /// properties in place of the owner's, and only the times of an event the owner keeps private.
    fn view(&self) -> Map<String, Value> {
        let event = self.parsed.event();
        if !self.shared {
            return event.clone();
        }
        let prefs = self.prefs.as_ref().map(|(prefs, _)| prefs);
        let mut view = jscal::per_user_view(event, prefs, || jscal::recurrence_ids(&self.record.content, event));
        if let Some((_, changed)) = &self.prefs {
            // The later of the owner's and one's own change.
            let own = jscal::format_utc(*changed);
            if view.get("updated").and_then(Value::as_str).and_then(jscal::parse_utc).is_none_or(|t| t < *changed) {
                view.insert("updated".into(), json!(own));
            }
        }
        let (with_time, without_time) = (self.defaults.0.as_ref(), self.defaults.1.as_ref());
        let defaults = calendar_alerts::for_event(&view, with_time, without_time);
        calendar_alerts::materialize(&mut view, defaults);
        if privacy(event) == "private" {
            reduce_private(&mut view);
        }
        view
    }
}

impl Loaded {
    /// Another single instance of an object that holds instances without their series, as the
    /// account sees it: `None` when `rid` is none of them, `Some(None)` when it is one the owner
    /// keeps secret from others.
    fn other_instance(&self, rid: &str) -> Option<Option<Map<String, Value>>> {
        let (_, index) = self.parsed.other_instances().into_iter().find(|(other, _)| other == rid)?;
        let Some(parsed) = self.parsed.at(index) else { return Some(None) };
        let mut event = parsed.event().clone();
        if self.shared {
            if privacy(&event) == "secret" {
                return Some(None);
            }
            event = jscal::per_user_view(&event, None, BTreeSet::new);
            if privacy(&event) == "private" {
                reduce_private(&mut event);
            }
        }
        Some(Some(event))
    }
}

async fn load(ctx: &Ctx<'_>, ids: Option<Vec<i64>>) -> MethodResult<Vec<Loaded>> {
    let all = ids.is_none();
    let records = ctx.jmap.store.calendar_events(ctx.account.id, ids).await?;
    if all && records.len() > MAX_OBJECTS_IN_GET {
        return Err(MethodError::new("requestTooLarge", "too many events to fetch at once; ask for ids"));
    }
    let me = ctx.account.id;
    let shared: Vec<i64> = records.iter().filter(|record| record.owner_id != me).map(|record| record.id).collect();
    let calendar_prefs = if shared.is_empty() { Default::default() } else { ctx.jmap.store.calendar_prefs(me).await? };
    let mut prefs = ctx.jmap.store.calendar_event_prefs(me, shared).await?;
    run_blocking(move || {
        records
            .into_iter()
            .filter_map(|record| {
                let parsed = jscal::from_icalendar(&record.content)?;
                let shared = record.owner_id != me;
                // A secret event is not there for anyone but the calendar's owner.
                if shared && privacy(parsed.event()) == "secret" {
                    return None;
                }
                let prefs = prefs.remove(&record.id).and_then(|prefs| match serde_json::from_str(&prefs.data) {
                    Ok(Value::Object(data)) => Some((data, prefs.updated_at)),
                    _ => None,
                });
                let defaults = match calendar_prefs.get(&record.calendar_id) {
                    Some(own) if shared => {
                        (alerts_of(&own.default_alerts_with_time), alerts_of(&own.default_alerts_without_time))
                    }
                    _ => (None, None),
                };
                Some(Loaded { record, parsed, shared, prefs, defaults })
            })
            .collect()
    })
    .await
}

async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> MethodResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|_| MethodError::server_fail("the calendar work failed"))
}

/// An event as the account sees it, for the alert worker: its record, the event with the
/// account's own alerts (default alerts in), and the time zone its floating times are in.
pub(crate) async fn event_for_alerts(
    ctx: &Ctx<'_>,
    id: i64,
) -> MethodResult<Option<(CalendarEventRecord, Map<String, Value>, Tz)>> {
    let Some(loaded) = load(ctx, Some(vec![id])).await?.pop() else { return Ok(None) };
    let zone = calendars(ctx)
        .await?
        .into_iter()
        .find(|calendar| calendar.id == loaded.record.calendar_id)
        .and_then(|calendar| calendar.timezone)
        .and_then(|timezone| uwumail_store::ical::timezone_id(&timezone))
        .and_then(|name| jscal::time_zone(&name))
        .unwrap_or(chrono_tz::UTC);
    let view = loaded.view();
    Ok(Some((loaded.record, view, zone)))
}

// ------------------------------------------------------------------------------------------------
// CalendarEvent/get

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let deadline = request_deadline(ctx);
    if Instant::now() > deadline {
        return Err(out_of_time());
    }
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
    let own = own_addresses(ctx).await?;
    let shaping = Shaping::from(args, own.clone())?;
    let (list, not_found) = run_blocking(move || -> MethodResult<(Vec<Value>, Vec<String>)> {
        let by_id: HashMap<i64, &Loaded> = loaded.iter().map(|l| (l.record.id, l)).collect();
        let stored = |loaded: &Loaded| {
            let mut object = loaded.view();
            shaping.apply(&mut object, floating);
            decorate(&mut object, ids::calendar_event(loaded.record.id), &loaded.record, None, &own);
            output(object, &properties, floating)
        };
        let Some(wanted) = wanted else {
            return Ok((loaded.iter().map(stored).collect::<Vec<_>>(), Vec::new()));
        };
        let mut series: HashMap<i64, BTreeSet<String>> = HashMap::new();
        let mut list = Vec::new();
        let mut not_found = Vec::new();
        for (text, id) in wanted {
            // Instances each expand their series; the request's time bounds them all.
            if Instant::now() > deadline {
                return Err(out_of_time());
            }
            let found = match &id {
                Some(EventId::Stored(n)) => by_id.get(n).map(|loaded| stored(loaded)),
                Some(EventId::Instance(n, rid)) => by_id.get(n).and_then(|loaded| {
                    // Another single instance of an object without its series.
                    match loaded.other_instance(rid) {
                        Some(Some(mut object)) => {
                            shaping.apply(&mut object, floating);
                            decorate(&mut object, text.clone(), &loaded.record, Some(*n), &own);
                            return Some(output(object, &properties, floating));
                        }
                        Some(None) => return None,
                        None => {}
                    }
                    let event = loaded.parsed.event();
                    let rids = series.entry(*n).or_insert_with(|| jscal::recurrence_ids(&loaded.record.content, event));
                    if !rids.contains(rid) {
                        return None;
                    }
                    let mut object = jscal::instance(&loaded.view(), rid)?;
                    shaping.apply(&mut object, floating);
                    decorate(&mut object, text.clone(), &loaded.record, Some(*n), &own);
                    Some(output(object, &properties, floating))
                }),
                None => None,
            };
            match found {
                Some(object) => list.push(object),
                None => not_found.push(text),
            }
        }
        Ok((list, not_found))
    })
    .await??;
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
        let start = jscal::utc_to_local(event, parse(&value, "utcStart")?, zone)
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

/// Checks an event and turns it into iCalendar the way a CalDAV PUT would take it.
fn convert(
    parsed: &Parsed,
    event: &Map<String, Value>,
    components: &[String],
) -> Result<(String, uwumail_store::ical::Checked), SetError> {
    jscal::validate(event).map_err(invalid)?;
    let content = parsed.to_icalendar(event).map_err(|message| SetError::new("invalidProperties", message))?;
    if content.len() > DAV_RESOURCE_MAX_BYTES {
        return Err(SetError::new("tooLarge", format!("an event may take up to {DAV_RESOURCE_MAX_BYTES} bytes")));
    }
    // The same check a CalDAV PUT goes through: what JMAP writes, phones can read.
    let checked = match uwumail_store::ical::check_calendar(&content, components) {
        Ok(checked) if checked.component == "VEVENT" => checked,
        Ok(_) | Err(uwumail_store::ical::Refused::UnsupportedComponent(_)) => {
            return Err(SetError::invalid_properties(&["calendarIds"], "this calendar does not hold events"));
        }
        Err(_) => return Err(SetError::new("invalidProperties", "the event cannot be stored as iCalendar")),
    };
    if jscal::from_icalendar(&content).is_none() {
        return Err(SetError::new("invalidProperties", "the event cannot be stored as iCalendar"));
    }
    Ok((content, checked))
}

/// Everything a write needs from around it.
struct Writer<'a> {
    calendars: Vec<DavCollection>,
    own: Vec<String>,
    scheduling: bool,
    /// When the request's time for calendar work is up; later objects are refused.
    deadline: Instant,
    ctx: &'a Ctx<'a>,
}

impl Writer<'_> {
    fn in_time(&self) -> Result<(), SetError> {
        if Instant::now() > self.deadline {
            return Err(SetError::new(
                "rateLimit",
                "this request has used up its time for calendar events; send the rest in a new request",
            ));
        }
        Ok(())
    }

    fn calendar(&self, id: i64) -> Result<&DavCollection, SetError> {
        self.calendars.iter().find(|c| c.id == id).ok_or_else(|| {
            SetError::invalid_properties(&["calendarIds"], "an event belongs to exactly one of your calendars")
        })
    }

    /// Tells attendees or the organizer about a change, when the client asked for it
    /// (`sendSchedulingMessages`) and the event is in one of the account's own calendars.
    async fn schedule(&self, calendar_id: i64, old: Option<&str>, new: Option<&str>) {
        if self.scheduling && owns(self.ctx, calendar_id).await {
            self.ctx.jmap.smtp.schedule_change(&self.ctx.account, old, new).await;
        }
    }

    /// Checks an event, turns it into iCalendar and stores it, making it a draft or not with
    /// `draft`; `draft_after` says whether it is one afterwards. Returns its id and what was stored.
    #[allow(clippy::too_many_arguments)]
    async fn store(
        &self,
        parsed: &Parsed,
        event: &Map<String, Value>,
        id: Option<i64>,
        calendar_id: i64,
        if_etag: Option<String>,
        old: Option<&str>,
        (draft, draft_after): (Option<bool>, bool),
    ) -> Result<Result<(i64, String), SetError>, ()> {
        let calendar = match self.calendar(calendar_id) {
            Ok(calendar) => calendar,
            Err(err) => return Ok(Err(err)),
        };
        // An event that uses the default alerts carries them, for CalDAV clients to ring.
        let mut event = event.clone();
        if event.get("useDefaultAlerts") == Some(&Value::Bool(true)) {
            let prefs = self.ctx.jmap.store.calendar_prefs(calendar.account_id).await;
            let prefs = prefs.ok().and_then(|mut prefs| prefs.remove(&calendar_id)).unwrap_or_default();
            let (with_time, without_time) =
                (alerts_of(&prefs.default_alerts_with_time), alerts_of(&prefs.default_alerts_without_time));
            let defaults = calendar_alerts::for_event(&event, with_time.as_ref(), without_time.as_ref()).cloned();
            calendar_alerts::materialize(&mut event, defaults.as_ref());
        }
        // Checking and converting is work in proportion to the event: off the async threads.
        let (parsed, components) = (parsed.clone(), calendar.components.clone());
        let converted = run_blocking(move || convert(&parsed, &event, &components)).await;
        let (content, checked) = match converted {
            Ok(Ok(converted)) => converted,
            Ok(Err(err)) => return Ok(Err(err)),
            Err(_) => return Ok(Err(SetError::new("serverFail", "the event could not be converted"))),
        };
        // One change may not send more scheduling messages than one mail may have recipients
        // (security-audit-0.16.0 PROTOCOLS-5); refused before anything is stored.
        if self.scheduling
            && !draft_after
            && owns(self.ctx, calendar_id).await
            && let Err(refused) = self.ctx.jmap.smtp.check_schedule(&self.ctx.account, old, Some(&content)).await
        {
            return Ok(Err(SetError::invalid_properties(&["participants"], refused.to_string())));
        }
        let uid = checked.uid.clone();
        let write = CalendarEventWrite {
            id,
            calendar_id,
            content: content.clone(),
            uid: checked.uid,
            starts_at: checked.starts_at,
            ends_at: checked.ends_at,
            if_etag,
            keep_schedule_tag: false,
            draft,
            author: uwumail_store::Author::Account,
        };
        match self.ctx.jmap.store.put_calendar_event(self.ctx.account.id, write).await {
            Ok((id, _)) => Ok(Ok((id, content))),
            // Changed over CalDAV meanwhile: read it again.
            Err(StoreError::Conflict(_)) => Err(()),
            Err(StoreError::QuotaExceeded) => Ok(Err(SetError::new("overQuota", "the calendar is full"))),
            Err(StoreError::NotFound(_)) if id.is_some() => Ok(Err(SetError::not_found())),
            Err(StoreError::NotFound(_)) => Ok(Err(SetError::invalid_properties(&["calendarIds"], "no such calendar"))),
            // The event in the way, among those of the same calendar owner (RFC 8620, 5.3).
            Err(StoreError::Rule { code: "alreadyExists", message }) => {
                let events = self.ctx.jmap.store.calendar_events_between(self.ctx.account.id, None, None, None).await;
                let existing = events.ok().and_then(|events| {
                    events.into_iter().find(|e| e.uid == uid && e.owner_id == calendar.account_id && Some(e.id) != id)
                });
                Ok(Err(SetError {
                    existing_id: existing.map(|e| ids::calendar_event(e.id)),
                    ..SetError::new("alreadyExists", message)
                }))
            }
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
        let draft = match event.remove("isDraft") {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(SetError::invalid_properties(&["isDraft"], "must be true or false")),
        };
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
        // Inviting people makes the account the organizer, unless the client named one.
        if self.scheduling
            && event.get("participants").and_then(Value::as_object).is_some_and(|p| !p.is_empty())
            && !event.contains_key("organizerCalendarAddress")
        {
            let organizer = json!(format!("mailto:{}", self.ctx.account.login.to_lowercase()));
            event.insert("organizerCalendarAddress".into(), organizer.clone());
            server_set.insert("organizerCalendarAddress".into(), organizer);
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
        // In someone else's calendar, what is per user stays the account's own.
        let shared = self.calendar(calendar_id)?.account_id != self.ctx.account.id;
        let (event, prefs) = if shared { jscal::split_per_user(&event, &Map::new()) } else { (event, Map::new()) };
        let parsed = Parsed::new_event();
        let (id, content) = match self.store(&parsed, &event, None, calendar_id, None, None, (Some(draft), draft)).await
        {
            Ok(result) => result?,
            Err(()) => return Err(SetError::new("serverFail", "the event could not be stored")),
        };
        if !prefs.is_empty() {
            let data = Some(Value::Object(prefs).to_string());
            if let Err(err) = self.ctx.jmap.store.set_calendar_event_prefs(self.ctx.account.id, id, data).await {
                tracing::warn!(%err, "one's own properties of a new event could not be kept");
            }
        }
        // Nobody hears about a draft.
        if !draft {
            self.schedule(calendar_id, None, Some(&content)).await;
        }
        server_set.insert("id".into(), json!(ids::calendar_event(id)));
        server_set.insert("isDraft".into(), json!(draft));
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
            let mut event = loaded.view();
            let mut calendars: BTreeSet<i64> = BTreeSet::from([loaded.record.calendar_id]);
            let (mut utc_start, mut utc_end) = (None, None);
            let mut new_version = false;
            let mut publish = false;
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
                    // A draft may become an event, never the other way round.
                    "isDraft" if tokens.len() == 1 && value == &Value::Bool(loaded.record.is_draft) => {}
                    "isDraft" if tokens.len() == 1 && value == &Value::Bool(false) => publish = true,
                    "isDraft" if tokens.len() == 1 && value == &Value::Bool(true) => {
                        return Err(SetError::invalid_properties(&["isDraft"], "only a new event can be a draft"));
                    }
                    "uid" | "@type" | "recurrenceId" | "recurrenceIdTimeZone"
                        if tokens.len() == 1 && event.get(top).unwrap_or(&Value::Null) == value => {}
                    "recurrenceId" | "recurrenceIdTimeZone" => {
                        return Err(SetError::invalid_properties(&[top], "cannot be changed"));
                    }
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
            let asked = patch.get("sequence").and_then(Value::as_u64);
            if let Some(set) = self.commit(&loaded, event, calendar_id, new_version, asked, publish).await? {
                server_set.extend(set);
                return Ok(Value::Object(server_set));
            }
        }
        Err(SetError::new("serverFail", "the event keeps changing; try again"))
    }

    /// Stores what a change made of an event the account sees as `event`: the owner's part as
    /// iCalendar, with a new `sequence` for a `new_version` and `updated` where the account is the
    /// origin; in someone else's calendar the account's own per-user properties apart. Returns
    /// what the server set, or `None` when CalDAV changed the event meanwhile.
    async fn commit(
        &self,
        loaded: &Loaded,
        event: Map<String, Value>,
        calendar_id: i64,
        new_version: bool,
        asked_sequence: Option<u64>,
        publish: bool,
    ) -> Result<Option<Map<String, Value>>, SetError> {
        let owner = loaded.parsed.event();
        let (mut stored, prefs) = if loaded.shared {
            // Not even one's own properties of an event the owner keeps private.
            if privacy(owner) == "private" {
                return Err(SetError::new("forbidden", "the owner keeps this event private"));
            }
            let (mut stored, prefs) = jscal::split_per_user(&event, owner);
            match owner.get("updated") {
                Some(updated) => stored.insert("updated".into(), updated.clone()),
                None => stored.remove("updated"),
            };
            (stored, Some(prefs))
        } else {
            (event, None)
        };
        let mut server_set = Map::new();
        let owner_changed = !loaded.shared || publish || stored != *owner || calendar_id != loaded.record.calendar_id;
        let draft_after = loaded.record.is_draft && !publish;
        if owner_changed {
            let current = owner.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            if new_version && asked_sequence.is_none_or(|asked| asked <= current) {
                stored.insert("sequence".into(), json!(current + 1));
                server_set.insert("sequence".into(), json!(current + 1));
            }
            if is_origin(&stored, &self.own) {
                let now = json!(jscal::format_utc(jscal::now()));
                stored.insert("updated".into(), now.clone());
                server_set.insert("updated".into(), now);
            }
            // A draft that becomes an event is new to everyone in it.
            let etag = Some(loaded.record.etag.clone());
            let old = (!publish).then_some(loaded.record.content.as_str());
            let draft = (publish.then_some(false), draft_after);
            match self.store(&loaded.parsed, &stored, Some(loaded.record.id), calendar_id, etag, old, draft).await {
                Ok(result) => {
                    let (_, content) = result?;
                    if !draft_after {
                        self.schedule(calendar_id, old, Some(&content)).await;
                    }
                }
                Err(()) => return Ok(None),
            }
        }
        if publish {
            server_set.insert("isDraft".into(), json!(false));
        }
        if let Some(prefs) = prefs {
            let current = loaded.prefs.as_ref().map(|(prefs, _)| prefs.clone()).unwrap_or_default();
            if prefs != current {
                let data = (!prefs.is_empty()).then(|| Value::Object(prefs).to_string());
                self.ctx
                    .jmap
                    .store
                    .set_calendar_event_prefs(self.ctx.account.id, loaded.record.id, data)
                    .await
                    .map_err(|err| match err {
                        StoreError::QuotaExceeded => {
                            SetError::new("tooLarge", "your own properties of this event are too large")
                        }
                        other => SetError::from(other),
                    })?;
                if !owner_changed {
                    server_set.insert("updated".into(), json!(jscal::format_utc(jscal::now())));
                }
            }
        }
        Ok(Some(server_set))
    }

    /// Changes one instance of a series: the change becomes an override of the series. Another
    /// single instance of an object without its series is changed as it is.
    async fn update_instance(&self, id: i64, rid: &str, patch: &Map<String, Value>) -> Result<Value, SetError> {
        if let Some(result) = self.update_other_instance(id, rid, patch).await {
            return result;
        }
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
            let new_version = patch.keys().any(|path| !PER_USER.contains(&path.split('/').next().unwrap_or_default()));
            Ok((Value::Object(own), new_version))
        })
        .await
        .map(|()| Value::Null)
    }

    /// Applies a patch to another single instance of an object that holds instances without their
    /// series; `None` when `rid` is none of them.
    async fn update_other_instance(
        &self,
        id: i64,
        rid: &str,
        patch: &Map<String, Value>,
    ) -> Option<Result<Value, SetError>> {
        for _ in 0..WRITE_ATTEMPTS {
            let loaded = match self.load_one(id).await {
                Ok(loaded) => loaded,
                Err(err) => return Some(Err(err)),
            };
            let (_, index) = loaded.parsed.other_instances().into_iter().find(|(other, _)| other == rid)?;
            let result = async {
                let parsed = loaded.parsed.at(index).ok_or_else(SetError::not_found)?;
                let mut event = parsed.event().clone();
                let mut new_version = false;
                for (path, value) in patch {
                    let top = path.split('/').next().unwrap_or_default();
                    match top {
                        "isDraft" if value == &Value::Bool(loaded.record.is_draft) || value.is_null() => {}
                        "id"
                        | "baseEventId"
                        | "isOrigin"
                        | "isDraft"
                        | "calendarIds"
                        | "uid"
                        | "@type"
                        | "recurrenceId"
                        | "recurrenceIdTimeZone"
                        | "utcStart"
                        | "utcEnd" => {
                            return Err(SetError::invalid_properties(&[top], "cannot be changed for one instance"));
                        }
                        _ => {
                            jscal::apply_patch(&mut event, path, value.clone())
                                .map_err(|message| SetError::new("invalidPatch", message))?;
                            new_version |= !PER_USER.contains(&top);
                        }
                    }
                }
                match privacy(parsed.event()) {
                    "secret" if loaded.shared => return Err(SetError::not_found()),
                    "private" if loaded.shared => {
                        return Err(SetError::new("forbidden", "the owner keeps this event private"));
                    }
                    _ => {}
                }
                // Written as it is: one's own properties are kept apart for the main instance only.
                let instance = Loaded { parsed, shared: false, prefs: None, defaults: (None, None), ..loaded };
                let calendar_id = instance.record.calendar_id;
                self.commit(
                    &instance,
                    event,
                    calendar_id,
                    new_version,
                    patch.get("sequence").and_then(Value::as_u64),
                    false,
                )
                .await
            }
            .await;
            match result {
                Ok(Some(set)) => return Some(Ok(if set.is_empty() { Value::Null } else { Value::Object(set) })),
                Ok(None) => continue,
                Err(err) => return Some(Err(err)),
            }
        }
        Some(Err(SetError::new("serverFail", "the event keeps changing; try again")))
    }

    /// Takes another single instance out of an object without its series; the object goes with
    /// its last one. `None` when `rid` is none of them.
    async fn destroy_other_instance(&self, id: i64, rid: &str) -> Option<Result<(), SetError>> {
        for _ in 0..WRITE_ATTEMPTS {
            let loaded = match self.load_one(id).await {
                Ok(loaded) => loaded,
                Err(err) => return Some(Err(err)),
            };
            let (_, index) = loaded.parsed.other_instances().into_iter().find(|(other, _)| other == rid)?;
            // What the owner keeps private or secret is theirs to delete.
            if loaded.shared
                && let Some(parsed) = loaded.parsed.at(index)
                && privacy(parsed.event()) != "public"
            {
                return Some(Err(match privacy(parsed.event()) {
                    "secret" => SetError::not_found(),
                    _ => SetError::new("forbidden", "the owner keeps this event private"),
                }));
            }
            let Some(rest) = loaded.parsed.at(index).and_then(|parsed| parsed.without_event()) else {
                return Some(Err(SetError::not_found()));
            };
            let event = rest.event().clone();
            let instance = Loaded { parsed: rest, shared: false, prefs: None, defaults: (None, None), ..loaded };
            let calendar_id = instance.record.calendar_id;
            match self.commit(&instance, event, calendar_id, true, None, false).await {
                Ok(Some(_)) => return Some(Ok(())),
                Ok(None) => continue,
                Err(err) => return Some(Err(err)),
            }
        }
        Some(Err(SetError::new("serverFail", "the event keeps changing; try again")))
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
            let base = loaded.view();
            let content = loaded.record.content.clone();
            let series = loaded.parsed.event().clone();
            let rids = run_blocking(move || jscal::recurrence_ids(&content, &series))
                .await
                .map_err(|_| SetError::new("serverFail", "expansion failed"))?;
            if !rids.contains(rid) {
                return Err(SetError::not_found());
            }
            let instance = jscal::instance(&base, rid).ok_or_else(SetError::not_found)?;
            let (patch, new_version) = change(&base, &instance)?;
            let mut event = base.clone();
            let overrides = event.entry("recurrenceOverrides").or_insert_with(|| json!({}));
            if !overrides.is_object() {
                *overrides = json!({});
            }
            overrides[rid] = patch;
            let calendar_id = loaded.record.calendar_id;
            if self.commit(&loaded, event, calendar_id, new_version, None, false).await?.is_some() {
                return Ok(());
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
    let deadline = request_deadline(ctx);
    let mut response = SetResponse::default();
    let mut created_ids: Vec<(String, String)> = Vec::new();

    {
        let writer = Writer { calendars, own, scheduling, deadline, ctx };
        if let Some(create) = args.get("create").and_then(Value::as_object) {
            for (creation_id, object) in create {
                let created = match writer.in_time() {
                    Ok(()) => writer.create(object).await,
                    Err(err) => Err(err),
                };
                match created {
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
    let writer = Writer { calendars, own, scheduling, deadline, ctx };

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result = async {
                writer.in_time()?;
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                match EventId::parse(ctx, id) {
                    Some(EventId::Stored(n)) => writer.update_stored(n, patch).await,
                    Some(EventId::Instance(n, rid)) => writer.update_instance(n, &rid, patch).await,
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
            let parsed = writer.in_time().map(|()| EventId::parse(ctx, id));
            let result = match parsed {
                Err(err) => Err(err),
                Ok(Some(EventId::Stored(n))) => match writer.load_one(n).await {
                    Err(err) => Err(err),
                    // What the owner keeps private is theirs to delete, as it is theirs to change; a
                    // secret one is not there for others at all (load_one does not find it).
                    Ok(old) if old.shared && privacy(old.parsed.event()) != "public" => {
                        Err(SetError::new("forbidden", "the owner keeps this event private"))
                    }
                    Ok(old) => {
                        let destroyed = ctx
                            .jmap
                            .store
                            .destroy_calendar_event(ctx.account.id, n, None)
                            .await
                            .map_err(SetError::from);
                        if destroyed.is_ok() && scheduling && !old.record.is_draft {
                            writer.schedule(old.record.calendar_id, Some(&old.record.content), None).await;
                        }
                        destroyed
                    }
                },
                Ok(Some(EventId::Instance(n, rid))) => match writer.destroy_other_instance(n, &rid).await {
                    Some(result) => result,
                    None => writer.destroy_instance(n, &rid).await,
                },
                Ok(None) => Err(SetError::not_found()),
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
                let mut hit = false;
                for rid in rids.iter() {
                    self.check_time()?;
                    if instance_span(event, rid, self.floating)
                        .is_some_and(|(s, e)| jscal::overlaps(s, e, condition.after, condition.before))
                    {
                        hit = true;
                        break;
                    }
                }
                hit
            } else {
                jscal::span(event, self.floating)
                    .is_some_and(|(s, e)| jscal::overlaps(s, e, condition.after, condition.before))
            };
            if !hit {
                return Ok(false);
            }
        }
        // What others may not read of a private event is not searched for them either.
        let reduced;
        let event = if loaded.shared && privacy(event) == "private" {
            let mut copy = event.clone();
            reduce_private(&mut copy);
            reduced = copy;
            &reduced
        } else {
            event
        };
        if text_matches(condition, event) {
            return Ok(true);
        }
        // Changed instances have texts of their own.
        if let Some(overrides) = event.get("recurrenceOverrides").and_then(Value::as_object) {
            for rid in overrides.keys() {
                self.check_time()?;
                if jscal::instance(event, rid).is_some_and(|instance| text_matches(condition, &instance)) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn hit(&self, id: String, object: &Map<String, Value>, recurrence_id: Option<&str>, start: i64) -> Hit {
        let text = |name: &str| object.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
        Hit {
            id,
            start,
            uid: text("uid"),
            recurrence_id: recurrence_id.map_or_else(|| text("recurrenceId"), str::to_owned),
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
            let mut hits = self.stored_hits(&Some(Filter::Condition(condition.clone())), loaded)?;
            // The other single instances of an object without its series.
            for (rid, index) in loaded.parsed.other_instances() {
                let Some(parsed) = loaded.parsed.at(index) else { continue };
                // A secret one is not there for others.
                if loaded.shared && privacy(parsed.event()) == "secret" {
                    continue;
                }
                let other = Loaded {
                    parsed,
                    shared: loaded.shared,
                    prefs: None,
                    defaults: (None, None),
                    record: loaded.record.clone(),
                };
                for hit in self.stored_hits(&Some(Filter::Condition(condition.clone())), &other)? {
                    let id = ids::event_instance(loaded.record.id, &rid);
                    hits.push(Hit { id, recurrence_id: rid.clone(), ..hit });
                }
            }
            return Ok(hits);
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
            self.check_time()?;
            let Some((start, end)) = instance_span(event, &rid, self.floating) else { continue };
            if !jscal::overlaps(start, end, condition.after, condition.before) {
                continue;
            }
            let Some(mut instance) = jscal::instance(event, &rid) else { continue };
            if loaded.shared && privacy(event) == "private" {
                reduce_private(&mut instance);
            }
            if text_matches(condition, &instance) {
                hits.push(self.hit(ids::event_instance(loaded.record.id, &rid), &instance, Some(&rid), start));
            }
        }
        Ok(hits)
    }
}

/// The instance ids a query that expands recurrences (`args`) finds in events as they were
/// (`old`, by event id), for /queryChanges. Their calendar is not looked at: what the client
/// never had it ignores.
///
/// Only the query's time window counts, never its text conditions: earlier versions are kept for
/// everyone who sees a calendar — private events of others, and calendars no longer shared with
/// the account, included — so matching their text would tell what they said. Naming more removed
/// ids than the client had is allowed (RFC 8620, section 5.6). A secret event counts only for its
/// calendar's owner; when that cannot be told any more, the changes cannot be calculated.
pub(super) async fn expanded_ids_of(ctx: &Ctx<'_>, args: &Value, old: Vec<(i64, String)>) -> MethodResult<Vec<String>> {
    if old.is_empty() {
        return Ok(Vec::new());
    }
    let floating = floating_zone(args)?;
    let Some(Filter::Condition(condition)) =
        args.get("filter").filter(|f| !f.is_null()).map(|f| parse_filter(ctx, f, floating)).transpose()?
    else {
        return Err(MethodError::invalid_arguments("expandRecurrences needs a filter condition with after and before"));
    };
    let condition = Condition { after: condition.after, before: condition.before, ..Default::default() };
    let deadline = (Instant::now() + QUERY_TIME_LIMIT).min(request_deadline(ctx));
    let evaluator = Evaluator { floating, deadline };
    let me = ctx.account.id;
    // The instance ids found, and apart those of secret events by event id.
    type Found = (Vec<String>, Vec<(i64, Vec<String>)>);
    let (mut ids, secret) = run_blocking(move || -> MethodResult<Found> {
        let (mut ids, mut secret) = (Vec::new(), Vec::new());
        for (id, content) in old {
            evaluator.check_time()?;
            let Some(parsed) = jscal::from_icalendar(&content) else { continue };
            let is_secret = privacy(parsed.event()) == "secret";
            let record = CalendarEventRecord {
                id,
                calendar_id: 0,
                owner_id: me,
                name: String::new(),
                uid: String::new(),
                etag: String::new(),
                content,
                starts_at: None,
                ends_at: None,
                modified_at: 0,
                is_draft: false,
            };
            let loaded = Loaded { record, parsed, shared: false, prefs: None, defaults: (None, None) };
            let hits = evaluator.expanded_hits(&condition, &loaded)?.into_iter().map(|hit| hit.id);
            if is_secret {
                secret.push((id, hits.collect()));
            } else {
                ids.extend(hits);
            }
        }
        Ok((ids, secret))
    })
    .await??;
    for (id, hits) in secret {
        match ctx.jmap.store.calendar_events(me, Some(vec![id])).await?.pop() {
            Some(record) if record.owner_id == me => ids.extend(hits),
            // Someone else's secret event: the account never had its instances.
            Some(_) => {}
            None => return Err(MethodError::kind("cannotCalculateChanges")),
        }
    }
    Ok(ids)
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let floating = floating_zone(args)?;
    let expand = args.get("expandRecurrences").and_then(Value::as_bool).unwrap_or(false);
    check_filter_size(args.get("filter"))?;
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
        let list = list.as_array().ok_or_else(|| MethodError::invalid_arguments("sort must be a list"))?;
        if list.len() > SORTS.len() {
            return Err(MethodError::new("unsupportedSort", format!("sort by at most {} properties", SORTS.len())));
        }
        for comparator in list {
            let property = comparator.get("property").and_then(Value::as_str).unwrap_or_default();
            if !SORTS.contains(&property) {
                return Err(MethodError::new("unsupportedSort", format!("cannot sort by {property}")));
            }
            sort.push((property.to_owned(), comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true)));
        }
    }
    let records = ctx.jmap.store.calendar_events_between(ctx.account.id, window.0, window.1, window.2).await?;
    // A query's own limit, within what is left of the request's.
    let deadline = (Instant::now() + QUERY_TIME_LIMIT).min(request_deadline(ctx));
    let evaluator = Evaluator { floating, deadline };
    let me = ctx.account.id;
    let mut hits = run_blocking(move || -> MethodResult<Vec<Hit>> {
        let mut hits = Vec::new();
        for record in records {
            evaluator.check_time()?;
            let Some(parsed) = jscal::from_icalendar(&record.content) else { continue };
            let shared = record.owner_id != me;
            // A secret event is not there for anyone but the calendar's owner.
            if shared && privacy(parsed.event()) == "secret" {
                continue;
            }
            let loaded = Loaded { record, parsed, shared, prefs: None, defaults: (None, None) };
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
    let ids = hits.into_iter().map(|hit| hit.id).collect();
    query_response(ctx, args, state, ids, MAX_QUERY_LIMIT)
}

// ------------------------------------------------------------------------------------------------
// CalendarEvent/parse

/// The largest iCalendar file one blob may be, and the events read from it.
const MAX_PARSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PARSED_EVENTS: usize = 1000;

/// Turns blobs (uploads, attachments of mail) of iCalendar into CalendarEvents, without storing
/// anything.
pub async fn parse(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let blob_ids: Vec<String> = args
        .get("blobIds")
        .and_then(Value::as_array)
        .ok_or_else(|| MethodError::invalid_arguments("blobIds is required"))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    if blob_ids.len() > MAX_OBJECTS_IN_GET {
        return Err(MethodError::kind("requestTooLarge"));
    }
    let properties = match args.get("properties") {
        None | Some(Value::Null) => None,
        Some(_) => Some(super::properties(args, "properties", &[])?),
    };
    let deadline = request_deadline(ctx);
    let (mut parsed, mut not_parsable, mut not_found) = (Map::new(), Vec::new(), Vec::new());
    for blob_id in blob_ids {
        if Instant::now() > deadline {
            return Err(out_of_time());
        }
        let Some(raw) = super::email::read_blob(ctx, &blob_id).await else {
            not_found.push(blob_id);
            continue;
        };
        let events = run_blocking(move || {
            let text = String::from_utf8(raw).ok().filter(|text| text.len() <= MAX_PARSE_BYTES)?;
            jscal::events_of(&text, MAX_PARSED_EVENTS).filter(|events| !events.is_empty())
        })
        .await?;
        let Some(events) = events else {
            not_parsable.push(blob_id);
            continue;
        };
        let list: Vec<Value> = events
            .into_iter()
            .map(|mut event| {
                event.remove("iCalendar");
                // What only a stored event has.
                for property in ["id", "baseEventId", "calendarIds", "isDraft", "isOrigin"] {
                    event.insert(property.into(), Value::Null);
                }
                match &properties {
                    Some(list) => pick(event, list),
                    None => Value::Object(event),
                }
            })
            .collect();
        parsed.insert(blob_id, Value::Array(list));
    }
    let or_null = |list: Vec<String>| if list.is_empty() { Value::Null } else { json!(list) };
    Ok(json!({
        "accountId": ctx.account_id(),
        "parsed": if parsed.is_empty() { Value::Null } else { Value::Object(parsed) },
        "notParsable": or_null(not_parsable),
        "notFound": or_null(not_found),
    }))
}

// ------------------------------------------------------------------------------------------------
// CalendarEvent/copy

/// What only the stored event has, or only its place in the account.
const NOT_COPIED: &[&str] = &["id", "baseEventId", "isOrigin", "utcStart", "utcEnd", "iCalendar"];

/// Copies events into a calendar as new ones (RFC 8620, section 5.4). Every calendar the login
/// sees, its own and those shared with it, is in its own account here, so events are copied
/// within it: `fromAccountId` is the account itself. The copy keeps the uid unless the create
/// gives another, and one that the calendar's owner already has is `alreadyExists`.
pub async fn copy(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<super::Outputs> {
    check_enabled(ctx)?;
    let from = args
        .get("fromAccountId")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("fromAccountId is required"))?;
    if from != ctx.account_id() {
        return Err(MethodError::kind("fromAccountNotFound"));
    }
    let create = match args.get("create") {
        Some(Value::Object(create)) => create.clone(),
        _ => return Err(MethodError::invalid_arguments("create must be an object")),
    };
    check_set_size(args)?;
    let old_state = ctx.state().await?;
    if let Some(expected) = args.get("ifFromInState").and_then(Value::as_str)
        && expected != old_state
    {
        return Err(MethodError::kind("stateMismatch"));
    }
    if_in_state(args, &old_state)?;
    let calendars = calendars(ctx).await?;
    let own = own_addresses(ctx).await?;
    let deadline = request_deadline(ctx);
    let (mut created, mut not_created, mut copied) = (Map::new(), Map::new(), Vec::new());
    let mut created_ids = Vec::new();
    {
        let writer = Writer { calendars, own, scheduling: false, deadline, ctx };
        for (creation_id, object) in &create {
            let result = async {
                writer.in_time()?;
                let object =
                    object.as_object().ok_or_else(|| SetError::new("invalidProperties", "must be an object"))?;
                let source = object
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SetError::invalid_properties(&["id"], "the event to copy"))?;
                let parsed = EventId::parse(ctx, source).ok_or_else(SetError::not_found)?;
                let loaded = writer.load_one(parsed.base()).await?;
                // Others do not get the whole of what the owner keeps private.
                if loaded.shared && privacy(loaded.parsed.event()) == "private" {
                    return Err(SetError::new("forbidden", "the owner keeps this event private"));
                }
                let mut event = match &parsed {
                    EventId::Stored(_) => loaded.view(),
                    EventId::Instance(_, rid) => {
                        // An instance becomes an event of its own.
                        let mut instance = match loaded.other_instance(rid) {
                            Some(Some(instance)) => instance,
                            Some(None) => return Err(SetError::not_found()),
                            None => {
                                let (content, series) = (loaded.record.content.clone(), loaded.parsed.event().clone());
                                let rids = run_blocking(move || jscal::recurrence_ids(&content, &series))
                                    .await
                                    .map_err(|_| SetError::new("serverFail", "expansion failed"))?;
                                if !rids.contains(rid) {
                                    return Err(SetError::not_found());
                                }
                                jscal::instance(&loaded.view(), rid).ok_or_else(SetError::not_found)?
                            }
                        };
                        for property in
                            ["recurrenceId", "recurrenceIdTimeZone", "recurrenceRule", "recurrenceOverrides"]
                        {
                            instance.remove(property);
                        }
                        instance
                    }
                };
                for property in NOT_COPIED {
                    event.remove(*property);
                }
                event.insert("calendarIds".into(), json!({ ids::calendar(loaded.record.calendar_id): true }));
                event.insert("isDraft".into(), json!(loaded.record.is_draft));
                for (key, value) in object.iter().filter(|(key, _)| key.as_str() != "id") {
                    event.insert(key.clone(), value.clone());
                }
                let (id, mut server_set) = writer.create(&Value::Object(event)).await?;
                server_set.insert("id".into(), json!(ids::calendar_event(id)));
                Ok((id, server_set, source.to_owned()))
            }
            .await;
            match result {
                Ok((id, server_set, source)) => {
                    created_ids.push((creation_id.clone(), ids::calendar_event(id)));
                    created.insert(creation_id.clone(), Value::Object(server_set));
                    copied.push(source);
                }
                Err(err) => {
                    not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }
    ctx.created_ids.extend(created_ids);
    let new_state = ctx.state().await?;
    let or_null = |map: Map<String, Value>| if map.is_empty() { Value::Null } else { Value::Object(map) };
    let mut outputs = vec![(
        "CalendarEvent/copy".to_owned(),
        json!({
            "fromAccountId": from,
            "accountId": ctx.account_id(),
            "oldState": old_state,
            "newState": new_state,
            "created": or_null(created),
            "notCreated": or_null(not_created),
        }),
    )];
    if args.get("onSuccessDestroyOriginal").and_then(Value::as_bool).unwrap_or(false) && !copied.is_empty() {
        let mut destroy = json!({ "accountId": from, "destroy": copied });
        if let Some(expected) = args.get("destroyFromIfInState").filter(|value| !value.is_null()) {
            destroy["ifInState"] = expected.clone();
        }
        match set(ctx, &destroy).await {
            Ok(response) => outputs.push(("CalendarEvent/set".to_owned(), response)),
            Err(err) => outputs.push(("error".to_owned(), err.to_json())),
        }
    }
    Ok(outputs)
}
