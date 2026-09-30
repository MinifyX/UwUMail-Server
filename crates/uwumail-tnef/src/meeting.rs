//! Meetings in TNEF (MS-OXOCAL) as iCalendar with a METHOD (iTIP, RFC 5546), the way MS-OXCICAL
//! maps them: `IPM.Schedule.Meeting.Request` becomes REQUEST, `.Canceled` CANCEL, `.Resp.Pos`,
//! `.Resp.Neg` and `.Resp.Tent` a REPLY, `IPM.Appointment` PUBLISH.

use crate::mapi::{PSETID_APPOINTMENT, PSETID_COMMON, PSETID_MEETING, Value};
use crate::reader::Reader;
use crate::time::{civil_from_days, days_from_civil, ical_date, ical_datetime, minutes_1601, nth_weekday};
use crate::{Message, Person, RecipientKind, codepage, mapi};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartStat {
    Accepted,
    Declined,
    Tentative,
}

impl PartStat {
    fn ical(self) -> &'static str {
        match self {
            PartStat::Accepted => "ACCEPTED",
            PartStat::Declined => "DECLINED",
            PartStat::Tentative => "TENTATIVE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingKind {
    /// An invitation, or an update of one.
    Request,
    Cancel,
    /// An attendee's answer.
    Reply(PartStat),
    /// An appointment sent as information, not an invitation.
    Publish,
}

impl MeetingKind {
    pub fn method(self) -> &'static str {
        match self {
            MeetingKind::Request => "REQUEST",
            MeetingKind::Cancel => "CANCEL",
            MeetingKind::Reply(_) => "REPLY",
            MeetingKind::Publish => "PUBLISH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub person: Person,
    /// To: required, Cc: optional, Bcc: a resource.
    pub kind: RecipientKind,
}

/// A time-zone rule's switch: the `week`th (5 = last) `weekday` (0 = Sunday) of `month`, at a
/// local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub month: u32,
    pub week: u32,
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Transition {
    fn read(r: &mut Reader<'_>) -> Option<Option<Transition>> {
        let year = r.u16().ok()?;
        let month = u32::from(r.u16().ok()?);
        let weekday = u32::from(r.u16().ok()?);
        let week = u32::from(r.u16().ok()?);
        let hour = u32::from(r.u16().ok()?);
        let minute = u32::from(r.u16().ok()?);
        let second = u32::from(r.u16().ok()?);
        r.u16().ok()?;
        // Month 0: no switch. A year: a switch on one date only, which is not kept.
        let valid = year == 0
            && (1..=12).contains(&month)
            && weekday <= 6
            && (1..=5).contains(&week)
            && hour <= 23
            && minute <= 59
            && second <= 59;
        Some(valid.then_some(Transition { month, week, weekday, hour, minute, second }))
    }

    /// The local time of the switch in `year`, as seconds.
    fn local(&self, year: i64) -> i64 {
        let day = nth_weekday(year, self.month, self.week, self.weekday);
        days_from_civil(year, self.month, day) * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    fn rrule(&self) -> String {
        let week = if self.week == 5 { "-1".to_owned() } else { self.week.to_string() };
        format!("FREQ=YEARLY;BYMONTH={};BYDAY={week}{}", self.month, WEEKDAYS[self.weekday as usize])
    }
}

const WEEKDAYS: [&str; 7] = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];

/// A Windows time zone as the meeting gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZone {
    /// The registry key name (`W. Europe Standard Time`), or the description.
    pub name: String,
    /// Minutes: UTC = local time + bias.
    pub bias: i32,
    pub standard_bias: i32,
    pub daylight_bias: i32,
    pub standard: Option<Transition>,
    pub daylight: Option<Transition>,
}

impl TimeZone {
    /// A TZDEFINITION (`PidLidAppointmentTimeZoneDefinitionStartDisplay`, MS-OXOCAL 2.2.1.41).
    fn definition(data: &[u8]) -> Option<TimeZone> {
        let mut r = Reader::new(data);
        r.u8().ok()?;
        r.u8().ok()?;
        let header = usize::from(r.u16().ok()?);
        r.u16().ok()?;
        let chars = usize::from(r.u16().ok()?);
        let name = codepage::utf16le(r.take(chars.checked_mul(2)?).ok()?);
        let rules = r.u16().ok()?;
        let mut r = Reader::new(data.get(4 + header..)?);
        let mut chosen = None;
        for _ in 0..rules.min(1000) {
            let rule = r.take(66).ok()?;
            let mut rr = Reader::new(rule);
            rr.skip(4).ok()?;
            let flags = rr.u16().ok()?;
            rr.skip(16).ok()?;
            let bias = rr.i32().ok()?;
            let standard_bias = rr.i32().ok()?;
            let daylight_bias = rr.i32().ok()?;
            let standard = Transition::read(&mut rr)?;
            let daylight = Transition::read(&mut rr)?;
            let zone = TimeZone { name: name.clone(), bias, standard_bias, daylight_bias, standard, daylight };
            let effective = flags & 0x0002 != 0;
            if effective || chosen.is_none() {
                chosen = Some(zone);
            }
            if effective {
                break;
            }
        }
        chosen.map(TimeZone::checked)
    }

