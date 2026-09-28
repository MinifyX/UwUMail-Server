//! Calendars and address books taken over from elsewhere (docs/calendar-import.md): an `.ics` or
//! `.vcf` file, what another provider's CalDAV or CardDAV server hands out, or a subscribed feed.
//!
//! Such a file holds many entries, while CalDAV and CardDAV keep one per resource: one event with
//! its exceptions, or one card. So the file is cut apart first, line by line and without parsing
//! it into a tree, so an entry is stored exactly as it came, folds and all. Each event takes along
//! the time zones it names. Then the entries go in through the same checks and the same writes as
//! everything CalDAV stores, in portions, so the database is never held for long. Store-level
//! writes send no invitations, so taking over a calendar mails nobody.
//!
//! A subscribed feed is mirrored instead: what the feed no longer has goes, what changed is
//! rewritten, and an entry that differs only in its `DTSTAMP` is left alone, as some feeds stamp
//! every entry with the time they were asked.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::dav::{
    ChangeLog, DavCollection, DavKind, DavPrecondition, DavWrite, DavWriteOutcome, check_entries_writable,
    delete_entry_unchecked, new_entry_name, own_collection, put_entry, put_entry_unchecked,
};
use crate::ical::{Checked, Refused, check_calendar, check_contact};
use crate::{DAV_RESOURCE_MAX_BYTES, Result, Store, StoreError, dav_etag};

/// Entries one file may hold. A collection holds no more either.
pub const IMPORT_MAX_OBJECTS: usize = crate::DAV_RESOURCES_PER_COLLECTION as usize;
/// Problems a report lists one by one; beyond, only their number grows.
pub const IMPORT_MAX_PROBLEMS: usize = 100;
/// Entries written per transaction.
const CHUNK: usize = 500;
/// How deep components may nest (VCALENDAR > VEVENT > VALARM is three).
const MAX_DEPTH: usize = 16;

/// One entry cut out of a file: a whole iCalendar object or one vCard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitObject {
    /// What a person recognizes it by in a report: the summary or the name.
    pub label: String,
    pub uid: String,
    pub content: String,
}

/// What the calendar in a file says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IcsMeta {
    pub name: Option<String>,
    pub description: Option<String>,
    /// As the file has it; the caller decides whether it is a colour it can use.
    pub color: Option<String>,
    /// How often the publisher asks to be fetched again, in seconds.
    pub refresh_secs: Option<i64>,
}

/// Why an entry was left out, as a stable code the portal translates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProblem {
    pub item: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Split {
    pub objects: Vec<SplitObject>,
    pub problems: Vec<ImportProblem>,
    pub meta: IcsMeta,
    /// Whether the text held a calendar (`BEGIN:VCALENDAR`) or a card at all. An empty calendar
    /// counts; an HTML page does not.
    pub recognized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DavImportMode {
    /// New entries are added, entries with a known UID are overwritten.
    Merge,
    /// Only entries whose UID is not there yet are added.
    OnlyNew,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DavImportReport {
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub problems: Vec<ImportProblem>,
    /// More problems than listed.
    pub truncated: bool,
}

impl DavImportReport {
    pub fn problem(&mut self, item: impl Into<String>, reason: &'static str) {
        self.skipped += 1;
        self.note(item, reason);
    }

    /// A problem that is not an entry left out, such as a file cut short.
    pub fn note(&mut self, item: impl Into<String>, reason: &'static str) {
        if self.problems.len() < IMPORT_MAX_PROBLEMS {
            self.problems.push(ImportProblem { item: shorten(&item.into(), 120), reason });
        } else {
            self.truncated = true;
        }
    }

    /// Adds another report's numbers, as when a collection comes in several parts.
    pub fn absorb(&mut self, other: DavImportReport) {
        self.total += other.total;
        self.created += other.created;
        self.updated += other.updated;
        self.unchanged += other.unchanged;
        self.skipped += other.skipped;
        self.truncated |= other.truncated;
        for problem in other.problems {
            if self.problems.len() < IMPORT_MAX_PROBLEMS {
                self.problems.push(problem);
            } else {
                self.truncated = true;
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DavMirrorReport {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub problems: Vec<ImportProblem>,
    /// Entries the calendar holds afterwards.
    pub entries: usize,
}

fn shorten(value: &str, max: usize) -> String {
    let value = value.trim();
    match value.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &value[..cut]),
        None => value.to_owned(),
    }
}

/// A property with its continuation lines, as they were.
#[derive(Debug, Clone)]
struct Line<'a> {
    raw: Vec<&'a str>,
}

impl Line<'_> {
    fn unfolded(&self) -> String {
        let mut out = String::from(self.raw[0]);
        for next in &self.raw[1..] {
            out.push_str(&next[1..]);
        }
        out
    }

    /// The property name, upper case: what comes before the first `;` or `:`.
    fn name(&self) -> String {
        let first = self.raw[0];
        let end = first.find([';', ':']).unwrap_or(first.len());
        first[..end].trim().to_ascii_uppercase()
    }

    /// The value: what comes after the first `:` that is not inside a quoted parameter.
    fn value(&self) -> String {
        let unfolded = self.unfolded();
        let mut quoted = false;
        for (index, c) in unfolded.char_indices() {
            match c {
                '"' => quoted = !quoted,
                ':' if !quoted => return unfolded[index + 1..].to_owned(),
                _ => {}
            }
        }
        String::new()
    }

    fn push_to(&self, out: &mut String) {
        for raw in &self.raw {
            out.push_str(raw);
            out.push_str("\r\n");
        }
    }
}

/// Lines with their folds joined to the line they continue. A byte order mark and empty lines
/// are dropped, `\n` and `\r\n` are both line ends.
fn logical_lines(text: &str) -> Vec<Line<'_>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines: Vec<Line<'_>> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.trim().is_empty() {
            continue;
        }
        if raw.starts_with([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.raw.push(raw);
            continue;
        }
        lines.push(Line { raw: vec![raw] });
    }
    lines
}

fn begins(line: &Line<'_>) -> Option<String> {
    (line.name() == "BEGIN").then(|| line.value().trim().to_ascii_uppercase())
}

fn ends(line: &Line<'_>) -> Option<String> {
    (line.name() == "END").then(|| line.value().trim().to_ascii_uppercase())
}

/// iCalendar text escapes, undone for showing.
fn unescape(value: &str) -> String {
    value.replace("\\n", " ").replace("\\N", " ").replace("\\,", ",").replace("\\;", ";").replace("\\\\", "\\")
}

/// A UID for an entry that has none, the same each time the same entry comes, so importing a file
/// twice or fetching a feed again finds it again.
fn made_up_uid(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    format!("uwumail-import-{}", &hex::encode(digest)[..24])
}

/// A component inside the VCALENDAR: its type and its lines, `BEGIN` and `END` included.
struct Block<'a> {
    kind: String,
    lines: Vec<Line<'a>>,
}

