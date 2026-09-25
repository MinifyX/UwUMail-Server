//! iTIP (RFC 5546): the scheduling messages organizers and attendees send each other, read from and
//! written into iCalendar.
//!
//! Scheduling only looks at a few properties and has to keep everything else exactly as the client
//! wrote it, so objects are handled here as a plain tree of components and content lines rather
//! than through a full iCalendar model. What comes out is checked by the same check a CalDAV PUT
//! goes through before it is stored anywhere.
//!
//! Everything here is pure: deciding whom to tell what after a change ([`plan`]), writing the
//! messages ([`request`], [`cancel`], [`reply`]) and applying them to someone's copy
//! ([`attendee_copy`], [`apply_reply`], [`apply_cancel`]). Delivering them is the mail server's job.

/// Longest content line before it is folded, in octets (RFC 5545, 3.1).
const FOLD_AT: usize = 75;
/// Components and lines one object may have before it is not read at all.
const MAX_LINES: usize = 100_000;
const MAX_DEPTH: usize = 16;

/// One content line: `NAME;PARAM=value:VALUE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// Upper case.
    pub name: String,
    /// Names upper case; values as written, without surrounding quotes.
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Property {
    pub fn new(name: &str, value: impl Into<String>) -> Property {
        Property { name: name.to_ascii_uppercase(), params: Vec::new(), value: value.into() }
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    pub fn set_param(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.params.iter_mut().find(|(key, _)| key.eq_ignore_ascii_case(name)) {
            Some((_, old)) => *old = value,
            None => self.params.push((name.to_ascii_uppercase(), value)),
        }
    }

    pub fn remove_param(&mut self, name: &str) {
        self.params.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
    }

    /// The calendar user address of an ORGANIZER or ATTENDEE: the mail address, lower case.
    pub fn address(&self) -> Option<String> {
        address(&self.value)
    }

    fn write(&self, out: &mut String) {
        let mut line = self.name.clone();
        for (key, value) in &self.params {
            line.push(';');
            line.push_str(key);
            line.push('=');
            if value.contains([':', ';', ',']) && !value.starts_with('"') {
                line.push('"');
                line.push_str(&value.replace('"', ""));
                line.push('"');
            } else {
                line.push_str(value);
            }
        }
        line.push(':');
        line.push_str(&self.value);
        fold(&line, out);
    }
}

/// `mailto:Mini@Example.org` as `mini@example.org`. `None` for other kinds of address.
pub fn address(value: &str) -> Option<String> {
    let value = value.trim();
    let rest = value.get(..7).filter(|scheme| scheme.eq_ignore_ascii_case("mailto:")).map(|_| &value[7..])?;
    let rest = rest.trim();
    (rest.contains('@') && !rest.contains([' ', '<', '>'])).then(|| rest.to_lowercase())
}

fn fold(line: &str, out: &mut String) {
    let mut width = 0;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if width + len > FOLD_AT {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += len;
    }
    out.push_str("\r\n");
}

/// A component with its lines and the components inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Upper case, like `VEVENT`.
    pub name: String,
    pub properties: Vec<Property>,
    pub components: Vec<Component>,
}

impl Component {
    pub fn new(name: &str) -> Component {
        Component { name: name.to_ascii_uppercase(), properties: Vec::new(), components: Vec::new() }
    }

    /// Reads an iCalendar object. `None` when it is not one.
    pub fn parse(text: &str) -> Option<Component> {
        let mut lines: Vec<String> = Vec::new();
        for raw in text.split('\n') {
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            if let Some(rest) = raw.strip_prefix([' ', '\t']) {
                lines.last_mut()?.push_str(rest);
            } else if !raw.is_empty() {
                if lines.len() >= MAX_LINES {
                    return None;
                }
                lines.push(raw.to_owned());
            }
        }
        let mut stack: Vec<Component> = Vec::new();
        let mut root = None;
        for line in lines {
            let property = parse_line(&line)?;
            match property.name.as_str() {
                "BEGIN" => {
                    if root.is_some() || stack.len() >= MAX_DEPTH {
                        return None;
                    }
                    stack.push(Component::new(&property.value));
                }
                "END" => {
                    let done = stack.pop()?;
                    if !done.name.eq_ignore_ascii_case(property.value.trim()) {
                        return None;
                    }
                    match stack.last_mut() {
                        Some(parent) => parent.components.push(done),
                        None => root = Some(done),
                    }
                }
                _ => stack.last_mut()?.properties.push(property),
            }
        }
        root.filter(|root| stack.is_empty() && root.name == "VCALENDAR")
    }

