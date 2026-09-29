//! Moving birthdays out of calendars into the contacts (docs/birthdays.md): finding the yearly
//! all-day events other calendars kept for birthdays ("Geburtstag von Max", "Max's birthday",
//! "🎂 Max", what Google, KDE and others mark as one), reading the name and the date from them,
//! finding the contact each belongs to, and then writing the date into the card and deleting the
//! event in one transaction, so an event never goes without its birthday having arrived.
//!
//! Names are compared by characters, never bytes: lower case, with umlauts both spelled out
//! ("Müller" = "Mueller") and without their dots ("Müller" = "Muller"), and other accents dropped.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::birthdays::{PartialDate, card_dates, card_name};
use crate::dav::{
    ChangeLog, DavCollection, DavKind, DavPrecondition, DavWrite, DavWriteOutcome, delete_entry, new_entry_name,
    put_entry,
};
use crate::itip::Component;
use crate::sharing::VISIBLE;
use crate::{DAV_RESOURCE_MAX_BYTES, Result, Store, StoreError};

/// Events looked at in one scan, and candidates it returns at most.
pub const MAX_SCANNED_EVENTS: usize = 20_000;
pub const MAX_CANDIDATES: usize = 1_000;
/// Contacts offered for one ambiguous name.
pub const MAX_OPTIONS: usize = 10;
/// The longest name a title gives, in characters.
const MAX_NAME_CHARS: usize = 200;
/// Titles longer than this are no birthday entries.
const MAX_TITLE_CHARS: usize = 300;

// ------------------------------------------------------------------------------------------------
// Reading events

/// A birthday found in an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundBirthday {
    pub name: String,
    pub date: PartialDate,
    /// The event says it came from an address book (Google, KDE, a category, ...).
    pub marked: bool,
}

/// Words of a title that say "birthday", in the forms people write them, after [`fold`].
const BIRTHDAY_WORDS: &[&str] = &[
    "geburtstag",
    "geburtstage",
    "geb",
    "geburtsdatum",
    "birthday",
    "bday",
    "b day",
    "bd",
    "hbd",
    "cumpleanos",
    "anniversaire",
    "verjaardag",
    "compleanno",
];
/// Words around the name that are not part of it.
const FILLER_WORDS: &[&str] =
    &["von", "vom", "der", "des", "hat", "of", "happy", "alles", "gute", "zum", "has", "is", "de", "van", "di", "la"];
/// Pictures people put into birthday titles.
const BIRTHDAY_SIGNS: &[char] = &['🎂', '🎉', '🎈', '🎁', '🥳', '🍰', '🧁', '🎊'];

/// Lower case, umlauts spelled out (`ü` → `ue`), accents dropped, everything that is no letter
/// or digit a space, spaces single.
pub fn fold(text: &str) -> String {
    fold_with(text, true)
}

/// As [`fold`], with umlauts losing their dots instead (`ü` → `u`).
pub fn fold_plain(text: &str) -> String {
    fold_with(text, false)
}

