//! The birthdays calendar (migration 0056, docs/birthdays.md): a calendar of its own per account,
//! made from the birthdays and anniversaries in the account's own address books. Every write of a
//! card, over CardDAV and JMAP alike, brings the card's entries in it up to date in the same
//! transaction, so sync tokens, CTags, JMAP states and push move with the card. Nothing else writes
//! into it: CalDAV and JMAP read it like any other calendar and are refused when they write.
//!
//! Each date of a card is one yearly all-day event, `bday-<card>-<n>.ics`. It carries what JMAP
//! needs to name each year's instance with the age (`X-UWUMAIL-BIRTHDAY`), and the reminders the
//! card asks for (`X-UWUMAIL-REMINDER`) as its alarms, which the alert worker and the phones ring.
//!
//! Everything that reads text here counts characters, never bytes: names are anything people type.

use calcard::vcard::{VCard, VCardProperty, VCardValue};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::dav::{
    ChangeLog, DavCollection, DavKind, DavPrecondition, DavWrite, NewDavCollection, delete_entry_unchecked,
    insert_collection, own_collection, put_entry_unchecked,
};
use crate::itip::{Component, Property};
use crate::{Result, Store, StoreError, now};

/// The URL segment of the birthdays calendar, when it is free.
pub const BIRTHDAYS_SLUG: &str = "birthdays";
/// The vCard property of a reminder: `X-UWUMAIL-REMINDER:1 09:00` (a day before, at nine).
pub const REMINDER_PROPERTY: &str = "X-UWUMAIL-REMINDER";
/// The iCalendar property that says whose date an event of the birthdays calendar is.
const EVENT_PROPERTY: &str = "X-UWUMAIL-BIRTHDAY";
const LABEL_PROPERTY: &str = "X-UWUMAIL-BIRTHDAY-LABEL";
/// Reminders one card may have, and how many days before the day one may ring.
pub const MAX_REMINDERS: usize = 5;
pub const MAX_REMINDER_DAYS: u32 = 28;
/// Dates of one card that become events; real cards have one to three.
const MAX_DATES_PER_CARD: usize = 10;
/// The longest name or label an event title takes over, in characters.
const MAX_NAME_CHARS: usize = 200;
/// Events of a series start no earlier than JMAP's `minDateTime` and no later than this.
const FIRST_START_YEAR: i32 = 1900;
const LAST_START_YEAR: i32 = 2100;
/// Where a date without a year starts its series.
const YEARLESS_START_YEAR: i32 = 1970;
/// Apple's contacts write this year for a birthday without one (with `X-APPLE-OMIT-YEAR`).
const APPLE_NO_YEAR: i32 = 1604;
const BACKFILL_MARKER: &str = "birthdays.backfill";
const COLOR: &str = "#F5A623FF";

/// The server's language, for people who left theirs to it; set once when the server starts.
static SERVER_LANGUAGE: std::sync::RwLock<BirthdayLanguage> = std::sync::RwLock::new(BirthdayLanguage::De);

/// The language of the calendar's name and its titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BirthdayLanguage {
    De,
    En,
}

impl BirthdayLanguage {
    /// German for `de`, English for every other language the server speaks.
    pub fn from_code(code: &str) -> BirthdayLanguage {
        let base = code.trim().split(['-', '_']).next().unwrap_or_default();
        if base.eq_ignore_ascii_case("de") { BirthdayLanguage::De } else { BirthdayLanguage::En }
    }

    pub fn code(self) -> &'static str {
        match self {
            BirthdayLanguage::De => "de",
            BirthdayLanguage::En => "en",
        }
    }

    pub fn calendar_name(self) -> &'static str {
        match self {
            BirthdayLanguage::De => "Geburtstage",
            BirthdayLanguage::En => "Birthdays",
        }
    }
}

/// Sets the language of people who chose none (`tone.language`).
pub fn set_server_language(code: &str) {
    if let Ok(mut language) = SERVER_LANGUAGE.write() {
        *language = BirthdayLanguage::from_code(code);
    }
}

fn server_language() -> BirthdayLanguage {
    SERVER_LANGUAGE.read().map(|language| *language).unwrap_or(BirthdayLanguage::De)
}

// ------------------------------------------------------------------------------------------------
// Dates

pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: Option<i32>, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        // Without a year, the 29th may be; with one, only in a leap year.
        2 if year.is_none_or(is_leap_year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// A day of the year, with the year when it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PartialDate {
    pub year: Option<i32>,
    pub month: u32,
    pub day: u32,
}

impl PartialDate {
    /// `None` for a day that does not exist, like 31 April or 29 February 2023.
    pub fn new(year: Option<i32>, month: u32, day: u32) -> Option<PartialDate> {
        let year = year.filter(|year| (1..=9999).contains(year));
        (1..=12)
            .contains(&month)
            .then_some(())
            .filter(|_| day >= 1 && day <= days_in_month(year, month))
            .map(|_| PartialDate { year, month, day })
    }

    /// The day it falls on in `year`: 29 February is the 28th in a year that has no 29th.
    pub fn in_year(self, year: i32) -> (u32, u32) {
        if self.month == 2 && self.day == 29 && !is_leap_year(year) { (2, 28) } else { (self.month, self.day) }
    }
}

/// Reads the dates vCard and its clients write: `1996-04-12`, `19960412`, `--04-12`, `--0412`,
/// also with a time behind it (`1996-04-12T00:00:00Z`), and `0000-04-12` or `1604-04-12` for a date
/// whose year is not known.
pub fn parse_date(text: &str) -> Option<PartialDate> {
    let text = text.trim();
    let date = text.split(['T', 't', ' ']).next().unwrap_or_default();
    let digits: String = date.chars().filter(|c| *c != '-').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<u32> { digits.get(from..to)?.parse().ok() };
    if date.starts_with("--") {
        return match digits.len() {
            4 => PartialDate::new(None, number(0, 2)?, number(2, 4)?),
            _ => None,
        };
    }
    if digits.len() != 8 {
        return None;
    }
    let year = number(0, 4)? as i32;
    let year = (year != 0 && year != APPLE_NO_YEAR).then_some(year);
    PartialDate::new(year, number(4, 6)?, number(6, 8)?)
}

// ------------------------------------------------------------------------------------------------
// Reading cards

/// What kind of date an event is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateKind {
    Birth,
    Wedding,
    /// Another anniversary, named by its label when there is one.
    Other,
}