impl Block<'_> {
    /// The value of a property of the component itself, not of one nested in it.
    fn property(&self, name: &str) -> Option<String> {
        let mut depth = 0usize;
        for line in &self.lines {
            if begins(line).is_some() {
                depth += 1;
            } else if ends(line).is_some() {
                depth = depth.saturating_sub(1);
            } else if depth == 1 && line.name() == name {
                return Some(line.value());
            }
        }
        None
    }

    /// The time zones the component names in `TZID=` parameters.
    fn tzids(&self, into: &mut HashSet<String>) {
        for line in &self.lines {
            let unfolded = line.unfolded();
            let head = match unfolded.find(':') {
                Some(colon) if !unfolded[..colon].contains('"') => &unfolded[..colon],
                _ => unfolded.as_str(),
            };
            let mut rest = head;
            while let Some(at) = rest.to_ascii_uppercase().find(";TZID=") {
                let value = &rest[at + 6..];
                let (tzid, after) = if let Some(quoted) = value.strip_prefix('"') {
                    let end = quoted.find('"').unwrap_or(quoted.len());
                    (&quoted[..end], &quoted[(end + 1).min(quoted.len())..])
                } else {
                    let end = value.find([';', ':']).unwrap_or(value.len());
                    (&value[..end], &value[end..])
                };
                into.insert(tzid.to_owned());
                rest = after;
            }
        }
    }

    /// The component written out, reminders dropped if asked, a UID put in when it has none.
    fn write(&self, strip_alarms: bool, uid: Option<&str>, out: &mut String) {
        let mut skipping = 0usize;
        for (index, line) in self.lines.iter().enumerate() {
            if strip_alarms && index > 0 && (skipping > 0 || begins(line).as_deref() == Some("VALARM")) {
                if begins(line).is_some() {
                    skipping += 1;
                } else if ends(line).is_some() {
                    skipping -= 1;
                }
                continue;
            }
            line.push_to(out);
            if index == 0
                && let Some(uid) = uid
            {
                out.push_str(&format!("UID:{uid}\r\n"));
            }
        }
    }
}

