//! The built-in calendars (migration 0091, docs/holidays.md): the public holidays of Germany (with
//! its 16 states), Austria or Switzerland (with a rough set per canton), and the ISO 8601 week
//! numbers. The settings `holidays` and `weekNumbers` turn them on and off; everything is worked
//! out here, without asking anyone on the network.
//!
//! Each holiday of each year, and each week, is one all-day event, from the year before the current
//! one to three years ahead. Like the birthdays calendar, only the server writes them: CalDAV and
//! JMAP read them, they never count as busy, never become the default calendar and ring no alarms.
//! When the year turns, the hourly job writes the next year in and the oldest one out.

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::birthdays::{BirthdayLanguage, Derived, language_of, write_card_events};
use crate::dav::{
    ChangeLog, DavCollection, DavKind, NewDavCollection, delete_collection, insert_collection, own_collection,
};
use crate::itip::{Component, Property};
use crate::{Result, Store};

const HOLIDAYS: &str = "holidays";
const WEEKS: &str = "weeks";
const HOLIDAYS_COLOR: &str = "#D0021BFF";
const WEEKS_COLOR: &str = "#9B9B9BFF";
/// Years before and after the current one that get events.
const YEARS_BEFORE: i32 = 1;
const YEARS_AFTER: i32 = 3;

/// The countries with holidays, as the setting names them, with their German and English names.
pub const COUNTRIES: &[(&str, &str, &str)] =
    &[("DE", "Deutschland", "Germany"), ("AT", "Österreich", "Austria"), ("CH", "Schweiz", "Switzerland")];

/// The regions of a country (ISO 3166-2), in the language of the region. Austria has none: its
/// public holidays are the same everywhere.
pub const REGIONS: &[(&str, &str)] = &[
    ("DE-BW", "Baden-Württemberg"),
    ("DE-BY", "Bayern"),
    ("DE-BE", "Berlin"),
    ("DE-BB", "Brandenburg"),
    ("DE-HB", "Bremen"),
    ("DE-HH", "Hamburg"),
    ("DE-HE", "Hessen"),
    ("DE-MV", "Mecklenburg-Vorpommern"),
    ("DE-NI", "Niedersachsen"),
    ("DE-NW", "Nordrhein-Westfalen"),
    ("DE-RP", "Rheinland-Pfalz"),
    ("DE-SL", "Saarland"),
    ("DE-SN", "Sachsen"),
    ("DE-ST", "Sachsen-Anhalt"),
    ("DE-SH", "Schleswig-Holstein"),
    ("DE-TH", "Thüringen"),
    ("CH-AG", "Aargau"),
    ("CH-AI", "Appenzell Innerrhoden"),
    ("CH-AR", "Appenzell Ausserrhoden"),
    ("CH-BE", "Bern"),
    ("CH-BL", "Basel-Landschaft"),
    ("CH-BS", "Basel-Stadt"),
    ("CH-FR", "Fribourg"),
    ("CH-GE", "Genève"),
    ("CH-GL", "Glarus"),
    ("CH-GR", "Graubünden"),
    ("CH-JU", "Jura"),
    ("CH-LU", "Luzern"),
    ("CH-NE", "Neuchâtel"),
    ("CH-NW", "Nidwalden"),
    ("CH-OW", "Obwalden"),
    ("CH-SG", "St. Gallen"),
    ("CH-SH", "Schaffhausen"),
    ("CH-SO", "Solothurn"),
    ("CH-SZ", "Schwyz"),
    ("CH-TG", "Thurgau"),
    ("CH-TI", "Ticino"),
    ("CH-UR", "Uri"),
    ("CH-VD", "Vaud"),
    ("CH-VS", "Valais"),
    ("CH-ZG", "Zug"),
    ("CH-ZH", "Zürich"),
];

// ------------------------------------------------------------------------------------------------
// Days

#[derive(Debug, Clone, Copy)]
enum Day {
    Fixed(u32, u32),
    /// Days after Easter Sunday.
    Easter(i64),
    /// Buß- und Bettag: the Wednesday before 23 November.
    Repentance,
    /// Näfelser Fahrt: the first Thursday of April, a week later in Holy Week or Easter week.
    Naefels,
    /// Jeûne genevois: the Thursday after the first Sunday of September.
    GenevaFast,
    /// Lundi du Jeûne: the Monday after the third Sunday of September.
    FastMonday,
}

/// Easter Sunday of a Gregorian year (the anonymous Gregorian computus).
pub fn easter(year: i32) -> NaiveDate {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32).expect("the computus gives a real day")
}