fn fold_with(text: &str, spell_out: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        let mapped: &str = match c {
            'ä' if spell_out => "ae",
            'ö' if spell_out => "oe",
            'ü' if spell_out => "ue",
            'ä' | 'à' | 'á' | 'â' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => "a",
            'ö' | 'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'ō' | 'ő' => "o",
            'ü' | 'ù' | 'ú' | 'û' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
            'ß' => "ss",
            'æ' => "ae",
            'œ' => "oe",
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
            'ç' | 'ć' | 'č' => "c",
            'ñ' | 'ń' | 'ň' => "n",
            'ł' => "l",
            'ś' | 'š' | 'ş' | 'ș' => "s",
            'ź' | 'ż' | 'ž' => "z",
            'ř' => "r",
            'ý' | 'ÿ' => "y",
            'đ' | 'ď' => "d",
            'ğ' => "g",
            'ť' | 'ț' => "t",
            'þ' => "th",
            c if c.is_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => " ",
        };
        out.push_str(mapped);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A four-digit year a title gives ("(*1990)", "1990", "b. 1990"), if it is a plausible one.
fn year_in(tokens: &[String]) -> Option<(usize, i32)> {
    tokens.iter().enumerate().find_map(|(i, token)| {
        let digits: String = token.chars().filter(char::is_ascii_digit).collect();
        let only_year = token.chars().all(|c| c.is_ascii_digit() || "()*.,:;-–[]".contains(c));
        (only_year && digits.chars().count() == 4)
            .then(|| digits.parse::<i32>().ok())
            .flatten()
            .filter(|year| (1850..=2100).contains(year))
            .map(|year| (i, year))
    })
}

/// Reads the name, and maybe the year, from a birthday title. `None` when the title says nothing
/// of a birthday and `marked` does not either, or leaves no name.
pub fn name_from_title(title: &str, marked: bool) -> Option<(String, Option<i32>)> {
    if title.chars().count() > MAX_TITLE_CHARS {
        return None;
    }
    let signed = title.chars().any(|c| BIRTHDAY_SIGNS.contains(&c));
    let cleaned: String = title.chars().map(|c| if BIRTHDAY_SIGNS.contains(&c) { ' ' } else { c }).collect();
    // "Max's birthday", "Max’ Geburtstag": the possessive goes.
    let cleaned = cleaned.replace(['’', '`', '´'], "'");
    let mut tokens: Vec<String> = cleaned.split_whitespace().map(str::to_owned).collect();
    let year = year_in(&tokens).map(|(i, year)| {
        tokens.remove(i);
        year
    });
    let mut said = signed;
    let mut name: Vec<String> = Vec::new();
    for token in tokens {
        let folded = fold(&token);
        // "b-day" folds to "b day"; "Geburtstag:" to "geburtstag".
        if BIRTHDAY_WORDS.contains(&folded.as_str()) || folded.split(' ').any(|word| word == "geburtstag") {
            said = true;
            continue;
        }
        if folded.is_empty()
            || FILLER_WORDS.contains(&folded.as_str())
            || (folded.chars().all(|c| c.is_ascii_digit()) && folded.chars().count() <= 3)
            || token.starts_with('*')
            || folded == "b"
            || folded == "s"
        {
            continue;
        }
        // Brackets and punctuation around a word go; the possessive too.
        let mut word: String = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-').to_owned();
        for suffix in ["'s", "'S", "'"] {
            if let Some(stripped) = word.strip_suffix(suffix) {
                word = stripped.to_owned();
                break;
            }
        }
        let word = word.trim_matches(|c: char| !c.is_alphanumeric()).to_owned();
        if !word.is_empty() {
            name.push(word);
        }
    }
    if !said && !marked {
        return None;
    }
    let name: String = name.join(" ").chars().take(MAX_NAME_CHARS).collect();
    (!name.is_empty()).then_some((name, year))
}

/// Whether an event says it is a birthday on its own: a property named like one (KDE's
/// `X-KDE-KABC-BIRTHDAY`), a category, or a uid like Google's.
fn is_marked(event: &Component) -> bool {
    let says = |text: &str| {
        let folded = fold(text);
        folded.contains("birthday") || folded.contains("geburtstag")
    };
    event.properties.iter().any(|p| {
        (p.name.starts_with("X-") && p.name.contains("BIRTHDAY") && !p.name.starts_with("X-UWUMAIL"))
            || (p.name == "CATEGORIES" && says(&p.value))
            || (p.name == "UID" && p.value.to_ascii_lowercase().contains("birthday"))
    })
}

/// The birthday an event keeps, if it is one: all day, yearly (or marked), and a title that
/// names someone. The year comes from the title, or from the start of a marked event, which
/// address books write with the year of birth.
pub fn birthday_in_event(content: &str) -> Option<FoundBirthday> {
    let calendar = Component::parse(content)?;
    let event = calendar.main_event()?;
    if event.property("X-UWUMAIL-BIRTHDAY").is_some() {
        return None;
    }
    let start = event.property("DTSTART")?;
    let all_day = start.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
        || (start.value.trim().chars().count() == 8 && start.value.trim().chars().all(|c| c.is_ascii_digit()));
    if !all_day {
        return None;
    }
    let marked = is_marked(event);
    let yearly = event.value("RRULE").is_some_and(|rule| {
        rule.split(';').any(|part| part.trim().eq_ignore_ascii_case("FREQ=YEARLY"))
            && !rule.split(';').any(|part| {
                part.split_once('=')
                    .is_some_and(|(key, value)| key.eq_ignore_ascii_case("INTERVAL") && value.trim() != "1")
            })
    });
    if !yearly && !marked {
        return None;
    }
    let title = crate::birthdays::unescape_text(event.value("SUMMARY")?);
    let (name, title_year) = name_from_title(&title, marked)?;
    let value = start.value.trim();
    let digits: String = value.chars().take(8).collect();
    let started = PartialDate::new(
        digits.get(0..4)?.parse().ok(),
        digits.get(4..6)?.parse().ok()?,
        digits.get(6..8)?.parse().ok()?,
    )?;
    let this_year = chrono::Utc::now().format("%Y").to_string().parse::<i32>().unwrap_or(2100);
    let start_year = started.year.filter(|year| marked && *year > 1900 && *year <= this_year && *year != 1604);
    let year = title_year.or(start_year);
    let date =
        PartialDate::new(year, started.month, started.day).or(PartialDate::new(None, started.month, started.day))?;
    Some(FoundBirthday { name, date, marked })
}

// ------------------------------------------------------------------------------------------------
// Matching contacts

/// A name as the keys it is compared by: spelled-out and plain umlauts, whole and in words.
#[derive(Debug, Clone)]
struct NameKeys {
    whole: [String; 2],
    words: Vec<[String; 2]>,
}

impl NameKeys {
    fn of(name: &str) -> NameKeys {
        let (spelled, plain) = (fold(name), fold_plain(name));
        let words = spelled.split(' ').zip(plain.split(' ')).map(|(a, b)| [a.to_owned(), b.to_owned()]).collect();
        NameKeys { whole: [spelled, plain], words }
    }

    fn same_as(&self, other: &NameKeys) -> bool {
        !self.whole[0].is_empty() && self.whole.iter().any(|key| other.whole.contains(key))
    }

    /// Every word of `self` is a word of `other`.
    fn words_in(&self, other: &NameKeys) -> bool {
        !self.words.is_empty()
            && self.words.iter().all(|word| other.words.iter().any(|theirs| word.iter().any(|w| theirs.contains(w))))
    }
}

/// A contact that may be the person of a birthday.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardChoice {
    pub card_id: i64,
    pub address_book_id: i64,
    pub name: String,
    /// The birthday the card has already.
    pub birthday: Option<PartialDate>,
}

