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
const SIGNATURE_ID_MAX_CHARS: usize = 64;
const SIGNATURE_NAME_MAX_CHARS: usize = 100;
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
        "conversations" | "senderPictures" | "linkConfirm" | "darkImages" => return boolean(key, value),
        "remoteImages" => return one_of(key, value, &["ask", "always"]),
        "mailAppearance" => return one_of(key, value, &["auto", "light", "dark"]),
        "undoSendSeconds" => {
            return match value.as_u64() {
                Some(0 | 5 | 10 | 20 | 30) => Ok(()),
                _ => Err(invalid("undoSendSeconds must be one of 0, 5, 10, 20, 30")),
            };
        }
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
            if !is_list_entry_length(rest) || !is_address(rest) {
                return Err(invalid(format!("{key}: the entry must be a lower-case address")));
            }
            one_of(key, value, &["light", "dark"])
        }
        "linkDomains" => {
            if !is_ascii_host(rest) {
                return Err(invalid(format!("{key}: the entry must be a lower-case ASCII host name")));
            }
            only_true(key, value)
        }
        "signature" => {
            let valid_id = (1..=SIGNATURE_ID_MAX_CHARS).contains(&rest.len())
                && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            if !valid_id {
                return Err(invalid(format!("{key}: the id must be 1 to 64 of A-Z, a-z, 0-9, - and _")));
            }
            signature(key, value)
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