/// The first `weekday` on or after `date`.
fn on_or_after(date: NaiveDate, weekday: Weekday) -> NaiveDate {
    let ahead = (7 + weekday.num_days_from_monday() as i64 - date.weekday().num_days_from_monday() as i64) % 7;
    date + Duration::days(ahead)
}

impl Day {
    fn in_year(self, year: i32) -> Option<NaiveDate> {
        let ymd = |m, d| NaiveDate::from_ymd_opt(year, m, d);
        Some(match self {
            Day::Fixed(month, day) => ymd(month, day)?,
            Day::Easter(offset) => easter(year) + Duration::days(offset),
            Day::Repentance => on_or_after(ymd(11, 16)?, Weekday::Wed),
            Day::Naefels => {
                let easter = easter(year);
                let mut day = on_or_after(ymd(4, 1)?, Weekday::Thu);
                while day == easter - Duration::days(3) || day == easter + Duration::days(4) {
                    day += Duration::days(7);
                }
                day
            }
            Day::GenevaFast => on_or_after(ymd(9, 1)?, Weekday::Sun) + Duration::days(4),
            Day::FastMonday => on_or_after(ymd(9, 1)?, Weekday::Sun) + Duration::days(15),
        })
    }
}

/// One holiday of a country: where it is one, as region codes without the country (`*`: in the
/// whole country, `-XX YY`: everywhere but there; both also without a region).
struct Holiday {
    day: Day,
    de: &'static str,
    en: &'static str,
    regions: &'static str,
    /// Said in the event's description, German and English.
    note: Option<(&'static str, &'static str)>,
}

const fn h(day: Day, de: &'static str, en: &'static str, regions: &'static str) -> Holiday {
    Holiday { day, de, en, regions, note: None }
}

use Day::{Easter, Fixed};

const GERMANY: &[Holiday] = &[
    h(Fixed(1, 1), "Neujahr", "New Year's Day", "*"),
    h(Fixed(1, 6), "Heilige Drei Könige", "Epiphany", "BW BY ST"),
    h(Fixed(3, 8), "Internationaler Frauentag", "International Women's Day", "BE MV"),
    h(Easter(-2), "Karfreitag", "Good Friday", "*"),
    h(Easter(0), "Ostersonntag", "Easter Sunday", "BB"),
    h(Easter(1), "Ostermontag", "Easter Monday", "*"),
    h(Fixed(5, 1), "Tag der Arbeit", "Labour Day", "*"),
    h(Easter(39), "Christi Himmelfahrt", "Ascension Day", "*"),
    h(Easter(49), "Pfingstsonntag", "Whit Sunday", "BB"),
    h(Easter(50), "Pfingstmontag", "Whit Monday", "*"),
    h(Easter(60), "Fronleichnam", "Corpus Christi", "BW BY HE NW RP SL"),
    h(Fixed(8, 15), "Mariä Himmelfahrt", "Assumption Day", "SL"),
    Holiday {
        day: Fixed(8, 15),
        de: "Mariä Himmelfahrt",
        en: "Assumption Day",
        regions: "BY",
        note: Some((
            "Nur in Gemeinden mit überwiegend katholischer Bevölkerung.",
            "Only in municipalities with a mainly Catholic population.",
        )),
    },
    h(Fixed(9, 20), "Weltkindertag", "World Children's Day", "TH"),
    h(Fixed(10, 3), "Tag der Deutschen Einheit", "German Unity Day", "*"),
    h(Fixed(10, 31), "Reformationstag", "Reformation Day", "BB HB HH MV NI SN ST SH TH"),
    h(Fixed(11, 1), "Allerheiligen", "All Saints' Day", "BW BY NW RP SL"),
    h(Day::Repentance, "Buß- und Bettag", "Day of Repentance and Prayer", "SN"),
    h(Fixed(12, 25), "1. Weihnachtstag", "Christmas Day", "*"),
    h(Fixed(12, 26), "2. Weihnachtstag", "Second Day of Christmas", "*"),
];

const AUSTRIA: &[Holiday] = &[
    h(Fixed(1, 1), "Neujahr", "New Year's Day", "*"),
    h(Fixed(1, 6), "Heilige Drei Könige", "Epiphany", "*"),
    h(Easter(1), "Ostermontag", "Easter Monday", "*"),
    h(Fixed(5, 1), "Staatsfeiertag", "National Holiday (Labour Day)", "*"),
    h(Easter(39), "Christi Himmelfahrt", "Ascension Day", "*"),
    h(Easter(50), "Pfingstmontag", "Whit Monday", "*"),
    h(Easter(60), "Fronleichnam", "Corpus Christi", "*"),
    h(Fixed(8, 15), "Mariä Himmelfahrt", "Assumption Day", "*"),
    h(Fixed(10, 26), "Nationalfeiertag", "National Day", "*"),
    h(Fixed(11, 1), "Allerheiligen", "All Saints' Day", "*"),
    h(Fixed(12, 8), "Mariä Empfängnis", "Immaculate Conception", "*"),
    h(Fixed(12, 25), "Christtag", "Christmas Day", "*"),
    h(Fixed(12, 26), "Stefanitag", "St. Stephen's Day", "*"),
];