impl DateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DateKind::Birth => "birth",
            DateKind::Wedding => "wedding",
            DateKind::Other => "other",
        }
    }

    fn parse(value: &str) -> Option<DateKind> {
        match value.to_ascii_lowercase().as_str() {
            "birth" => Some(DateKind::Birth),
            "wedding" => Some(DateKind::Wedding),
            "other" => Some(DateKind::Other),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardDate {
    pub kind: DateKind,
    pub label: Option<String>,
    pub date: PartialDate,
}

/// A reminder of a card: this many days before the day, at this time of day there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reminder {
    pub days_before: u32,
    /// Minutes after midnight.
    pub minute: u32,
}

impl Reminder {
    /// `1 09:00`: a day before, at nine.
    pub fn parse(value: &str) -> Option<Reminder> {
        let (days, time) = value.trim().split_once(' ')?;
        let days_before: u32 = days.trim().parse().ok().filter(|days| *days <= MAX_REMINDER_DAYS)?;
        let (hours, minutes) = time.trim().split_once(':')?;
        let hours: u32 = hours.parse().ok().filter(|h| *h < 24)?;
        let minutes: u32 = minutes.parse().ok().filter(|m| *m < 60)?;
        (hours.to_string().len() <= 2 && minutes.to_string().len() <= 2)
            .then_some(Reminder { days_before, minute: hours * 60 + minutes })
    }

    pub fn format(self) -> String {
        format!("{} {}", self.days_before, self.time())
    }

    /// `09:00`
    pub fn time(self) -> String {
        format!("{:02}:{:02}", self.minute / 60, self.minute % 60)
    }

    /// The alarm trigger relative to the start of the day: `-PT15H` for a day before at nine.
    fn trigger(self) -> String {
        let offset = i64::from(self.minute) - i64::from(self.days_before) * 24 * 60;
        let sign = if offset < 0 { "-" } else { "" };
        let offset = offset.unsigned_abs();
        let (days, hours, minutes) = (offset / (24 * 60), offset / 60 % 24, offset % 60);
        let mut out = format!("{sign}P");
        if days > 0 {
            out.push_str(&format!("{days}D"));
        }
        if hours > 0 || minutes > 0 || days == 0 {
            out.push('T');
            if hours > 0 || minutes == 0 {
                out.push_str(&format!("{hours}H"));
            }
            if minutes > 0 {
                out.push_str(&format!("{minutes}M"));
            }
        }
        out
    }
}

/// The dates of a card that become events, the name they go under and its reminders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardDates {
    pub name: String,
    pub dates: Vec<CardDate>,
    pub reminders: Vec<Reminder>,
}