/// Cuts an iCalendar file into one object per UID: an event with its exceptions, a task or a
/// journal entry, each with the time zones it uses. Several calendars one after the other are
/// fine. `METHOD` is dropped, as stored objects have none (RFC 4791, 4.1); reminders too when
/// `strip_alarms` says so.
pub fn split_ics(text: &str, strip_alarms: bool) -> Split {
    let mut split = Split::default();
    let lines = logical_lines(text);
    let mut prodid: Option<String> = None;
    let mut calscale: Option<String> = None;
    let mut timezones: HashMap<String, Block<'_>> = HashMap::new();
    let mut entries: Vec<Block<'_>> = Vec::new();
    let mut in_calendar = false;
    let mut current: Option<Block<'_>> = None;
    let mut depth = 0usize;
    let mut too_deep = false;
    for line in lines {
        if let Some(block) = current.as_mut() {
            if begins(&line).is_some() {
                depth += 1;
                too_deep |= depth > MAX_DEPTH;
            } else if ends(&line).is_some() {
                depth -= 1;
            }
            block.lines.push(line);
            if depth == 0 {
                let block = current.take().expect("a block is open");
                match block.kind.as_str() {
                    _ if too_deep => {
                        split.problems.push(ImportProblem { item: block.kind.clone(), reason: "invalid" });
                    }
                    "VTIMEZONE" => {
                        if let Some(tzid) = block.property("TZID") {
                            timezones.entry(tzid).or_insert(block);
                        }
                    }
                    "VEVENT" | "VTODO" | "VJOURNAL" => entries.push(block),
                    other => {
                        split.problems.push(ImportProblem { item: other.to_owned(), reason: "unsupportedComponent" })
                    }
                }
                too_deep = false;
            }
            continue;
        }
        if !in_calendar {
            if begins(&line).as_deref() == Some("VCALENDAR") {
                in_calendar = true;
                split.recognized = true;
            }
            continue;
        }
        if let Some(kind) = begins(&line) {
            depth = 1;
            current = Some(Block { kind, lines: vec![line] });
            continue;
        }
        if ends(&line).as_deref() == Some("VCALENDAR") {
            in_calendar = false;
            continue;
        }
        let value = unescape(line.value().trim());
        match line.name().as_str() {
            "PRODID" => {
                prodid.get_or_insert(value);
            }
            "CALSCALE" => {
                calscale.get_or_insert(value);
            }
            "X-WR-CALNAME" | "NAME" if !value.is_empty() => {
                split.meta.name.get_or_insert(shorten(&value, 255));
            }
            "X-WR-CALDESC" | "DESCRIPTION" if !value.is_empty() => {
                split.meta.description.get_or_insert(shorten(&value, 1000));
            }
            "COLOR" | "X-APPLE-CALENDAR-COLOR" if !value.is_empty() => {
                split.meta.color.get_or_insert(value);
            }
            "REFRESH-INTERVAL" | "X-PUBLISHED-TTL" => {
                if let Some(secs) = duration_secs(&value) {
                    split.meta.refresh_secs.get_or_insert(secs);
                }
            }
            _ => {}
        }
    }
    if current.is_some() {
        split.problems.push(ImportProblem { item: "VCALENDAR".into(), reason: "cutShort" });
    }

    // One object per UID, in the order the UIDs first came; the entry without RECURRENCE-ID first.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<(Block<'_>, Option<String>)>> = HashMap::new();
    for block in entries {
        let (uid, invented) = match block.property("UID").map(|uid| uid.trim().to_owned()).filter(|u| !u.is_empty()) {
            Some(uid) => (uid, None),
            None => {
                let mut content = String::new();
                block.write(false, None, &mut content);
                let uid = made_up_uid(&content);
                (uid.clone(), Some(uid))
            }
        };
        let group = groups.entry(uid.clone()).or_insert_with(|| {
            order.push(uid.clone());
            Vec::new()
        });
        let rid = block.property("RECURRENCE-ID").map(|rid| rid.trim().to_owned());
        if let Some(existing) =
            group.iter_mut().find(|(other, _)| other.property("RECURRENCE-ID").map(|r| r.trim().to_owned()) == rid)
        {
            split.problems.push(ImportProblem { item: label_of(&block, &uid), reason: "duplicate" });
            *existing = (block, invented);
        } else {
            group.push((block, invented));
        }
    }
    let prodid = prodid.unwrap_or_else(|| "-//UwUMail//Import//EN".into());
    for uid in order {
        let mut group = groups.remove(&uid).unwrap_or_default();
        if split.objects.len() >= IMPORT_MAX_OBJECTS {
            split.problems.push(ImportProblem { item: uid, reason: "tooMany" });
            break;
        }
        let label = group
            .iter()
            .find(|(block, _)| block.property("RECURRENCE-ID").is_none())
            .or(group.first())
            .map(|(block, _)| label_of(block, &uid))
            .unwrap_or_else(|| uid.clone());
        if group.iter().any(|(block, _)| block.kind != group[0].0.kind) {
            split.problems.push(ImportProblem { item: label, reason: "mixedComponents" });
            continue;
        }
        group.sort_by_key(|(block, _)| block.property("RECURRENCE-ID").is_some());
        let mut tzids = HashSet::new();
        for (block, _) in &group {
            block.tzids(&mut tzids);
        }
        let mut content = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:{prodid}\r\n");
        if let Some(calscale) = &calscale {
            content.push_str(&format!("CALSCALE:{calscale}\r\n"));
        }
        let mut tzids: Vec<String> = tzids.into_iter().collect();
        tzids.sort();
        for tzid in tzids {
            if let Some(timezone) = timezones.get(&tzid) {
                timezone.write(false, None, &mut content);
            }
        }
        for (block, invented) in &group {
            block.write(strip_alarms, invented.as_deref(), &mut content);
        }
        content.push_str("END:VCALENDAR\r\n");
        if content.len() > DAV_RESOURCE_MAX_BYTES {
            split.problems.push(ImportProblem { item: label, reason: "tooLarge" });
            continue;
        }
        split.objects.push(SplitObject { label, uid, content });
    }
    split
}

fn label_of(block: &Block<'_>, uid: &str) -> String {
    block
        .property("SUMMARY")
        .map(|summary| shorten(&unescape(summary.trim()), 80))
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| uid.to_owned())
}