struct KnownCard {
    choice: CardChoice,
    /// Its name, and the others it goes by (given + surname either way round, nicknames).
    names: Vec<NameKeys>,
}

fn known_card(card_id: i64, address_book_id: i64, content: &str) -> Option<KnownCard> {
    let card = calcard::vcard::VCard::parse(content).ok()?;
    let name = card_name(&card);
    if name.is_empty() {
        return None;
    }
    let mut names = vec![NameKeys::of(&name)];
    use calcard::vcard::{VCardProperty, VCardValue};
    let text = |value: Option<&VCardValue>| match value {
        Some(VCardValue::Text(text)) => text.clone(),
        Some(VCardValue::Component(parts)) => parts.join(" "),
        _ => String::new(),
    };
    for entry in &card.entries {
        if entry.name == VCardProperty::N {
            let (surname, given) = (text(entry.values.first()), text(entry.values.get(1)));
            if !surname.is_empty() && !given.is_empty() {
                names.push(NameKeys::of(&format!("{given} {surname}")));
                names.push(NameKeys::of(&format!("{surname} {given}")));
            }
        }
        if entry.name == VCardProperty::Nickname {
            for value in &entry.values {
                let nick = text(Some(value));
                if !nick.trim().is_empty() {
                    names.push(NameKeys::of(&nick));
                }
            }
        }
    }
    let birthday = card_dates(content)
        .and_then(|dates| dates.dates.into_iter().find(|d| d.kind == crate::birthdays::DateKind::Birth))
        .map(|d| d.date);
    Some(KnownCard { choice: CardChoice { card_id, address_book_id, name, birthday }, names })
}

/// The cards `name` may mean: those with exactly that name when there are any, else those
/// whose names hold every word of it ("Max" for "Max Muster", "Oma" for a nickname).
fn matching<'a>(name: &str, cards: &'a [KnownCard]) -> Vec<&'a KnownCard> {
    let wanted = NameKeys::of(name);
    let exact: Vec<&KnownCard> = cards.iter().filter(|card| card.names.iter().any(|n| wanted.same_as(n))).collect();
    if !exact.is_empty() {
        return exact;
    }
    cards.iter().filter(|card| card.names.iter().any(|n| wanted.words_in(n))).collect()
}

/// What a scan found for one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchState {
    /// One contact, without a birthday: moved without asking.
    Matched,
    /// One contact that has this very birthday: only the event is left to delete.
    Known,
    /// One contact with another birthday: the person decides.
    Conflict,
    /// Several contacts: the person picks one.
    Ambiguous,
    /// None: a new contact, or another one the person picks.
    Unmatched,
}

impl MatchState {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchState::Matched => "matched",
            MatchState::Known => "known",
            MatchState::Conflict => "conflict",
            MatchState::Ambiguous => "ambiguous",
            MatchState::Unmatched => "unmatched",
        }
    }
}

/// A birthday event found in one of the account's calendars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BirthdayCandidate {
    pub event_id: i64,
    pub calendar_id: i64,
    pub title: String,
    pub found: FoundBirthday,
    /// The event can be deleted once its birthday is in a contact; not so in a subscribed
    /// calendar or one shared for reading.
    pub deletable: bool,
    pub state: MatchState,
    /// The contacts it may belong to, the one of a match first.
    pub choices: Vec<CardChoice>,
}

/// The card has this birthday already: the same day, and the year when the event knows one.
fn same_birthday(known: &PartialDate, found: &PartialDate) -> bool {
    known.month == found.month && known.day == found.day && (found.year.is_none() || known.year == found.year)
}

/// The event only adds the year to the day the card has.
fn adds_year(known: &PartialDate, found: &PartialDate) -> bool {
    known.month == found.month && known.day == found.day && known.year.is_none() && found.year.is_some()
}

fn candidate_state(found: &FoundBirthday, choices: &[&KnownCard]) -> MatchState {
    match choices {
        [] => MatchState::Unmatched,
        [one] => match &one.choice.birthday {
            None => MatchState::Matched,
            Some(known) if same_birthday(known, &found.date) => MatchState::Known,
            Some(known) if adds_year(known, &found.date) => MatchState::Matched,
            Some(_) => MatchState::Conflict,
        },
        _ => MatchState::Ambiguous,
    }
}