    /// The object as iCalendar text, lines folded.
    pub fn to_ics(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        fold(&format!("BEGIN:{}", self.name), out);
        for property in &self.properties {
            property.write(out);
        }
        for component in &self.components {
            component.write(out);
        }
        fold(&format!("END:{}", self.name), out);
    }

    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn property_mut(&mut self, name: &str) -> Option<&mut Property> {
        self.properties.iter_mut().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.property(name).map(|p| p.value.as_str())
    }

    pub fn properties_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.properties.iter().filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    /// Replaces the first property of the name, or adds it.
    pub fn set(&mut self, property: Property) {
        match self.properties.iter_mut().find(|p| p.name == property.name) {
            Some(old) => *old = property,
            None => self.properties.push(property),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.properties.retain(|p| !p.name.eq_ignore_ascii_case(name));
    }

    /// The events of a calendar object: the series (or single event) and its changed instances.
    pub fn events(&self) -> impl Iterator<Item = &Component> {
        self.components.iter().filter(|c| c.name == "VEVENT")
    }

    fn events_mut(&mut self) -> impl Iterator<Item = &mut Component> {
        self.components.iter_mut().filter(|c| c.name == "VEVENT")
    }

    /// The instance an event component stands for: `None` for the series or a single event.
    pub fn recurrence_id(&self) -> Option<String> {
        self.value("RECURRENCE-ID").map(|v| v.trim().to_owned())
    }

    fn event_for(&self, rid: &Option<String>) -> Option<&Component> {
        self.events().find(|event| event.recurrence_id() == *rid)
    }

    fn event_for_mut(&mut self, rid: &Option<String>) -> Option<&mut Component> {
        self.events_mut().find(|event| event.recurrence_id() == *rid)
    }

    /// The first event: the series if there is one.
    pub fn main_event(&self) -> Option<&Component> {
        self.event_for(&None).or_else(|| self.events().next())
    }
}

/// `NAME;PARAM=value;PARAM="quoted:value":VALUE`
fn parse_line(line: &str) -> Option<Property> {
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut name_end = None;
    let mut colon = None;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            b';' if !quoted && name_end.is_none() => name_end = Some(i),
            b':' if !quoted => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let name_end = name_end.unwrap_or(colon);
    let name = line[..name_end].trim();
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    let mut params = Vec::new();
    if name_end < colon {
        let mut rest = &line[name_end + 1..colon];
        while !rest.is_empty() {
            let (key, after) = rest.split_once('=')?;
            let mut end = after.len();
            let mut in_quotes = false;
            for (i, c) in after.char_indices() {
                match c {
                    '"' => in_quotes = !in_quotes,
                    ';' if !in_quotes => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            let value = &after[..end];
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .filter(|v| !v.contains('"'))
                .unwrap_or(value);
            params.push((key.trim().to_ascii_uppercase(), value.to_owned()));
            rest = after.get(end + 1..).unwrap_or("");
        }
    }
    Some(Property { name: name.to_ascii_uppercase(), params, value: line[colon + 1..].to_owned() })
}

// ------------------------------------------------------------------------------------------------
// Reading scheduling information.

/// The iTIP method of a message (`REQUEST`, `REPLY`, `CANCEL`, ...), upper case.
pub fn method(calendar: &Component) -> Option<String> {
    calendar.value("METHOD").map(|m| m.trim().to_ascii_uppercase())
}

pub fn uid(calendar: &Component) -> Option<String> {
    calendar.main_event()?.value("UID").map(|uid| uid.trim().to_owned())
}

/// The highest SEQUENCE of the object's events.
pub fn sequence(calendar: &Component) -> i64 {
    calendar.events().filter_map(|e| e.value("SEQUENCE")?.trim().parse().ok()).max().unwrap_or(0)
}

/// The organizer's address, if the object is a scheduled one.
pub fn organizer(calendar: &Component) -> Option<String> {
    calendar.events().find_map(|event| event.property("ORGANIZER")?.address())
}

fn schedule_agent_is_server(property: &Property) -> bool {
    property.param("SCHEDULE-AGENT").is_none_or(|agent| agent.eq_ignore_ascii_case("SERVER"))
}

/// Someone an event invites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub address: String,
    pub name: Option<String>,
    /// `NEEDS-ACTION`, `ACCEPTED`, `DECLINED`, `TENTATIVE`, `DELEGATED`, upper case.
    pub partstat: String,
    /// Whether the server is to send them scheduling messages (`SCHEDULE-AGENT`, RFC 6638).
    pub by_server: bool,
    /// `SCHEDULE-FORCE-SEND=REQUEST`: the client asks for a new invitation even without a change.
    pub force_request: bool,
}

fn attendee_of(property: &Property) -> Option<Attendee> {
    Some(Attendee {
        address: property.address()?,
        name: property.param("CN").map(str::to_owned),
        partstat: property.param("PARTSTAT").unwrap_or("NEEDS-ACTION").to_ascii_uppercase(),
        by_server: schedule_agent_is_server(property),
        force_request: property.param("SCHEDULE-FORCE-SEND").is_some_and(|v| v.eq_ignore_ascii_case("REQUEST")),
    })
}

/// Everyone any event of the object invites, each once, in order.
pub fn attendees(calendar: &Component) -> Vec<Attendee> {
    let mut out: Vec<Attendee> = Vec::new();
    for event in calendar.events() {
        for attendee in event.properties_named("ATTENDEE").filter_map(attendee_of) {
            match out.iter_mut().find(|known| known.address == attendee.address) {
                Some(known) => known.force_request |= attendee.force_request,
                None => out.push(attendee),
            }
        }
    }
    out
}

/// One attendee's answer for each event of the object, by recurrence id.
pub fn partstats(calendar: &Component, address: &str) -> Vec<(Option<String>, String)> {
    calendar
        .events()
        .filter_map(|event| {
            let attendee = event.properties_named("ATTENDEE").filter_map(attendee_of).find(|a| a.address == address)?;
            Some((event.recurrence_id(), attendee.partstat))
        })
        .collect()
}

/// What one of the addresses of an account is in an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// It is the organizer's own event.
    Organizer(String),
    /// It is invited, with this address.
    Attendee(String),
    /// Not scheduled, or not with any of these addresses.
    None,
}

/// Which part `own` (lower-case addresses) plays in an object.
pub fn role(calendar: &Component, own: &[String]) -> Role {
    let Some(organizer) = organizer(calendar) else { return Role::None };
    if own.contains(&organizer) {
        return Role::Organizer(organizer);
    }
    match attendees(calendar).into_iter().find(|a| own.contains(&a.address)) {
        Some(attendee) => Role::Attendee(attendee.address),
        None => Role::None,
    }
}

/// Whether the organizer left scheduling to the clients (`SCHEDULE-AGENT=CLIENT` or `NONE`).
fn organizer_schedules_itself(calendar: &Component) -> bool {
    calendar.events().any(|event| event.property("ORGANIZER").is_some_and(|p| !schedule_agent_is_server(p)))
}

/// The attendees the server sends messages to for an organizer: not the organizer, not the
/// account itself, and only those the server schedules for.
fn recipients(calendar: &Component, own: &[String]) -> Vec<Attendee> {
    attendees(calendar).into_iter().filter(|a| a.by_server && !own.contains(&a.address)).collect()
}

/// What an event is without what does not concern attendees: per-user data, stamps and answers.
fn essence(calendar: &Component) -> Vec<(Option<String>, Vec<String>)> {
    const IGNORED: &[&str] = &["DTSTAMP", "LAST-MODIFIED", "SEQUENCE", "CREATED", "TRANSP", "CATEGORIES"];
    const ANSWER_PARAMS: &[&str] = &["PARTSTAT", "RSVP", "SCHEDULE-STATUS", "SCHEDULE-FORCE-SEND"];
    let mut out: Vec<(Option<String>, Vec<String>)> = calendar
        .events()
        .map(|event| {
            let mut lines: Vec<String> = event
                .properties
                .iter()
                .filter(|p| !IGNORED.contains(&p.name.as_str()) && !p.name.starts_with("X-"))
                .map(|p| {
                    let mut p = p.clone();
                    if p.name == "ATTENDEE" || p.name == "ORGANIZER" {
                        p.params.retain(|(key, _)| !ANSWER_PARAMS.contains(&key.as_str()));
                        p.value = p.address().unwrap_or(p.value);
                    }
                    p.params.sort();
                    let mut line = String::new();
                    p.write(&mut line);
                    line
                })
                .collect();
            lines.sort();
            (event.recurrence_id(), lines)
        })
        .collect();
    out.sort();
    out
}

/// What the server has to send after a scheduling object changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// As organizer: attendees to (re)invite with a REQUEST.
    pub requests: Vec<String>,
    /// As organizer: attendees taken off, or everyone when the event is gone, with a CANCEL.
    pub cancels: Vec<String>,
    /// As attendee: the address to answer the organizer with, in a REPLY.
    pub reply_as: Option<String>,
    /// The organizer's address; for a reply, where it goes.
    pub organizer: Option<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty() && self.cancels.is_empty() && self.reply_as.is_none()
    }
}