/// An iCalendar duration like `PT1H` or `P1D` in seconds (RFC 5545, 3.3.6), for refresh hints.
fn duration_secs(value: &str) -> Option<i64> {
    let value = value.trim().trim_start_matches('+');
    let rest = value.strip_prefix('P')?;
    let (mut total, mut number, mut in_time) = (0i64, String::new(), false);
    for c in rest.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => number.push(c),
            _ => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                total += n * match (c, in_time) {
                    ('W', false) => 7 * 86_400,
                    ('D', false) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    (number.is_empty() && total > 0).then_some(total)
}

/// Cuts a vCard file into its cards. Cards of vCard 2.1 (and 2.0), as old phones and Outlook
/// write them, are rewritten as vCard 3.0, with their quoted-printable text and character sets
/// decoded; the others stay as they are. A card without a UID gets one.
pub fn split_vcf(text: &str) -> Split {
    let mut split = Split::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut cards: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut depth = 0usize;
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let marker = raw.trim().to_ascii_uppercase();
        if marker == "BEGIN:VCARD" {
            depth += 1;
            split.recognized = true;
        }
        if depth > 0 && !(raw.trim().is_empty() && depth == 1) {
            current.push(raw);
        }
        if marker == "END:VCARD" && depth > 0 {
            depth -= 1;
            if depth == 0 {
                cards.push(std::mem::take(&mut current));
            }
        }
    }
    if depth > 0 {
        split.problems.push(ImportProblem { item: "VCARD".into(), reason: "cutShort" });
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for lines in cards {
        if split.objects.len() >= IMPORT_MAX_OBJECTS {
            split.problems.push(ImportProblem { item: "VCARD".into(), reason: "tooMany" });
            break;
        }
        let mut content: String = lines.iter().map(|line| format!("{line}\r\n")).collect();
        let parsed = logical_lines(&content);
        let version = parsed.iter().find(|line| line.name() == "VERSION").map(|line| line.value().trim().to_owned());
        let label = card_label(&parsed);
        if matches!(version.as_deref(), Some("2.1" | "2.0")) {
            match calcard::vcard::VCard::parse(&content) {
                Ok(card) => {
                    let mut out = String::new();
                    if card.write_to(&mut out, calcard::vcard::VCardVersion::V3_0).is_err() {
                        split.problems.push(ImportProblem { item: label.unwrap_or_default(), reason: "invalid" });
                        continue;
                    }
                    content = out;
                }
                Err(_) => {
                    split
                        .problems
                        .push(ImportProblem { item: label.unwrap_or_else(|| "VCARD".into()), reason: "invalid" });
                    continue;
                }
            }
        }
        let parsed = logical_lines(&content);
        let label = card_label(&parsed).or(label);
        let uid = parsed
            .iter()
            .find(|line| line.name() == "UID")
            .map(|line| line.value().trim().to_owned())
            .filter(|uid| !uid.is_empty());
        let uid = match uid {
            Some(uid) => uid,
            None => {
                let uid = made_up_uid(&content);
                // Right after VERSION, or after BEGIN when there is none: where readers look.
                let after = parsed.iter().position(|line| line.name() == "VERSION").unwrap_or(0);
                let mut with_uid = String::with_capacity(content.len() + 60);
                for (index, line) in parsed.iter().enumerate() {
                    line.push_to(&mut with_uid);
                    if index == after {
                        with_uid.push_str(&format!("UID:{uid}\r\n"));
                    }
                }
                content = with_uid;
                uid
            }
        };
        if content.len() > DAV_RESOURCE_MAX_BYTES {
            split.problems.push(ImportProblem { item: label.unwrap_or(uid), reason: "tooLarge" });
            continue;
        }
        let label = label.unwrap_or_else(|| uid.clone());
        // The same UID twice in one file: the later card wins.
        match seen.get(&uid) {
            Some(&index) => {
                split.problems.push(ImportProblem { item: label.clone(), reason: "duplicate" });
                split.objects[index] = SplitObject { label, uid, content };
            }
            None => {
                seen.insert(uid.clone(), split.objects.len());
                split.objects.push(SplitObject { label, uid, content });
            }
        }
    }
    split
}

fn card_label(lines: &[Line<'_>]) -> Option<String> {
    lines
        .iter()
        .find(|line| line.name() == "FN")
        .map(|line| shorten(&unescape(line.value().trim()), 80))
        .filter(|name| !name.is_empty())
}

/// The text of an uploaded file: UTF-8, or else Latin-1, which old address books were written in.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// A colour as CalDAV keeps it (`#RRGGBBAA`, as Apple writes it) from `#rgb`, `#rrggbb` or
/// `#rrggbbaa`; `None` for anything else.
pub fn dav_color(css: &str) -> Option<String> {
    let hex = css.trim().strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let full: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).chain("FF".chars()).collect(),
        6 => format!("{hex}FF"),
        8 => hex.to_owned(),
        _ => return None,
    };
    Some(format!("#{}", full.to_ascii_uppercase()))
}

/// A URL segment made from a name: `Ferien Bayern` becomes `ferien-bayern`.
fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'ä' => 'a',
            'ö' => 'o',
            'ü' => 'u',
            'ß' => 's',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() { "import".into() } else { slug }
}

/// A new collection of an import, named by its taker or its file.
#[derive(Debug, Clone, Default)]
pub struct NewImportCollection {
    pub name: String,
    pub description: String,
    /// Any colour [`dav_color`] reads; others are dropped.
    pub color: Option<String>,
}

/// An entry without the lines that change on every fetch without anything changing.
fn fingerprint(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for line in logical_lines(content) {
        if line.name() != "DTSTAMP" {
            line.push_to(&mut out);
        }
    }
    out
}

/// Checks each entry the way CalDAV and CardDAV check what clients store.
fn check(kind: DavKind, components: &[String], object: &SplitObject) -> std::result::Result<Checked, &'static str> {
    let checked = match kind {
        DavKind::Calendar => check_calendar(&object.content, components),
        DavKind::Addressbook => check_contact(&object.content, &object.uid),
    };
    match checked {
        Ok(checked) if checked.uid != object.uid && kind == DavKind::Calendar => Err("invalid"),
        Ok(checked) => Ok(checked),
        Err(Refused::UnsupportedComponent(_)) => Err("unsupportedComponent"),
        Err(Refused::InvalidData(_) | Refused::InvalidObject(_)) => Err("invalid"),
    }
}

async fn check_all(
    kind: DavKind,
    components: Vec<String>,
    objects: Vec<SplitObject>,
) -> Result<Vec<(SplitObject, std::result::Result<Checked, &'static str>)>> {
    tokio::task::spawn_blocking(move || {
        objects
            .into_iter()
            .map(|object| {
                let checked = check(kind, &components, &object);
                (object, checked)
            })
            .collect()
    })
    .await
    .map_err(|err| StoreError::Internal(err.to_string()))
}

fn write_of(name: String, object: &SplitObject, checked: &Checked) -> DavWrite {
    DavWrite {
        name,
        content: object.content.clone(),
        uid: checked.uid.clone(),
        component: checked.component.clone(),
        starts_at: checked.starts_at,
        ends_at: checked.ends_at,
    }
}

fn extension(kind: DavKind) -> &'static str {
    if kind == DavKind::Calendar { "ics" } else { "vcf" }
}