    /// A TimeZoneStruct (`PidLidTimeZoneStruct`, MS-OXOCAL 2.2.1.39), named by its description.
    fn from_struct(data: &[u8], name: &str) -> Option<TimeZone> {
        let mut r = Reader::new(data);
        let bias = r.i32().ok()?;
        let standard_bias = r.i32().ok()?;
        let daylight_bias = r.i32().ok()?;
        r.u16().ok()?;
        let standard = Transition::read(&mut r)?;
        r.u16().ok()?;
        let daylight = Transition::read(&mut r)?;
        Some(TimeZone { name: name.to_owned(), bias, standard_bias, daylight_bias, standard, daylight }.checked())
    }

    /// Only plausible offsets; a zone switches both ways or not at all.
    fn checked(mut self) -> TimeZone {
        let plausible = |m: i32| (-24 * 60..=24 * 60).contains(&m);
        if !plausible(self.bias) || !plausible(self.standard_bias) || !plausible(self.daylight_bias) {
            self.bias = 0;
            self.standard_bias = 0;
            self.daylight_bias = 0;
        }
        if self.standard.is_none() || self.daylight.is_none() {
            self.standard = None;
            self.daylight = None;
        }
        let name: String =
            self.name.chars().map(|c| if c.is_control() || "\";:,".contains(c) { ' ' } else { c }).take(100).collect();
        self.name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if self.name.is_empty() {
            self.name = "Windows".to_owned();
        }
        self
    }

    /// Minutes east of UTC in standard and in daylight time.
    fn standard_offset(&self) -> i32 {
        -(self.bias + self.standard_bias)
    }

    fn daylight_offset(&self) -> i32 {
        -(self.bias + self.daylight_bias)
    }

    /// Minutes east of UTC at a moment.
    pub fn offset_at(&self, utc: i64) -> i32 {
        let (Some(standard), Some(daylight)) = (&self.standard, &self.daylight) else {
            return self.standard_offset();
        };
        let std_off = i64::from(self.standard_offset()) * 60;
        let dst_off = i64::from(self.daylight_offset()) * 60;
        let year = civil_from_days((utc + std_off).div_euclid(86_400)).0;
        let dst_start = daylight.local(year) - std_off;
        let std_start = standard.local(year) - dst_off;
        let in_daylight = if dst_start < std_start {
            utc >= dst_start && utc < std_start
        } else {
            !(utc >= std_start && utc < dst_start)
        };
        if in_daylight { self.daylight_offset() } else { self.standard_offset() }
    }

    pub fn to_local(&self, utc: i64) -> i64 {
        utc + i64::from(self.offset_at(utc)) * 60
    }

    pub fn to_utc(&self, local: i64) -> i64 {
        let guess = local - i64::from(self.standard_offset()) * 60;
        local - i64::from(self.offset_at(guess)) * 60
    }