/// shortcut: the cantons' lists are an approximation (some holidays only hold in parts of a
/// canton, or count only as days of rest); upgrade to official per-canton sources if people ask.
const SWITZERLAND: &[Holiday] = &[
    h(Fixed(1, 1), "Neujahr", "New Year's Day", "*"),
    h(Fixed(1, 2), "Berchtoldstag", "St. Berchtold's Day", "ZH BE LU OW GL ZG FR SO SH TG VD NE JU AG"),
    h(Fixed(1, 6), "Heilige Drei Könige", "Epiphany", "UR SZ TI"),
    h(Fixed(3, 1), "Jahrestag der Republik Neuenburg", "Republic Day (Neuchâtel)", "NE"),
    h(Fixed(3, 19), "Josefstag", "St. Joseph's Day", "UR SZ NW TI VS"),
    h(Easter(-2), "Karfreitag", "Good Friday", "-TI VS"),
    h(Easter(1), "Ostermontag", "Easter Monday", "-VS"),
    h(Day::Naefels, "Näfelser Fahrt", "Näfels Pilgrimage", "GL"),
    h(Fixed(5, 1), "Tag der Arbeit", "Labour Day", "ZH BS BL SH TG TI NE JU"),
    h(Easter(39), "Auffahrt", "Ascension Day", "*"),
    h(Easter(50), "Pfingstmontag", "Whit Monday", "-VS"),
    h(Easter(60), "Fronleichnam", "Corpus Christi", "LU UR SZ OW NW ZG FR SO AI AG TI VS JU"),
    h(Fixed(6, 23), "Fest der Unabhängigkeit", "Independence Day (Jura)", "JU"),
    h(Fixed(6, 29), "Peter und Paul", "Saints Peter and Paul", "TI"),
    h(Fixed(8, 1), "Bundesfeier", "Swiss National Day", "*"),
    h(Fixed(8, 15), "Mariä Himmelfahrt", "Assumption Day", "LU UR SZ OW NW ZG FR SO AI AG TI VS JU"),
    h(Day::GenevaFast, "Genfer Bettag", "Genevan Fast", "GE"),
    h(Day::FastMonday, "Bettagsmontag", "Federal Fast Monday", "VD"),
    h(Fixed(9, 25), "Bruder Klaus", "St. Nicholas of Flüe", "OW"),
    h(Fixed(11, 1), "Allerheiligen", "All Saints' Day", "LU UR SZ OW NW GL ZG FR SO AI SG TI VS JU AG"),
    h(Fixed(12, 8), "Mariä Empfängnis", "Immaculate Conception", "LU UR SZ OW NW ZG FR AI TI VS"),
    h(Fixed(12, 25), "Weihnachten", "Christmas Day", "*"),
    h(Fixed(12, 26), "Stephanstag", "St. Stephen's Day", "-GE NE JU VD VS"),
    h(Fixed(12, 31), "Restauration der Republik", "Restoration of the Republic (Geneva)", "GE"),
];

fn table(country: &str) -> &'static [Holiday] {
    match country {
        "DE" => GERMANY,
        "AT" => AUSTRIA,
        _ => SWITZERLAND,
    }
}

impl Holiday {
    /// Whether it is a holiday in `region` (without the country), or in the whole country.
    fn holds_in(&self, region: Option<&str>) -> bool {
        if self.regions == "*" {
            return true;
        }
        let Some(region) = region else { return false };
        match self.regions.strip_prefix('-') {
            Some(except) => !except.split_whitespace().any(|r| r == region),
            None => self.regions.split_whitespace().any(|r| r == region),
        }
    }
}

/// The holidays of `area` (`DE`, `DE-NW`, `AT`, `CH-ZH`, …) in `year`, by date, with the index of
/// each in its country's table.
fn holidays_of(area: &str, year: i32) -> Vec<(NaiveDate, usize, &'static Holiday)> {
    let (country, region) = match area.split_once('-') {
        Some((country, region)) => (country, Some(region)),
        None => (area, None),
    };
    let mut days: Vec<_> = table(country)
        .iter()
        .enumerate()
        .filter(|(_, holiday)| holiday.holds_in(region))
        .filter_map(|(index, holiday)| Some((holiday.day.in_year(year)?, index, holiday)))
        .collect();
    days.sort_by_key(|(date, index, _)| (*date, *index));
    days
}