/// Whether one of the owner's other calendars (not subscribed ones) has this UID already: JMAP
/// keeps UIDs unique across them.
fn uid_elsewhere(tx: &Transaction<'_>, collection: &DavCollection, uid: &str) -> Result<bool> {
    if collection.kind != DavKind::Calendar {
        return Ok(false);
    }
    Ok(tx.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                 WHERE c.account_id = ?1 AND c.kind = 'calendar' AND c.id <> ?2 AND r.uid = ?3
                   AND NOT {})",
            crate::calendar::SUBSCRIBED
        ),
        params![collection.account_id, collection.id, uid],
        |row| row.get(0),
    )?)
}

impl Store {
    /// Takes entries over into one of the account's own calendars or address books, which must
    /// not be a subscribed one. Entries the checks refuse are listed in the report, as are those
    /// whose UID another calendar of the account holds already.
    pub async fn dav_import(
        &self,
        account_id: i64,
        collection_id: i64,
        objects: Vec<SplitObject>,
        mode: DavImportMode,
    ) -> Result<DavImportReport> {
        let collection = self.read(move |conn| own_collection(conn, account_id, collection_id)).await?;
        check_entries_writable(&collection)?;
        let mut report = DavImportReport { total: objects.len(), ..Default::default() };
        let checked = check_all(collection.kind, collection.components.clone(), objects).await?;
        let mut valid = Vec::with_capacity(checked.len());
        for (object, result) in checked {
            match result {
                Ok(checked) => valid.push((object, checked)),
                Err(reason) => report.problem(&object.label, reason),
            }
        }
        let mut rest = valid.into_iter();
        loop {
            let chunk: Vec<(SplitObject, Checked)> = rest.by_ref().take(CHUNK).collect();
            if chunk.is_empty() {
                break;
            }
            let (part, full, modseq) = self
                .write(move |tx| {
                    let mut log = ChangeLog::new(account_id);
                    let collection = own_collection(tx, account_id, collection_id)?;
                    let mut part = DavImportReport::default();
                    let mut full = false;
                    for (object, checked) in &chunk {
                        if full {
                            part.problem(&object.label, "collectionFull");
                            continue;
                        }
                        let existing: Option<(String, String)> = tx
                            .query_row(
                                "SELECT name, etag FROM dav_resources WHERE collection_id = ?1 AND uid = ?2",
                                params![collection.id, checked.uid],
                                |row| Ok((row.get(0)?, row.get(1)?)),
                            )
                            .optional()?;
                        if existing.is_none() && uid_elsewhere(tx, &collection, &checked.uid)? {
                            part.problem(&object.label, "uidElsewhere");
                            continue;
                        }
                        let name = match existing {
                            Some((_, etag)) if etag == dav_etag(&object.content) => {
                                part.unchanged += 1;
                                continue;
                            }
                            Some(_) if mode == DavImportMode::OnlyNew => {
                                part.problem(&object.label, "exists");
                                continue;
                            }
                            Some((name, _)) => name,
                            None => new_entry_name(tx, collection.id, &checked.uid, extension(collection.kind))?,
                        };
                        let write = write_of(name, object, checked);
                        match put_entry(tx, &mut log, &collection, &write, &DavPrecondition::default()) {
                            Ok((DavWriteOutcome::Created { .. }, _)) => part.created += 1,
                            Ok((DavWriteOutcome::Updated { .. }, _)) => part.updated += 1,
                            Ok(_) => part.problem(&object.label, "invalid"),
                            Err(StoreError::QuotaExceeded) => {
                                full = true;
                                part.problem(&object.label, "collectionFull");
                            }
                            Err(err) => return Err(err),
                        }
                    }
                    Ok((part, full, log.modseq()))
                })
                .await?;
            self.notify_log(account_id, modseq);
            report.absorb(DavImportReport { total: 0, ..part });
            if full {
                for (object, _) in rest.by_ref() {
                    report.problem(&object.label, "collectionFull");
                }
            }
        }
        Ok(report)
    }