/// Whom to tell what after an account changed an object from `old` to `new` (`None`: it was not
/// there, or is gone). `own` are the account's addresses, lower case (RFC 6638, 3.2).
pub fn plan(old: Option<&Component>, new: Option<&Component>, own: &[String]) -> Plan {
    let Some(reference) = new.or(old) else { return Plan::default() };
    let organizer = organizer(reference);
    let mut plan = Plan { organizer: organizer.clone(), ..Plan::default() };
    if organizer_schedules_itself(reference) {
        return plan;
    }
    match role(reference, own) {
        Role::Organizer(_) => {
            let before: Vec<Attendee> = old.map(|old| recipients(old, own)).unwrap_or_default();
            let after: Vec<Attendee> = new.map(|new| recipients(new, own)).unwrap_or_default();
            let changed = match (old, new) {
                (Some(old), Some(new)) => essence(old) != essence(new),
                _ => true,
            };
            plan.cancels = before
                .iter()
                .filter(|a| !after.iter().any(|b| b.address == a.address))
                .map(|a| a.address.clone())
                .collect();
            plan.requests = after
                .iter()
                .filter(|a| changed || a.force_request || !before.iter().any(|b| b.address == a.address))
                .map(|a| a.address.clone())
                .collect();
        }
        Role::Attendee(me) => {
            let answered = match (old, new) {
                (Some(old), Some(new)) => partstats(old, &me) != partstats(new, &me),
                // Taken into the calendar with an answer, as clients do with an invitation mail.
                (None, Some(new)) => partstats(new, &me).iter().any(|(_, status)| status != "NEEDS-ACTION"),
                // Deleting an invitation declines it, unless it was declined already.
                (Some(old), None) => partstats(old, &me).iter().any(|(_, status)| status != "DECLINED"),
                (None, None) => false,
            };
            if answered {
                plan.reply_as = Some(me);
            }
        }
        Role::None => {}
    }
    plan
}