    fn vtimezone(&self, out: &mut Vec<String>) {
        out.push("BEGIN:VTIMEZONE".into());
        out.push(format!("TZID:{}", self.name));
        let std = offset(self.standard_offset());
        match (&self.standard, &self.daylight) {
            (Some(standard), Some(daylight)) => {
                let dst = offset(self.daylight_offset());
                for (kind, rule, from, to) in [("STANDARD", standard, &dst, &std), ("DAYLIGHT", daylight, &std, &dst)] {
                    out.push(format!("BEGIN:{kind}"));
                    out.push(format!("DTSTART:{}", ical_datetime(rule.local(1601), false)));
                    out.push(format!("RRULE:{}", rule.rrule()));
                    out.push(format!("TZOFFSETFROM:{from}"));
                    out.push(format!("TZOFFSETTO:{to}"));
                    out.push(format!("END:{kind}"));
                }
            }
            _ => {
                out.push("BEGIN:STANDARD".into());
                out.push("DTSTART:16010101T000000".into());
                out.push(format!("TZOFFSETFROM:{std}"));
                out.push(format!("TZOFFSETTO:{std}"));
                out.push("END:STANDARD".into());
            }
        }
        out.push("END:VTIMEZONE".into());
    }
}

fn offset(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.unsigned_abs();
    format!("{sign}{:02}{:02}", m / 60, m % 60)
}

/// A recurring meeting's pattern (`PidLidAppointmentRecur`, MS-OXOCAL 2.2.1.44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    /// `DAILY`, `WEEKLY`, `MONTHLY` or `YEARLY`.
    pub frequency: &'static str,
    pub interval: u32,
    /// Weekdays, 0 = Sunday.
    pub weekdays: Vec<u32>,
    /// With `weekdays`: which of them in the month (1–4, -1 = last).
    pub set_position: Option<i32>,
    /// Day of the month, -1 = the last.
    pub month_day: Option<i32>,
    pub count: Option<u32>,
    /// The last day, local midnight as seconds.
    pub until_local: Option<i64>,
    pub week_start: u32,
    /// Instances taken out of the series, local midnights as seconds.
    pub deleted_local: Vec<i64>,
    /// Minutes after local midnight each instance starts.
    pub start_offset: Option<u32>,
}

impl Recurrence {
    fn parse(data: &[u8]) -> Option<Recurrence> {
        let mut r = Reader::new(data);
        r.skip(4).ok()?;
        let frequency = r.u16().ok()?;
        let pattern = r.u16().ok()?;
        let calendar = r.u16().ok()?;
        r.u32().ok()?;
        let period = r.u32().ok()?;
        r.u32().ok()?;
        // Only the Gregorian calendar.
        if calendar > 1 {
            return None;
        }
        let (mask, day, nth) = match pattern {
            0 => (0, 0, 0),
            1 => (r.u32().ok()?, 0, 0),
            2 | 4 => (0, r.u32().ok()?, 0),
            3 => (r.u32().ok()?, 0, r.u32().ok()?),
            _ => return None,
        };
        let end_type = r.u32().ok()?;
        let occurrences = r.u32().ok()?;
        let first_dow = r.u32().ok()?;
        let mut deleted = Vec::new();
        for _ in 0..r.u32().ok()? {
            deleted.push(r.u32().ok()?);
        }
        let mut modified = Vec::new();
        for _ in 0..r.u32().ok()? {
            modified.push(r.u32().ok()?);
        }
        r.u32().ok()?;
        let end_date = r.u32().ok()?;
        // AppointmentRecurrencePattern goes on with the time of day.
        let start_offset = (|| {
            r.skip(8).ok()?;
            r.u32().ok()
        })()
        .filter(|m| *m < 24 * 60);
        let weekdays: Vec<u32> = (0..7).filter(|d| mask & (1 << d) != 0).collect();
        let yearly = frequency == 0x200D;
        let (frequency, interval) = match pattern {
            0 => ("DAILY", period / 1440),
            1 if frequency == 0x200A => ("WEEKLY", 1),
            1 => ("WEEKLY", period),
            _ if yearly => ("YEARLY", period / 12),
            _ => ("MONTHLY", period),
        };
        if pattern == 1 && weekdays.is_empty() || pattern == 3 && (weekdays.is_empty() || !(1..=5).contains(&nth)) {
            return None;
        }
        let month_day = match pattern {
            2 if (1..=31).contains(&day) => Some(day as i32),
            4 => Some(-1),
            2 => return None,
            _ => None,
        };
        let deleted_local =
            deleted.iter().filter(|d| !modified.contains(d)).take(1000).map(|d| minutes_1601(*d)).collect();
        Some(Recurrence {
            frequency,
            interval: interval.clamp(1, 1000),
            weekdays: if pattern == 1 || pattern == 3 { weekdays } else { Vec::new() },
            set_position: (pattern == 3).then_some(if nth == 5 { -1 } else { nth as i32 }),
            month_day,
            count: (end_type == 0x2022).then_some(occurrences.clamp(1, 10_000)),
            until_local: (end_type == 0x2021).then(|| minutes_1601(end_date)),
            week_start: first_dow.min(6),
            deleted_local,
            start_offset,
        })
    }
}

