//! JSContact for JMAP Contacts: the vCards CardDAV stores turned into ContactCard JSON and back
//! (with calcard), the rules a card has to follow to be stored, and the texts a query searches.
//!
//! Cards stay vCard on disk, so phones and Thunderbird see exactly what JMAP clients see. What
//! calcard cannot map to JSContact it keeps in the card's `vCard` property (RFC 9555), which goes
//! back into the vCard when the card is written, so a change over JMAP loses nothing a phone put
//! there.

use std::borrow::Cow;

use calcard::jscontact::JSContact;
use calcard::vcard::{VCard, VCardVersion};
use serde_json::{Map, Value};
use uwumail_store::birthdays::{MAX_REMINDER_DAYS, MAX_REMINDERS, REMINDER_PROPERTY, Reminder};

/// Properties JMAP adds to a card; they never go into the vCard.
pub const JMAP_PROPERTIES: &[&str] = &["id", "addressBookIds"];
/// The card's birthday reminders (docs/birthdays.md): `[{daysBefore, time}]` in JMAP,
/// `X-UWUMAIL-REMINDER:1 09:00` lines in the vCard, which CardDAV clients keep as they are.
pub const REMINDERS: &str = "uwuReminders";
/// Longest uid, in bytes, as for events.
const MAX_UID_BYTES: usize = 255;
/// Media types a photo, logo or sound given as a `data:` URI may have.
const MEDIA_TYPES: &[(&str, &str)] = &[("photo", "image/"), ("logo", "image/"), ("sound", "audio/")];

/// Reads a stored vCard. `None` when calcard cannot read it.
pub fn from_vcard(content: &str) -> Option<Map<String, Value>> {
    let card = VCard::parse(basic_dates(content).as_ref()).ok()?;
    match serde_json::to_value(card.into_jscontact::<String, String>()) {
        Ok(Value::Object(mut object)) => {
            take_reminders(&mut object);
            Some(object)
        }
        _ => None,
    }
}

/// The card with its dates (`BDAY`, `ANNIVERSARY`, `DEATHDATE`) in the basic format, `19961003` and
/// `--1015`: calcard reads `--10-15` as October without a day in every version, and `1996-10-03` so in
/// vCard 4.0, although Google and our own birthday import write the first and many apps the second.
/// Everything else of the card stays as it is.
fn basic_dates(content: &str) -> Cow<'_, str> {
    let mut out: Option<String> = None;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        if let Some(fixed) = basic_date_line(line) {
            // Lines end after a '\n', so `offset` is on a char boundary.
            out.get_or_insert_with(|| String::with_capacity(content.len()) + &content[..offset]).push_str(&fixed);
        } else if let Some(out) = out.as_mut() {
            out.push_str(line);
        }
        offset += line.len();
    }
    out.map_or(Cow::Borrowed(content), Cow::Owned)
}

/// A date line in the basic format, or `None` when it has nothing to change.
fn basic_date_line(line: &str) -> Option<String> {
    let colon = line.find(':')?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let name = head.split(';').next().unwrap_or_default();
    let name = name.rsplit('.').next().unwrap_or_default();
    if !["BDAY", "ANNIVERSARY", "DEATHDATE"].iter().any(|n| name.eq_ignore_ascii_case(n)) {
        return None;
    }
    let bytes = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes.get(range).is_some_and(|b| b.iter().all(u8::is_ascii_digit));
    let ends = |at: usize| matches!(bytes.get(at), None | Some(b'\r' | b'\n' | b'T' | b't'));
    // `YYYY-MM-DD` and `--MM-DD`; the dashes to drop are ASCII, so the cuts are on char boundaries.
    let cut: &[usize] = if digits(0..4)
        && bytes.get(4) == Some(&b'-')
        && digits(5..7)
        && bytes.get(7) == Some(&b'-')
        && digits(8..10)
        && ends(10)
    {
        &[4, 7]
    } else if value.starts_with("--") && digits(2..4) && bytes.get(4) == Some(&b'-') && digits(5..7) && ends(7) {
        &[4]
    } else {
        return None;
    };
    let mut fixed = String::with_capacity(line.len());
    fixed.push_str(head);
    fixed.push(':');
    let mut from = 0;
    for &at in cut {
        fixed.push_str(&value[from..at]);
        from = at + 1;
    }
    fixed.push_str(&value[from..]);
    Some(fixed)
}