/// The names of the holidays of `area` in `year`, by date, in German.
pub fn holiday_names(area: &str, year: i32) -> Vec<(NaiveDate, &'static str)> {
    holidays_of(area, year).into_iter().map(|(date, _, holiday)| (date, holiday.de)).collect()
}

// ------------------------------------------------------------------------------------------------
// The setting

/// Checks the value of the setting `holidays`: `{"country": "DE", "region": "DE-NW" | null}`.
pub fn check_setting(value: &Value) -> std::result::Result<(), String> {
    area_of(value).map(|_| ())
}

/// The area a valid `holidays` value names: the region, or the country without one.
fn area_of(value: &Value) -> std::result::Result<String, String> {
    let object = value.as_object().ok_or("holidays must be an object or null")?;
    if let Some(unknown) = object.keys().find(|key| *key != "country" && *key != "region") {
        return Err(format!("holidays has an unknown property {unknown}"));
    }
    let country = object
        .get("country")
        .and_then(Value::as_str)
        .filter(|code| COUNTRIES.iter().any(|(known, _, _)| known == code))
        .ok_or("holidays: country must be one of DE, AT, CH")?;
    match object.get("region") {
        None | Some(Value::Null) => Ok(country.to_owned()),
        Some(Value::String(region))
            if region.split_once('-').is_some_and(|(of, _)| of == country)
                && REGIONS.iter().any(|(known, _)| known == region) =>
        {
            Ok(region.clone())
        }
        _ => Err(format!("holidays: region must be null or a region of {country}")),
    }
}

// ------------------------------------------------------------------------------------------------
// Events

fn all_day(uid: String, name: String, title: &str, start: NaiveDate, end: NaiveDate, note: Option<&str>) -> Derived {
    let mut vevent = Component::new("VEVENT");
    vevent.properties.push(Property::new("UID", uid.clone()));
    for (property, date) in [("DTSTART", start), ("DTEND", end)] {
        let mut value = Property::new(property, date.format("%Y%m%d").to_string());
        value.set_param("VALUE", "DATE");
        vevent.properties.push(value);
    }
    // The names and notes here are plain text without characters iCalendar escapes.
    vevent.properties.push(Property::new("SUMMARY", title));
    if let Some(note) = note {
        vevent.properties.push(Property::new("DESCRIPTION", note));
    }
    vevent.properties.push(Property::new("TRANSP", "TRANSPARENT"));
    vevent.properties.push(Property::new("CLASS", "PUBLIC"));
    let mut calendar = Component::new("VCALENDAR");
    calendar.properties.push(Property::new("VERSION", "2.0"));
    calendar.properties.push(Property::new("PRODID", "-//UwUMail//Holidays//EN"));
    calendar.properties.push(Property::new("CALSCALE", "GREGORIAN"));
    calendar.components.push(vevent);
    Derived { name, uid, body: calendar }
}

fn holiday_events(
    collection_id: i64,
    area: &str,
    years: std::ops::RangeInclusive<i32>,
    language: BirthdayLanguage,
) -> Vec<Derived> {
    let de = language == BirthdayLanguage::De;
    years
        .flat_map(|year| holidays_of(area, year))
        .map(|(date, index, holiday)| {
            let stamp = date.format("%Y%m%d");
            let title = if de { holiday.de } else { holiday.en };
            let note = holiday.note.map(|(note_de, note_en)| if de { note_de } else { note_en });
            all_day(
                format!("uwumail-holiday-{collection_id}-{stamp}-{index}"),
                format!("h-{stamp}-{index}.ics"),
                title,
                date,
                date + Duration::days(1),
                note,
            )
        })
        .collect()
}

/// The Monday each ISO week of `year` starts on, with its number.
fn weeks_of(year: i32) -> impl Iterator<Item = (u32, NaiveDate)> {
    (1..=53).filter_map(move |week| NaiveDate::from_isoywd_opt(year, week, Weekday::Mon).map(|monday| (week, monday)))
}