/// A meeting a message carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meeting {
    pub kind: MeetingKind,
    /// From the GlobalObjectId, as MS-OXCICAL derives it.
    pub uid: Option<String>,
    /// The instance of a series this message is about, UTC.
    pub recurrence_id: Option<i64>,
    pub sequence: i64,
    /// UTC.
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub all_day: bool,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// For requests and cancellations; answers leave it to whom they are sent to.
    pub organizer: Option<Person>,
    /// Invited people; for an answer, the one who answers.
    pub attendees: Vec<Attendee>,
    /// 0 free, 1 tentative, 2 busy, 3 away.
    pub busy_status: Option<i64>,
    pub time_zone: Option<TimeZone>,
    pub recurrence: Option<Recurrence>,
    /// When it was sent, UTC.
    pub stamp: Option<i64>,
}

/// What the surrounding mail says, for what the TNEF leaves out, and the time now.
#[derive(Debug, Clone, Default)]
pub struct IcsOptions {
    /// Unix seconds, for DTSTAMP when the message has no time.
    pub now: i64,
    /// The mail's From: the organizer of a request, the one who answers of a reply.
    pub from: Option<Person>,
    /// The mail's To: the attendees of a request (required), the organizer of a reply.
    pub to: Vec<Person>,
    /// The mail's Cc: optional attendees of a request.
    pub cc: Vec<Person>,
}

const MAX_DESCRIPTION_CHARS: usize = 32_000;

