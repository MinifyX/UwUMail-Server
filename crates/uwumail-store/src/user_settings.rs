//! Settings the webmail and the UwUMail apps keep in sync for an account: one flat map of keys to
//! JSON values, served over JMAP as `UserSettings` (docs/jmap-settings.md).
//!
//! Every key is on a whitelist with rules for its value, because whatever is stored here is handed
//! to every other device of the account. The keys the portal already keeps as preferences (theme,
//! tone, language and the webmail's mail choices) are not stored twice: they are read from and
//! written to `accounts.preferences`, so the portal, the webmail and the apps see the same value.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Map, Value};

use crate::db::{next_modseq, record_change};
use crate::{Result, Store, StoreError};

/// How many keys one account may keep.
pub const USER_SETTINGS_MAX_KEYS: usize = 5000;
/// How large all values together may be, as the JSON object `values`, in bytes.
pub const USER_SETTINGS_MAX_SIZE: usize = 1_048_576;
/// How large one value may be, as JSON, in bytes.
pub const USER_SETTINGS_MAX_VALUE_SIZE: usize = 262_144;

/// Longest address or domain in a list key.
const ENTRY_MAX_CHARS: usize = 254;
const DOMAIN_MAX_CHARS: usize = 253;
/// Longest id in a key like `signature:<id>`.
const ID_MAX_CHARS: usize = 64;
const SIGNATURE_NAME_MAX_CHARS: usize = 100;
/// Longest folder path in a folder reference or a snoozed message.
const FOLDER_MAX_CHARS: usize = 1024;
/// How many accounts `accounts.order` may list.
const ACCOUNTS_ORDER_MAX: usize = 200;
/// Longest Message-ID of a snoozed message, the longest line RFC 5322 allows.
const MESSAGE_ID_MAX_CHARS: usize = 998;
const TEMPLATE_NAME_MAX_CHARS: usize = 100;
const TEMPLATE_SUBJECT_MAX_CHARS: usize = 998;
/// The portal's preferences may not grow past this (see `update_preferences`).
const PREFERENCES_MAX_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy)]
enum Mirror {
    /// Stored as the same string.
    Text,
    /// A boolean here, `"on"`/`"off"` in the preferences.
    OnOff,
    /// A whole number here, its digits as a string in the preferences.
    Number,
}

/// Settings that live in the portal preferences: the key here, its name there, how it is spelled.
const MIRRORED: &[(&str, &str, Mirror)] = &[
    ("theme", "theme", Mirror::Text),
    ("tone", "tone", Mirror::Text),
    ("language", "language", Mirror::Text),
    ("conversations", "mailConversations", Mirror::OnOff),
    ("remoteImages", "mailRemoteImages", Mirror::Text),
    ("mailAppearance", "mailAppearance", Mirror::Text),
    ("senderPictures", "mailSenderPictures", Mirror::OnOff),
    ("undoSendSeconds", "mailUndoSend", Mirror::Number),
];

/// How long a message waits before it goes, when the person has not chosen: long enough to
/// notice the typo in the address.
pub const DEFAULT_UNDO_SEND_SECONDS: u64 = 10;

/// Why a key or value is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingProblem {
    /// Not a known key, or not a value the key allows.
    Invalid(String),
    /// The value is larger than [`USER_SETTINGS_MAX_VALUE_SIZE`].
    TooLarge,
}

/// One account's settings and their state.
#[derive(Debug, Clone, PartialEq)]
pub struct UserSettings {
    /// Changes with every write; the JMAP state of `UserSettings`.
    pub state: String,
    pub values: Map<String, Value>,
}

/// A write to the settings.
#[derive(Debug, Clone)]
pub enum SettingsChange {
    /// Sets each key to its value, or removes it for `None`; other keys stay as they are.
    Patch(Vec<(String, Option<Value>)>),
    /// Replaces all settings with these.
    Replace(Map<String, Value>),
}

fn invalid(reason: impl Into<String>) -> SettingProblem {
    SettingProblem::Invalid(reason.into())
}

fn one_of(key: &str, value: &Value, allowed: &[&str]) -> Result<(), SettingProblem> {
    match value.as_str() {
        Some(text) if allowed.contains(&text) => Ok(()),
        _ => Err(invalid(format!("{key} must be one of {}", allowed.join(", ")))),
    }
}

fn boolean(key: &str, value: &Value) -> Result<(), SettingProblem> {
    if value.is_boolean() { Ok(()) } else { Err(invalid(format!("{key} must be true or false"))) }
}

fn only_true(key: &str, value: &Value) -> Result<(), SettingProblem> {
    if value == &Value::Bool(true) { Ok(()) } else { Err(invalid(format!("{key} must be true"))) }
}

/// A lower-case ASCII host name (IDNs in punycode), as a link points at it.
fn is_ascii_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= DOMAIN_MAX_CHARS
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The domain of a mail address: at least two labels, lower case; international letters are
/// allowed, because addresses are compared the way they are written in a message.
fn is_mail_domain(domain: &str) -> bool {
    domain.chars().count() <= DOMAIN_MAX_CHARS
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.chars().count() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| {
                    c == '-'
                        || c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || (!c.is_ascii() && c.is_alphanumeric() && !c.is_uppercase())
                })
        })
}