/// The cards of the account's own address books, and those shared with it for writing.
fn writable_cards(conn: &Connection, account_id: i64) -> Result<Vec<KnownCard>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT r.id, r.collection_id, r.content FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
         WHERE {VISIBLE} AND c.kind = 'addressbook' AND r.component = 'VCARD'"
    ))?;
    let rows = stmt
        .query_map([account_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)))?;
    let mut cards = Vec::new();
    let mut writable: std::collections::HashMap<i64, bool> = Default::default();
    for row in rows {
        let (id, book, content) = row?;
        let may = match writable.get(&book) {
            Some(may) => *may,
            None => {
                let may = crate::sharing::writable(conn, account_id, book).is_ok();
                writable.insert(book, may);
                may
            }
        };
        if !may || is_group(&content) {
            continue;
        }
        if let Some(card) = known_card(id, book, &content) {
            cards.push(card);
        }
    }
    Ok(cards)
}

fn is_group(content: &str) -> bool {
    content.lines().any(|line| {
        let line = line.trim_end_matches('\r').to_ascii_uppercase();
        line == "KIND:GROUP" || line == "X-ADDRESSBOOKSERVER-KIND:GROUP"
    })
}

// ------------------------------------------------------------------------------------------------
// Writing

/// What to do with the birthday of one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BirthdayTarget {
    /// Into this card; `overwrite` replaces another birthday it has.
    Card { card_id: i64, overwrite: bool },
    /// Into a new card with this name, in this address book (the default one without).
    NewCard { name: String, address_book_id: Option<i64> },
}

/// What moving one birthday did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BirthdayMoved {
    pub card_id: i64,
    pub created: bool,
    /// The event went; `false` when its calendar cannot lose it (subscribed, shared to read) or
    /// the person wanted to keep it.
    pub event_deleted: bool,
}

/// vCard TEXT escaping.
fn escape_vcard(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The BDAY value for a card of this vCard version.
fn bday_value(date: &PartialDate, version4: bool) -> String {
    match (date.year, version4) {
        (Some(year), true) => format!("{year:04}{:02}{:02}", date.month, date.day),
        (Some(year), false) => format!("{year:04}-{:02}-{:02}", date.month, date.day),
        (None, true) => format!("--{:02}{:02}", date.month, date.day),
        (None, false) => format!("--{:02}-{:02}", date.month, date.day),
    }
}

/// The card with this birthday in place of any it had; everything else stays as written.
pub fn with_birthday(content: &str, date: &PartialDate) -> Option<String> {
    let version4 = content.lines().any(|line| line.trim_end_matches('\r').eq_ignore_ascii_case("VERSION:4.0"));
    let mut out = String::with_capacity(content.len() + 32);
    let mut skipping = false;
    let mut ended = false;
    for line in content.split_inclusive('\n') {
        if skipping && line.starts_with([' ', '\t']) {
            continue;
        }
        skipping = false;
        let head = line.split([':', ';']).next().unwrap_or_default();
        let name = head.rsplit('.').next().unwrap_or_default().trim();
        if name.eq_ignore_ascii_case("BDAY") {
            skipping = true;
            continue;
        }
        if !ended && line.trim_end().eq_ignore_ascii_case("END:VCARD") {
            out.push_str(&format!("BDAY:{}\r\n", bday_value(date, version4)));
            ended = true;
        }
        out.push_str(line);
    }
    ended.then_some(out)
}

/// A new card of this name and birthday, as vCard 3.0.
fn new_card(name: &str, date: &PartialDate) -> (String, String) {
    let uid = format!("urn:uuid:{}", uuid_v4());
    let words: Vec<&str> = name.split_whitespace().collect();
    let (given, surname) = match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => (rest.join(" "), (*last).to_owned()),
        _ => (name.to_owned(), String::new()),
    };
    let content = format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{}\r\nN:{};{};;;\r\nBDAY:{}\r\nEND:VCARD\r\n",
        escape_vcard(name),
        escape_vcard(&surname),
        escape_vcard(&given),
        bday_value(date, false)
    );
    (uid, content)
}

fn uuid_v4() -> String {
    let mut bytes = crate::random_bytes::<16>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    let part = |from: usize, to: usize| hex.get(from..to).unwrap_or_default().to_owned();
    format!("{}-{}-{}-{}-{}", part(0, 8), part(8, 12), part(12, 16), part(16, 20), part(20, 32))
}

fn not_found(what: &str) -> StoreError {
    StoreError::NotFound(what.into())
}