// ------------------------------------------------------------------------------------------------
// Writing messages.

/// `20260917T080000Z` for now.
pub fn stamp(now: i64) -> String {
    let days = now.div_euclid(86_400);
    let seconds = now.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

/// A new calendar object with the method and the object's time zones, for a message.
fn message(calendar: &Component, method: &str) -> Component {
    let mut out = Component::new("VCALENDAR");
    out.properties.push(Property::new("VERSION", "2.0"));
    out.properties.push(Property::new("PRODID", "-//UwUMail//Server//EN"));
    out.properties.push(Property::new("METHOD", method));
    out.components.extend(calendar.components.iter().filter(|c| c.name == "VTIMEZONE").cloned());
    out
}

/// Scheduling parameters are between a client and its server; they stay out of messages.
fn clean_scheduling_params(event: &mut Component) {
    for property in event.properties.iter_mut().filter(|p| p.name == "ATTENDEE" || p.name == "ORGANIZER") {
        for param in ["SCHEDULE-STATUS", "SCHEDULE-AGENT", "SCHEDULE-FORCE-SEND"] {
            property.remove_param(param);
        }
    }
}

/// An organizer's invitation or update: the whole object, without the organizer's alarms.
pub fn request(calendar: &Component, now: i64) -> Component {
    let mut out = message(calendar, "REQUEST");
    for event in calendar.events() {
        let mut event = event.clone();
        event.components.retain(|c| c.name != "VALARM");
        event.properties.retain(|p| !p.name.starts_with("X-MOZ-") && !p.name.starts_with("X-APPLE-"));
        clean_scheduling_params(&mut event);
        event.set(Property::new("DTSTAMP", stamp(now)));
        out.components.push(event);
    }
    out
}

/// The lines that say which event a short message is about.
fn identifying(event: &Component, sequence: Option<i64>, now: i64) -> Component {
    let mut out = Component::new("VEVENT");
    for name in ["UID", "RECURRENCE-ID", "DTSTART", "DTEND", "DURATION", "SUMMARY", "LOCATION", "ORGANIZER"] {
        if let Some(property) = event.property(name) {
            out.properties.push(property.clone());
        }
    }
    let sequence = sequence.unwrap_or_else(|| event.value("SEQUENCE").and_then(|s| s.trim().parse().ok()).unwrap_or(0));
    out.properties.push(Property::new("SEQUENCE", sequence.to_string()));
    out.properties.push(Property::new("DTSTAMP", stamp(now)));
    out
}

/// An organizer's cancellation for `attendees`: the whole event when it is gone (`whole`), or only
/// for them when they were taken off it.
pub fn cancel(calendar: &Component, attendees: &[String], whole: bool, now: i64) -> Component {
    let mut out = message(calendar, "CANCEL");
    let bump = i64::from(whole);
    for event in calendar.events() {
        let invited: Vec<&Property> = event
            .properties_named("ATTENDEE")
            .filter(|p| p.address().is_some_and(|a| attendees.contains(&a)))
            .collect();
        if invited.is_empty() {
            continue;
        }
        let current: i64 = event.value("SEQUENCE").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        let mut cancelled = identifying(event, Some(current + bump), now);
        cancelled.properties.extend(invited.into_iter().cloned());
        cancelled.properties.push(Property::new("STATUS", "CANCELLED"));
        clean_scheduling_params(&mut cancelled);
        out.components.push(cancelled);
    }
    out
}

/// An attendee's answer to the organizer, for every event of the object that invites them.
/// `partstat` overrides what the object says, as when deleting an invitation declines it.
pub fn reply(calendar: &Component, me: &str, partstat: Option<&str>, now: i64) -> Component {
    let mut out = message(calendar, "REPLY");
    for event in calendar.events() {
        let Some(attendee) = event.properties_named("ATTENDEE").find(|p| p.address().as_deref() == Some(me)) else {
            continue;
        };
        let mut attendee = attendee.clone();
        if let Some(partstat) = partstat {
            attendee.set_param("PARTSTAT", partstat);
        }
        attendee.remove_param("RSVP");
        let mut answer = identifying(event, None, now);
        answer.properties.push(attendee);
        clean_scheduling_params(&mut answer);
        out.components.push(answer);
    }
    out
}

// ------------------------------------------------------------------------------------------------
// Applying messages.

fn times(event: &Component) -> Vec<String> {
    ["DTSTART", "DTEND", "DURATION", "RRULE", "RDATE", "EXDATE"]
        .iter()
        .flat_map(|name| event.properties_named(name).map(move |p| format!("{name}{:?}{}", p.params, p.value)))
        .collect()
}

/// What an attendee keeps of an organizer's REQUEST: the organizer's object, with the attendee's
/// own alarms and, while the time stays the same, their own answer from their current copy.
/// `own` are the attendee's addresses.
pub fn attendee_copy(request: &Component, current: Option<&Component>, own: &[String]) -> Component {
    let mut copy = request.clone();
    copy.remove("METHOD");
    for event in copy.events_mut() {
        let rid = event.recurrence_id();
        let mine = current.and_then(|current| current.event_for(&rid));
        if let Some(mine) = mine {
            event.components.retain(|c| c.name != "VALARM");
            event.components.extend(mine.components.iter().filter(|c| c.name == "VALARM").cloned());
            if times(mine) == times(event) {
                let answers: Vec<(String, String)> = mine
                    .properties_named("ATTENDEE")
                    .filter_map(|p| Some((p.address()?, p.param("PARTSTAT")?.to_owned())))
                    .filter(|(address, _)| own.contains(address))
                    .collect();
                for property in event.properties.iter_mut().filter(|p| p.name == "ATTENDEE") {
                    if let Some((_, partstat)) =
                        answers.iter().find(|(address, _)| property.address().as_ref() == Some(address))
                    {
                        property.set_param("PARTSTAT", partstat.clone());
                    }
                }
            }
        }
        clean_scheduling_params(event);
    }
    copy
}

/// Carries the attendees' answers from the stored copy into what an organizer's client stores with
/// `If-Schedule-Tag-Match`: answers that came in meanwhile, which the client has not seen yet
/// (RFC 6638, 3.2.10). The organizer's own answer stays as the client sent it.
pub fn keep_answers(new: &mut Component, old: &Component, own: &[String]) {
    for event in new.events_mut() {
        let rid = event.recurrence_id();
        let Some(stored) = old.event_for(&rid) else { continue };
        for property in event.properties.iter_mut().filter(|p| p.name == "ATTENDEE") {
            let Some(address) = property.address() else { continue };
            if own.contains(&address) {
                continue;
            }
            let answer = stored
                .properties_named("ATTENDEE")
                .find(|p| p.address().as_ref() == Some(&address))
                .and_then(|p| p.param("PARTSTAT"));
            if let Some(answer) = answer {
                property.set_param("PARTSTAT", answer.to_owned());
            }
        }
    }
}

/// Writes an attendee's answer into the organizer's copy. Only lines of `from` count, so nobody
/// answers for someone else. Returns whether anything changed.
pub fn apply_reply(copy: &mut Component, reply: &Component, from: &str) -> bool {
    let mut changed = false;
    for answer in reply.events() {
        let Some(partstat) = answer
            .properties_named("ATTENDEE")
            .find(|p| p.address().as_deref() == Some(from))
            .and_then(|p| p.param("PARTSTAT"))
            .map(str::to_ascii_uppercase)
        else {
            continue;
        };
        if !matches!(partstat.as_str(), "ACCEPTED" | "DECLINED" | "TENTATIVE" | "NEEDS-ACTION" | "DELEGATED") {
            continue;
        }
        let rid = answer.recurrence_id();
        let Some(event) = copy.event_for_mut(&rid) else { continue };
        if let Some(attendee) =
            event.properties.iter_mut().find(|p| p.name == "ATTENDEE" && p.address().as_deref() == Some(from))
        {
            if attendee.param("PARTSTAT").is_none_or(|old| !old.eq_ignore_ascii_case(&partstat)) {
                attendee.set_param("PARTSTAT", partstat.clone());
                changed = true;
            }
            attendee.remove_param("RSVP");
            attendee.set_param("SCHEDULE-STATUS", "2.0");
        }
    }
    changed
}

/// Marks what an organizer cancelled in an attendee's copy: the whole event, or single instances,
/// which come out of the series when it has no changed copy of them. Returns whether anything
/// changed.
pub fn apply_cancel(copy: &mut Component, cancel: &Component) -> bool {
    let mut changed = false;
    for cancelled in cancel.events() {
        match cancelled.recurrence_id() {
            None => {
                for event in copy.events_mut() {
                    if event.value("STATUS") != Some("CANCELLED") {
                        event.set(Property::new("STATUS", "CANCELLED"));
                        changed = true;
                    }
                }
            }
            Some(rid) => match copy.event_for_mut(&Some(rid.clone())) {
                Some(event) => {
                    if event.value("STATUS") != Some("CANCELLED") {
                        event.set(Property::new("STATUS", "CANCELLED"));
                        changed = true;
                    }
                }
                None => {
                    let Some(series) = copy.event_for_mut(&None) else { continue };
                    let mut exdate = cancelled.property("RECURRENCE-ID").cloned().unwrap_or(Property::new("", ""));
                    exdate.name = "EXDATE".into();
                    exdate.remove_param("RANGE");
                    series.properties.push(exdate);
                    changed = true;
                }
            },
        }
    }
    changed
}

/// A short description of an event for the mail that carries a message: title, start, place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub title: String,
    /// `2026-09-20 09:00 (Europe/Berlin)`, `2026-09-20` for a day, or empty.
    pub when: String,
    pub location: String,
    pub organizer_name: String,
}