/// A lower-case mail address `local@domain.tld`.
fn is_address(entry: &str) -> bool {
    let Some((local, domain)) = entry.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.chars().count() <= 64
        && local.chars().all(|c| c != '@' && !c.is_whitespace() && !c.is_control() && !c.is_uppercase())
        && is_mail_domain(domain)
}

fn is_list_entry_length(entry: &str) -> bool {
    entry.chars().count() <= ENTRY_MAX_CHARS
}

fn signature(key: &str, value: &Value) -> Result<(), SettingProblem> {
    let Some(object) = value.as_object() else {
        return Err(invalid(format!("{key} must be an object")));
    };
    for property in object.keys() {
        if !["email", "name", "html", "forNew", "forReplies"].contains(&property.as_str()) {
            return Err(invalid(format!("{key} has an unknown property {property}")));
        }
    }
    let text = |property: &str| -> Result<&str, SettingProblem> {
        object
            .get(property)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("{key}: {property} must be a string")))
    };
    let email = text("email")?;
    // Empty means the signature is not tied to one address.
    if !email.is_empty() && !(is_list_entry_length(email) && is_address(&email.to_lowercase())) {
        return Err(invalid(format!("{key}: email must be a mail address or empty")));
    }
    let name = text("name")?;
    if name.chars().count() > SIGNATURE_NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(invalid(format!("{key}: name must be at most {SIGNATURE_NAME_MAX_CHARS} characters")));
    }
    // The HTML is only stored; every client cleans it like mail HTML before showing it. Its size
    // is bounded by the size of the whole value.
    text("html")?;
    for flag in ["forNew", "forReplies"] {
        if !object.get(flag).is_some_and(Value::is_boolean) {
            return Err(invalid(format!("{key}: {flag} must be true or false")));
        }
    }
    Ok(())
}

/// The id in a key like `signature:<id>`: 1 to 64 of `A-Z a-z 0-9 _ -`.
fn is_id(id: &str) -> bool {
    (1..=ID_MAX_CHARS).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn id_entry(key: &str, id: &str) -> Result<(), SettingProblem> {
    if is_id(id) { Ok(()) } else { Err(invalid(format!("{key}: the id must be 1 to 64 of A-Z, a-z, 0-9, - and _"))) }
}

fn address_entry(key: &str, entry: &str) -> Result<(), SettingProblem> {
    if is_list_entry_length(entry) && is_address(entry) {
        Ok(())
    } else {
        Err(invalid(format!("{key}: the entry must be a lower-case address")))
    }
}

/// An object with exactly these properties.
fn object_with<'a>(key: &str, value: &'a Value, properties: &[&str]) -> Result<&'a Map<String, Value>, SettingProblem> {
    let Some(object) = value.as_object() else {
        return Err(invalid(format!("{key} must be an object")));
    };
    for property in object.keys() {
        if !properties.contains(&property.as_str()) {
            return Err(invalid(format!("{key} has an unknown property {property}")));
        }
    }
    for property in properties {
        if !object.contains_key(*property) {
            return Err(invalid(format!("{key}: {property} is missing")));
        }
    }
    Ok(object)
}

fn text<'a>(key: &str, object: &'a Map<String, Value>, property: &str) -> Result<&'a str, SettingProblem> {
    object.get(property).and_then(Value::as_str).ok_or_else(|| invalid(format!("{key}: {property} must be a string")))
}

/// A string of `min..=max` characters without control characters.
fn plain_text(
    key: &str,
    object: &Map<String, Value>,
    property: &str,
    min: usize,
    max: usize,
) -> Result<(), SettingProblem> {
    let value = text(key, object, property)?;
    if (min..=max).contains(&value.chars().count()) && !value.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(invalid(format!("{key}: {property} must be {min} to {max} characters without control characters")))
    }
}

fn account_property(key: &str, object: &Map<String, Value>) -> Result<(), SettingProblem> {
    let account = text(key, object, "account")?;
    if is_list_entry_length(account) && is_address(account) {
        Ok(())
    } else {
        Err(invalid(format!("{key}: account must be a lower-case address")))
    }
}

/// `{ "account": <address>, "folder": <path> }`: a folder of one of the person's accounts.
fn folder_ref(key: &str, value: &Value) -> Result<(), SettingProblem> {
    let object = object_with(key, value, &["account", "folder"])?;
    account_property(key, object)?;
    plain_text(key, object, "folder", 1, FOLDER_MAX_CHARS)
}

/// The accounts in the order the person sorted them: unique lower-case addresses.
fn accounts_order(key: &str, value: &Value) -> Result<(), SettingProblem> {
    let Some(list) = value.as_array() else {
        return Err(invalid(format!("{key} must be a list of addresses")));
    };
    if list.len() > ACCOUNTS_ORDER_MAX {
        return Err(invalid(format!("{key} may list at most {ACCOUNTS_ORDER_MAX} accounts")));
    }
    let mut seen = std::collections::HashSet::new();
    for entry in list {
        let Some(address) = entry.as_str().filter(|a| is_list_entry_length(a) && is_address(a)) else {
            return Err(invalid(format!("{key} must list lower-case addresses")));
        };
        if !seen.insert(address) {
            return Err(invalid(format!("{key} lists {address} twice")));
        }
    }
    Ok(())
}