    /// Makes a calendar or address book for an import, with a URL segment of its own made from
    /// its name. The account's first collection of the kind is made before, when it has none, so
    /// the default stays the one everybody starts with.
    pub async fn dav_create_import_collection(
        &self,
        account_id: i64,
        kind: DavKind,
        new: NewImportCollection,
        default: crate::NewDavCollection,
    ) -> Result<DavCollection> {
        let (collection, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let has_one: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM dav_collections WHERE account_id = ?1 AND kind = ?2)",
                    params![account_id, kind.as_str()],
                    |row| row.get(0),
                )?;
                if !has_one {
                    crate::dav::insert_collection(tx, &mut log, account_id, kind, &default)?;
                }
                let base = slug_of(&new.name);
                let mut slug = base.clone();
                for n in 2.. {
                    let taken: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM dav_collections WHERE account_id = ?1 AND kind = ?2 AND slug = ?3)",
                        params![account_id, kind.as_str(), slug],
                        |row| row.get(0),
                    )?;
                    if !taken {
                        break;
                    }
                    slug = format!("{base}-{n}");
                }
                let name = shorten(&new.name, 255);
                let collection = crate::NewDavCollection {
                    slug,
                    display_name: if name.is_empty() { default.display_name.clone() } else { name },
                    description: shorten(&new.description, 1000),
                    color: new.color.as_deref().and_then(dav_color),
                    components: default.components.clone(),
                };
                let id = crate::dav::insert_collection(tx, &mut log, account_id, kind, &collection)?;
                Ok((own_collection(tx, account_id, id)?, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(collection)
    }

    /// Makes a subscribed calendar hold exactly what its feed holds now, in one step, so nobody
    /// ever sees half a feed. Entries the checks refuse keep what they were before.
    pub async fn dav_mirror(
        &self,
        account_id: i64,
        collection_id: i64,
        objects: Vec<SplitObject>,
    ) -> Result<DavMirrorReport> {
        let (collection, current) = self
            .read(move |conn| {
                let collection = own_collection(conn, account_id, collection_id)?;
                let mut stmt = conn.prepare("SELECT uid, name, content FROM dav_resources WHERE collection_id = ?1")?;
                let rows = stmt.query_map([collection_id], |row| {
                    Ok((row.get::<_, String>(0)?, (row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
                })?;
                let current: HashMap<String, (String, String)> = rows.collect::<rusqlite::Result<_>>()?;
                Ok((collection, current))
            })
            .await?;
        if !collection.subscribed {
            return Err(StoreError::Invalid(format!("calendar {collection_id} is not a subscribed one")));
        }
        let mut report = DavMirrorReport::default();
        let wanted: HashSet<String> = objects.iter().map(|object| object.uid.clone()).collect();
        let mut changed = Vec::new();
        for object in objects {
            match current.get(&object.uid) {
                Some((_, content)) if fingerprint(content) == fingerprint(&object.content) => report.unchanged += 1,
                _ => changed.push(object),
            }
        }
        let checked = check_all(collection.kind, collection.components.clone(), changed).await?;
        let mut writes = Vec::with_capacity(checked.len());
        for (object, result) in checked {
            match result {
                Ok(checked) => writes.push((object, checked)),
                Err(reason) => {
                    if report.problems.len() < IMPORT_MAX_PROBLEMS {
                        report.problems.push(ImportProblem { item: shorten(&object.label, 120), reason });
                    }
                }
            }
        }
        let gone: Vec<String> =
            current.iter().filter(|(uid, _)| !wanted.contains(*uid)).map(|(_, (name, _))| name.clone()).collect();
        let names: BTreeMap<String, String> = current.into_iter().map(|(uid, (name, _))| (uid, name)).collect();
        let (report, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let collection = own_collection(tx, account_id, collection_id)?;
                let mut report = report;
                for name in &gone {
                    if delete_entry_unchecked(tx, &mut log, &collection, name, None)? {
                        report.deleted += 1;
                    }
                }
                for (object, checked) in &writes {
                    let name = match names.get(&checked.uid) {
                        Some(name) => name.clone(),
                        None => new_entry_name(tx, collection.id, &checked.uid, extension(collection.kind))?,
                    };
                    let write = write_of(name, object, checked);
                    match put_entry_unchecked(tx, &mut log, &collection, &write, &DavPrecondition::default()) {
                        Ok((DavWriteOutcome::Created { .. }, _)) => report.created += 1,
                        Ok((DavWriteOutcome::Updated { .. }, _)) => report.updated += 1,
                        Ok(_) => {}
                        Err(StoreError::QuotaExceeded) => {
                            if report.problems.len() < IMPORT_MAX_PROBLEMS {
                                report
                                    .problems
                                    .push(ImportProblem { item: object.label.clone(), reason: "collectionFull" });
                            }
                        }
                        Err(err) => return Err(err),
                    }
                }
                report.entries = tx.query_row(
                    "SELECT count(*) FROM dav_resources WHERE collection_id = ?1",
                    [collection.id],
                    |row| row.get::<_, i64>(0),
                )? as usize;
                Ok((report, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, NewDavCollection, Role};

    const FEED: &str = "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//Example Corp//Feed//EN\nMETHOD:PUBLISH\n\
X-WR-CALNAME:Ferien\nX-APPLE-CALENDAR-COLOR:#FF0000\nREFRESH-INTERVAL;VALUE=DURATION:PT12H\n\
BEGIN:VTIMEZONE\nTZID:Europe/Berlin\nBEGIN:STANDARD\nDTSTART:19701025T030000\nTZOFFSETFROM:+0200\n\
TZOFFSETTO:+0100\nEND:STANDARD\nEND:VTIMEZONE\n\
BEGIN:VTIMEZONE\nTZID:America/New_York\nBEGIN:STANDARD\nDTSTART:19701101T020000\nTZOFFSETFROM:-0400\n\
TZOFFSETTO:-0500\nEND:STANDARD\nEND:VTIMEZONE\n\
BEGIN:VEVENT\nUID:yoga@example.org\nDTSTAMP:20260101T000000Z\nDTSTART;TZID=Europe/Berlin:20260105T180000\n\
DTEND;TZID=Europe/Berlin:20260105T190000\nRRULE:FREQ=WEEKLY;COUNT=10\nSUMMARY:Yoga\\, abends\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT10M\nDESCRIPTION:Yoga\nEND:VALARM\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:yoga@example.org\nRECURRENCE-ID;TZID=Europe/Berlin:20260112T180000\nDTSTAMP:20260101T000000Z\n\
DTSTART;TZID=Europe/Berlin:20260112T190000\nDTEND;TZID=Europe/Berlin:20260112T200000\nSUMMARY:Yoga später\nEND:VEVENT\n\
BEGIN:VEVENT\nDTSTAMP:20260101T000000Z\nDTSTART;VALUE=DATE:20260401\nSUMMARY:Ostern ohne UID\n\x20\
und mit Faltung\nEND:VEVENT\n\
BEGIN:VFREEBUSY\nUID:fb\nEND:VFREEBUSY\nEND:VCALENDAR\n";

    #[test]
    fn feeds_are_cut_per_uid_with_their_time_zones() {
        let split = split_ics(FEED, true);
        assert!(split.recognized);
        assert_eq!(split.meta.name.as_deref(), Some("Ferien"));
        assert_eq!(split.meta.color.as_deref(), Some("#FF0000"));
        assert_eq!(split.meta.refresh_secs, Some(12 * 3600));
        assert_eq!(split.objects.len(), 2, "{:?}", split.objects);
        let yoga = &split.objects[0];
        assert_eq!((yoga.uid.as_str(), yoga.label.as_str()), ("yoga@example.org", "Yoga, abends"));
        assert!(yoga.content.contains("TZID:Europe/Berlin") && !yoga.content.contains("America/New_York"));
        assert!(!yoga.content.contains("METHOD") && !yoga.content.contains("VALARM"));
        assert!(yoga.content.find("RRULE").unwrap() < yoga.content.find("RECURRENCE-ID").unwrap());
        assert!(yoga.content.contains("\r\n") && !yoga.content.replace("\r\n", "").contains('\n'));
        let checked = check_calendar(&yoga.content, &[]).unwrap();
        assert_eq!(checked.uid, "yoga@example.org");

        let easter = &split.objects[1];
        assert!(easter.uid.starts_with("uwumail-import-"), "{}", easter.uid);
        assert!(easter.content.contains("\r\n und mit Faltung\r\n"), "folds stay as they were");
        assert_eq!(check_calendar(&easter.content, &[]).unwrap().uid, easter.uid);
        assert_eq!(split_ics(FEED, true).objects[1].uid, easter.uid, "the same UID every time");
        assert_eq!(split.problems, vec![ImportProblem { item: "VFREEBUSY".into(), reason: "unsupportedComponent" }]);

        let with_alarms = split_ics(FEED, false);
        assert!(with_alarms.objects[0].content.contains("BEGIN:VALARM"));
    }

    #[test]
    fn odd_files() {
        assert!(!split_ics("<html><body>Not found</body></html>", false).recognized);
        let empty = split_ics("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n", false);
        assert!(empty.recognized && empty.objects.is_empty());
        let twice = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:eins\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n\
BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:zwei\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:b\r\nEND:VTODO\r\n\
BEGIN:VEVENT\r\nUID:b\r\nRECURRENCE-ID:20260101T000000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let split = split_ics(twice, false);
        assert_eq!(split.objects.len(), 1);
        assert!(split.objects[0].content.contains("zwei") && !split.objects[0].content.contains("eins"));
        let reasons: Vec<_> = split.problems.iter().map(|p| p.reason).collect();
        assert_eq!(reasons, vec!["duplicate", "mixedComponents"]);
        let cut = split_ics("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\n", false);
        assert_eq!(cut.problems[0].reason, "cutShort");
        assert_eq!(duration_secs("P1W"), Some(604_800));
        assert_eq!(duration_secs("PT1H30M"), Some(5400));
        assert_eq!(duration_secs("nonsense"), None);
    }

    #[test]
    fn cards_old_and_new() {
        let file = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Nyu Katze\r\nUID:nyu\r\nEND:VCARD\r\n\r\n\
BEGIN:VCARD\nVERSION:2.1\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:M=C3=BCller;J=C3=BCrgen\n\
FN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:J=C3=BCrgen M=C3=BCller\nTEL;HOME;VOICE:+49 30 1234567\nEND:VCARD\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ohne UID\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Nyu Katze neu\r\nUID:nyu\r\nEND:VCARD\r\n";
        let split = split_vcf(file);
        assert!(split.recognized);
        assert_eq!(split.objects.len(), 3, "{:?}", split.objects);
        assert_eq!(split.objects[0].label, "Nyu Katze neu", "the later card with the same UID wins");
        assert_eq!(split.problems[0].reason, "duplicate");
        let old = &split.objects[1];
        assert_eq!(old.label, "Jürgen Müller");
        assert!(old.content.contains("VERSION:3.0") && old.content.contains("Müller"), "{}", old.content);
        assert!(!old.content.contains("QUOTED-PRINTABLE"));
        assert!(old.uid.starts_with("uwumail-import-"));
        assert_eq!(check_contact(&old.content, "x").unwrap().uid, old.uid);
        let new = &split.objects[2];
        assert!(new.content.starts_with("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:uwumail-import-"), "{}", new.content);
        assert_eq!(check_contact(&new.content, "x").unwrap().uid, new.uid);
        assert!(!split_vcf("hallo").recognized);
    }

    #[test]
    fn names_colours_and_text() {
        assert_eq!(slug_of("Ferien Bayern 2026!"), "ferien-bayern-2026");
        assert_eq!(slug_of("Müßiggang"), "musiggang");
        assert_eq!(slug_of("日本の祝日"), "import");
        assert_eq!(dav_color("#f00").as_deref(), Some("#FF0000FF"));
        assert_eq!(dav_color("#12ab34").as_deref(), Some("#12AB34FF"));
        assert_eq!(dav_color("red"), None);
        assert_eq!(decode_text(b"\xef\xbb\xbfM\xc3\xbcller"), "Müller");
        assert_eq!(decode_text(b"M\xfcller"), "Müller");
    }

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

    fn event(uid: &str, summary: &str) -> SplitObject {
        SplitObject {
            label: summary.into(),
            uid: uid.into(),
            content: format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART:20260105T180000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            ),
        }
    }

    #[tokio::test]
    async fn imports_merge_and_keep_uids_unique() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.org").await;
        let personal = store
            .dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("Kalender"))
            .await
            .unwrap()[0]
            .clone();
        let work = store
            .dav_create_collection(
                mini,
                DavKind::Calendar,
                NewDavCollection { slug: "work".into(), display_name: "Arbeit".into(), ..Default::default() },
            )
            .await
            .unwrap();
        let broken = SplitObject { label: "kaputt".into(), uid: "x".into(), content: "BEGIN:VCALENDAR\r\n".into() };
        let report = store
            .dav_import(mini, personal.id, vec![event("a", "Eins"), event("b", "Zwei"), broken], DavImportMode::Merge)
            .await
            .unwrap();
        assert_eq!((report.total, report.created, report.skipped), (3, 2, 1));
        assert_eq!(report.problems[0], ImportProblem { item: "kaputt".into(), reason: "invalid" });

        let again = store
            .dav_import(mini, personal.id, vec![event("a", "Eins"), event("b", "Zwei neu")], DavImportMode::OnlyNew)
            .await
            .unwrap();
        assert_eq!((again.unchanged, again.skipped), (1, 1));
        let merged =
            store.dav_import(mini, personal.id, vec![event("b", "Zwei neu")], DavImportMode::Merge).await.unwrap();
        assert_eq!(merged.updated, 1);
        assert_eq!(store.dav_resources(mini, personal.id).await.unwrap().len(), 2);

        let elsewhere = store.dav_import(mini, work.id, vec![event("a", "Eins")], DavImportMode::Merge).await.unwrap();
        assert_eq!(elsewhere.problems[0].reason, "uidElsewhere");

        let new = NewImportCollection { name: "Arbeit".into(), color: Some("#0f0".into()), ..Default::default() };
        let named = store
            .dav_create_import_collection(mini, DavKind::Calendar, new, NewDavCollection::default_calendar("K"))
            .await
            .unwrap();
        assert_eq!(
            (named.slug.as_str(), named.color.as_deref(), named.is_default),
            ("arbeit", Some("#00FF00FF"), false)
        );
        let leni = account(&store, "leni@example.org").await;
        let book = store
            .dav_create_import_collection(
                leni,
                DavKind::Addressbook,
                NewImportCollection { name: "Handy".into(), ..Default::default() },
                NewDavCollection::default_address_book("Kontakte"),
            )
            .await
            .unwrap();
        let books = store
            .dav_collections(leni, DavKind::Addressbook, NewDavCollection::default_address_book("Kontakte"))
            .await
            .unwrap();
        assert_eq!(books.len(), 2);
        assert!(!book.is_default && books.iter().any(|b| b.is_default && b.slug == "contacts"));
    }

    #[tokio::test]
    async fn mirrors_follow_their_feed_and_stay_read_only() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.org").await;
        let personal = store
            .dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("Kalender"))
            .await
            .unwrap()[0]
            .clone();
        store
            .put_calendar_event(
                mini,
                crate::CalendarEventWrite {
                    id: None,
                    calendar_id: personal.id,
                    content: event("mine", "Eigener Termin").content,
                    uid: "mine".into(),
                    starts_at: None,
                    ends_at: None,
                    if_etag: None,
                    keep_schedule_tag: false,
                    draft: None,
                    author: crate::Author::Account,
                },
            )
            .await
            .unwrap();
        let (feed, subscription) = store
            .create_calendar_subscription(
                mini,
                crate::NewCalendarSubscription {
                    collection: NewDavCollection {
                        slug: "ferien".into(),
                        display_name: "Ferien".into(),
                        ..Default::default()
                    },
                    url: "https://calendar.example.net/secret/basic.ics".into(),
                    interval_secs: 3600,
                    keep_alarms: false,
                },
                NewDavCollection::default_calendar("Kalender"),
            )
            .await
            .unwrap();
        assert!(feed.subscribed && !feed.is_default);

        let first = store.dav_mirror(mini, feed.id, vec![event("a", "Eins"), event("mine", "Kopie")]).await.unwrap();
        assert_eq!((first.created, first.entries), (2, 2), "a UID of an own calendar may be in a feed too");
        let stamped = SplitObject {
            content: event("a", "Eins").content.replace("20260101T000000Z", "20270101T000000Z"),
            ..event("a", "Eins")
        };
        let second = store.dav_mirror(mini, feed.id, vec![stamped, event("b", "Zwei")]).await.unwrap();
        assert_eq!((second.unchanged, second.created, second.deleted, second.entries), (1, 1, 1, 2));

        // Scheduling and JMAP look past the feed.
        let own = store.own_calendar_event_by_uid(mini, "mine").await.unwrap().unwrap();
        assert_eq!(own.calendar_id, personal.id);
        assert!(matches!(
            store
                .dav_put(
                    mini,
                    feed.id,
                    write_of("x.ics".into(), &event("x", "x"), &check_calendar(&event("x", "x").content, &[]).unwrap()),
                    DavPrecondition::default()
                )
                .await,
            Err(StoreError::Rule { code: "readOnly", .. })
        ));
        assert!(matches!(
            store.dav_delete(mini, feed.id, "a.ics", None).await,
            Err(StoreError::Rule { code: "readOnly", .. })
        ));
        assert!(matches!(
            store.dav_import(mini, feed.id, vec![event("c", "Drei")], DavImportMode::Merge).await,
            Err(StoreError::Rule { code: "readOnly", .. })
        ));
        let event_id =
            store.calendar_events(mini, None).await.unwrap().into_iter().find(|e| e.calendar_id == feed.id).unwrap().id;
        assert!(matches!(
            store.destroy_calendar_event(mini, event_id, None).await,
            Err(StoreError::Rule { code: "readOnly", .. })
        ));
        let moved = crate::CalendarEventWrite {
            id: Some(own.id),
            calendar_id: feed.id,
            content: own.content.clone(),
            uid: "mine".into(),
            starts_at: None,
            ends_at: None,
            if_etag: None,
            keep_schedule_tag: false,
            draft: None,
            author: crate::Author::Account,
        };
        assert!(matches!(store.put_calendar_event(mini, moved).await, Err(StoreError::Rule { code: "readOnly", .. })));
        assert!(matches!(
            store.set_default_calendar(mini, feed.id).await,
            Err(StoreError::Rule { code: "readOnly", .. })
        ));

        // Ending the subscription but keeping the entries leaves a normal calendar.
        store.delete_calendar_subscription(mini, subscription.id, true).await.unwrap();
        let kept = store.dav_collection(mini, DavKind::Calendar, "ferien").await.unwrap().unwrap();
        assert!(!kept.subscribed && kept.resources == 2);
    }
}