fn week_events(collection_id: i64, years: std::ops::RangeInclusive<i32>, language: BirthdayLanguage) -> Vec<Derived> {
    let word = if language == BirthdayLanguage::De { "KW" } else { "Week" };
    years
        .flat_map(|year| weeks_of(year).map(move |week| (year, week)))
        .map(|(year, (week, monday))| {
            all_day(
                format!("uwumail-week-{collection_id}-{year}-{week:02}"),
                format!("kw-{year}-{week:02}.ics"),
                &format!("{word} {week}"),
                monday,
                monday + Duration::days(7),
                None,
            )
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------
// The calendars

fn calendar_name(kind: &str, area: &str, language: BirthdayLanguage) -> String {
    let de = language == BirthdayLanguage::De;
    if kind == WEEKS {
        return if de { "Kalenderwochen" } else { "Week numbers" }.to_owned();
    }
    let place = REGIONS.iter().find(|(code, _)| *code == area).map(|(_, name)| *name).or_else(|| {
        COUNTRIES
            .iter()
            .find(|(code, _, _)| *code == area)
            .map(|(_, name_de, name_en)| if de { *name_de } else { *name_en })
    });
    format!("{} {}", if de { "Feiertage" } else { "Holidays" }, place.unwrap_or(area))
}

struct Row {
    collection_id: i64,
    area: String,
    language: String,
    first_year: i32,
}

fn row_of(conn: &Connection, account_id: i64, kind: &str) -> Result<Option<Row>> {
    Ok(conn
        .query_row(
            "SELECT collection_id, region, language, first_year FROM builtin_calendars WHERE account_id = ?1 AND kind = ?2",
            params![account_id, kind],
            |row| Ok(Row { collection_id: row.get(0)?, area: row.get(1)?, language: row.get(2)?, first_year: row.get(3)? }),
        )
        .optional()?)
}

fn current_year() -> i32 {
    chrono::DateTime::from_timestamp(crate::now(), 0).unwrap_or_default().year()
}

fn create(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    account_id: i64,
    kind: &str,
    name: String,
) -> Result<DavCollection> {
    let taken: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM dav_collections WHERE account_id = ?1 AND kind = 'calendar' AND slug = ?2)",
        params![account_id, kind],
        |row| row.get(0),
    )?;
    let slug = if taken { format!("{kind}-{}", hex::encode(crate::random_bytes::<4>())) } else { kind.to_owned() };
    let new = NewDavCollection {
        slug,
        display_name: name,
        color: Some(if kind == WEEKS { WEEKS_COLOR } else { HOLIDAYS_COLOR }.into()),
        components: vec!["VEVENT".into()],
        ..Default::default()
    };
    let id = insert_collection(tx, log, account_id, DavKind::Calendar, &new)?;
    tx.execute("UPDATE dav_collections SET is_default = 0 WHERE id = ?1", [id])?;
    tx.execute(
        "INSERT INTO builtin_calendars (collection_id, account_id, kind, region, language, first_year)
         VALUES (?1, ?2, ?3, '', '', 0)",
        params![id, account_id, kind],
    )?;
    own_collection(tx, account_id, id)
}

/// Brings one built-in calendar to what the settings ask for: `area` is the holidays' region (or
/// `""` for the week numbers), `None` when it is turned off.
fn sync_one(tx: &Transaction<'_>, log: &mut ChangeLog, account_id: i64, kind: &str, area: Option<&str>) -> Result<()> {
    let row = row_of(tx, account_id, kind)?;
    let Some(area) = area else {
        if let Some(row) = row {
            let calendar = own_collection(tx, account_id, row.collection_id)?;
            delete_collection(tx, log, &calendar)?;
        }
        return Ok(());
    };
    let language = language_of(tx, account_id)?;
    let first_year = current_year() - YEARS_BEFORE;
    let name = calendar_name(kind, area, language);
    let calendar = match row {
        Some(row) if row.area == area && row.language == language.code() && row.first_year == first_year => {
            return Ok(());
        }
        Some(row) => {
            let calendar = own_collection(tx, account_id, row.collection_id)?;
            // Renamed with the region or language, unless the person named it themselves.
            let old = calendar_name(kind, &row.area, BirthdayLanguage::from_code(&row.language));
            if calendar.display_name == old && calendar.display_name != name {
                tx.execute(
                    "UPDATE dav_collections SET display_name = ?1, change = change + 1 WHERE id = ?2",
                    params![name, calendar.id],
                )?;
                log.collection(tx, &calendar, "updated")?;
            }
            calendar
        }
        None => create(tx, log, account_id, kind, name)?,
    };
    tx.execute(
        "UPDATE builtin_calendars SET region = ?1, language = ?2, first_year = ?3 WHERE collection_id = ?4",
        params![area, language.code(), first_year, calendar.id],
    )?;
    let years = first_year..=first_year + YEARS_BEFORE + YEARS_AFTER;
    let wanted = if kind == WEEKS {
        week_events(calendar.id, years, language)
    } else {
        holiday_events(calendar.id, area, years, language)
    };
    let mut stmt = tx.prepare("SELECT name, content FROM dav_resources WHERE collection_id = ?1")?;
    let existing: Vec<(String, String)> =
        stmt.query_map([calendar.id], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    // Nobody who sees the calendar is told about each holiday as if a person had written it.
    let author = log.author.take();
    let result = write_card_events(tx, log, &calendar, &existing, &wanted);
    log.author = author;
    result
}

/// Makes, rewrites or deletes the built-in calendars after the settings were written, in the same
/// transaction. `holidays` and `weekNumbers` are the settings' new values.
pub(crate) fn apply(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    account_id: i64,
    holidays: Option<&Value>,
    week_numbers: Option<&Value>,
) -> Result<()> {
    // Checked when it was written; a value that does not name an area turns them off.
    let area = holidays.and_then(|value| area_of(value).ok());
    sync_one(tx, log, account_id, HOLIDAYS, area.as_deref())?;
    let weeks = week_numbers.and_then(Value::as_bool).unwrap_or(false);
    sync_one(tx, log, account_id, WEEKS, weeks.then_some(""))
}

/// Writes the built-in calendars anew when the year or the person's language changed since: on
/// every listing of the calendars, and from the hourly job. Cheap when nothing changed.
pub(crate) fn refresh(tx: &Transaction<'_>, log: &mut ChangeLog, account_id: i64) -> Result<()> {
    let mut stmt = tx.prepare("SELECT kind FROM builtin_calendars WHERE account_id = ?1")?;
    let kinds: Vec<String> = stmt.query_map([account_id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if kinds.is_empty() {
        return Ok(());
    }
    let setting = |key: &str| -> Result<Option<Value>> {
        let text: Option<String> = tx
            .query_row(
                "SELECT value FROM user_settings WHERE account_id = ?1 AND key = ?2",
                params![account_id, key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(text.and_then(|text| serde_json::from_str(&text).ok()))
    };
    apply(tx, log, account_id, setting("holidays")?.as_ref(), setting("weekNumbers")?.as_ref())
}

impl Store {
    /// Writes the next years into the built-in calendars once the year turned (the hourly job).
    /// Returns how many accounts' calendars it wrote.
    pub async fn roll_builtin_calendars(&self) -> Result<usize> {
        let first_year = current_year() - YEARS_BEFORE;
        let accounts: Vec<i64> = self
            .read(move |conn| {
                let mut stmt =
                    conn.prepare("SELECT DISTINCT account_id FROM builtin_calendars WHERE first_year != ?1")?;
                let rows = stmt.query_map([first_year], |row| row.get(0))?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        for &account_id in &accounts {
            let modseq = self
                .write(move |tx| {
                    let mut log = ChangeLog::new(account_id);
                    refresh(tx, &mut log, account_id)?;
                    Ok(log.modseq())
                })
                .await?;
            self.notify_log(account_id, modseq);
        }
        Ok(accounts.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn days(area: &str, year: i32) -> Vec<(u32, u32, &'static str)> {
        holiday_names(area, year).into_iter().map(|(date, name)| (date.month(), date.day(), name)).collect()
    }

    #[test]
    fn easter_sundays() {
        for (year, month, day) in [
            (1961, 4, 2),
            (2000, 4, 23),
            (2008, 3, 23),
            (2011, 4, 24),
            (2019, 4, 21),
            (2024, 3, 31),
            (2025, 4, 20),
            (2026, 4, 5),
            (2027, 3, 28),
            (2028, 4, 16),
            (2038, 4, 25),
            (2285, 3, 22),
        ] {
            assert_eq!(easter(year), date(year, month, day), "{year}");
        }
    }

    // Checked against the lists of the Länder (e.g. land.nrw, stmi.bayern.de, revosax.sachsen.de).
    #[test]
    fn north_rhine_westphalia() {
        type Md = (u32, u32);
        let common = |gf: Md, em: Md, asc: Md, wm: Md, cc: Md| {
            vec![
                (1, 1, "Neujahr"),
                (gf.0, gf.1, "Karfreitag"),
                (em.0, em.1, "Ostermontag"),
                (5, 1, "Tag der Arbeit"),
                (asc.0, asc.1, "Christi Himmelfahrt"),
                (wm.0, wm.1, "Pfingstmontag"),
                (cc.0, cc.1, "Fronleichnam"),
                (10, 3, "Tag der Deutschen Einheit"),
                (11, 1, "Allerheiligen"),
                (12, 25, "1. Weihnachtstag"),
                (12, 26, "2. Weihnachtstag"),
            ]
        };
        assert_eq!(days("DE-NW", 2026), common((4, 3), (4, 6), (5, 14), (5, 25), (6, 4)));
        assert_eq!(days("DE-NW", 2027), common((3, 26), (3, 29), (5, 6), (5, 17), (5, 27)));
    }

    #[test]
    fn bavaria() {
        assert_eq!(
            days("DE-BY", 2026),
            vec![
                (1, 1, "Neujahr"),
                (1, 6, "Heilige Drei Könige"),
                (4, 3, "Karfreitag"),
                (4, 6, "Ostermontag"),
                (5, 1, "Tag der Arbeit"),
                (5, 14, "Christi Himmelfahrt"),
                (5, 25, "Pfingstmontag"),
                (6, 4, "Fronleichnam"),
                (8, 15, "Mariä Himmelfahrt"),
                (10, 3, "Tag der Deutschen Einheit"),
                (11, 1, "Allerheiligen"),
                (12, 25, "1. Weihnachtstag"),
                (12, 26, "2. Weihnachtstag"),
            ]
        );
        assert_eq!(
            days("DE-BY", 2027),
            vec![
                (1, 1, "Neujahr"),
                (1, 6, "Heilige Drei Könige"),
                (3, 26, "Karfreitag"),
                (3, 29, "Ostermontag"),
                (5, 1, "Tag der Arbeit"),
                (5, 6, "Christi Himmelfahrt"),
                (5, 17, "Pfingstmontag"),
                (5, 27, "Fronleichnam"),
                (8, 15, "Mariä Himmelfahrt"),
                (10, 3, "Tag der Deutschen Einheit"),
                (11, 1, "Allerheiligen"),
                (12, 25, "1. Weihnachtstag"),
                (12, 26, "2. Weihnachtstag"),
            ]
        );
        // The Assumption holds only in Catholic municipalities, and says so.
        let events = holiday_events(1, "DE-BY", 2026..=2026, BirthdayLanguage::De);
        let assumption = events.iter().find(|event| event.name == "h-20260815-12.ics").expect("the Assumption");
        assert!(assumption.body.to_ics().contains("DESCRIPTION:Nur in Gemeinden"));
        assert!(!days("DE-BY", 2026).iter().any(|(_, _, name)| name.contains("Friedensfest")));
    }

    #[test]
    fn saxony() {
        let saxony = |gf: (u32, u32), em: (u32, u32), asc: (u32, u32), wm: (u32, u32), repentance: u32| {
            vec![
                (1, 1, "Neujahr"),
                (gf.0, gf.1, "Karfreitag"),
                (em.0, em.1, "Ostermontag"),
                (5, 1, "Tag der Arbeit"),
                (asc.0, asc.1, "Christi Himmelfahrt"),
                (wm.0, wm.1, "Pfingstmontag"),
                (10, 3, "Tag der Deutschen Einheit"),
                (10, 31, "Reformationstag"),
                (11, repentance, "Buß- und Bettag"),
                (12, 25, "1. Weihnachtstag"),
                (12, 26, "2. Weihnachtstag"),
            ]
        };
        assert_eq!(days("DE-SN", 2026), saxony((4, 3), (4, 6), (5, 14), (5, 25), 18));
        assert_eq!(days("DE-SN", 2027), saxony((3, 26), (3, 29), (5, 6), (5, 17), 17));
    }

    #[test]
    fn other_states_and_the_whole_country() {
        let names = |area, year| days(area, year).into_iter().map(|(m, d, _)| (m, d)).collect::<Vec<_>>();
        assert!(names("DE-BE", 2026).contains(&(3, 8)));
        assert!(names("DE-MV", 2026).contains(&(3, 8)));
        assert!(names("DE-TH", 2026).contains(&(9, 20)));
        assert!(names("DE-BB", 2026).contains(&(4, 5)), "Easter Sunday in Brandenburg");
        assert_eq!(names("DE", 2026).len(), 9, "the nationwide holidays alone");
        for (code, _) in REGIONS.iter().filter(|(code, _)| code.starts_with("DE-")) {
            assert!(names(code, 2026).len() >= 10, "{code}");
        }
    }

    #[test]
    fn austria() {
        let names = days("AT", 2026);
        assert_eq!(names.len(), 13);
        assert!(names.contains(&(10, 26, "Nationalfeiertag")));
        assert!(names.contains(&(12, 8, "Mariä Empfängnis")));
        assert!(names.contains(&(6, 4, "Fronleichnam")));
        assert!(!names.iter().any(|(_, _, name)| *name == "Karfreitag"), "not since 2019");
    }

    #[test]
    fn switzerland() {
        let names = |area, year| days(area, year).into_iter().map(|(m, d, _)| (m, d)).collect::<Vec<_>>();
        assert_eq!(names("CH", 2026), vec![(1, 1), (5, 14), (8, 1), (12, 25)]);
        assert_eq!(
            names("CH-ZH", 2026),
            vec![(1, 1), (1, 2), (4, 3), (4, 6), (5, 1), (5, 14), (5, 25), (8, 1), (12, 25), (12, 26)]
        );
        // Jeûne genevois: Thursday after the first Sunday of September (6 September 2026).
        assert!(names("CH-GE", 2026).contains(&(9, 10)));
        assert!(names("CH-GE", 2026).contains(&(12, 31)));
        // Lundi du Jeûne: the Monday after the third Sunday of September (20 September 2026).
        assert!(names("CH-VD", 2026).contains(&(9, 21)));
        // Näfelser Fahrt: 2 April 2026 is Maundy Thursday, 9 April the Thursday of Easter week.
        assert!(names("CH-GL", 2026).contains(&(4, 16)));
        assert!(names("CH-GL", 2027).contains(&(4, 8)), "1 April 2027 is in Easter week");
        assert!(names("CH-TI", 2026).contains(&(6, 29)));
        assert!(!names("CH-TI", 2026).contains(&(4, 3)), "no Good Friday in Ticino");
        for (code, _) in REGIONS.iter().filter(|(code, _)| code.starts_with("CH-")) {
            assert!(names(code, 2026).len() >= 7, "{code}");
        }
    }

    #[test]
    fn week_53_of_2026() {
        let weeks: Vec<_> = weeks_of(2026).collect();
        assert_eq!(weeks.len(), 53);
        assert_eq!(weeks[0], (1, date(2025, 12, 29)));
        assert_eq!(weeks[52], (53, date(2026, 12, 28)));
        assert_eq!(weeks_of(2027).count(), 52);
        let events = week_events(7, 2026..=2026, BirthdayLanguage::De);
        let last = events.last().unwrap().body.to_ics();
        assert!(last.contains("SUMMARY:KW 53"), "{last}");
        assert!(last.contains("DTSTART;VALUE=DATE:20261228"));
        assert!(last.contains("DTEND;VALUE=DATE:20270104"), "Monday to Sunday");
        assert!(week_events(7, 2026..=2026, BirthdayLanguage::En)[45].body.to_ics().contains("SUMMARY:Week 46"));
    }

    #[test]
    fn the_setting_names_a_country_and_maybe_a_region() {
        let check = |value: Value| check_setting(&value);
        assert!(check(serde_json::json!({"country": "DE", "region": "DE-NW"})).is_ok());
        assert!(check(serde_json::json!({"country": "DE", "region": null})).is_ok());
        assert!(check(serde_json::json!({"country": "AT"})).is_ok());
        assert!(check(serde_json::json!({"country": "CH", "region": "CH-GE"})).is_ok());
        assert!(check(serde_json::json!({"country": "AT", "region": "DE-NW"})).is_err());
        assert!(check(serde_json::json!({"country": "DE", "region": "DE-XX"})).is_err());
        assert!(check(serde_json::json!({"country": "FR", "region": null})).is_err());
        assert!(check(serde_json::json!({"country": "DE", "extra": 1})).is_err());
        assert!(check(serde_json::json!("DE")).is_err());
    }

    #[test]
    fn names_need_no_escaping() {
        for holiday in GERMANY.iter().chain(AUSTRIA).chain(SWITZERLAND) {
            let texts = [holiday.de, holiday.en, holiday.note.map_or("", |n| n.0), holiday.note.map_or("", |n| n.1)];
            assert!(texts.iter().all(|text| !text.contains([',', ';', '\\', '\n'])), "{}", holiday.de);
        }
    }

    #[tokio::test]
    async fn the_year_turning_writes_the_next_years() {
        use crate::test_support::store;
        use crate::user_settings::SettingsChange;
        use crate::{NewAccount, Role};
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let account = NewAccount {
            address: "nyu@example.org".into(),
            display_name: "Nyu".into(),
            password: Some("katzenpfote-123".into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let id = store.create_account(account).await.unwrap().id;
        let change = SettingsChange::Patch(vec![("weekNumbers".into(), Some(Value::Bool(true)))]);
        store.update_user_settings(id, change, None).await.unwrap();
        assert_eq!(store.roll_builtin_calendars().await.unwrap(), 0, "up to date");
        let first = current_year() - YEARS_BEFORE;
        store
            .write(move |tx| {
                tx.execute("UPDATE builtin_calendars SET first_year = ?1", [first - 1])?;
                tx.execute(
                    "DELETE FROM dav_resources WHERE name = ?1",
                    [format!("kw-{}-10.ics", first + YEARS_BEFORE + YEARS_AFTER)],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(store.roll_builtin_calendars().await.unwrap(), 1);
        let name = format!("kw-{}-10.ics", first + YEARS_BEFORE + YEARS_AFTER);
        let found: bool = store
            .read(move |conn| {
                Ok(conn.query_row("SELECT EXISTS (SELECT 1 FROM dav_resources WHERE name = ?1)", [name], |row| {
                    row.get(0)
                })?)
            })
            .await
            .unwrap();
        assert!(found, "the last year is written again");
    }
}