/// `HH:MM`, 00:00 to 23:59.
fn is_clock_time(time: &str) -> bool {
    let bytes = time.as_bytes();
    bytes.len() == 5
        && bytes[2] == b':'
        && time[..2].bytes().chain(time[3..].bytes()).all(|b| b.is_ascii_digit())
        && time[..2].parse::<u8>().is_ok_and(|hour| hour < 24)
        && time[3..].parse::<u8>().is_ok_and(|minute| minute < 60)
}

/// `{ "enabled", "from": "HH:MM", "to": "HH:MM", "days": [0-6] }`; `from` after `to` runs over midnight.
fn quiet_hours(key: &str, value: &Value) -> Result<(), SettingProblem> {
    let object = object_with(key, value, &["enabled", "from", "to", "days"])?;
    if !object["enabled"].is_boolean() {
        return Err(invalid(format!("{key}: enabled must be true or false")));
    }
    for property in ["from", "to"] {
        if !is_clock_time(text(key, object, property)?) {
            return Err(invalid(format!("{key}: {property} must be a time HH:MM")));
        }
    }
    let Some(days) = object["days"].as_array() else {
        return Err(invalid(format!("{key}: days must be a list")));
    };
    let mut seen = [false; 7];
    for day in days {
        match day.as_u64().filter(|day| *day < 7) {
            // Sunday is 0, as in JavaScript.
            Some(day) if !seen[day as usize] => seen[day as usize] = true,
            _ => return Err(invalid(format!("{key}: days must be different numbers from 0 (Sunday) to 6"))),
        }
    }
    Ok(())
}

/// A UTC time like `2026-10-21T07:00:00Z`, with or without fractions of a second.
fn is_utc_timestamp(time: &str) -> bool {
    let bytes = time.as_bytes();
    let shape = bytes.len() >= 20
        && bytes.iter().enumerate().take(19).all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            10 => *b == b'T',
            13 | 16 => *b == b':',
            _ => b.is_ascii_digit(),
        })
        && bytes[bytes.len() - 1] == b'Z'
        && match &bytes[19..bytes.len() - 1] {
            [] => true,
            [b'.', fraction @ ..] => (1..=9).contains(&fraction.len()) && fraction.iter().all(u8::is_ascii_digit),
            _ => false,
        };
    shape && chrono::DateTime::parse_from_rfc3339(time).is_ok()
}

/// A message put away until later: where it is now, where it came from, and when it comes back.
fn snooze(key: &str, value: &Value) -> Result<(), SettingProblem> {
    let object = object_with(key, value, &["account", "messageId", "folder", "until"])?;
    account_property(key, object)?;
    plain_text(key, object, "messageId", 1, MESSAGE_ID_MAX_CHARS)?;
    let message_id = text(key, object, "messageId")?;
    if message_id.contains(['<', '>']) || message_id.chars().any(char::is_whitespace) {
        return Err(invalid(format!("{key}: messageId must be the Message-ID without <>")));
    }
    plain_text(key, object, "folder", 1, FOLDER_MAX_CHARS)?;
    if !is_utc_timestamp(text(key, object, "until")?) {
        return Err(invalid(format!("{key}: until must be a UTC time like 2026-10-21T07:00:00Z")));
    }
    Ok(())
}

/// A text snippet for the composer: `{ "name", "subject", "html" }`.
fn template(key: &str, value: &Value) -> Result<(), SettingProblem> {
    let object = object_with(key, value, &["name", "subject", "html"])?;
    plain_text(key, object, "name", 1, TEMPLATE_NAME_MAX_CHARS)?;
    plain_text(key, object, "subject", 0, TEMPLATE_SUBJECT_MAX_CHARS)?;
    // Like a signature's: only stored, cleaned by every client, bounded by the size of the value.
    text(key, object, "html")?;
    Ok(())
}