impl Message {
    /// The meeting this message is about, when it is a meeting message.
    pub fn meeting(&self) -> Option<Meeting> {
        let class = self.message_class.as_deref()?.to_ascii_lowercase();
        let kind = if class.starts_with("ipm.schedule.meeting.request") {
            MeetingKind::Request
        } else if class.starts_with("ipm.schedule.meeting.canceled") {
            MeetingKind::Cancel
        } else if class.starts_with("ipm.schedule.meeting.resp.pos") {
            MeetingKind::Reply(PartStat::Accepted)
        } else if class.starts_with("ipm.schedule.meeting.resp.neg") {
            MeetingKind::Reply(PartStat::Declined)
        } else if class.starts_with("ipm.schedule.meeting.resp.tent") {
            MeetingKind::Reply(PartStat::Tentative)
        } else if class.starts_with("ipm.appointment") {
            MeetingKind::Publish
        } else {
            return None;
        };
        let p = &self.properties;
        let appt = |id| p.named(&PSETID_APPOINTMENT, id);
        let time = |v: Option<&Value>| v.and_then(Value::as_time);
        let start = time(appt(0x820D))
            .or_else(|| time(p.tag(mapi::PR_START_DATE)))
            .or_else(|| time(p.named(&PSETID_COMMON, 0x8516)))
            .or(self.legacy_start);
        let end = time(appt(0x820E))
            .or_else(|| time(p.tag(mapi::PR_END_DATE)))
            .or_else(|| time(p.named(&PSETID_COMMON, 0x8517)))
            .or(self.legacy_end);
        let goid = p
            .named(&PSETID_MEETING, 0x0003)
            .and_then(Value::as_bytes)
            .or_else(|| p.named(&PSETID_MEETING, 0x0023).and_then(Value::as_bytes));
        let (uid, instance) = goid.and_then(uid_of).unzip();
        let recurrence_id = if instance == Some(true) { time(appt(0x8228)) } else { None };
        let text =
            |v: Option<&Value>| v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let time_zone = appt(0x825E).and_then(Value::as_bytes).and_then(TimeZone::definition).or_else(|| {
            let name = text(appt(0x8234)).unwrap_or_default();
            appt(0x8233).and_then(Value::as_bytes).and_then(|d| TimeZone::from_struct(d, &name))
        });
        let recurring = appt(0x8223).and_then(Value::as_bool).unwrap_or(false) || recurrence_id.is_none();
        let recurrence =
            if recurring { appt(0x8216).and_then(Value::as_bytes).and_then(Recurrence::parse) } else { None };
        let (organizer, attendees) = match kind {
            MeetingKind::Reply(_) => (
                None,
                self.sender
                    .clone()
                    .map(|person| vec![Attendee { person, kind: RecipientKind::To }])
                    .unwrap_or_default(),
            ),
            _ => (
                self.sender.clone(),
                self.recipients.iter().map(|r| Attendee { person: r.person.clone(), kind: r.kind }).collect(),
            ),
        };
        let description = match kind {
            MeetingKind::Reply(_) => None,
            _ => self.body.text.as_deref().map(|t| {
                let t = t.trim();
                match t.char_indices().nth(MAX_DESCRIPTION_CHARS) {
                    Some((cut, _)) => t[..cut].to_owned(),
                    None => t.to_owned(),
                }
            }),
        }
        .filter(|d| !d.is_empty());
        Some(Meeting {
            kind,
            uid,
            recurrence_id,
            sequence: appt(0x8201).and_then(Value::as_i64).unwrap_or(0).clamp(0, i64::from(i32::MAX)),
            start,
            end,
            all_day: appt(0x8215).and_then(Value::as_bool).unwrap_or(false),
            summary: text(p.tag(mapi::PR_CONVERSATION_TOPIC)).or_else(|| self.subject.clone()),
            location: text(appt(0x8208)).or_else(|| text(p.named(&PSETID_MEETING, 0x0002))),
            description,
            organizer,
            attendees,
            busy_status: appt(0x8205).and_then(Value::as_i64),
            time_zone,
            recurrence,
            stamp: self.sent_at.or_else(|| time(p.tag(mapi::PR_MESSAGE_DELIVERY_TIME))),
        })
    }
}

/// The UID MS-OXCICAL (2.1.3.1.1.20.26) derives from a GlobalObjectId, and whether the id
/// names one instance of a series.
fn uid_of(goid: &[u8]) -> Option<(String, bool)> {
    const VCAL: &[u8] = b"vCal-Uid\x01\x00\x00\x00";
    if goid.is_empty() {
        return None;
    }
    let instance = goid.get(16..20).is_some_and(|d| d != [0, 0, 0, 0]);
    if goid.len() >= 40 {
        let size = u32::from_le_bytes([goid[36], goid[37], goid[38], goid[39]]) as usize;
        let data = &goid[40..(40usize.saturating_add(size)).min(goid.len())];
        if let Some(uid) = data.strip_prefix(VCAL) {
            let end = uid.iter().position(|b| *b == 0).unwrap_or(uid.len());
            let uid: String = String::from_utf8_lossy(&uid[..end]).chars().filter(|c| !c.is_control()).collect();
            let uid = uid.trim();
            if !uid.is_empty() && uid.len() <= 1000 {
                return Some((uid.to_owned(), instance));
            }
        }
    }
    let mut clean = goid[..goid.len().min(1000)].to_vec();
    if let Some(date) = clean.get_mut(16..20) {
        date.fill(0);
    }
    let hex: String = clean.iter().map(|b| format!("{b:02X}")).collect();
    Some((hex, instance))
}

/// iCalendar TEXT escaping (RFC 5545 3.3.11).
fn text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let value = value.replace("\r\n", "\n");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' | '\r' => out.push_str("\\n"),
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// A parameter value, always quoted, without what a quoted value may not hold.
fn param(value: &str) -> String {
    let clean: String = value.chars().filter(|c| !c.is_control() && *c != '"').take(200).collect();
    format!("\"{}\"", clean.trim())
}

/// Folds a content line at 75 octets (RFC 5545 3.1).
fn fold(line: &str, out: &mut String) {
    let mut width = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += len;
    }
    out.push_str("\r\n");
}