/// The `X-UWUMAIL-REMINDER` lines calcard kept in `vCard/properties`, as `uwuReminders`.
fn take_reminders(object: &mut Map<String, Value>) {
    let Some(Value::Array(properties)) = object.get_mut("vCard").and_then(|v| v.get_mut("properties")) else {
        return;
    };
    let is_reminder = |entry: &Value| {
        entry.get(0).and_then(Value::as_str).is_some_and(|name| name.eq_ignore_ascii_case(REMINDER_PROPERTY))
    };
    let mut reminders: Vec<Reminder> = properties
        .iter()
        .filter(|entry| is_reminder(entry))
        .filter_map(|entry| entry.get(3).and_then(Value::as_str).and_then(Reminder::parse))
        .collect();
    properties.retain(|entry| !is_reminder(entry));
    reminders.sort_unstable();
    reminders.dedup();
    reminders.truncate(MAX_REMINDERS);
    if !reminders.is_empty() {
        let list: Vec<Value> = reminders
            .iter()
            .map(|reminder| serde_json::json!({ "daysBefore": reminder.days_before, "time": reminder.time() }))
            .collect();
        object.insert(REMINDERS.into(), Value::Array(list));
    }
}

/// `uwuReminders` read, or why not.
fn reminders_of(value: &Value) -> Result<Vec<Reminder>, String> {
    let error = || {
        format!(
            "uwuReminders is a list of at most {MAX_REMINDERS} {{daysBefore: 0 to {MAX_REMINDER_DAYS}, time: \"HH:MM\"}}"
        )
    };
    let list = match value {
        Value::Null => return Ok(Vec::new()),
        Value::Array(list) if list.len() <= MAX_REMINDERS => list,
        _ => return Err(error()),
    };
    let mut reminders = Vec::new();
    for item in list {
        let days = item.get("daysBefore").and_then(Value::as_u64).filter(|d| *d <= u64::from(MAX_REMINDER_DAYS));
        let time = item.get("time").and_then(Value::as_str).filter(|t| t.len() == 5);
        let (Some(days), Some(time)) = (days, time) else { return Err(error()) };
        reminders.push(Reminder::parse(&format!("{days} {time}")).ok_or_else(error)?);
    }
    reminders.sort_unstable();
    reminders.dedup();
    Ok(reminders)
}

/// A card as vCard text, in the version it came in; a new card as vCard 3.0, which every
/// CardDAV client reads.
pub fn to_vcard(card: &Map<String, Value>) -> Result<String, String> {
    let mut card = card.clone();
    for property in JMAP_PROPERTIES {
        card.remove(*property);
    }
    let reminders = reminders_of(&card.remove(REMINDERS).unwrap_or(Value::Null))?;
    if !reminders.is_empty() {
        let vcard = card.entry("vCard").or_insert_with(|| serde_json::json!({}));
        let Value::Object(vcard) = vcard else { return Err("vCard must be an object".into()) };
        let properties = vcard.entry("properties").or_insert_with(|| Value::Array(Vec::new()));
        let Value::Array(properties) = properties else { return Err("vCard/properties must be a list".into()) };
        properties.retain(|entry| {
            !entry.get(0).and_then(Value::as_str).is_some_and(|name| name.eq_ignore_ascii_case(REMINDER_PROPERTY))
        });
        for reminder in reminders {
            properties.push(serde_json::json!([
                REMINDER_PROPERTY.to_ascii_lowercase(),
                {},
                "unknown",
                reminder.format()
            ]));
        }
    }
    let json = Value::Object(card).to_string();
    let contact = JSContact::<String, String>::parse(&json).map_err(|err| format!("not JSContact: {err}"))?;
    let vcard = contact.into_vcard().ok_or("the card cannot be written as vCard")?;
    let mut out = String::new();
    vcard
        .write_to(&mut out, vcard.version().unwrap_or(VCardVersion::V3_0))
        .map_err(|_| "the vCard cannot be written")?;
    Ok(out)
}