/// Leaves out control characters, trims, and keeps at most [`MAX_NAME_CHARS`] characters.
fn clean(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn text_of(values: &[VCardValue]) -> String {
    values
        .iter()
        .filter_map(|value| match value {
            VCardValue::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The name of a card, as the address book shows it.
pub fn card_name(card: &VCard) -> String {
    let entries = &card.entries;
    let full = entries.iter().find(|e| e.name == VCardProperty::Fn).map(|e| clean(&text_of(&e.values)));
    if let Some(full) = full.filter(|full| !full.is_empty()) {
        return full;
    }
    if let Some(entry) = entries.iter().find(|e| e.name == VCardProperty::N) {
        // N is surname;given;additional;prefix;suffix.
        let part = |i: usize| match entry.values.get(i) {
            Some(VCardValue::Text(text)) => clean(text),
            Some(VCardValue::Component(parts)) => clean(&parts.join(" ")),
            _ => String::new(),
        };
        let name = clean(&format!("{} {}", part(1), part(0)));
        if !name.is_empty() {
            return name;
        }
    }
    for property in [VCardProperty::Org, VCardProperty::Email] {
        if let Some(entry) = entries.iter().find(|e| e.name == property) {
            let text = match entry.values.first() {
                Some(VCardValue::Component(parts)) => clean(parts.first().map(String::as_str).unwrap_or_default()),
                _ => clean(&text_of(&entry.values)),
            };
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

/// Apple's and Google's label of an `X-ABDATE`: its kind, and its text when it has its own.
fn apple_label(label: &str) -> (DateKind, Option<String>) {
    let label = label.trim();
    if let Some(inner) = label.strip_prefix("_$!<").and_then(|rest| rest.strip_suffix(">!$_")) {
        return match inner.to_ascii_lowercase().as_str() {
            "anniversary" => (DateKind::Wedding, None),
            _ => (DateKind::Other, None),
        };
    }
    let cleaned = clean(label);
    (DateKind::Other, (!cleaned.is_empty()).then_some(cleaned))
}

/// The dates of a vCard, its name and its reminders; `None` for a group, a card that is not one,
/// or one without a name or a date.
pub fn card_dates(content: &str) -> Option<CardDates> {
    // Most cards have no date at all; they are not read.
    let upper = content.to_ascii_uppercase();
    if !upper.contains("BDAY") && !upper.contains("ANNIVERSARY") && !upper.contains("X-ABDATE") {
        return None;
    }
    let lines = raw_lines(content);
    let is_group = lines.iter().any(|line| {
        (line.name == "KIND" || line.name == "X-ADDRESSBOOKSERVER-KIND")
            && line.value.trim().eq_ignore_ascii_case("group")
    });
    if is_group {
        return None;
    }
    let card = VCard::parse(content).ok()?;
    let name = card_name(&card);
    if name.is_empty() {
        return None;
    }
    let mut dates: Vec<CardDate> = Vec::new();
    let mut add = |kind: DateKind, label: Option<String>, date: Option<PartialDate>| {
        if let Some(date) = date
            && dates.len() < MAX_DATES_PER_CARD
            && !dates.iter().any(|known| known.kind == kind && known.date == date && known.label == label)
            && !(kind == DateKind::Birth && dates.iter().any(|known| known.kind == DateKind::Birth))
        {
            dates.push(CardDate { kind, label, date });
        }
    };
    for line in &lines {
        // Only dates: `VALUE=text` holds words like "circa 1800".
        if line.has_param("VALUE=TEXT") {
            continue;
        }
        let date = parse_date(&line.value)
            .map(|date| if line.has_param("X-APPLE-OMIT-YEAR") { PartialDate { year: None, ..date } } else { date });
        match line.name.as_str() {
            "BDAY" => add(DateKind::Birth, None, date),
            "ANNIVERSARY" | "X-ANNIVERSARY" => add(DateKind::Wedding, None, date),
            "X-ABDATE" => {
                let label = line.group.as_deref().and_then(|group| {
                    lines.iter().find(|other| {
                        other.name == "X-ABLABEL"
                            && other.group.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(group))
                    })
                });
                let (kind, label) =
                    label.map(|other| apple_label(&unescape(&other.value))).unwrap_or((DateKind::Other, None));
                add(kind, label, date);
            }
            _ => {}
        }
    }
    if dates.is_empty() {
        return None;
    }
    // A wedding anniversary Apple wrote twice (ANNIVERSARY and X-ABDATE) is one.
    let mut seen: Vec<(DateKind, PartialDate)> = Vec::new();
    dates.retain(|date| {
        if date.label.is_some() {
            return true;
        }
        let key = (date.kind, date.date);
        let new = !seen.contains(&key);
        seen.push(key);
        new
    });
    Some(CardDates { name, dates, reminders: reminders_of(&lines) })
}

fn reminders_of(lines: &[RawLine]) -> Vec<Reminder> {
    let mut reminders: Vec<Reminder> = lines
        .iter()
        .filter(|line| line.name == REMINDER_PROPERTY)
        .filter_map(|line| Reminder::parse(&line.value))
        .collect();
    reminders.sort_unstable();
    reminders.dedup();
    reminders.truncate(MAX_REMINDERS);
    reminders
}

/// The reminders of a card, also of one without dates, as `X-UWUMAIL-REMINDER` has them.
pub fn card_reminders(content: &str) -> Vec<Reminder> {
    if !content.to_ascii_uppercase().contains(REMINDER_PROPERTY) {
        return Vec::new();
    }
    reminders_of(&raw_lines(content))
}

/// One content line of a vCard as written: `group.NAME;params:value`, unfolded.
struct RawLine {
    group: Option<String>,
    /// Upper case.
    name: String,
    /// Upper case, as written.
    params: String,
    value: String,
}

impl RawLine {
    /// Whether a parameter starts like this (`X-APPLE-OMIT-YEAR`, `VALUE=TEXT`), in any case.
    fn has_param(&self, wanted: &str) -> bool {
        self.params.split(';').any(|param| param.trim().starts_with(wanted))
    }
}

/// The content lines of a vCard: enough to read dates and our own properties exactly as written,
/// which calcard reads differently for some (`--04-12`).
fn raw_lines(content: &str) -> Vec<RawLine> {
    let mut unfolded: Vec<String> = Vec::new();
    for raw in content.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match raw.strip_prefix([' ', '\t']) {
            Some(rest) => {
                if let Some(last) = unfolded.last_mut() {
                    last.push_str(rest);
                }
            }
            None if !raw.is_empty() => unfolded.push(raw.to_owned()),
            None => {}
        }
    }
    unfolded
        .into_iter()
        .filter_map(|line| {
            let (head, value) = line.split_once(':')?;
            let (head, params) = head.split_once(';').unwrap_or((head, ""));
            let (group, name) = match head.split_once('.') {
                Some((group, name)) => (Some(group.to_owned()), name),
                None => (None, head),
            };
            Some(RawLine {
                group,
                name: name.trim().to_ascii_uppercase(),
                params: params.to_ascii_uppercase(),
                value: value.to_owned(),
            })
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------
// Titles

/// iCalendar TEXT escaping (RFC 5545, 3.3.11).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// iCalendar and vCard TEXT unescaped.
pub(crate) fn unescape_text(text: &str) -> String {
    unescape(text)
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
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

/// What an event of the birthdays calendar is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BirthdayEvent {
    /// The card's row id (the JMAP ContactCard `k…`).
    pub card_id: i64,
    pub kind: DateKind,
    pub label: Option<String>,
    pub name: String,
    pub year: Option<i32>,
    pub language: BirthdayLanguage,
}

impl BirthdayEvent {
    /// How many years it is in `year`: the age on a birthday. `None` without a year, or before it.
    pub fn years_in(&self, year: i32) -> Option<i32> {
        self.year.and_then(|born| year.checked_sub(born)).filter(|years| *years >= 0)
    }

    /// The title CalDAV sees, the same every year.
    pub fn title(&self) -> String {
        let name = &self.name;
        let de = self.language == BirthdayLanguage::De;
        let what = self.what();
        match (self.kind, self.year) {
            (DateKind::Birth, Some(year)) if de => format!("{name} (*{year})"),
            (DateKind::Birth, Some(year)) => format!("{name} (b. {year})"),
            (DateKind::Birth, None) => name.clone(),
            (_, Some(year)) if de => format!("{what} (seit {year})"),
            (_, Some(year)) => format!("{what} (since {year})"),
            (_, None) => what,
        }
    }

    /// The title of the instance in `year`, with the age or the years.
    pub fn title_in(&self, year: i32) -> String {
        let de = self.language == BirthdayLanguage::De;
        match (self.kind, self.years_in(year).filter(|years| *years > 0)) {
            (_, None) if self.year.is_none() => self.title(),
            (DateKind::Birth, None) => self.name.clone(),
            (DateKind::Birth, Some(age)) => format!("{} ({age})", self.name),
            (_, None) => self.what(),
            (_, Some(1)) if de => format!("{} (1 Jahr)", self.what()),
            (_, Some(years)) if de => format!("{} ({years} Jahre)", self.what()),
            (_, Some(1)) => format!("{} (1 year)", self.what()),
            (_, Some(years)) => format!("{} ({years} years)", self.what()),
        }
    }

    /// `Hochzeitstag von Max Muster`, `Wedding anniversary of Max Muster`.
    fn what(&self) -> String {
        let name = &self.name;
        let de = self.language == BirthdayLanguage::De;
        match (self.kind, &self.label) {
            (DateKind::Birth, _) if de => format!("Geburtstag von {name}"),
            (DateKind::Birth, _) => format!("Birthday of {name}"),
            (DateKind::Wedding, _) if de => format!("Hochzeitstag von {name}"),
            (DateKind::Wedding, _) => format!("Wedding anniversary of {name}"),
            (DateKind::Other, Some(label)) if de => format!("{label} von {name}"),
            (DateKind::Other, Some(label)) => format!("{label} of {name}"),
            (DateKind::Other, None) if de => format!("Jahrestag von {name}"),
            (DateKind::Other, None) => format!("Anniversary of {name}"),
        }
    }

    fn description(&self) -> String {
        let de = self.language == BirthdayLanguage::De;
        let what = self.what();
        let from = if de { "Aus deinen Kontakten." } else { "From your contacts." };
        match (self.kind, self.year) {
            (DateKind::Birth, Some(year)) if de => format!("{what}, geboren {year}. {from}"),
            (DateKind::Birth, Some(year)) => format!("{what}, born {year}. {from}"),
            (DateKind::Wedding, Some(year)) if de => format!("{what}, geheiratet {year}. {from}"),
            (DateKind::Wedding, Some(year)) => format!("{what}, married {year}. {from}"),
            (_, Some(year)) if de => format!("{what}, seit {year}. {from}"),
            (_, Some(year)) => format!("{what}, since {year}. {from}"),
            (_, None) => format!("{what}. {from}"),
        }
    }
}

/// What an event of the birthdays calendar is for, read from its `X-UWUMAIL-BIRTHDAY`; `None` for
/// any other event.
pub fn birthday_event(content: &str) -> Option<BirthdayEvent> {
    if !content.contains(EVENT_PROPERTY) {
        return None;
    }
    let calendar = Component::parse(content)?;
    let event = calendar.main_event()?;
    let property = event.property(EVENT_PROPERTY)?;
    Some(BirthdayEvent {
        card_id: property.param("X-CARD")?.trim().parse().ok()?,
        kind: DateKind::parse(property.param("X-KIND")?)?,
        label: event.value(LABEL_PROPERTY).map(|label| clean(&unescape(label))).filter(|label| !label.is_empty()),
        name: clean(&unescape(&property.value)),
        // Any calendar entry can carry the marker: a year is only taken where one makes sense.
        year: property
            .param("X-YEAR")
            .and_then(|year| year.trim().parse().ok())
            .filter(|year| (1..=9999).contains(year)),
        language: BirthdayLanguage::from_code(property.param("X-LANGUAGE").unwrap_or("de")),
    })
}

// ------------------------------------------------------------------------------------------------
// Events

/// One event of the birthdays calendar as it should be.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Derived {
    name: String,
    uid: String,
    /// Without its DTSTAMP, which is added when it is written.
    body: Component,
}

fn date_value(year: i32, month: u32, day: u32) -> String {
    format!("{year:04}{month:02}{day:02}")
}

fn next_day(year: i32, month: u32, day: u32) -> (i32, u32, u32) {
    if day < days_in_month(Some(year), month) {
        (year, month, day + 1)
    } else if month < 12 {
        (year, month + 1, 1)
    } else {
        (year + 1, 1, 1)
    }
}

fn derived_event(
    card_id: i64,
    suffix: &str,
    event: &BirthdayEvent,
    date: PartialDate,
    reminders: &[Reminder],
) -> Derived {
    let uid = format!("uwumail-birthday-{card_id}-{suffix}");
    let start_year = date.year.map_or(YEARLESS_START_YEAR, |year| year.clamp(FIRST_START_YEAR, LAST_START_YEAR));
    let (month, day) = date.in_year(start_year);
    let (end_year, end_month, end_day) = next_day(start_year, month, day);
    let mut vevent = Component::new("VEVENT");
    let mut push = |name: &str, value: String| vevent.properties.push(Property::new(name, value));
    push("UID", uid.clone());
    let mut start = Property::new("DTSTART", date_value(start_year, month, day));
    start.set_param("VALUE", "DATE");
    let mut end = Property::new("DTEND", date_value(end_year, end_month, end_day));
    end.set_param("VALUE", "DATE");
    vevent.properties.push(start);
    vevent.properties.push(end);
    // 29 February falls on the last day of February: the 28th in the other years.
    let rule = if date.month == 2 && date.day == 29 { "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1" } else { "FREQ=YEARLY" };
    let mut push = |name: &str, value: String| vevent.properties.push(Property::new(name, value));
    push("RRULE", rule.into());
    let title = event.title();
    push("SUMMARY", escape(&title));
    push("DESCRIPTION", escape(&event.description()));
    push("TRANSP", "TRANSPARENT".into());
    push("CLASS", "PUBLIC".into());
    let mut marker = Property::new(EVENT_PROPERTY, escape(&event.name));
    marker.set_param("X-KIND", event.kind.as_str());
    marker.set_param("X-CARD", card_id.to_string());
    marker.set_param("X-LANGUAGE", event.language.code());
    if let Some(year) = event.year {
        marker.set_param("X-YEAR", year.to_string());
    }
    vevent.properties.push(marker);
    if let Some(label) = &event.label {
        vevent.properties.push(Property::new(LABEL_PROPERTY, escape(label)));
    }
    for reminder in reminders {
        let mut alarm = Component::new("VALARM");
        alarm.properties.push(Property::new("ACTION", "DISPLAY"));
        alarm.properties.push(Property::new("DESCRIPTION", escape(&title)));
        alarm.properties.push(Property::new("TRIGGER", reminder.trigger()));
        vevent.components.push(alarm);
    }
    let mut calendar = Component::new("VCALENDAR");
    calendar.properties.push(Property::new("VERSION", "2.0"));
    calendar.properties.push(Property::new("PRODID", "-//UwUMail//Birthdays//EN"));
    calendar.properties.push(Property::new("CALSCALE", "GREGORIAN"));
    calendar.components.push(vevent);
    Derived { name: format!("bday-{card_id}-{suffix}.ics"), uid, body: calendar }
}

fn derived_events(card_id: i64, dates: &CardDates, language: BirthdayLanguage) -> Vec<Derived> {
    let mut others = 0;
    dates
        .dates
        .iter()
        .map(|date| {
            let suffix = match date.kind {
                DateKind::Birth => "b".to_owned(),
                _ => {
                    others += 1;
                    format!("a{others}")
                }
            };
            let event = BirthdayEvent {
                card_id,
                kind: date.kind,
                label: date.label.clone(),
                name: dates.name.clone(),
                year: date.date.year,
                language,
            };
            derived_event(card_id, &suffix, &event, date.date, &dates.reminders)
        })
        .collect()
}

/// The object with a DTSTAMP, as it is stored.
fn stamped(body: &Component, at: i64) -> String {
    let mut calendar = body.clone();
    if let Some(event) = calendar.components.iter_mut().find(|c| c.name == "VEVENT") {
        let stamp = chrono::DateTime::from_timestamp(at, 0).unwrap_or_default().format("%Y%m%dT%H%M%SZ").to_string();
        event.properties.insert(1, Property::new("DTSTAMP", stamp));
    }
    calendar.to_ics()
}

/// A stored object without its DTSTAMP, to compare with what it should be.
fn unstamped(content: &str) -> Option<Component> {
    let mut calendar = Component::parse(content)?;
    for event in calendar.components.iter_mut().filter(|c| c.name == "VEVENT") {
        event.remove("DTSTAMP");
    }
    Some(calendar)
}

fn start_and_end(body: &Component) -> (Option<i64>, Option<i64>) {
    let start = body
        .main_event()
        .and_then(|event| event.value("DTSTART"))
        .and_then(|value| chrono::NaiveDate::parse_from_str(value.trim(), "%Y%m%d").ok())
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|at| at.and_utc().timestamp());
    // A yearly series never ends.
    (start, None)
}

// ------------------------------------------------------------------------------------------------
// The calendar

/// The account's birthdays calendar and the language it is in.
fn calendar_of(conn: &Connection, account_id: i64) -> Result<Option<(DavCollection, BirthdayLanguage)>> {
    let found: Option<(i64, String)> = conn
        .query_row(
            "SELECT collection_id, language FROM birthday_calendars WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((collection_id, language)) = found else { return Ok(None) };
    Ok(Some((own_collection(conn, account_id, collection_id)?, BirthdayLanguage::from_code(&language))))
}

/// The language the person chose, or the server's.
fn language_of(conn: &Connection, account_id: i64) -> Result<BirthdayLanguage> {
    let preferences: Option<String> =
        conn.query_row("SELECT preferences FROM accounts WHERE id = ?1", [account_id], |row| row.get(0)).optional()?;
    let chosen = preferences
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("language").and_then(|v| v.as_str()).map(str::to_owned))
        .filter(|code| code != "system" && !code.is_empty());
    Ok(chosen.map_or_else(server_language, |code| BirthdayLanguage::from_code(&code)))
}

/// Makes the birthdays calendar. It never becomes the default calendar, so the account's first
/// calendar of its own is made later as usual; its time zone is that of the default calendar, so
/// reminders ring at the time of day the person means.
fn create(tx: &Transaction<'_>, log: &mut ChangeLog, account_id: i64) -> Result<(DavCollection, BirthdayLanguage)> {
    let language = language_of(tx, account_id)?;
    let taken: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM dav_collections WHERE account_id = ?1 AND kind = 'calendar' AND slug = ?2)",
        params![account_id, BIRTHDAYS_SLUG],
        |row| row.get(0),
    )?;
    let slug = if taken {
        format!("{BIRTHDAYS_SLUG}-{}", hex::encode(crate::random_bytes::<4>()))
    } else {
        BIRTHDAYS_SLUG.to_owned()
    };
    let new = NewDavCollection {
        slug,
        display_name: language.calendar_name().into(),
        color: Some(COLOR.into()),
        components: vec!["VEVENT".into()],
        ..Default::default()
    };
    let id = insert_collection(tx, log, account_id, DavKind::Calendar, &new)?;
    tx.execute("UPDATE dav_collections SET is_default = 0 WHERE id = ?1", [id])?;
    tx.execute(
        "UPDATE dav_collections SET timezone = (SELECT timezone FROM dav_collections
             WHERE account_id = ?1 AND kind = 'calendar' AND is_default)
         WHERE id = ?2",
        params![account_id, id],
    )?;
    tx.execute(
        "INSERT INTO birthday_calendars (account_id, collection_id, language) VALUES (?1, ?2, ?3)",
        params![account_id, id, language.code()],
    )?;
    Ok((own_collection(tx, account_id, id)?, language))
}

/// Brings the events of the card `card_id` in `calendar` to what they should be.
fn sync_card(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    calendar: &DavCollection,
    card_id: i64,
    wanted: Vec<Derived>,
) -> Result<()> {
    let prefix = format!("bday-{card_id}-");
    // Every name that starts with the prefix sorts between it and the prefix with '.' for '-'.
    let upper = format!("bday-{card_id}.");
    let mut stmt = tx.prepare_cached(
        "SELECT name, content FROM dav_resources WHERE collection_id = ?1 AND name >= ?2 AND name < ?3",
    )?;
    let existing: Vec<(String, String)> = stmt
        .query_map(params![calendar.id, prefix, upper], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    // The calendar follows the cards: nobody who sees it is told about each change as if a person
    // had made it.
    let author = log.author.take();
    let result = write_card_events(tx, log, calendar, &existing, &wanted);
    log.author = author;
    result
}

fn write_card_events(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    calendar: &DavCollection,
    existing: &[(String, String)],
    wanted: &[Derived],
) -> Result<()> {
    let at = now();
    for derived in wanted {
        let same = existing
            .iter()
            .find(|(name, _)| *name == derived.name)
            .is_some_and(|(_, content)| unstamped(content).as_ref() == Some(&derived.body));
        if same {
            continue;
        }
        let (starts_at, ends_at) = start_and_end(&derived.body);
        let write = DavWrite {
            name: derived.name.clone(),
            content: stamped(&derived.body, at),
            uid: derived.uid.clone(),
            component: "VEVENT".into(),
            starts_at,
            ends_at,
        };
        put_entry_unchecked(tx, log, calendar, &write, &DavPrecondition::default())?;
    }
    for (name, _) in existing {
        if !wanted.iter().any(|derived| derived.name == *name) {
            delete_entry_unchecked(tx, log, calendar, name, None)?;
        }
    }
    Ok(())
}

/// Brings the events of one card up to date after it was written into `collection` (`None`: it
/// went). Only the owner's own address books count; a card of one shared with the account is in
/// its owner's birthdays calendar. The calendar is made with the first date there is.
pub(crate) fn index_card(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    collection: &DavCollection,
    card_id: i64,
    content: Option<&str>,
) -> Result<()> {
    if collection.kind != DavKind::Addressbook {
        return Ok(());
    }
    let dates = content.and_then(card_dates);
    let (calendar, language) = match calendar_of(tx, collection.account_id)? {
        Some(found) => found,
        None if dates.is_some() => match create(tx, log, collection.account_id) {
            Ok(made) => made,
            // With a hundred calendars and address books already, there is no birthdays calendar;
            // the card is stored all the same.
            Err(StoreError::Rule { .. }) => return Ok(()),
            Err(err) => return Err(err),
        },
        None => return Ok(()),
    };
    let wanted = dates.map(|dates| derived_events(card_id, &dates, language)).unwrap_or_default();
    match sync_card(tx, log, &calendar, card_id, wanted) {
        // A full calendar (50 000 entries) leaves the card's dates out, not the card.
        Err(StoreError::QuotaExceeded) => Ok(()),
        other => other,
    }
}

/// Takes the events of every card of an address book that goes away.
pub(crate) fn drop_address_book(tx: &Transaction<'_>, log: &mut ChangeLog, collection: &DavCollection) -> Result<()> {
    if collection.kind != DavKind::Addressbook {
        return Ok(());
    }
    let Some((calendar, _)) = calendar_of(tx, collection.account_id)? else { return Ok(()) };
    let mut stmt = tx.prepare("SELECT id FROM dav_resources WHERE collection_id = ?1")?;
    let cards: Vec<i64> = stmt.query_map([collection.id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for card in cards {
        sync_card(tx, log, &calendar, card, Vec::new())?;
    }
    Ok(())
}

/// Writes the whole calendar anew from the cards of the account's own address books: after it was
/// made, and when the person's language changed. Returns the calendar, `None` when the account has
/// none and `create` is false or there is no date to put into one.
fn rebuild(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    account_id: i64,
    create_it: bool,
) -> Result<Option<DavCollection>> {
    let mut stmt = tx.prepare(
        "SELECT r.id, r.content FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
         WHERE c.account_id = ?1 AND c.kind = 'addressbook' AND r.component = 'VCARD'
           AND (r.content LIKE '%BDAY%' OR r.content LIKE '%ANNIVERSARY%' OR r.content LIKE '%X-ABDATE%')
         ORDER BY r.id",
    )?;
    let cards: Vec<(i64, CardDates)> = stmt
        .query_map([account_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
        .filter_map(|row| row.ok())
        .filter_map(|(id, content)| card_dates(&content).map(|dates| (id, dates)))
        .collect();
    drop(stmt);
    let (calendar, language) = match calendar_of(tx, account_id)? {
        Some(found) => found,
        None if create_it && !cards.is_empty() => create(tx, log, account_id)?,
        None => return Ok(None),
    };
    let mut stmt = tx.prepare("SELECT name FROM dav_resources WHERE collection_id = ?1")?;
    let names: Vec<String> = stmt.query_map([calendar.id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let wanted_cards: std::collections::HashSet<i64> = cards.iter().map(|(card, _)| *card).collect();
    // Events of cards that are gone, or have no date any more, go first: they make room.
    let mut gone = std::collections::HashSet::new();
    for name in names {
        let card = name.strip_prefix("bday-").and_then(|rest| rest.split('-').next()).and_then(|id| id.parse().ok());
        match card {
            Some(card) if wanted_cards.contains(&card) => {}
            Some(card) => {
                if gone.insert(card) {
                    sync_card(tx, log, &calendar, card, Vec::new())?;
                }
            }
            None => {
                delete_entry_unchecked(tx, log, &calendar, &name, None)?;
            }
        }
    }
    for (card, dates) in &cards {
        match sync_card(tx, log, &calendar, *card, derived_events(*card, dates, language)) {
            // A full calendar (50 000 entries) leaves the card's dates out, as `index_card` does:
            // failing here would undo the whole rebuild, and the next listing would start it again.
            Err(StoreError::QuotaExceeded) => {}
            other => other?,
        }
    }
    Ok(Some(calendar))
}

/// When the person chose another language since the calendar was written, writes it anew in the
/// new one, and renames it unless they named it themselves.
pub(crate) fn follow_language(tx: &Transaction<'_>, log: &mut ChangeLog, account_id: i64) -> Result<()> {
    let Some((calendar, language)) = calendar_of(tx, account_id)? else { return Ok(()) };
    let wanted = language_of(tx, account_id)?;
    if wanted == language {
        return Ok(());
    }
    tx.execute(
        "UPDATE birthday_calendars SET language = ?1 WHERE account_id = ?2",
        params![wanted.code(), account_id],
    )?;
    if calendar.display_name == language.calendar_name() {
        tx.execute(
            "UPDATE dav_collections SET display_name = ?1, change = change + 1 WHERE id = ?2",
            params![wanted.calendar_name(), calendar.id],
        )?;
        log.collection(tx, &calendar, "updated")?;
    }
    rebuild(tx, log, account_id, false)?;
    Ok(())
}

/// Refuses deleting the birthdays calendar: it can be hidden, and follows the address books.
pub(crate) fn check_deletable(collection: &DavCollection) -> Result<()> {
    if collection.birthdays {
        return Err(StoreError::Rule {
            code: "forbidden",
            message: "the birthdays calendar comes from your contacts; hide it instead".into(),
        });
    }
    Ok(())
}

impl Store {
    /// The account's birthdays calendar, when it has one.
    pub async fn birthday_calendar(&self, account_id: i64) -> Result<Option<DavCollection>> {
        self.read(move |conn| Ok(calendar_of(conn, account_id)?.map(|(calendar, _)| calendar))).await
    }

    /// Makes the account's birthdays calendar when its cards have a date and it has none yet, and
    /// writes it anew from the cards otherwise. Returns it, `None` without dates.
    pub async fn rebuild_birthday_calendar(&self, account_id: i64) -> Result<Option<DavCollection>> {
        let (calendar, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let calendar = rebuild(tx, &mut log, account_id, true)?;
                Ok((calendar, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(calendar)
    }

    /// Makes the birthdays calendars of the people who had dates in their cards before the
    /// calendar existed, once after migration 0056. One account at a time, so nobody waits long.
    pub async fn backfill_birthday_calendars(&self) -> Result<usize> {
        let accounts: Option<Vec<i64>> = self
            .read(|conn| {
                if crate::db::get_setting(conn, BACKFILL_MARKER)?.is_none() {
                    return Ok(None);
                }
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT c.account_id FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                     WHERE c.kind = 'addressbook' AND r.component = 'VCARD'
                       AND (r.content LIKE '%BDAY%' OR r.content LIKE '%ANNIVERSARY%' OR r.content LIKE '%X-ABDATE%')
                       AND c.account_id NOT IN (SELECT account_id FROM birthday_calendars)",
                )?;
                let rows = stmt.query_map([], |row| row.get(0))?;
                Ok(Some(rows.collect::<rusqlite::Result<_>>()?))
            })
            .await?;
        let Some(accounts) = accounts else { return Ok(0) };
        let mut made = 0;
        for account in accounts {
            match self.rebuild_birthday_calendar(account).await {
                Ok(Some(_)) => made += 1,
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, account, "making a birthdays calendar failed"),
            }
        }
        self.write(|tx| crate::db::delete_setting(tx, BACKFILL_MARKER)).await?;
        if made > 0 {
            tracing::info!(accounts = made, "made the birthdays calendars of existing contacts");
        }
        Ok(made)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:max-1\r\nFN:Max Müller\r\nN:Müller;Max;;;\r\n\
BDAY:1996-04-12\r\nANNIVERSARY:20210612\r\nX-UWUMAIL-REMINDER:1 09:00\r\nX-UWUMAIL-REMINDER:0 08:30\r\nEND:VCARD\r\n";

    #[test]
    fn dates_in_every_spelling() {
        let date = |y, m, d| PartialDate::new(y, m, d);
        assert_eq!(parse_date("1996-04-12"), date(Some(1996), 4, 12));
        assert_eq!(parse_date("19960412"), date(Some(1996), 4, 12));
        assert_eq!(parse_date("1996-04-12T00:00:00Z"), date(Some(1996), 4, 12));
        assert_eq!(parse_date("--04-12"), date(None, 4, 12));
        assert_eq!(parse_date("--0412"), date(None, 4, 12));
        assert_eq!(parse_date("1604-04-12"), date(None, 4, 12), "Apple's year for none");
        assert_eq!(parse_date("0000-04-12"), date(None, 4, 12));
        assert_eq!(parse_date("--02-29"), date(None, 2, 29));
        assert_eq!(parse_date("2000-02-29"), date(Some(2000), 2, 29));
        assert_eq!(parse_date("2023-02-29"), None, "no such day");
        assert_eq!(parse_date("1996-13-01"), None);
        assert_eq!(parse_date("circa 1800"), None);
        assert_eq!(parse_date("１９９６-04-12"), None, "digits of other scripts are not dates");
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn leap_days_fall_on_the_28th_otherwise() {
        let leap = PartialDate::new(Some(2000), 2, 29).unwrap();
        assert_eq!(leap.in_year(2024), (2, 29));
        assert_eq!(leap.in_year(2025), (2, 28));
        assert_eq!(leap.in_year(2100), (2, 28));
        assert_eq!(PartialDate::new(None, 4, 12).unwrap().in_year(2025), (4, 12));
    }

    #[test]
    fn reminders_read_and_write() {
        let reminder = Reminder::parse("1 09:00").unwrap();
        assert_eq!(reminder, Reminder { days_before: 1, minute: 540 });
        assert_eq!(reminder.format(), "1 09:00");
        assert_eq!(reminder.trigger(), "-PT15H");
        assert_eq!(Reminder::parse("0 09:00").unwrap().trigger(), "PT9H");
        assert_eq!(Reminder::parse("0 00:00").unwrap().trigger(), "PT0H");
        assert_eq!(Reminder::parse("7 09:30").unwrap().trigger(), "-P6DT14H30M");
        assert_eq!(Reminder::parse("1 00:00").unwrap().trigger(), "-P1D");
        assert!(Reminder::parse("29 09:00").is_none(), "four weeks at most");
        assert!(Reminder::parse("1 24:00").is_none());
        assert!(Reminder::parse("1 9").is_none());
        assert!(Reminder::parse("-1 09:00").is_none());
    }

    #[test]
    fn cards_give_their_dates_name_and_reminders() {
        let dates = card_dates(CARD).unwrap();
        assert_eq!(dates.name, "Max Müller");
        assert_eq!(
            dates.dates,
            vec![
                CardDate { kind: DateKind::Birth, label: None, date: PartialDate::new(Some(1996), 4, 12).unwrap() },
                CardDate { kind: DateKind::Wedding, label: None, date: PartialDate::new(Some(2021), 6, 12).unwrap() },
            ]
        );
        assert_eq!(
            dates.reminders,
            vec![Reminder { days_before: 0, minute: 510 }, Reminder { days_before: 1, minute: 540 }]
        );

        let apple = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Katze;Nyu;;;\r\nFN:Nyu Katze\r\n\
BDAY;X-APPLE-OMIT-YEAR=1604:1604-03-15\r\nitem1.X-ABDATE;type=pref:2010-06-01\r\n\
item1.X-ABLabel:_$!<Anniversary>!$_\r\nitem2.X-ABDATE:2015-09-09\r\nitem2.X-ABLabel:Kennenlerntag\r\n\
UID:nyu\r\nEND:VCARD\r\n";
        let dates = card_dates(apple).unwrap();
        assert_eq!(dates.dates[0].date, PartialDate::new(None, 3, 15).unwrap());
        assert_eq!(dates.dates[1].kind, DateKind::Wedding);
        assert_eq!(dates.dates[2].label.as_deref(), Some("Kennenlerntag"));

        let group = "BEGIN:VCARD\r\nVERSION:4.0\r\nKIND:group\r\nFN:Familie\r\nBDAY:--0101\r\nUID:g\r\nEND:VCARD\r\n";
        assert!(card_dates(group).is_none());
        let none = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Leni\r\nUID:l\r\nEND:VCARD\r\n";
        assert!(card_dates(none).is_none());
        let nameless = "BEGIN:VCARD\r\nVERSION:3.0\r\nBDAY:--0101\r\nUID:x\r\nEND:VCARD\r\n";
        assert!(card_dates(nameless).is_none());
        let org =
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:\r\nORG:Katzen GmbH;\r\nANNIVERSARY:2001-01-01\r\nUID:o\r\nEND:VCARD\r\n";
        assert_eq!(card_dates(org).unwrap().name, "Katzen GmbH");
    }

    #[test]
    fn titles_say_the_age_of_each_year() {
        let event = BirthdayEvent {
            card_id: 1,
            kind: DateKind::Birth,
            label: None,
            name: "Max Müller".into(),
            year: Some(1996),
            language: BirthdayLanguage::De,
        };
        assert_eq!(event.title(), "Max Müller (*1996)");
        assert_eq!(event.title_in(2026), "Max Müller (30)");
        assert_eq!(event.title_in(1996), "Max Müller");
        let english = BirthdayEvent { language: BirthdayLanguage::En, ..event.clone() };
        assert_eq!(english.title(), "Max Müller (b. 1996)");
        let yearless = BirthdayEvent { year: None, ..event.clone() };
        assert_eq!(yearless.title_in(2026), "Max Müller");
        let wedding = BirthdayEvent { kind: DateKind::Wedding, year: Some(2021), ..event.clone() };
        assert_eq!(wedding.title(), "Hochzeitstag von Max Müller (seit 2021)");
        assert_eq!(wedding.title_in(2026), "Hochzeitstag von Max Müller (5 Jahre)");
        assert_eq!(wedding.title_in(2022), "Hochzeitstag von Max Müller (1 Jahr)");
        let english = BirthdayEvent { language: BirthdayLanguage::En, ..wedding.clone() };
        assert_eq!(english.title_in(2026), "Wedding anniversary of Max Müller (5 years)");
        let other = BirthdayEvent { kind: DateKind::Other, label: Some("Kennenlerntag".into()), ..wedding };
        assert_eq!(other.title_in(2026), "Kennenlerntag von Max Müller (5 Jahre)");
        let future = BirthdayEvent { year: Some(2400), ..event };
        assert_eq!(future.title_in(2100), "Max Müller", "no negative ages");
    }

    #[test]
    fn events_are_yearly_all_day_and_read_back() {
        let dates = card_dates(CARD).unwrap();
        let events = derived_events(7, &dates, BirthdayLanguage::De);
        assert_eq!(events.len(), 2);
        let text = stamped(&events[0].body, 1_790_000_000);
        assert!(text.contains("DTSTART;VALUE=DATE:19960412\r\n"), "{text}");
        assert!(text.contains("DTEND;VALUE=DATE:19960413\r\n"), "{text}");
        assert!(text.contains("RRULE:FREQ=YEARLY\r\n"), "{text}");
        assert!(text.contains("SUMMARY:Max Müller (*1996)\r\n"), "{text}");
        assert!(text.contains("TRIGGER:-PT15H\r\n") && text.contains("TRIGGER:PT8H30M\r\n"), "{text}");
        assert_eq!(crate::ical::check_calendar(&text, &[]).err(), None, "{text}");
        let read = birthday_event(&text).unwrap();
        assert_eq!((read.card_id, read.kind, read.year), (7, DateKind::Birth, Some(1996)));
        assert_eq!(read.name, "Max Müller");
        assert_eq!(unstamped(&text).unwrap(), events[0].body, "the stamp alone is no change");

        let leap = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Lea\r\nBDAY:--0229\r\nUID:lea\r\nEND:VCARD\r\n";
        let events = derived_events(8, &card_dates(leap).unwrap(), BirthdayLanguage::En);
        let text = stamped(&events[0].body, 0);
        assert!(text.contains("DTSTART;VALUE=DATE:19700228\r\n"), "{text}");
        assert!(text.contains("RRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1\r\n"), "{text}");
        assert!(crate::ical::check_calendar(&text, &[]).is_ok());

        let odd = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\\, die \"Erste\"\\; ok\r\nBDAY:1815-12-10\r\nUID:ada\r\nEND:VCARD\r\n";
        let events = derived_events(9, &card_dates(odd).unwrap(), BirthdayLanguage::En);
        let text = stamped(&events[0].body, 0);
        assert!(text.contains("DTSTART;VALUE=DATE:19001210\r\n"), "old years start in 1900: {text}");
        let read = birthday_event(&text).unwrap();
        assert_eq!(read.name, "Ada, die \"Erste\"; ok");
        assert_eq!(read.year, Some(1815));
        assert!(crate::ical::check_calendar(&text, &[]).is_ok());
    }

    mod stored {
        use super::super::*;
        use crate::test_support::store;
        use crate::{ContactCardWrite, DavCollectionUpdate, NewAccount, Role};

        async fn account(store: &Store, address: &str) -> i64 {
            store.create_domain("example.org").await.ok();
            store
                .create_account(NewAccount {
                    address: address.into(),
                    display_name: String::new(),
                    password: None,
                    role: Role::User,
                    quota_bytes: 0,
                    protocols: None,
                })
                .await
                .unwrap()
                .id
        }

        fn book() -> NewDavCollection {
            NewDavCollection { slug: "contacts".into(), display_name: "Kontakte".into(), ..Default::default() }
        }

        fn vcard(uid: &str, name: &str, extra: &str) -> String {
            format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\n{extra}END:VCARD\r\n")
        }

        fn put(uid: &str, content: String) -> DavWrite {
            DavWrite {
                name: format!("{uid}.vcf"),
                content,
                uid: uid.into(),
                component: "VCARD".into(),
                starts_at: None,
                ends_at: None,
            }
        }

        async fn events(store: &Store, account: i64) -> Vec<(String, String)> {
            let Some(calendar) = store.birthday_calendar(account).await.unwrap() else { return Vec::new() };
            let mut list: Vec<(String, String)> = store
                .dav_resource_contents(account, calendar.id, None)
                .await
                .unwrap()
                .into_iter()
                .map(|r| (r.info.name, r.content))
                .collect();
            list.sort();
            list
        }

        #[tokio::test]
        async fn the_calendar_follows_the_cards() {
            let (store, _dir) = store().await;
            let mini = account(&store, "mini@example.org").await;
            let contacts = store.dav_collections(mini, DavKind::Addressbook, book()).await.unwrap()[0].clone();
            // Nothing without a date.
            store
                .dav_put(mini, contacts.id, put("leni", vcard("leni", "Leni", "")), DavPrecondition::default())
                .await
                .unwrap();
            assert!(store.birthday_calendar(mini).await.unwrap().is_none());

            let start = store.account_modseq(mini).await.unwrap();
            let max = vcard("max", "Max Müller", "BDAY:1996-04-12\r\n");
            store.dav_put(mini, contacts.id, put("max", max), DavPrecondition::default()).await.unwrap();
            let calendar = store.birthday_calendar(mini).await.unwrap().expect("made with the first date");
            assert!(calendar.birthdays && calendar.filled() && !calendar.is_default);
            assert_eq!(calendar.display_name, "Geburtstage");
            assert_eq!(calendar.slug, BIRTHDAYS_SLUG);
            let card = store.contact_cards(mini, None).await.unwrap().into_iter().find(|c| c.uid == "max").unwrap();
            let list = events(&store, mini).await;
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].0, format!("bday-{}-b.ics", card.id));
            assert!(list[0].1.contains("SUMMARY:Max Müller (*1996)"));
            assert_eq!(store.changes(mini, "Calendar", start, 0).await.unwrap().created, vec![calendar.id]);
            let event_id = store.calendar_events(mini, None).await.unwrap()[0].id;
            assert_eq!(store.changes(mini, "CalendarEvent", start, 0).await.unwrap().created, vec![event_id]);

            // The account's own first calendar is still made, and stays the default.
            let calendars = store
                .dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("Kalender"))
                .await
                .unwrap();
            assert_eq!(calendars.len(), 2);
            assert!(calendars.iter().any(|c| c.is_default && !c.birthdays));

            // The same card again changes nothing: no new sync token for the calendar.
            let change = store.birthday_calendar(mini).await.unwrap().unwrap().change;
            let again = vcard("max", "Max Müller", "BDAY:1996-04-12\r\nNOTE:neu\r\n");
            store.dav_put(mini, contacts.id, put("max", again), DavPrecondition::default()).await.unwrap();
            assert_eq!(store.birthday_calendar(mini).await.unwrap().unwrap().change, change);

            // A new date and a reminder change the event; a wedding comes along.
            let after = store.account_modseq(mini).await.unwrap();
            let changed =
                vcard("max", "Max Müller", "BDAY:--04-13\r\nANNIVERSARY:2021-06-12\r\nX-UWUMAIL-REMINDER:1 09:00\r\n");
            let written = ContactCardWrite {
                id: Some(card.id),
                address_book_id: contacts.id,
                content: changed,
                uid: "max".into(),
                if_etag: None,
            };
            store.put_contact_card(mini, written).await.unwrap();
            let list = events(&store, mini).await;
            assert_eq!(list.len(), 2);
            assert!(list[1].1.contains("SUMMARY:Max Müller\r\n") && list[1].1.contains("DTSTART;VALUE=DATE:19700413"));
            assert!(list[1].1.contains("TRIGGER:-PT15H"));
            assert!(list[0].1.contains("Hochzeitstag von Max Müller (seit 2021)"));
            let changes = store.changes(mini, "CalendarEvent", after, 0).await.unwrap();
            assert_eq!((changes.created.len(), changes.updated, changes.destroyed.len()), (1, vec![event_id], 0));

            // CalDAV and JMAP cannot write into it.
            let refused = store.dav_delete(mini, calendar.id, &list[0].0, None).await;
            assert!(matches!(refused, Err(StoreError::Rule { code: "readOnly", .. })));
            assert!(matches!(
                store.destroy_calendar_event(mini, event_id, None).await,
                Err(StoreError::Rule { code: "readOnly", .. })
            ));
            assert!(matches!(store.dav_delete_collection(mini, calendar.id).await, Err(StoreError::Rule { .. })));
            assert!(matches!(store.destroy_calendar(mini, calendar.id, true).await, Err(StoreError::Rule { .. })));
            assert!(matches!(store.set_default_calendar(mini, calendar.id).await, Err(StoreError::Rule { .. })));
            // Its name and colour are the person's.
            let update = DavCollectionUpdate { color: Some(Some("#00AA00FF".into())), ..Default::default() };
            store.dav_update_collection(mini, calendar.id, update).await.unwrap();
            // Only the calendar of one's own cannot go: the birthdays one does not count.
            let own = calendars.iter().find(|c| !c.birthdays).unwrap();
            assert!(matches!(store.destroy_calendar(mini, own.id, true).await, Err(StoreError::Rule { .. })));

            // The card goes, and its events with it.
            let after = store.account_modseq(mini).await.unwrap();
            store.destroy_contact_card(mini, card.id, None).await.unwrap();
            assert!(events(&store, mini).await.is_empty());
            assert_eq!(store.changes(mini, "CalendarEvent", after, 0).await.unwrap().destroyed.len(), 2);
            assert!(store.birthday_calendar(mini).await.unwrap().is_some(), "the calendar stays");
        }

        #[tokio::test]
        async fn address_books_moves_and_languages() {
            let (store, _dir) = store().await;
            let mini = account(&store, "mini@example.org").await;
            let leni = account(&store, "leni@example.org").await;
            let contacts = store.dav_collections(mini, DavKind::Addressbook, book()).await.unwrap()[0].clone();
            let family = store
                .create_address_book(
                    mini,
                    NewDavCollection { slug: "family".into(), display_name: "Familie".into(), ..Default::default() },
                    DavCollectionUpdate::default(),
                )
                .await
                .unwrap();
            let oma = vcard("oma", "Oma Änne", "BDAY:1940-02-29\r\n");
            store.dav_put(mini, family.id, put("oma", oma), DavPrecondition::default()).await.unwrap();
            let max = vcard("max", "Max", "BDAY:2000-01-01\r\n");
            store.dav_put(mini, contacts.id, put("max", max), DavPrecondition::default()).await.unwrap();
            assert_eq!(events(&store, mini).await.len(), 2);
            let oma_event = events(&store, mini).await.into_iter().find(|(_, c)| c.contains("Oma")).unwrap().1;
            assert!(oma_event.contains("RRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1"), "{oma_event}");
            assert!(oma_event.contains("DTSTART;VALUE=DATE:19400229"), "{oma_event}");

            // A card moved into another of one's address books keeps its event.
            let oma_card = store.contact_cards(mini, None).await.unwrap().into_iter().find(|c| c.uid == "oma").unwrap();
            let moved = ContactCardWrite {
                id: Some(oma_card.id),
                address_book_id: contacts.id,
                content: oma_card.content.clone(),
                uid: "oma".into(),
                if_etag: None,
            };
            store.put_contact_card(mini, moved).await.unwrap();
            assert_eq!(events(&store, mini).await.len(), 2);

            // An address book that goes takes its cards' events along.
            store
                .dav_put(
                    mini,
                    family.id,
                    put("opa", vcard("opa", "Opa", "BDAY:1938-05-05\r\n")),
                    DavPrecondition::default(),
                )
                .await
                .unwrap();
            assert_eq!(events(&store, mini).await.len(), 3);
            store.destroy_address_book(mini, family.id, true).await.unwrap();
            assert_eq!(events(&store, mini).await.len(), 2);

            // Another language writes it anew; a name the person gave stays.
            let mut english = serde_json::Map::new();
            english.insert("language".into(), serde_json::json!("en"));
            store.update_preferences(mini, english).await.unwrap();
            let calendars = store
                .dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("Kalender"))
                .await
                .unwrap();
            let birthdays = calendars.iter().find(|c| c.birthdays).unwrap();
            assert_eq!(birthdays.display_name, "Birthdays");
            assert!(events(&store, mini).await.iter().any(|(_, c)| c.contains("SUMMARY:Oma Änne (b. 1940)")));

            // Leni's cards are in her own calendar, not in Mini's.
            let hers = store.dav_collections(leni, DavKind::Addressbook, book()).await.unwrap()[0].clone();
            store
                .dav_put(leni, hers.id, put("nyu", vcard("nyu", "Nyu", "BDAY:--12-24\r\n")), DavPrecondition::default())
                .await
                .unwrap();
            assert_eq!(events(&store, leni).await.len(), 1);
            assert_eq!(events(&store, mini).await.len(), 2);
        }

        /// BDAY-1 of the 0.18.0 audit: a full birthdays calendar did not stop a rebuild from
        /// failing, and a failed rebuild was started again by every calendar listing.
        #[tokio::test]
        async fn a_full_calendar_does_not_break_the_listing() {
            let (store, _dir) = store().await;
            let mini = account(&store, "mini@example.org").await;
            let contacts = store.dav_collections(mini, DavKind::Addressbook, book()).await.unwrap()[0].clone();
            // Two dates on each of 200 cards: more than a calendar holds (300 in the store's tests).
            for n in 0..200 {
                let uid = format!("c{n}");
                let card = vcard(&uid, &format!("Card {n}"), "BDAY:2000-01-01\r\nANNIVERSARY:2020-06-01\r\n");
                store.dav_put(mini, contacts.id, put(&uid, card), DavPrecondition::default()).await.unwrap();
            }
            assert_eq!(events(&store, mini).await.len() as i64, crate::dav::DAV_RESOURCES_PER_COLLECTION);

            let mut english = serde_json::Map::new();
            english.insert("language".into(), serde_json::json!("en"));
            store.update_preferences(mini, english).await.unwrap();
            let listed =
                store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("Kalender")).await;
            let birthdays = listed.expect("the listing works").into_iter().find(|c| c.birthdays).unwrap();
            assert_eq!(birthdays.display_name, "Birthdays", "the new language stuck");
        }

        #[tokio::test]
        async fn existing_cards_are_backfilled_once() {
            let (store, _dir) = store().await;
            let mini = account(&store, "mini@example.org").await;
            let contacts = store.dav_collections(mini, DavKind::Addressbook, book()).await.unwrap()[0].clone();
            store
                .dav_put(
                    mini,
                    contacts.id,
                    put("max", vcard("max", "Max", "BDAY:2000-01-01\r\n")),
                    DavPrecondition::default(),
                )
                .await
                .unwrap();
            // As if the card had been there before migration 0056.
            let calendar = store.birthday_calendar(mini).await.unwrap().unwrap();
            store
                .write(move |tx| {
                    tx.execute("DELETE FROM birthday_calendars", [])?;
                    tx.execute("DELETE FROM dav_collections WHERE id = ?1", [calendar.id])?;
                    crate::db::set_setting(tx, BACKFILL_MARKER, "pending")
                })
                .await
                .unwrap();
            assert_eq!(store.backfill_birthday_calendars().await.unwrap(), 1);
            assert_eq!(events(&store, mini).await.len(), 1);
            assert_eq!(store.backfill_birthday_calendars().await.unwrap(), 0, "once");
        }
    }
}