fn address_line(name: &str, person: &Person, params: &[&str]) -> Option<String> {
    // Addresses from `IcsOptions` come from the surrounding mail's headers, not through the
    // decoder's own check: a line break or a quote in one would start a property of its own.
    let email = crate::internet_address(person.email.as_deref()?)?;
    let mut line = name.to_owned();
    if let Some(cn) = person.name.as_deref().filter(|n| !n.trim().is_empty()) {
        line.push_str(";CN=");
        line.push_str(&param(cn));
    }
    for p in params {
        line.push(';');
        line.push_str(p);
    }
    line.push_str(":mailto:");
    line.push_str(&email);
    Some(line)
}

fn same_address(a: &Person, b: &Person) -> bool {
    match (&a.email, &b.email) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

impl Meeting {
    /// The meeting as an iCalendar object with a METHOD, or nothing when it lacks what one needs
    /// (a UID and a start). Addresses the TNEF leaves out come from `options`.
    pub fn to_ical(&self, options: &IcsOptions) -> Option<String> {
        let uid = self.uid.as_deref()?;
        let start = self.start?;
        let zone = self.time_zone.as_ref();
        let mut lines: Vec<String> = vec![
            "BEGIN:VCALENDAR".into(),
            "PRODID:-//UwUMail//TNEF//EN".into(),
            "VERSION:2.0".into(),
            format!("METHOD:{}", self.kind.method()),
        ];
        // Times in the meeting's zone, with its VTIMEZONE; in UTC without one.
        let timed_zone = zone.filter(|_| !self.all_day);
        if let Some(zone) = timed_zone {
            zone.vtimezone(&mut lines);
        }
        let local_date = |utc: i64| match zone {
            Some(zone) => ical_date(zone.to_local(utc)),
            None => ical_date(utc + 12 * 3600),
        };
        let when = |name: &str, utc: i64| -> String {
            match timed_zone {
                _ if self.all_day => format!("{name};VALUE=DATE:{}", local_date(utc)),
                Some(zone) => format!("{name};TZID={}:{}", zone.name, ical_datetime(zone.to_local(utc), false)),
                None => format!("{name}:{}", ical_datetime(utc, true)),
            }
        };
        lines.push("BEGIN:VEVENT".into());
        lines.push(format!("UID:{}", text(uid)));
        if let Some(rid) = self.recurrence_id {
            lines.push(when("RECURRENCE-ID", rid));
        }
        lines.push(format!("SEQUENCE:{}", self.sequence));
        lines.push(format!("DTSTAMP:{}", ical_datetime(self.stamp.unwrap_or(options.now), true)));
        lines.push(when("DTSTART", start));
        match self.end {
            Some(end) if self.all_day => {
                let (s, e) = (local_date(start), local_date(end));
                if e > s {
                    lines.push(format!("DTEND;VALUE=DATE:{e}"));
                }
            }
            Some(end) if end >= start => lines.push(when("DTEND", end)),
            _ => {}
        }
        if let Some(rule) = self.recurrence.as_ref().filter(|_| self.recurrence_id.is_none()) {
            self.recurrence_lines(rule, start, &mut lines);
        }
        if let Some(summary) = &self.summary {
            lines.push(format!("SUMMARY:{}", text(summary)));
        }
        if let Some(location) = &self.location {
            lines.push(format!("LOCATION:{}", text(location)));
        }
        if let Some(description) = &self.description {
            lines.push(format!("DESCRIPTION:{}", text(description)));
        }
        let with_email = |p: &&Person| p.email.as_deref().and_then(crate::internet_address).is_some();
        match self.kind {
            MeetingKind::Reply(partstat) => {
                let organizer = self.organizer.as_ref().filter(with_email).or(options.to.first())?;
                let attendee =
                    self.attendees.first().map(|a| &a.person).filter(with_email).or(options.from.as_ref())?;
                lines.extend(address_line("ORGANIZER", organizer, &[]));
                lines.extend(address_line("ATTENDEE", attendee, &[&format!("PARTSTAT={}", partstat.ical())]));
            }
            _ => {
                let organizer = self.organizer.as_ref().filter(with_email).or(options.from.as_ref());
                if let Some(organizer) = organizer {
                    lines.extend(address_line("ORGANIZER", organizer, &[]));
                }
                let mut attendees: Vec<Attendee> =
                    self.attendees.iter().filter(|a| with_email(&&a.person)).cloned().collect();
                if attendees.is_empty() {
                    let people = options.to.iter().map(|p| (p, RecipientKind::To));
                    let people = people.chain(options.cc.iter().map(|p| (p, RecipientKind::Cc)));
                    attendees = people.map(|(p, kind)| Attendee { person: p.clone(), kind }).collect();
                }
                let mut seen: Vec<&Person> = Vec::new();
                for attendee in attendees.iter().take(1000) {
                    if organizer.is_some_and(|o| same_address(o, &attendee.person))
                        || seen.iter().any(|p| same_address(p, &attendee.person))
                    {
                        continue;
                    }
                    seen.push(&attendee.person);
                    let role = match attendee.kind {
                        RecipientKind::To => "ROLE=REQ-PARTICIPANT",
                        RecipientKind::Cc => "ROLE=OPT-PARTICIPANT",
                        RecipientKind::Bcc => "ROLE=NON-PARTICIPANT;CUTYPE=RESOURCE",
                    };
                    let mut params = vec![role];
                    if self.kind == MeetingKind::Request {
                        params.extend(["PARTSTAT=NEEDS-ACTION", "RSVP=TRUE"]);
                    }
                    lines.extend(address_line("ATTENDEE", &attendee.person, &params));
                }
                lines.push(
                    match self.kind {
                        MeetingKind::Cancel => "STATUS:CANCELLED",
                        _ => "STATUS:CONFIRMED",
                    }
                    .into(),
                );
            }
        }
        if let Some(busy) = self.busy_status {
            lines.push(format!("TRANSP:{}", if busy == 0 { "TRANSPARENT" } else { "OPAQUE" }));
            let status = match busy {
                0 => "FREE",
                1 => "TENTATIVE",
                3 => "OOF",
                _ => "BUSY",
            };
            lines.push(format!("X-MICROSOFT-CDO-BUSYSTATUS:{status}"));
        }
        lines.push("END:VEVENT".into());
        lines.push("END:VCALENDAR".into());
        let mut out = String::new();
        for line in &lines {
            fold(line, &mut out);
        }
        Some(out)
    }

    fn recurrence_lines(&self, rule: &Recurrence, start: i64, lines: &mut Vec<String>) {
        let zone = self.time_zone.as_ref();
        let local_start = zone.map_or(start, |z| z.to_local(start));
        // Local time of day of each instance: from the pattern, else from the first one.
        let time_of_day = rule.start_offset.map_or(local_start.rem_euclid(86_400), |m| i64::from(m) * 60);
        // Local time as UTC: by the zone, else by the offset the first instance has.
        let to_utc = |local: i64| zone.map_or(local, |z| z.to_utc(local));
        let mut rrule = format!("FREQ={}", rule.frequency);
        if rule.interval > 1 {
            rrule.push_str(&format!(";INTERVAL={}", rule.interval));
        }
        if !rule.weekdays.is_empty() {
            let days: Vec<&str> = rule.weekdays.iter().map(|d| WEEKDAYS[*d as usize % 7]).collect();
            rrule.push_str(&format!(";BYDAY={}", days.join(",")));
        }
        if let Some(position) = rule.set_position {
            rrule.push_str(&format!(";BYSETPOS={position}"));
        }
        if let Some(day) = rule.month_day {
            rrule.push_str(&format!(";BYMONTHDAY={day}"));
        }
        if rule.frequency == "YEARLY" {
            let month = civil_from_days(local_start.div_euclid(86_400)).1;
            rrule.push_str(&format!(";BYMONTH={month}"));
        }
        if let Some(count) = rule.count {
            rrule.push_str(&format!(";COUNT={count}"));
        } else if let Some(until) = rule.until_local {
            if self.all_day {
                rrule.push_str(&format!(";UNTIL={}", ical_date(until)));
            } else {
                rrule.push_str(&format!(";UNTIL={}", ical_datetime(to_utc(until + time_of_day), true)));
            }
        }
        if rule.week_start != 1 {
            rrule.push_str(&format!(";WKST={}", WEEKDAYS[rule.week_start as usize % 7]));
        }
        lines.push(format!("RRULE:{rrule}"));
        for day in &rule.deleted_local {
            let local = day + time_of_day;
            lines.push(match zone {
                _ if self.all_day => format!("EXDATE;VALUE=DATE:{}", ical_date(*day)),
                Some(z) => format!("EXDATE;TZID={}:{}", z.name, ical_datetime(local, false)),
                None => format!("EXDATE:{}", ical_datetime(local, true)),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin() -> TimeZone {
        TimeZone {
            name: "W. Europe Standard Time".into(),
            bias: -60,
            standard_bias: 0,
            daylight_bias: -60,
            standard: Some(Transition { month: 10, week: 5, weekday: 0, hour: 3, minute: 0, second: 0 }),
            daylight: Some(Transition { month: 3, week: 5, weekday: 0, hour: 2, minute: 0, second: 0 }),
        }
    }

    #[test]
    fn zones() {
        let zone = berlin();
        // 2026-07-01 12:00 UTC is 14:00 in Berlin, 2026-12-01 12:00 UTC is 13:00.
        let summer = days_from_civil(2026, 7, 1) * 86_400 + 12 * 3600;
        let winter = days_from_civil(2026, 12, 1) * 86_400 + 12 * 3600;
        assert_eq!(zone.offset_at(summer), 120);
        assert_eq!(zone.offset_at(winter), 60);
        assert_eq!(zone.to_utc(zone.to_local(summer)), summer);
        assert_eq!(zone.to_utc(zone.to_local(winter)), winter);
        // Southern hemisphere: daylight time in January.
        let sydney = TimeZone {
            name: "AUS Eastern Standard Time".into(),
            bias: -600,
            standard_bias: 0,
            daylight_bias: -60,
            standard: Some(Transition { month: 4, week: 1, weekday: 0, hour: 3, minute: 0, second: 0 }),
            daylight: Some(Transition { month: 10, week: 1, weekday: 0, hour: 2, minute: 0, second: 0 }),
        };
        assert_eq!(sydney.offset_at(days_from_civil(2026, 1, 15) * 86_400), 660);
        assert_eq!(sydney.offset_at(days_from_civil(2026, 7, 15) * 86_400), 600);
        let mut out = Vec::new();
        zone.vtimezone(&mut out);
        assert!(out.contains(&"RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU".to_owned()));
        assert!(out.contains(&"DTSTART:16011028T030000".to_owned()), "{out:?}");
        assert!(out.contains(&"TZOFFSETTO:+0200".to_owned()));
    }

    #[test]
    fn uids() {
        let mut goid = vec![0x04, 0, 0, 0, 0x82, 0, 0xE0, 0, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82, 0xE0, 0x08];
        goid.extend([0x07, 0xEA, 0x0A, 0x1B]);
        goid.extend([0u8; 16]);
        let uid = b"vCal-Uid\x01\x00\x00\x00abc@example.com\x00";
        goid.extend((uid.len() as u32).to_le_bytes());
        goid.extend(uid);
        assert_eq!(uid_of(&goid), Some(("abc@example.com".into(), true)));
        let mut plain = goid[..40].to_vec();
        plain[36..40].copy_from_slice(&4u32.to_le_bytes());
        plain.extend([1, 2, 3, 4]);
        let (hex, instance) = uid_of(&plain).unwrap();
        assert!(instance);
        assert!(hex.starts_with("040000008200E00074C5B7101A82E00800000000"), "{hex}");
        assert_eq!(uid_of(&[]), None);
        assert_eq!(uid_of(&[0xAB]), Some(("AB".into(), false)));
    }

    #[test]
    fn escaping_and_folding() {
        assert_eq!(text("a;b,c\\d\r\ne\u{0}"), "a\\;b\\,c\\\\d\\ne");
        assert_eq!(param("Nyu \"the\" Cat\r\n"), "\"Nyu the Cat\"");
        let mut out = String::new();
        fold(&"ä".repeat(60), &mut out);
        assert!(out.split("\r\n").all(|l| l.len() <= 75));
    }
}