pub fn summary(calendar: &Component) -> Summary {
    let Some(event) = calendar.main_event() else { return Summary::default() };
    let text = |name: &str| event.value(name).map(unescape).unwrap_or_default();
    let when = event
        .property("DTSTART")
        .map(|start| {
            let value = start.value.trim();
            let date = |v: &str| {
                v.get(..8).map(|d| format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8])).unwrap_or_default()
            };
            match value.split_once('T') {
                None => date(value),
                Some((day, time)) if time.len() >= 4 => {
                    let zone = if value.ends_with('Z') {
                        "UTC".to_owned()
                    } else {
                        start.param("TZID").map(str::to_owned).unwrap_or_default()
                    };
                    let clock = format!("{}:{}", &time[..2], &time[2..4]);
                    if zone.is_empty() {
                        format!("{} {clock}", date(day))
                    } else {
                        format!("{} {clock} ({zone})", date(day))
                    }
                }
                Some((day, _)) => date(day),
            }
        })
        .unwrap_or_default();
    Summary {
        title: text("SUMMARY"),
        when,
        location: text("LOCATION"),
        organizer_name: event.property("ORGANIZER").and_then(|p| p.param("CN")).unwrap_or_default().to_owned(),
    }
}

/// A TEXT value as a person reads it (RFC 5545, 3.3.11).
pub fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVITE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Test//EN\r\nBEGIN:VEVENT\r\n\
UID:kaffee@example.org\r\nDTSTAMP:20260917T080000Z\r\nDTSTART;TZID=Europe/Berlin:20260920T090000\r\n\
DTEND;TZID=Europe/Berlin:20260920T100000\r\nSUMMARY:Kaffee\\, Kuchen\r\nSEQUENCE:0\r\n\
ORGANIZER;CN=Mini:mailto:mini@example.org\r\n\
ATTENDEE;CN=Mini;PARTSTAT=ACCEPTED:mailto:mini@example.org\r\n\
ATTENDEE;CN=Leni;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:Leni@Example.org\r\n\
ATTENDEE;CN=\"Nyu, die Katze\";PARTSTAT=NEEDS-ACTION;SCHEDULE-AGENT=CLIENT:mailto:nyu@example.net\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:gast@example.com\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn own(addresses: &[&str]) -> Vec<String> {
        addresses.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn objects_round_trip_with_folding_and_quotes() {
        let calendar = Component::parse(INVITE).unwrap();
        let written = calendar.to_ics();
        assert_eq!(Component::parse(&written).unwrap(), calendar);
        assert!(written.contains("CN=\"Nyu, die Katze\""), "{written}");
        let long = Property::new("DESCRIPTION", "ü".repeat(100));
        let mut out = String::new();
        long.write(&mut out);
        assert!(out.split("\r\n").all(|line| line.len() <= FOLD_AT), "{out}");
        assert!(Component::parse("BEGIN:VEVENT\r\nEND:VEVENT\r\n").is_none());
        assert!(Component::parse("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nEND:VCALENDAR\r\n").is_none());
        assert_eq!(address("MAILTO:Mini@Example.org"), Some("mini@example.org".into()));
        assert_eq!(address("urn:uuid:1234"), None);
    }

    #[test]
    fn organizers_invite_update_and_cancel() {
        let calendar = Component::parse(INVITE).unwrap();
        let mini = own(&["mini@example.org"]);
        assert_eq!(role(&calendar, &mini), Role::Organizer("mini@example.org".into()));
        let created = plan(None, Some(&calendar), &mini);
        assert_eq!(created.requests, ["leni@example.org", "gast@example.com"], "not the client-scheduled one");
        assert!(created.cancels.is_empty());

        // Only an alarm and a stamp changed: nobody hears about it.
        let mut quiet = calendar.clone();
        quiet.components[0].set(Property::new("DTSTAMP", "20260918T080000Z"));
        quiet.components[0].components.clear();
        assert!(plan(Some(&calendar), Some(&quiet), &mini).is_empty());

        // An answer written into the organizer's copy is no change for the others either.
        let mut answered = calendar.clone();
        apply_reply(&mut answered, &reply(&calendar, "leni@example.org", Some("ACCEPTED"), 0), "leni@example.org");
        assert!(plan(Some(&calendar), Some(&answered), &mini).is_empty());

        // A new time goes to everyone; someone taken off gets a cancellation.
        let mut moved = calendar.clone();
        moved.components[0].property_mut("DTSTART").unwrap().value = "20260920T110000".into();
        moved.components[0].properties.retain(|p| p.address().as_deref() != Some("gast@example.com"));
        let update = plan(Some(&calendar), Some(&moved), &mini);
        assert_eq!(update.requests, ["leni@example.org"]);
        assert_eq!(update.cancels, ["gast@example.com"]);
        assert_eq!(plan(Some(&calendar), None, &mini).cancels, ["leni@example.org", "gast@example.com"]);

        let sent = request(&calendar, 1_789_894_800);
        assert_eq!(method(&sent).as_deref(), Some("REQUEST"));
        assert!(sent.components[0].components.is_empty(), "no alarms go out");
        assert_eq!(sent.components[0].value("DTSTAMP"), Some("20260920T090000Z"));
        assert!(!sent.to_ics().contains("SCHEDULE-AGENT"));

        let cancelled = cancel(&calendar, &own(&["gast@example.com"]), true, 0);
        let event = &cancelled.components[0];
        assert_eq!((event.value("STATUS"), event.value("SEQUENCE")), (Some("CANCELLED"), Some("1")));
        assert_eq!(event.properties_named("ATTENDEE").count(), 1);
    }

    #[test]
    fn attendees_answer_and_keep_their_answer() {
        let calendar = Component::parse(INVITE).unwrap();
        let leni = own(&["leni@example.org"]);
        assert_eq!(role(&calendar, &leni), Role::Attendee("leni@example.org".into()));
        let mut copy = attendee_copy(&request(&calendar, 0), None, &leni);
        assert_eq!(method(&copy), None);
        assert!(plan(None, Some(&copy), &leni).is_empty(), "a new invitation is not an answer yet");

        let before = copy.clone();
        let leni_line = copy.components[0]
            .properties
            .iter_mut()
            .find(|p| p.address().as_deref() == Some("leni@example.org"))
            .unwrap();
        leni_line.set_param("PARTSTAT", "ACCEPTED");
        let answer = plan(Some(&before), Some(&copy), &leni);
        assert_eq!(answer.reply_as.as_deref(), Some("leni@example.org"));
        assert_eq!(answer.organizer.as_deref(), Some("mini@example.org"));
        let sent = reply(&copy, "leni@example.org", None, 0);
        assert_eq!(sent.components[0].properties_named("ATTENDEE").count(), 1);

        // The organizer takes it in: only Leni's own line counts.
        let mut organizer_copy = calendar.clone();
        assert!(!apply_reply(&mut organizer_copy, &sent, "gast@example.com"), "nobody answers for Leni");
        assert!(apply_reply(&mut organizer_copy, &sent, "leni@example.org"));
        assert_eq!(partstats(&organizer_copy, "leni@example.org"), [(None, "ACCEPTED".to_owned())]);

        // An update with the same time keeps Leni's answer; a new time asks again.
        let kept = attendee_copy(&request(&calendar, 0), Some(&copy), &leni);
        assert_eq!(partstats(&kept, "leni@example.org"), [(None, "ACCEPTED".to_owned())]);
        let mut moved = calendar.clone();
        moved.components[0].property_mut("DTSTART").unwrap().value = "20260920T110000".into();
        let asked = attendee_copy(&request(&moved, 0), Some(&copy), &leni);
        assert_eq!(partstats(&asked, "leni@example.org"), [(None, "NEEDS-ACTION".to_owned())]);

        // Deleting declines.
        let declined = plan(Some(&copy), None, &leni);
        assert_eq!(declined.reply_as.as_deref(), Some("leni@example.org"));
        let sent = reply(&copy, "leni@example.org", Some("DECLINED"), 0);
        assert!(sent.to_ics().contains("PARTSTAT=DECLINED"));
    }

    #[test]
    fn cancellations_mark_events_and_instances() {
        let series = INVITE.replace("SEQUENCE:0\r\n", "SEQUENCE:0\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\n");
        let calendar = Component::parse(&series).unwrap();
        let mut copy = attendee_copy(&request(&calendar, 0), None, &own(&["leni@example.org"]));
        let mut one = cancel(&calendar, &own(&["leni@example.org"]), false, 0);
        one.components[0].properties.push({
            let mut rid = Property::new("RECURRENCE-ID", "20260927T090000");
            rid.set_param("TZID", "Europe/Berlin");
            rid
        });
        assert!(apply_cancel(&mut copy, &one));
        let exdate = copy.components[0].property("EXDATE").unwrap();
        assert_eq!((exdate.value.as_str(), exdate.param("TZID")), ("20260927T090000", Some("Europe/Berlin")));
        assert!(apply_cancel(&mut copy, &cancel(&calendar, &own(&["leni@example.org"]), true, 0)));
        assert_eq!(copy.components[0].value("STATUS"), Some("CANCELLED"));
    }

    #[test]
    fn summaries_for_the_mail() {
        let calendar = Component::parse(INVITE).unwrap();
        let summary = summary(&calendar);
        assert_eq!(summary.title, "Kaffee, Kuchen");
        assert_eq!(summary.when, "2026-09-20 09:00 (Europe/Berlin)");
        assert_eq!(summary.organizer_name, "Mini");
        assert_eq!(stamp(784_887_151), "19941115T081231Z");
    }
}