/// Checks one key and its (non-null) value against the whitelist.
pub fn validate_setting(key: &str, value: &Value) -> Result<(), SettingProblem> {
    // Measured as JSON, the way it is stored and sent.
    if value.to_string().len() > USER_SETTINGS_MAX_VALUE_SIZE {
        return Err(SettingProblem::TooLarge);
    }
    match key {
        "theme" => return one_of(key, value, &["system", "light", "dark"]),
        "tone" => return one_of(key, value, &["playful", "neutral"]),
        "language" => return one_of(key, value, &["system", "de", "en", "fr", "nl", "ja", "zh"]),
        "conversations"
        | "senderPictures"
        | "linkConfirm"
        | "darkImages"
        | "assist.refineEvents"
        | "mail.detectEvents"
        | "compose.attachmentReminder" => {
            return boolean(key, value);
        }
        "remoteImages" => return one_of(key, value, &["ask", "always"]),
        "assist.currency" => return one_of(key, value, &["EUR", "USD"]),
        "nyu.animations" => return one_of(key, value, &["on", "reduced", "off"]),
        "mailAppearance" => return one_of(key, value, &["auto", "light", "dark"]),
        "undoSendSeconds" => {
            return match value.as_u64() {
                Some(0 | 5 | 10 | 20 | 30) => Ok(()),
                _ => Err(invalid("undoSendSeconds must be one of 0, 5, 10, 20, 30")),
            };
        }
        "accounts.order" => return accounts_order(key, value),
        "notify.quietHours" => return quiet_hours(key, value),
        _ => {}
    }
    let Some((prefix, rest)) = key.split_once(':') else {
        return Err(invalid(format!("{key} is not a known setting")));
    };
    match prefix {
        "trustedSenders" => {
            let domain_entry = rest.strip_prefix('@').is_some_and(is_mail_domain);
            if !is_list_entry_length(rest) || !(domain_entry || is_address(rest)) {
                return Err(invalid(format!("{key}: the entry must be a lower-case address or @domain")));
            }
            only_true(key, value)
        }
        "senderAppearance" => {
            address_entry(key, rest)?;
            one_of(key, value, &["light", "dark"])
        }
        "linkDomains" => {
            if !is_ascii_host(rest) {
                return Err(invalid(format!("{key}: the entry must be a lower-case ASCII host name")));
            }
            only_true(key, value)
        }
        "signature" => {
            id_entry(key, rest)?;
            signature(key, value)
        }
        "unified.hidden" | "notify.muted" => {
            address_entry(key, rest)?;
            only_true(key, value)
        }
        "unified.folders" | "folders.favorite" | "folders.hidden" => {
            id_entry(key, rest)?;
            folder_ref(key, value)
        }
        "snooze" => {
            id_entry(key, rest)?;
            snooze(key, value)
        }
        "template" => {
            id_entry(key, rest)?;
            template(key, value)
        }
        _ => Err(invalid(format!("{key} is not a known setting"))),
    }
}

fn mirror(key: &str) -> Option<(&'static str, Mirror)> {
    MIRRORED.iter().find(|(name, _, _)| *name == key).map(|(_, preference, kind)| (*preference, *kind))
}

/// Whether a portal preference is also one of the synced settings.
pub(crate) fn is_mirrored_preference(preference: &str) -> bool {
    MIRRORED.iter().any(|(_, name, _)| *name == preference)
}

/// Records a write to the settings: a new state, and a change the push listeners see.
pub(crate) fn bump_settings_state(tx: &Transaction<'_>, account_id: i64) -> Result<i64> {
    let modseq = next_modseq(tx, account_id)?;
    tx.execute("UPDATE accounts SET settings_modseq = ?1 WHERE id = ?2", params![modseq, account_id])?;
    record_change(tx, account_id, modseq, "UserSettings", 0, "updated")?;
    Ok(modseq)
}