fn move_one(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    account_id: i64,
    event_id: i64,
    target: &BirthdayTarget,
    delete_event: bool,
) -> Result<BirthdayMoved> {
    let (calendar_id, event_name, event_etag, content): (i64, String, String, String) = tx
        .query_row(
            &format!(
                "SELECT r.collection_id, r.name, r.etag, r.content FROM dav_resources r
                 JOIN dav_collections c ON c.id = r.collection_id
                 WHERE r.id = ?2 AND {VISIBLE} AND c.kind = 'calendar' AND r.component = 'VEVENT'"
            ),
            params![account_id, event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| not_found("event"))?;
    let found = birthday_in_event(&content).ok_or_else(|| StoreError::Rule {
        code: "notABirthday",
        message: "the event is no birthday this server can read".into(),
    })?;
    let (card_id, created) = match target {
        BirthdayTarget::Card { card_id, overwrite } => {
            let (book_id, name, etag, card): (i64, String, String, String) = tx
                .query_row(
                    &format!(
                        "SELECT r.collection_id, r.name, r.etag, r.content FROM dav_resources r
                         JOIN dav_collections c ON c.id = r.collection_id
                         WHERE r.id = ?2 AND {VISIBLE} AND c.kind = 'addressbook' AND r.component = 'VCARD'"
                    ),
                    params![account_id, card_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
                .ok_or_else(|| not_found("contact"))?;
            let book = writable_book(tx, account_id, book_id)?;
            let known = card_dates(&card)
                .and_then(|dates| dates.dates.into_iter().find(|d| d.kind == crate::birthdays::DateKind::Birth))
                .map(|d| d.date);
            let date = match known {
                // The card knows the year the event does not: it stays.
                Some(known) if same_birthday(&known, &found.date) => None,
                Some(known) if adds_year(&known, &found.date) => Some(found.date),
                Some(_) if !overwrite => {
                    return Err(StoreError::Rule {
                        code: "birthdayExists",
                        message: "the contact has another birthday; overwrite it or pick another contact".into(),
                    });
                }
                _ => Some(found.date),
            };
            if let Some(date) = date {
                let changed = with_birthday(&card, &date).ok_or_else(|| StoreError::Rule {
                    code: "invalidCard",
                    message: "the contact's vCard cannot be changed".into(),
                })?;
                if changed.len() > DAV_RESOURCE_MAX_BYTES {
                    return Err(StoreError::QuotaExceeded);
                }
                let uid: String =
                    tx.query_row("SELECT uid FROM dav_resources WHERE id = ?1", [card_id], |r| r.get(0))?;
                let write =
                    DavWrite { name, content: changed, uid, component: "VCARD".into(), starts_at: None, ends_at: None };
                let condition = DavPrecondition { if_match: Some(etag), ..Default::default() };
                match put_entry(tx, log, &book, &write, &condition)? {
                    (DavWriteOutcome::Updated { .. }, _) => {}
                    other => return Err(StoreError::Conflict(format!("the contact changed meanwhile: {other:?}"))),
                }
            }
            (*card_id, false)
        }
        BirthdayTarget::NewCard { name, address_book_id } => {
            let name: String = name.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
            let name = name.trim();
            if name.is_empty() {
                return Err(StoreError::Rule { code: "invalidName", message: "a new contact needs a name".into() });
            }
            let book_id = match address_book_id {
                Some(id) => *id,
                None => tx
                    .query_row(
                        "SELECT id FROM dav_collections WHERE account_id = ?1 AND kind = 'addressbook'
                         ORDER BY is_default DESC, sort_order, id LIMIT 1",
                        [account_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| not_found("address book"))?,
            };
            let book = writable_book(tx, account_id, book_id)?;
            let (uid, content) = new_card(name, &found.date);
            let write = DavWrite {
                name: new_entry_name(tx, book.id, &uid, "vcf")?,
                content,
                uid,
                component: "VCARD".into(),
                starts_at: None,
                ends_at: None,
            };
            let condition = DavPrecondition { if_none_match_any: true, ..Default::default() };
            match put_entry(tx, log, &book, &write, &condition)? {
                (DavWriteOutcome::Created { .. }, Some(id)) => (id, true),
                other => return Err(StoreError::Internal(format!("a new contact could not be stored: {other:?}"))),
            }
        }
    };
    let mut event_deleted = false;
    if delete_event
        && let Ok(calendar) = crate::sharing::writable(tx, account_id, calendar_id)
        && calendar.kind == DavKind::Calendar
    {
        if !delete_entry(tx, log, &calendar, &event_name, Some(&event_etag))? {
            return Err(StoreError::Conflict("the event changed meanwhile".into()));
        }
        event_deleted = true;
    }
    Ok(BirthdayMoved { card_id, created, event_deleted })
}

fn writable_book(tx: &Transaction<'_>, account_id: i64, book_id: i64) -> Result<DavCollection> {
    let book = crate::sharing::writable(tx, account_id, book_id)?;
    if book.kind != DavKind::Addressbook {
        return Err(not_found("address book"));
    }
    Ok(book)
}

impl Store {
    /// The birthday events of the calendars the account sees (not the birthdays calendar), with
    /// the contacts each may belong to. Returns them and whether there were more than it looked at.
    pub async fn scan_birthday_events(&self, account_id: i64) -> Result<(Vec<BirthdayCandidate>, bool)> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT r.id, r.collection_id, r.content FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                 WHERE {VISIBLE} AND c.kind = 'calendar' AND r.component = 'VEVENT'
                   AND NOT EXISTS (SELECT 1 FROM birthday_calendars b WHERE b.collection_id = c.id)
                   AND (r.content LIKE '%YEARLY%' OR r.content LIKE '%BIRTHDAY%' OR r.content LIKE '%GEBURTSTAG%')
                 ORDER BY r.id LIMIT ?2"
            ))?;
            let rows: Vec<(i64, i64, String)> = stmt
                .query_map(params![account_id, MAX_SCANNED_EVENTS as i64 + 1], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let mut truncated = rows.len() > MAX_SCANNED_EVENTS;
            let mut found: Vec<(i64, i64, String, FoundBirthday)> = Vec::new();
            for (id, calendar, content) in rows.into_iter().take(MAX_SCANNED_EVENTS) {
                if let Some(birthday) = birthday_in_event(&content) {
                    let title = Component::parse(&content)
                        .and_then(|c| c.main_event().and_then(|e| e.value("SUMMARY")).map(crate::birthdays::unescape_text))
                        .unwrap_or_default();
                    found.push((id, calendar, title.chars().take(MAX_NAME_CHARS).collect(), birthday));
                }
                if found.len() >= MAX_CANDIDATES {
                    truncated = true;
                    break;
                }
            }
            if found.is_empty() {
                return Ok((Vec::new(), truncated));
            }
            let cards = writable_cards(conn, account_id)?;
            let mut deletable: std::collections::HashMap<i64, bool> = Default::default();
            let mut candidates = Vec::new();
            for (event_id, calendar_id, title, birthday) in found {
                let may_delete = *deletable
                    .entry(calendar_id)
                    .or_insert_with(|| crate::sharing::writable(conn, account_id, calendar_id).is_ok());
                let choices = matching(&birthday.name, &cards);
                let state = candidate_state(&birthday, &choices);
                candidates.push(BirthdayCandidate {
                    event_id,
                    calendar_id,
                    title,
                    found: birthday,
                    deletable: may_delete,
                    state,
                    choices: choices.iter().take(MAX_OPTIONS).map(|card| card.choice.clone()).collect(),
                });
            }
            Ok((candidates, truncated))
        })
        .await
    }

    /// Writes the birthday of an event into a contact and then deletes the event, when
    /// `delete_event` and its calendar allows it — both or neither.
    pub async fn move_birthday(
        &self,
        account_id: i64,
        event_id: i64,
        target: BirthdayTarget,
        delete_event: bool,
    ) -> Result<BirthdayMoved> {
        let (moved, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::by(account_id, crate::Author::Account);
                let moved = move_one(tx, &mut log, account_id, event_id, &target, delete_event)?;
                Ok((moved, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(moved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> Option<(String, Option<i32>)> {
        name_from_title(text, false)
    }

    #[test]
    fn titles_in_every_spelling() {
        let name = |n: &str| Some((n.to_owned(), None));
        assert_eq!(title("Geburtstag von Max Müller"), name("Max Müller"));
        assert_eq!(title("Geburtstag: Max Müller"), name("Max Müller"));
        assert_eq!(title("Max Müller Geburtstag"), name("Max Müller"));
        assert_eq!(title("Maxens Geburtstag"), name("Maxens"));
        assert_eq!(title("Geb. Oma Änne"), name("Oma Änne"));
        assert_eq!(title("Max's birthday"), name("Max"));
        assert_eq!(title("Max’s Birthday!"), name("Max"));
        assert_eq!(title("Birthday of Leni Muster"), name("Leni Muster"));
        assert_eq!(title("bday Nyu"), name("Nyu"));
        assert_eq!(title("🎂 Nyu Katze"), name("Nyu Katze"));
        assert_eq!(title("Nyu Katze 🎂"), name("Nyu Katze"));
        assert_eq!(title("Happy Birthday Ada"), name("Ada"));
        assert_eq!(title("Geburtstag Max (*1990)"), Some(("Max".into(), Some(1990))));
        assert_eq!(title("Max Muster (1990) Geburtstag"), Some(("Max Muster".into(), Some(1990))));
        assert_eq!(title("Max Muster (30) 🎂"), name("Max Muster"), "an age is no year");
        assert_eq!(title("Geburtstagsfeier Max"), None, "a party is no birthday");
        assert_eq!(title("Zahnarzt"), None);
        assert_eq!(title("Geburtstag"), None, "no name");
        assert_eq!(name_from_title("Max Muster", true), name("Max Muster"), "marked events need no word");
        assert_eq!(title("李小龙的生日 🎂"), name("李小龙的生日"), "other scripts are kept whole");
        assert_eq!(title(&"Geburtstag ".repeat(100)), None);
    }

    #[test]
    fn names_fold_by_characters() {
        assert_eq!(fold("Müller-Lüdenscheidt"), "mueller luedenscheidt");
        assert_eq!(fold_plain("Müller"), "muller");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("Zoë Łukasz"), "zoe lukasz");
        assert_eq!(fold("ÄÖÜ"), "aeoeue");
        assert_eq!(fold("İstanbul").chars().count(), fold("İstanbul").chars().count(), "no panic on odd lower cases");
    }

    fn event(summary: &str, start: &str, extra: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:u1\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART;VALUE=DATE:{start}\r\nSUMMARY:{summary}\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[test]
    fn events_are_birthdays_when_yearly_and_all_day() {
        let found = birthday_in_event(&event("Geburtstag von Max", "20100412", "RRULE:FREQ=YEARLY\r\n")).unwrap();
        assert_eq!(found.name, "Max");
        assert_eq!(
            found.date,
            PartialDate::new(None, 4, 12).unwrap(),
            "the start year of a plain event is no birth year"
        );
        assert!(!found.marked);
        let marked = event("Max Muster", "19900412", "RRULE:FREQ=YEARLY\r\nX-KDE-KABC-BIRTHDAY:YES\r\n");
        assert_eq!(birthday_in_event(&marked).unwrap().date, PartialDate::new(Some(1990), 4, 12).unwrap());
        let google = event("Max Muster", "19900412", "RRULE:FREQ=YEARLY\r\nCATEGORIES:Birthday\r\n");
        assert_eq!(birthday_in_event(&google).unwrap().date.year, Some(1990));
        // Leap days stay leap days, without a year too.
        let leap = event("Lea's birthday", "20000229", "RRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1\r\n");
        assert_eq!(birthday_in_event(&leap).unwrap().date, PartialDate::new(None, 2, 29).unwrap());
        // Not a birthday: a time of day, not yearly, every other year, our own.
        let timed = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20100412T100000Z\r\nRRULE:FREQ=YEARLY\r\n\
SUMMARY:Geburtstag Max\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(birthday_in_event(timed).is_none());
        assert!(birthday_in_event(&event("Geburtstag Max", "20100412", "")).is_none());
        assert!(birthday_in_event(&event("Geburtstag Max", "20100412", "RRULE:FREQ=YEARLY;INTERVAL=2\r\n")).is_none());
        let own = event("Max", "20100412", "RRULE:FREQ=YEARLY\r\nX-UWUMAIL-BIRTHDAY;X-CARD=1;X-KIND=birth:Max\r\n");
        assert!(birthday_in_event(&own).is_none());
    }

    fn card(id: i64, content: &str) -> KnownCard {
        known_card(id, 1, content).unwrap()
    }

    #[test]
    fn contacts_match_by_name() {
        let cards = vec![
            card(1, "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Max Müller\r\nN:Müller;Max;;;\r\nUID:1\r\nEND:VCARD\r\n"),
            card(2, "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Max Schmidt\r\nUID:2\r\nEND:VCARD\r\n"),
            card(
                3,
                "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Änne Groß\r\nNICKNAME:Oma\r\nUID:3\r\nBDAY:1940-02-29\r\nEND:VCARD\r\n",
            ),
        ];
        let ids = |name: &str| matching(name, &cards).iter().map(|c| c.choice.card_id).collect::<Vec<_>>();
        assert_eq!(ids("Max Müller"), vec![1]);
        assert_eq!(ids("Max Mueller"), vec![1], "umlauts spelled out");
        assert_eq!(ids("max muller"), vec![1], "umlauts without dots");
        assert_eq!(ids("Müller Max"), vec![1], "surname first");
        assert_eq!(ids("Max"), vec![1, 2]);
        assert_eq!(ids("Oma"), vec![3], "nickname");
        assert_eq!(ids("Anne Gross"), vec![3]);
        assert!(ids("Leni").is_empty());
        let found =
            |y, m, d| FoundBirthday { name: String::new(), date: PartialDate::new(y, m, d).unwrap(), marked: false };
        let oma = [&cards[2]];
        assert_eq!(candidate_state(&found(None, 2, 29), &oma), MatchState::Known);
        assert_eq!(candidate_state(&found(Some(1940), 2, 29), &oma), MatchState::Known);
        assert_eq!(candidate_state(&found(None, 3, 1), &oma), MatchState::Conflict);
        assert_eq!(candidate_state(&found(None, 3, 1), &[&cards[0]]), MatchState::Matched);
        let yearless = card(4, "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Nyu\r\nUID:4\r\nBDAY:--03-01\r\nEND:VCARD\r\n");
        assert_eq!(candidate_state(&found(Some(2001), 3, 1), &[&yearless]), MatchState::Matched, "adds the year");
        assert_eq!(candidate_state(&found(None, 3, 1), &[&cards[0], &cards[1]]), MatchState::Ambiguous);
    }

    #[test]
    fn birthdays_go_into_cards_as_written() {
        let v3 = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Max\r\nBDAY;X-APPLE-OMIT-YEAR=1604:\r\n 1604-01-01\r\nNOTE:bleibt\r\nEND:VCARD\r\n";
        let out = with_birthday(v3, &PartialDate::new(None, 4, 12).unwrap()).unwrap();
        assert_eq!(out, "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Max\r\nNOTE:bleibt\r\nBDAY:--04-12\r\nEND:VCARD\r\n");
        let v4 = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Max\r\nEND:VCARD\r\n";
        let out = with_birthday(v4, &PartialDate::new(Some(1990), 4, 12).unwrap()).unwrap();
        assert!(out.contains("BDAY:19900412\r\nEND:VCARD"), "{out}");
        assert_eq!(
            crate::birthdays::card_dates(&out).unwrap().dates[0].date,
            PartialDate::new(Some(1990), 4, 12).unwrap()
        );
        let (_, fresh) = new_card("Ada, die; Erste", &PartialDate::new(Some(1815), 12, 10).unwrap());
        assert!(fresh.contains("FN:Ada\\, die\\; Erste\r\n") && fresh.contains("BDAY:1815-12-10"), "{fresh}");
        assert!(calcard::vcard::VCard::parse(&fresh).is_ok());
        let yearless = with_birthday(v3, &PartialDate::new(None, 2, 29).unwrap()).unwrap();
        assert_eq!(
            crate::birthdays::card_dates(&yearless).unwrap().dates[0].date,
            PartialDate::new(None, 2, 29).unwrap()
        );
    }

    mod stored {
        use super::super::*;
        use crate::test_support::store;
        use crate::{NewAccount, NewDavCollection, Role};

        fn write(name: &str, uid: &str, component: &str, content: String) -> DavWrite {
            DavWrite {
                name: name.into(),
                content,
                uid: uid.into(),
                component: component.into(),
                starts_at: None,
                ends_at: None,
            }
        }

        #[tokio::test]
        async fn birthdays_move_from_calendars_into_contacts() {
            let (store, _dir) = store().await;
            store.create_domain("example.org").await.ok();
            let account = NewAccount {
                address: "mini@example.org".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            };
            let mini = store.create_account(account).await.unwrap().id;
            let book =
                NewDavCollection { slug: "contacts".into(), display_name: "Kontakte".into(), ..Default::default() };
            let book = store.dav_collections(mini, DavKind::Addressbook, book).await.unwrap()[0].clone();
            let calendar = NewDavCollection::default_calendar("Kalender");
            let calendar = store.dav_collections(mini, DavKind::Calendar, calendar).await.unwrap()[0].clone();
            let max = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:max\r\nFN:Max Müller\r\nEND:VCARD\r\n".to_owned();
            store
                .dav_put(mini, book.id, write("max.vcf", "max", "VCARD", max), DavPrecondition::default())
                .await
                .unwrap();
            let oma = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:oma\r\nFN:Änne\r\nBDAY:1940-01-01\r\nEND:VCARD\r\n".to_owned();
            store
                .dav_put(mini, book.id, write("oma.vcf", "oma", "VCARD", oma), DavPrecondition::default())
                .await
                .unwrap();
            let event = |uid: &str, summary: &str, start: &str| {
                format!(
                    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
DTSTAMP:20260101T000000Z\r\nDTSTART;VALUE=DATE:{start}\r\nRRULE:FREQ=YEARLY\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
                )
            };
            for (uid, summary, start) in [
                ("e1", "Geburtstag von Max Mueller (*1996)", "20100412"),
                ("e2", "🎂 Leni", "20000229"),
                ("e3", "Änne's birthday", "20100505"),
                ("e4", "Zahnarzt", "20100505"),
            ] {
                let put = write(&format!("{uid}.ics"), uid, "VEVENT", event(uid, summary, start));
                store.dav_put(mini, calendar.id, put, DavPrecondition::default()).await.unwrap();
            }
            let (found, truncated) = store.scan_birthday_events(mini).await.unwrap();
            assert!(!truncated);
            assert_eq!(found.len(), 3, "{found:?}");
            let by_title = |t: &str| found.iter().find(|c| c.title.starts_with(t)).unwrap().clone();
            let max = by_title("Geburtstag");
            assert_eq!(max.state, MatchState::Matched);
            assert_eq!(max.found.date, PartialDate::new(Some(1996), 4, 12).unwrap());
            assert!(max.deletable);
            let leni = by_title("🎂");
            assert_eq!((leni.state.clone(), leni.found.name.as_str()), (MatchState::Unmatched, "Leni"));
            let oma = by_title("Änne");
            assert_eq!(oma.state, MatchState::Conflict);

            // A conflict is not overwritten unasked, and the event stays.
            let target = BirthdayTarget::Card { card_id: oma.choices[0].card_id, overwrite: false };
            let refused = store.move_birthday(mini, oma.event_id, target, true).await;
            assert!(matches!(refused, Err(StoreError::Rule { code: "birthdayExists", .. })), "{refused:?}");
            assert_eq!(store.scan_birthday_events(mini).await.unwrap().0.len(), 3, "nothing deleted on failure");

            let target = BirthdayTarget::Card { card_id: max.choices[0].card_id, overwrite: false };
            let moved = store.move_birthday(mini, max.event_id, target, true).await.unwrap();
            assert!(moved.event_deleted && !moved.created);
            let card = store.contact_cards(mini, None).await.unwrap().into_iter().find(|c| c.uid == "max").unwrap();
            assert!(card.content.contains("BDAY:1996-04-12"), "{}", card.content);
            let target = BirthdayTarget::NewCard { name: leni.found.name.clone(), address_book_id: None };
            let moved = store.move_birthday(mini, leni.event_id, target, true).await.unwrap();
            assert!(moved.created && moved.event_deleted);
            let (left, _) = store.scan_birthday_events(mini).await.unwrap();
            assert_eq!(left.len(), 1);
            // And the birthdays calendar has them now.
            let birthdays = store.birthday_calendar(mini).await.unwrap().unwrap();
            let events = store.dav_resource_contents(mini, birthdays.id, None).await.unwrap();
            assert_eq!(events.len(), 3, "Max, Leni and Änne");
            assert!(
                events.iter().any(|e| e.content.contains("SUMMARY:Leni\r\n") && e.content.contains("BYMONTHDAY=-1"))
            );
        }
    }
}