/// A card that does not follow the rules, and which properties are at fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub properties: Vec<String>,
    pub description: String,
}

fn invalid(property: &str, description: impl Into<String>) -> Invalid {
    Invalid { properties: vec![property.into()], description: description.into() }
}

/// Checks what JSContact (RFC 9553) and this server insist on before a card is stored. Everything
/// else is data; calcard keeps what it has no vCard property for.
pub fn validate(card: &Map<String, Value>) -> Result<(), Invalid> {
    if card.get("@type").and_then(Value::as_str) != Some("Card") {
        return Err(invalid("@type", "a card has the @type Card"));
    }
    if let Some(value) = card.get(REMINDERS) {
        reminders_of(value).map_err(|message| invalid(REMINDERS, message))?;
    }
    match card.get("uid").and_then(Value::as_str) {
        Some(uid) if !uid.is_empty() && uid.len() <= MAX_UID_BYTES && !uid.chars().any(char::is_control) => {}
        _ => return Err(invalid("uid", format!("a uid has 1 to {MAX_UID_BYTES} bytes"))),
    }
    for property in ["created", "updated"] {
        if let Some(value) = card.get(property)
            && value.as_str().and_then(crate::jscal::parse_utc).is_none()
        {
            return Err(invalid(property, "must be a UTC date and time"));
        }
    }
    if let Some(kind) = card.get("kind")
        && !kind.as_str().is_some_and(|kind| !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_graphic()))
    {
        return Err(invalid("kind", "must be a kind like individual or group"));
    }
    // Every map of the card holds objects, as JSContact has them.
    for (property, value) in card {
        if let Value::Object(map) = value
            && ["emails", "phones", "addresses", "onlineServices", "organizations", "titles", "notes", "media"]
                .contains(&property.as_str())
            && map.values().any(|entry| !entry.is_object())
        {
            return Err(invalid(property, "the entries must be objects"));
        }
    }
    if let Some(Value::Object(emails)) = card.get("emails")
        && emails.values().any(|email| !email.get("address").and_then(Value::as_str).is_some_and(|a| !a.is_empty()))
    {
        return Err(invalid("emails", "every email has an address"));
    }
    if let Some(Value::Object(phones)) = card.get("phones")
        && phones.values().any(|phone| !phone.get("number").and_then(Value::as_str).is_some_and(|n| !n.is_empty()))
    {
        return Err(invalid("phones", "every phone has a number"));
    }
    if let Some(Value::Object(media)) = card.get("media") {
        for item in media.values() {
            if item.get("blobId").is_some() {
                return Err(invalid("media", "give pictures as data: URIs; blobs are not supported for cards"));
            }
            let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
            let Some(uri) = item.get("uri").and_then(Value::as_str) else {
                return Err(invalid("media", "every media entry has a uri"));
            };
            // A picture stored in the card has to be what it says it is.
            if let Some(data) = uri.strip_prefix("data:") {
                let media_type = data.split([';', ',']).next().unwrap_or_default().to_ascii_lowercase();
                let expected = MEDIA_TYPES.iter().find(|(k, _)| *k == kind).map(|(_, prefix)| *prefix);
                if expected.is_some_and(|prefix| !media_type.starts_with(prefix)) {
                    return Err(invalid("media", format!("a {kind} has to be of the type {}*", expected.unwrap())));
                }
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Texts a query looks at

fn texts(value: Option<&Value>, fields: &[&str], out: &mut String) {
    let Some(Value::Object(items)) = value else { return };
    for item in items.values() {
        for field in fields {
            match item.get(*field) {
                Some(Value::String(text)) => {
                    out.push_str(text);
                    out.push('\n');
                }
                // Components of names and addresses, units of organizations.
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(text) = part.get("value").or_else(|| part.get("name")).and_then(Value::as_str) {
                            out.push_str(text);
                            out.push('\n');
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The name components of one kind, or the whole name with its full form and nicknames.
pub fn name_text(card: &Map<String, Value>, kind: Option<&str>) -> String {
    let mut out = String::new();
    let name = card.get("name");
    if let Some(components) = name.and_then(|n| n.get("components")).and_then(Value::as_array) {
        for component in components {
            let component_kind = component.get("kind").and_then(Value::as_str);
            if kind.is_none_or(|kind| component_kind == Some(kind))
                && let Some(value) = component.get("value").and_then(Value::as_str)
            {
                out.push_str(value);
                out.push('\n');
            }
        }
    }
    if kind.is_none() {
        if let Some(full) = name.and_then(|n| n.get("full")).and_then(Value::as_str) {
            out.push_str(full);
            out.push('\n');
        }
        texts(card.get("nicknames"), &["name"], &mut out);
    }
    out
}

/// The texts of one filter condition of ContactCard/query (RFC 9610, section 3.3.1).
pub fn field_text(card: &Map<String, Value>, condition: &str) -> String {
    let mut out = String::new();
    match condition {
        "name" => return name_text(card, None),
        "name/given" => return name_text(card, Some("given")),
        "name/surname" => return name_text(card, Some("surname")),
        "name/surname2" => return name_text(card, Some("surname2")),
        "nickname" => texts(card.get("nicknames"), &["name"], &mut out),
        "organization" => texts(card.get("organizations"), &["name", "units"], &mut out),
        "email" => texts(card.get("emails"), &["address", "label"], &mut out),
        "phone" => texts(card.get("phones"), &["number", "label"], &mut out),
        "onlineService" => texts(card.get("onlineServices"), &["service", "uri", "user", "label"], &mut out),
        "address" => texts(card.get("addresses"), &["components", "full", "countryCode"], &mut out),
        "note" => texts(card.get("notes"), &["note"], &mut out),
        _ => {}
    }
    out
}

/// Everything a `text` condition searches.
pub fn all_text(card: &Map<String, Value>) -> String {
    let mut out = String::new();
    for condition in ["name", "organization", "email", "phone", "onlineService", "address", "note"] {
        out.push_str(&field_text(card, condition));
    }
    texts(card.get("titles"), &["name"], &mut out);
    out
}

/// The value a card is sorted by: the first name component of a kind.
pub fn sort_text(card: &Map<String, Value>, kind: &str) -> String {
    name_text(card, Some(kind)).lines().next().unwrap_or_default().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const APPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iOS 17//EN\r\nN:Katze;Nyu;;;\r\n\
FN:Nyu Katze\r\nORG:Katzen GmbH;\r\nEMAIL;type=INTERNET;type=HOME;type=pref:nyu@example.org\r\n\
TEL;type=CELL;type=VOICE;type=pref:+49 170 1234567\r\nitem1.ADR;type=HOME;type=pref:;;Hauptstr. 1;Berlin;;10115;Germany\r\n\
item1.X-ABADR:de\r\nBDAY:1990-05-17\r\nNOTE:mag Thunfisch\r\nX-APPLE-SPECIAL:bleibt\r\nUID:nyu-1\r\nEND:VCARD\r\n";

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn cards_go_back_as_they_came() {
        let card = from_vcard(APPLE).unwrap();
        assert_eq!(card["uid"], "nyu-1");
        assert_eq!(card["name"]["full"], "Nyu Katze");
        let mut changed = card.clone();
        changed.insert("notes".into(), json!({ "k1": { "note": "mag Lachs" } }));
        let written = to_vcard(&changed).unwrap();
        assert!(written.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n"), "{written}");
        assert!(written.contains("NOTE;PROP-ID=k1:mag Lachs"), "{written}");
        assert!(written.contains("X-APPLE-SPECIAL:bleibt"), "what JSContact has no word for stays: {written}");
        assert!(written.contains("item1.X-ABADR:de"), "{written}");
        let again = from_vcard(&written).unwrap();
        assert_eq!(again["emails"], card["emails"]);
        assert_eq!(again["addresses"]["k1"]["components"], card["addresses"]["k1"]["components"]);
    }

    /// Found in the 0.18.0 smoke test: a birthday imported into a vCard 3.0 card (`--10-15`) came back over
    /// JMAP as October without a day, and so did `1996-10-03` in a vCard 4.0 card.
    #[test]
    fn dates_in_the_extended_format_keep_their_day() {
        let birthday = |version: &str, line: &str| {
            let content = format!("BEGIN:VCARD\r\nVERSION:{version}\r\nFN:Tom\r\nUID:t\r\n{line}\r\nEND:VCARD\r\n");
            let card = from_vcard(&content).unwrap();
            let mut date = card["anniversaries"].as_object().unwrap().values().next().unwrap()["date"].clone();
            date.as_object_mut().unwrap().remove("@type");
            date
        };
        for version in ["3.0", "4.0"] {
            assert_eq!(birthday(version, "BDAY:--10-15"), json!({ "month": 10, "day": 15 }), "{version}");
            assert_eq!(birthday(version, "BDAY:--1015"), json!({ "month": 10, "day": 15 }), "{version}");
            assert_eq!(birthday(version, "BDAY:1996-10-03"), json!({ "year": 1996, "month": 10, "day": 3 }));
            assert_eq!(
                birthday(version, "item1.BDAY;PROP-ID=b:1996-10-03"),
                json!({ "year": 1996, "month": 10, "day": 3 })
            );
            assert_eq!(birthday(version, "ANNIVERSARY:2021-06-12"), json!({ "year": 2021, "month": 6, "day": 12 }));
        }
        // Other lines, and dates that are something else, stay as written.
        let content =
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Zoë\r\nNOTE:am 1996-10-03\r\nBDAY;VALUE=text:circa 1800\r\nEND:VCARD\r\n";
        assert!(matches!(basic_dates(content), Cow::Borrowed(_)));
        let content = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Zoë\r\nBDAY:--10-15\r\nNOTE:1996-10-03\r\nEND:VCARD\r\n";
        assert_eq!(
            basic_dates(content),
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Zoë\r\nBDAY:--1015\r\nNOTE:1996-10-03\r\nEND:VCARD\r\n"
        );
    }

    #[test]
    fn birthday_reminders_are_their_own_property() {
        let content = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Max\r\nUID:m\r\nBDAY:--0412\r\n\
X-UWUMAIL-REMINDER:1 09:00\r\nX-UWUMAIL-REMINDER:0 9:00\r\nX-UWUMAIL-REMINDER:kaputt\r\nEND:VCARD\r\n";
        let card = from_vcard(content).unwrap();
        assert_eq!(
            card["uwuReminders"],
            json!([{ "daysBefore": 0, "time": "09:00" }, { "daysBefore": 1, "time": "09:00" }])
        );
        assert!(!card["vCard"].to_string().to_ascii_lowercase().contains("reminder"), "one place only");
        let mut changed = card.clone();
        changed.insert("uwuReminders".into(), json!([{ "daysBefore": 7, "time": "18:30" }]));
        let written = to_vcard(&changed).unwrap();
        assert!(written.contains("X-UWUMAIL-REMINDER:7 18:30\r\n"), "{written}");
        assert!(!written.contains("1 09:00"), "{written}");
        assert!(written.contains("BDAY:--0412") || written.contains("BDAY;"), "{written}");
        // Without the property the vCard keeps none.
        let mut none = card.clone();
        none.remove("uwuReminders");
        assert!(!to_vcard(&none).unwrap().contains("X-UWUMAIL-REMINDER"));
        // A new card without a vCard property gets them too.
        let fresh = object(json!({ "@type": "Card", "uid": "n", "name": { "full": "Nyu" },
            "uwuReminders": [{ "daysBefore": 0, "time": "09:00" }] }));
        assert!(to_vcard(&fresh).unwrap().contains("X-UWUMAIL-REMINDER:0 09:00"));
        for bad in [
            json!([{ "daysBefore": 29, "time": "09:00" }]),
            json!([{ "daysBefore": 1, "time": "9:00" }]),
            json!("1 09:00"),
            json!([{}, {}, {}, {}, {}, {}]),
        ] {
            let card = object(json!({ "@type": "Card", "uid": "x", "uwuReminders": bad }));
            assert_eq!(validate(&card).unwrap_err().properties, vec!["uwuReminders"]);
        }
    }

    #[test]
    fn new_cards_are_vcard_3() {
        let card = object(json!({
            "@type": "Card", "version": "1.0", "uid": "urn:uuid:1",
            "name": { "components": [{ "kind": "given", "value": "Leni" }, { "kind": "surname", "value": "Muster" }] },
            "emails": { "e1": { "address": "leni@example.org" } },
            "id": "k1", "addressBookIds": { "b1": true }
        }));
        let written = to_vcard(&card).unwrap();
        assert!(written.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n"), "{written}");
        assert!(written.contains("FN") && written.contains("Leni Muster"), "a full name is derived: {written}");
        assert!(!written.contains("addressBookIds") && !written.contains("b1"), "{written}");
        assert!(VCard::parse(&written).is_ok());
    }

    #[test]
    fn rules() {
        let good = object(json!({ "@type": "Card", "uid": "x" }));
        assert!(validate(&good).is_ok());
        let check = |extra: Value| {
            let mut card = good.clone();
            card.extend(object(extra));
            validate(&card).map_err(|err| err.properties)
        };
        assert_eq!(check(json!({ "@type": "Event" })), Err(vec!["@type".into()]));
        assert_eq!(check(json!({ "uid": "" })), Err(vec!["uid".into()]));
        assert_eq!(check(json!({ "updated": "gestern" })), Err(vec!["updated".into()]));
        assert_eq!(check(json!({ "emails": { "e": { "label": "ohne" } } })), Err(vec!["emails".into()]));
        assert_eq!(check(json!({ "phones": { "p": "0170" } })), Err(vec!["phones".into()]));
        let photo = |uri: &str| json!({ "media": { "m": { "kind": "photo", "uri": uri } } });
        assert!(check(photo("data:image/jpeg;base64,/9j/")).is_ok());
        assert!(check(photo("https://example.net/nyu.jpg")).is_ok());
        assert_eq!(check(photo("data:text/html;base64,PGI+")), Err(vec!["media".into()]));
        assert_eq!(check(json!({ "media": { "m": { "kind": "photo", "blobId": "B1" } } })), Err(vec!["media".into()]));
    }

    #[test]
    fn texts_for_queries() {
        let card = from_vcard(APPLE).unwrap();
        assert!(field_text(&card, "name").contains("Nyu Katze"));
        assert_eq!(field_text(&card, "name/given"), "Nyu\n");
        assert!(field_text(&card, "email").contains("nyu@example.org"));
        assert!(field_text(&card, "organization").contains("Katzen GmbH"));
        assert!(field_text(&card, "address").contains("Berlin"));
        assert!(all_text(&card).contains("Thunfisch"));
        assert_eq!(sort_text(&card, "surname"), "katze");
    }
}