/// The preferences as JSON and the settings state of an account.
fn account_row(conn: &Connection, account_id: i64) -> Result<(Map<String, Value>, i64)> {
    let (raw, modseq): (String, i64) = conn
        .query_row("SELECT preferences, settings_modseq FROM accounts WHERE id = ?1", [account_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
    Ok((serde_json::from_str(&raw).unwrap_or_default(), modseq))
}

/// All settings of an account, the mirrored ones from its preferences. Anything stored that does
/// not pass the rules (an older value, a hand-edited database) is left out rather than handed on.
fn read_values(conn: &Connection, account_id: i64, preferences: &Map<String, Value>) -> Result<Map<String, Value>> {
    let mut values = Map::new();
    let mut stmt = conn.prepare("SELECT key, value FROM user_settings WHERE account_id = ?1")?;
    let rows = stmt.query_map([account_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    for row in rows {
        let (key, raw) = row?;
        if mirror(&key).is_some() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&raw)
            && validate_setting(&key, &value).is_ok()
        {
            values.insert(key, value);
        }
    }
    for (key, preference, kind) in MIRRORED {
        let Some(text) = preferences.get(*preference).and_then(Value::as_str) else {
            continue;
        };
        let value = match kind {
            Mirror::Text => Value::String(text.to_owned()),
            Mirror::OnOff => match text {
                "on" => Value::Bool(true),
                "off" => Value::Bool(false),
                _ => continue,
            },
            Mirror::Number => match text.parse::<u64>() {
                Ok(number) => Value::from(number),
                Err(_) => continue,
            },
        };
        if validate_setting(key, &value).is_ok() {
            values.insert((*key).to_owned(), value);
        }
    }
    Ok(values)
}

fn to_preference(kind: Mirror, value: &Value) -> Value {
    match (kind, value) {
        (Mirror::OnOff, Value::Bool(on)) => Value::String(if *on { "on" } else { "off" }.into()),
        (Mirror::Number, Value::Number(number)) => Value::String(number.to_string()),
        (_, other) => other.clone(),
    }
}

fn rule(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Rule { code, message: message.into() }
}

impl Store {
    pub async fn user_settings(&self, account_id: i64) -> Result<UserSettings> {
        self.read(move |conn| {
            let (preferences, modseq) = account_row(conn, account_id)?;
            let values = read_values(conn, account_id, &preferences)?;
            Ok(UserSettings { state: modseq.to_string(), values })
        })
        .await
    }

    /// How many seconds a JMAP submission without its own `sendAt` waits, so it can still be
    /// cancelled: the setting `undoSendSeconds`, [`DEFAULT_UNDO_SEND_SECONDS`] when there is none.
    pub async fn undo_send_seconds(&self, account_id: i64) -> Result<u64> {
        self.read(move |conn| {
            let (preferences, _) = account_row(conn, account_id)?;
            Ok(preferences
                .get("mailUndoSend")
                .and_then(Value::as_str)
                .and_then(|text| text.parse::<u64>().ok())
                .filter(|seconds| validate_setting("undoSendSeconds", &Value::from(*seconds)).is_ok())
                .unwrap_or(DEFAULT_UNDO_SEND_SECONDS))
        })
        .await
    }

    /// Only the state, for push.
    pub async fn user_settings_state(&self, account_id: i64) -> Result<String> {
        self.read(move |conn| Ok(account_row(conn, account_id)?.1.to_string())).await
    }

    /// Applies `change` as one write: all of it or nothing.
    ///
    /// With `if_in_state`, refuses with the rule `stateMismatch` when the settings have moved on
    /// since. Every key is checked again here; a bad one is `StoreError::Invalid`. Going over the
    /// limits is the rule `overQuota`, a value that is too large the rule `tooLarge`.
    pub async fn update_user_settings(
        &self,
        account_id: i64,
        change: SettingsChange,
        if_in_state: Option<String>,
    ) -> Result<UserSettings> {
        let (settings, modseq) = self
            .write(move |tx| {
                // Counted before anything else: removals and `null`s cost a statement each as well,
                // and no write needs to touch more keys than an account may keep.
                let touched = match &change {
                    SettingsChange::Patch(writes) => writes.len(),
                    SettingsChange::Replace(all) => all.len(),
                };
                if touched > USER_SETTINGS_MAX_KEYS {
                    return Err(rule("overQuota", format!("at most {USER_SETTINGS_MAX_KEYS} settings in one write")));
                }
                let (mut preferences, modseq) = account_row(tx, account_id)?;
                if if_in_state.is_some_and(|expected| expected != modseq.to_string()) {
                    return Err(rule("stateMismatch", "the settings have changed since"));
                }
                let old = read_values(tx, account_id, &preferences)?;
                let mut values = match &change {
                    SettingsChange::Patch(_) => old.clone(),
                    SettingsChange::Replace(_) => Map::new(),
                };
                let writes: Vec<(String, Option<Value>)> = match change {
                    SettingsChange::Patch(writes) => writes,
                    SettingsChange::Replace(all) => {
                        // Everything that was there and is not any more goes.
                        let removed = old.keys().filter(|key| !all.contains_key(*key)).map(|key| (key.clone(), None));
                        let removed: Vec<_> = removed.collect();
                        all.into_iter()
                            .map(|(key, value)| (key, (!value.is_null()).then_some(value)))
                            .chain(removed)
                            .collect()
                    }
                };
                for (key, value) in &writes {
                    match value {
                        Some(value) => {
                            match validate_setting(key, value) {
                                Ok(()) => {}
                                Err(SettingProblem::TooLarge) => {
                                    return Err(rule("tooLarge", format!("{key} is larger than the limit")));
                                }
                                Err(SettingProblem::Invalid(reason)) => return Err(StoreError::Invalid(reason)),
                            }
                            values.insert(key.clone(), value.clone());
                        }
                        None => {
                            values.remove(key);
                        }
                    }
                }
                if values.len() > USER_SETTINGS_MAX_KEYS {
                    return Err(rule("overQuota", format!("at most {USER_SETTINGS_MAX_KEYS} settings")));
                }
                if Value::Object(values.clone()).to_string().len() > USER_SETTINGS_MAX_SIZE {
                    return Err(rule(
                        "overQuota",
                        format!("the settings may take at most {USER_SETTINGS_MAX_SIZE} bytes"),
                    ));
                }

                let mut preferences_changed = false;
                for (key, value) in &writes {
                    if let Some((preference, kind)) = mirror(key) {
                        match value {
                            Some(value) => preferences.insert(preference.to_owned(), to_preference(kind, value)),
                            None => preferences.remove(preference),
                        };
                        preferences_changed = true;
                        continue;
                    }
                    match value {
                        Some(value) => tx.execute(
                            "INSERT INTO user_settings (account_id, key, value) VALUES (?1, ?2, ?3)
                             ON CONFLICT (account_id, key) DO UPDATE SET value = excluded.value",
                            params![account_id, key, value.to_string()],
                        )?,
                        None => tx.execute(
                            "DELETE FROM user_settings WHERE account_id = ?1 AND key = ?2",
                            params![account_id, key],
                        )?,
                    };
                }
                if preferences_changed {
                    let encoded = Value::Object(preferences).to_string();
                    if encoded.len() > PREFERENCES_MAX_BYTES {
                        return Err(StoreError::Invalid("preferences are too large".into()));
                    }
                    tx.execute("UPDATE accounts SET preferences = ?1 WHERE id = ?2", params![encoded, account_id])?;
                }
                let modseq = bump_settings_state(tx, account_id)?;
                Ok((UserSettings { state: modseq.to_string(), values }, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    fn check(key: &str, value: Value) -> Result<(), SettingProblem> {
        validate_setting(key, &value)
    }

    #[test]
    fn fixed_keys_take_only_their_values() {
        for (key, good, bad) in [
            ("theme", json!("dark"), json!("pink")),
            ("tone", json!("playful"), json!("rude")),
            ("language", json!("fr"), json!("klingon")),
            ("conversations", json!(true), json!("on")),
            ("remoteImages", json!("always"), json!("never")),
            ("mailAppearance", json!("auto"), json!("system")),
            ("senderPictures", json!(false), json!(0)),
            ("undoSendSeconds", json!(20), json!(15)),
            ("linkConfirm", json!(true), json!(null)),
            ("darkImages", json!(false), json!("on")),
            ("assist.currency", json!("USD"), json!("usd")),
            ("assist.currency", json!("EUR"), json!("JPY")),
            ("nyu.animations", json!("reduced"), json!("some")),
            ("mail.detectEvents", json!(false), json!("yes")),
            ("assist.refineEvents", json!(true), json!(1)),
        ] {
            assert_eq!(check(key, good), Ok(()), "{key}");
            assert!(matches!(check(key, bad), Err(SettingProblem::Invalid(_))), "{key}");
        }
        assert!(check("undoSendSeconds", json!(10.0)).is_err());
        assert!(check("undoSendSeconds", json!(-5)).is_err());
        assert!(check("colour", json!("pink")).is_err());
        assert!(check("", json!(true)).is_err());
        assert!(check("Theme", json!("dark")).is_err());
    }

    #[test]
    fn list_keys_check_their_entry() {
        assert_eq!(check("trustedSenders:news@shop.example", json!(true)), Ok(()));
        assert_eq!(check("trustedSenders:@shop.example", json!(true)), Ok(()));
        assert_eq!(check("trustedSenders:grüße@bäckerei.example", json!(true)), Ok(()));
        for bad in [
            "trustedSenders:News@shop.example",
            "trustedSenders:news@shop",
            "trustedSenders:@",
            "trustedSenders:",
            "trustedSenders:a b@shop.example",
            "trustedSenders:a@@shop.example",
            "trustedSenders:a@shop..example",
            "trustedSenders:a@-shop.example",
            "trustedSenders:a\n@shop.example",
        ] {
            assert!(check(bad, json!(true)).is_err(), "{bad}");
        }
        assert!(check("trustedSenders:news@shop.example", json!(false)).is_err());
        let long = format!("trustedSenders:{}@{}example.org", "a".repeat(60), "b.".repeat(100));
        assert!(check(&long, json!(true)).is_err());

        assert_eq!(check("senderAppearance:news@shop.example", json!("dark")), Ok(()));
        assert!(check("senderAppearance:@shop.example", json!("dark")).is_err());
        assert!(check("senderAppearance:news@shop.example", json!("auto")).is_err());

        assert_eq!(check("linkDomains:xn--bckerei-9wa.example", json!(true)), Ok(()));
        assert_eq!(check("linkDomains:intranet", json!(true)), Ok(()));
        for bad in ["linkDomains:Shop.example", "linkDomains:bäckerei.example", "linkDomains:a..b", "linkDomains:"] {
            assert!(check(bad, json!(true)).is_err(), "{bad}");
        }
        assert!(check(&format!("linkDomains:{}", "a.".repeat(127) + "ab"), json!(true)).is_err());
        assert!(check("somethingElse:x", json!(true)).is_err());
    }

    #[test]
    fn signatures_are_checked_strictly() {
        let good = json!({ "email": "Mini@Example.org", "name": "Work", "html": "<p>Hi</p>", "forNew": true, "forReplies": false });
        assert_eq!(check("signature:work_1-A", good.clone()), Ok(()));
        let mut empty_email = good.clone();
        empty_email["email"] = json!("");
        assert_eq!(check("signature:w", empty_email), Ok(()));

        assert!(check("signature:", good.clone()).is_err());
        assert!(check("signature:has space", good.clone()).is_err());
        assert!(check(&format!("signature:{}", "a".repeat(65)), good.clone()).is_err());
        for (property, value) in [
            ("email", json!("not an address")),
            ("name", json!("n".repeat(101))),
            ("html", json!(5)),
            ("forNew", json!("yes")),
            ("extra", json!(1)),
        ] {
            let mut bad = good.clone();
            bad[property] = value;
            assert!(check("signature:w", bad).is_err(), "{property}");
        }
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove("forReplies");
        assert!(check("signature:w", missing).is_err());
        assert!(check("signature:w", json!("<p>Hi</p>")).is_err());

        let mut huge = good;
        huge["html"] = json!("x".repeat(USER_SETTINGS_MAX_VALUE_SIZE));
        assert_eq!(check("signature:w", huge), Err(SettingProblem::TooLarge));
    }

    #[test]
    fn account_and_folder_keys_check_entry_and_value() {
        for key in ["unified.hidden", "notify.muted"] {
            assert_eq!(check(&format!("{key}:mini@example.org"), json!(true)), Ok(()), "{key}");
            assert!(check(&format!("{key}:mini@example.org"), json!(false)).is_err(), "{key}");
            for bad in ["Mini@example.org", "@example.org", "mini", ""] {
                assert!(check(&format!("{key}:{bad}"), json!(true)).is_err(), "{key}:{bad}");
            }
        }
        assert_eq!(check("compose.attachmentReminder", json!(false)), Ok(()));
        assert!(check("compose.attachmentReminder", json!("off")).is_err());

        let folder = json!({ "account": "mini@example.org", "folder": "Archiv/2026" });
        for key in ["unified.folders", "folders.favorite", "folders.hidden"] {
            assert_eq!(check(&format!("{key}:0123abcd0123abcd"), folder.clone()), Ok(()), "{key}");
            assert!(check(&format!("{key}:no/slash"), folder.clone()).is_err(), "{key}");
            assert!(check(&format!("{key}:"), folder.clone()).is_err(), "{key}");
            assert!(check(&format!("{key}:x"), json!(true)).is_err(), "{key}");
        }
        assert_eq!(
            check("folders.hidden:x", json!({ "account": "mini@example.org", "folder": "f".repeat(1024) })),
            Ok(())
        );
        for bad in [
            json!({ "account": "Mini@example.org", "folder": "INBOX" }),
            json!({ "account": "mini@example.org", "folder": "" }),
            json!({ "account": "mini@example.org", "folder": "f".repeat(1025) }),
            json!({ "account": "mini@example.org", "folder": "a\nb" }),
            json!({ "account": "mini@example.org", "folder": 5 }),
            json!({ "account": "mini@example.org" }),
            json!({ "account": "mini@example.org", "folder": "INBOX", "extra": 1 }),
        ] {
            assert!(check("folders.favorite:x", bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn account_order_is_a_short_list_of_unique_addresses() {
        assert_eq!(check("accounts.order", json!([])), Ok(()));
        assert_eq!(check("accounts.order", json!(["mini@example.org", "nyu@example.net"])), Ok(()));
        let most: Vec<String> = (0..200).map(|n| format!("a{n}@example.org")).collect();
        assert_eq!(check("accounts.order", json!(most)), Ok(()));
        let too_many: Vec<String> = (0..201).map(|n| format!("a{n}@example.org")).collect();
        assert!(check("accounts.order", json!(too_many)).is_err());
        for bad in [
            json!(["mini@example.org", "mini@example.org"]),
            json!(["Mini@example.org"]),
            json!(["mini"]),
            json!([1]),
            json!("mini@example.org"),
        ] {
            assert!(check("accounts.order", bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn quiet_hours_take_times_and_days() {
        let good = json!({ "enabled": true, "from": "22:00", "to": "07:30", "days": [0, 6, 3] });
        assert_eq!(check("notify.quietHours", good.clone()), Ok(()));
        assert_eq!(
            check("notify.quietHours", json!({ "enabled": false, "from": "00:00", "to": "23:59", "days": [] })),
            Ok(())
        );
        for (property, value) in [
            ("enabled", json!("yes")),
            ("from", json!("24:00")),
            ("from", json!("7:00")),
            ("to", json!("07:60")),
            ("to", json!("07-00")),
            ("days", json!([1, 1])),
            ("days", json!([7])),
            ("days", json!([-1])),
            ("days", json!(["1"])),
            ("days", json!(1)),
            ("extra", json!(true)),
        ] {
            let mut bad = good.clone();
            bad[property] = value;
            assert!(check("notify.quietHours", bad).is_err(), "{property}");
        }
        let mut missing = good;
        missing.as_object_mut().unwrap().remove("days");
        assert!(check("notify.quietHours", missing).is_err());
    }

    #[test]
    fn snoozes_say_where_and_until_when() {
        let good = json!({ "account": "mini@example.org", "messageId": "abc.123@mail.example.org", "folder": "INBOX", "until": "2026-10-21T07:00:00Z" });
        assert_eq!(check("snooze:a1B2c3D4e5F6g7H8", good.clone()), Ok(()));
        let mut fraction = good.clone();
        fraction["until"] = json!("2026-10-21T07:00:00.000Z");
        assert_eq!(check("snooze:s", fraction), Ok(()));
        assert!(check("snooze:", good.clone()).is_err());
        assert!(check("snooze:a.b", good.clone()).is_err());
        for (property, value) in [
            ("account", json!("Mini@example.org")),
            ("messageId", json!("<abc@mail.example.org>")),
            ("messageId", json!("")),
            ("messageId", json!("a b@example.org")),
            ("messageId", json!("m".repeat(999))),
            ("folder", json!("")),
            ("until", json!("2026-10-21T07:00:00+02:00")),
            ("until", json!("2026-10-21 07:00:00Z")),
            ("until", json!("2026-02-30T07:00:00Z")),
            ("until", json!("2026-10-21T07:00Z")),
            ("until", json!("2026-10-21T07:00:00.Z")),
            ("until", json!(1_792_998_000)),
            ("extra", json!(1)),
        ] {
            let mut bad = good.clone();
            bad[property] = value;
            assert!(check("snooze:s", bad.clone()).is_err(), "{property}: {bad}");
        }
    }

    #[test]
    fn templates_have_a_name_a_subject_and_html() {
        let good = json!({ "name": "Danke", "subject": "", "html": "<p>Hallo {firstName}</p>" });
        assert_eq!(check("template:t1", good.clone()), Ok(()));
        assert!(check("template:", good.clone()).is_err());
        for (property, value) in [
            ("name", json!("")),
            ("name", json!("n".repeat(101))),
            ("name", json!("a\tb")),
            ("subject", json!("s".repeat(999))),
            ("subject", json!("a\r\nBcc: x@example.org")),
            ("html", json!(null)),
            ("extra", json!(1)),
        ] {
            let mut bad = good.clone();
            bad[property] = value;
            assert!(check("template:t1", bad).is_err(), "{property}");
        }
        let mut huge = good;
        huge["html"] = json!("x".repeat(USER_SETTINGS_MAX_VALUE_SIZE));
        assert_eq!(check("template:t1", huge), Err(SettingProblem::TooLarge));
    }

    async fn account(store: &Store) -> i64 {
        store.create_domain("example.org").await.unwrap();
        store
            .create_account(NewAccount {
                address: "nyu@example.org".into(),
                display_name: "Nyu".into(),
                password: Some("katzenpfote-123".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn patches_share_the_portal_preferences_and_move_the_state() {
        let (store, _dir) = store().await;
        let nyu = account(&store).await;
        let empty = store.user_settings(nyu).await.unwrap();
        assert_eq!((empty.state.as_str(), empty.values.len()), ("0", 0));

        let patch = SettingsChange::Patch(vec![
            ("conversations".into(), Some(json!(false))),
            ("theme".into(), Some(json!("dark"))),
            ("linkDomains:example.net".into(), Some(json!(true))),
        ]);
        let written = store.update_user_settings(nyu, patch, None).await.unwrap();
        assert_ne!(written.state, empty.state);
        let preferences = store.preferences(nyu).await.unwrap();
        assert_eq!(preferences["mailConversations"], "off");
        assert_eq!(preferences["theme"], "dark");
        assert_eq!(store.user_settings(nyu).await.unwrap(), written);

        // The portal writes the same place, and the state moves with it.
        let changes = json!({ "mailConversations": "on", "mode": "pro" }).as_object().unwrap().clone();
        store.update_preferences(nyu, changes).await.unwrap();
        let after = store.user_settings(nyu).await.unwrap();
        assert_eq!(after.values["conversations"], json!(true));
        assert_ne!(after.state, written.state);
        assert!(!after.values.contains_key("mode"));

        // A portal preference that is not synced leaves the state alone.
        let changes = json!({ "motion": "off" }).as_object().unwrap().clone();
        store.update_preferences(nyu, changes).await.unwrap();
        assert_eq!(store.user_settings_state(nyu).await.unwrap(), after.state);

        let stale = store
            .update_user_settings(
                nyu,
                SettingsChange::Patch(vec![("tone".into(), Some(json!("neutral")))]),
                Some(written.state),
            )
            .await;
        assert!(matches!(stale, Err(StoreError::Rule { code: "stateMismatch", .. })));

        let replaced = store
            .update_user_settings(
                nyu,
                SettingsChange::Replace(json!({ "tone": "neutral" }).as_object().unwrap().clone()),
                Some(after.state),
            )
            .await
            .unwrap();
        assert_eq!(Value::Object(replaced.values), json!({ "tone": "neutral" }));
        let preferences = store.preferences(nyu).await.unwrap();
        assert!(!preferences.contains_key("theme") && !preferences.contains_key("mailConversations"));
        assert_eq!(preferences["mode"], "pro", "preferences that are not synced stay");
    }

    #[tokio::test]
    async fn limits_hold_and_a_refused_write_changes_nothing() {
        let (store, _dir) = store().await;
        let nyu = account(&store).await;
        let many: Vec<_> =
            (0..=USER_SETTINGS_MAX_KEYS).map(|n| (format!("linkDomains:host{n}.example"), Some(json!(true)))).collect();
        let refused = store.update_user_settings(nyu, SettingsChange::Patch(many), None).await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "overQuota", .. })));

        // Removals cost a statement each too, so they count against the same limit.
        let removals: Vec<_> =
            (0..=USER_SETTINGS_MAX_KEYS).map(|n| (format!("linkDomains:host{n}.example"), None)).collect();
        let refused = store.update_user_settings(nyu, SettingsChange::Patch(removals), None).await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "overQuota", .. })));
        let nulls: Map<String, Value> =
            (0..=USER_SETTINGS_MAX_KEYS).map(|n| (format!("junk{n}"), Value::Null)).collect();
        let refused = store.update_user_settings(nyu, SettingsChange::Replace(nulls), None).await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "overQuota", .. })));

        let big = json!({ "email": "", "name": "", "html": "x".repeat(200_000), "forNew": true, "forReplies": true });
        let signatures: Vec<_> = (0..6).map(|n| (format!("signature:s{n}"), Some(big.clone()))).collect();
        let refused = store.update_user_settings(nyu, SettingsChange::Patch(signatures), None).await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "overQuota", .. })));

        let mixed = vec![("theme".into(), Some(json!("dark"))), ("nope".into(), Some(json!(true)))];
        assert!(matches!(
            store.update_user_settings(nyu, SettingsChange::Patch(mixed), None).await,
            Err(StoreError::Invalid(_))
        ));
        let untouched = store.user_settings(nyu).await.unwrap();
        assert_eq!((untouched.state.as_str(), untouched.values.len()), ("0", 0));
    }
}
