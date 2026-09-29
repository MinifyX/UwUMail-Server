//! Server settings the admin panel can change.
//!
//! Changes are stored in the database as an overlay. The config file and
//! `UWUMAIL_*` environment variables always win: a setting they set is shown,
//! but locked. The server applies the result right away.

use serde::Serialize;
use serde_json::{Map, Value, json};
use uwumail_store::{Store, StoreError};

/// The settings key the database overlay is stored under.
pub const OVERLAY_KEY: &str = "config.overlay";

/// How a secret looks in the stored overlay: sealed with the store's key, as hex behind this.
const SEALED: &str = "sealed:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum SettingKind {
    Bool,
    Integer {
        min: i64,
        max: i64,
    },
    /// A number that may have decimals, like a spam score, between whole-number limits.
    Decimal {
        min: i64,
        max: i64,
    },
    Text,
    /// Never sent back to the browser; only whether it is set.
    Secret,
    Choice {
        options: &'static [&'static str],
    },
    /// A list of strings, e.g. networks.
    List,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SettingSpec {
    pub key: &'static str,
    #[serde(flatten)]
    pub kind: SettingKind,
}

const fn spec(key: &'static str, kind: SettingKind) -> SettingSpec {
    SettingSpec { key, kind }
}

/// Everything the admin panel may change. Keys follow the config file.
pub const SETTINGS: &[SettingSpec] = &[
    spec("tone.language", SettingKind::Choice { options: &["de", "en", "fr", "nl", "ja", "zh"] }),
    spec("tone.internal", SettingKind::Choice { options: &["playful", "neutral"] }),
    spec("tone.external", SettingKind::Choice { options: &["neutral", "light"] }),
    // What the server is called and how it looks; the logo is kept apart (routes/branding.rs).
    spec("brand.name", SettingKind::Text),
    spec("brand.color", SettingKind::Text),
    spec("brand.mascot", SettingKind::Bool),
    spec("delivery.relay.host", SettingKind::Text),
    spec("delivery.relay.port", SettingKind::Integer { min: 1, max: 65_535 }),
    spec("delivery.relay.security", SettingKind::Choice { options: &["starttls", "tls", "none"] }),
    spec("delivery.relay.username", SettingKind::Text),
    spec("delivery.relay.password", SettingKind::Secret),
    spec("delivery.require_tls", SettingKind::Bool),
    spec("delivery.max_lifetime_hours", SettingKind::Integer { min: 1, max: 720 }),
    spec("smtp.max_message_size", SettingKind::Integer { min: 1_048_576, max: 1_073_741_824 }),
    spec("smtp.max_recipients", SettingKind::Integer { min: 1, max: 10_000 }),
    spec("smtp.verify_senders", SettingKind::Bool),
    spec("smtp.enforce_dmarc_reject", SettingKind::Bool),
    spec("smtp.require_tls_for_auth", SettingKind::Bool),
    spec("smtp.reveal_client_ip", SettingKind::Bool),
    spec("smtp.trusted_relays", SettingKind::List),
    spec("http.webmail", SettingKind::Bool),
    spec("smtp.allow_external_forwarding", SettingKind::Bool),
    spec("spam.enabled", SettingKind::Bool),
    spec("spam.blocklists", SettingKind::Bool),
    spec("spam.uri_blocklists", SettingKind::Bool),
    spec("spam.bayes", SettingKind::Bool),
    spec("spam.junk_score", SettingKind::Decimal { min: 1, max: 100 }),
    spec("spam.greylist_score", SettingKind::Decimal { min: 1, max: 100 }),
    spec("spam.greylist_delay_secs", SettingKind::Integer { min: 60, max: 3600 }),
    spec("spam.greylist_hold", SettingKind::Bool),
    spec("spam.reject_score", SettingKind::Decimal { min: 1, max: 100 }),
    spec("spam.traps", SettingKind::List),
    spec("spam.feeds.urlhaus", SettingKind::Bool),
    spec("spam.feeds.malware_bazaar", SettingKind::Bool),
    spec("spam.feeds.bad_subjects", SettingKind::Bool),
    spec("spam.feeds.disposable", SettingKind::Bool),
    spec("spam.feeds.freemail", SettingKind::Bool),
    spec("spam.feeds.redirectors", SettingKind::Bool),
    spec("spam.feeds.abuse_ch_key", SettingKind::Secret),
    spec("spam.antivirus.enabled", SettingKind::Bool),
    spec("spam.antivirus.address", SettingKind::Text),
    spec("spam.antivirus.timeout_secs", SettingKind::Integer { min: 5, max: 300 }),
    spec("spam.antivirus.max_size", SettingKind::Integer { min: 1_048_576, max: 104_857_600 }),
    spec("spam.log.enabled", SettingKind::Bool),
    spec("spam.log.clean_subjects", SettingKind::Bool),
    spec("spam.log.retention_days", SettingKind::Integer { min: 1, max: 365 }),
    spec("log.loki.enabled", SettingKind::Bool),
    spec("log.loki.privacy_consent", SettingKind::Bool),
    spec("log.loki.url", SettingKind::Text),
    spec("log.loki.username", SettingKind::Text),
    spec("log.loki.password", SettingKind::Secret),
    spec("log.loki.token", SettingKind::Secret),
    spec("log.loki.tenant", SettingKind::Text),
    spec("log.loki.labels", SettingKind::List),
    spec("log.loki.level", SettingKind::Choice { options: &["error", "warn", "info", "debug"] }),
    spec("log.loki.gateway", SettingKind::Bool),
    // The way out for requests that tell about readers: a proxy (often a VPN's) and which requests take it.
    spec("egress.proxy", SettingKind::Secret),
    spec("egress.fallback", SettingKind::Choice { options: &["block", "direct"] }),
    spec("egress.pictures", SettingKind::Bool),
    spec("egress.updates", SettingKind::Bool),
    spec("egress.fetch", SettingKind::Bool),
    // The shared cache of remote pictures in messages, in megabytes; 0 keeps none (docs/jmap-remote.md).
    spec("egress.image_cache_mb", SettingKind::Integer { min: 0, max: 102_400 }),
    // Daily TLS reports (RFC 8460) to the domains mail went to, when they ask for them.
    spec("reports.send_tls_reports", SettingKind::Bool),
    // Prometheus metrics under /metrics (docs/metrics.md).
    spec("metrics.enabled", SettingKind::Bool),
    spec("metrics.token", SettingKind::Secret),
    spec("metrics.allowed_networks", SettingKind::List),
    // Logging in to the portal elsewhere (docs/login-oidc-ldap.md).
    spec("auth.oidc.enabled", SettingKind::Bool),
    spec("auth.oidc.issuer", SettingKind::Text),
    spec("auth.oidc.client_id", SettingKind::Text),
    spec("auth.oidc.client_secret", SettingKind::Secret),
    spec("auth.oidc.button_label", SettingKind::Text),
    spec("auth.oidc.auto_create", SettingKind::Bool),
    spec("auth.oidc.allowed_domains", SettingKind::List),
    spec("auth.oidc.admin_group_claim", SettingKind::Text),
    spec("auth.oidc.admin_group_value", SettingKind::Text),
    spec("auth.ldap.enabled", SettingKind::Bool),
    spec("auth.ldap.url", SettingKind::Text),
    spec("auth.ldap.starttls", SettingKind::Bool),
    spec("auth.ldap.insecure_localhost", SettingKind::Bool),
    spec("auth.ldap.bind_dn", SettingKind::Text),
    spec("auth.ldap.bind_password", SettingKind::Secret),
    spec("auth.ldap.user_dn_template", SettingKind::Text),
    spec("auth.ldap.base_dn", SettingKind::Text),
    spec("auth.ldap.user_filter", SettingKind::Text),
    spec("auth.ldap.mail_attribute", SettingKind::Text),
    spec("auth.ldap.name_attribute", SettingKind::Text),
    spec("auth.ldap.admin_group_dn", SettingKind::Text),
    spec("auth.ldap.auto_create", SettingKind::Bool),
    spec("auth.ldap.allowed_domains", SettingKind::List),
    // Signing in at Microsoft and Google for fetched mailboxes (docs/fetch.md).
    spec("fetch.oauth.microsoft_client_id", SettingKind::Text),
    spec("fetch.oauth.google_client_id", SettingKind::Text),
    spec("fetch.oauth.google_client_secret", SettingKind::Secret),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingSource {
    Default,
    /// Changed in the admin panel.
    Database,
    /// Set by the config file or an environment variable; cannot be changed here.
    File,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingValue {
    pub key: &'static str,
    /// `null` for secrets.
    pub value: Value,
    /// For secrets: whether one is set.
    pub set: bool,
    pub source: SettingSource,
}

/// The server's side of settings: what is in effect, and applying a new overlay.
pub trait SettingsBackend: Send + Sync + 'static {
    /// The effective value and origin of every setting in [`SETTINGS`], for this overlay.
    fn view(&self, overlay: &Value) -> Result<Vec<SettingValue>, String>;
    /// Checks the overlay and puts it into effect; an error explains what is wrong.
    fn apply(&self, overlay: &Value) -> Result<(), String>;
    /// Where the config file is, to explain locked settings.
    fn config_file(&self) -> Option<String>;
    /// Where and how logs would go to Loki with this overlay, switched on or not, to send a test line.
    fn loki_connection(&self, overlay: &Value) -> Result<crate::loki::LokiTarget, String> {
        let _ = overlay;
        Err("this server cannot send its logs to Loki".into())
    }
    /// Logging in elsewhere (`auth.*`) with this overlay, to try the settings before saving them.
    fn auth_config(&self, overlay: &Value) -> Result<crate::external::AuthConfig, String> {
        let _ = overlay;
        Err("this server cannot log in elsewhere".into())
    }
}

pub fn spec_for(key: &str) -> Option<&'static SettingSpec> {
    SETTINGS.iter().find(|spec| spec.key == key)
}

/// Checks one change against its spec. `null` means "back to the default".
pub fn check_value(spec: &SettingSpec, value: &Value) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    let ok = match spec.kind {
        SettingKind::Bool => value.is_boolean(),
        SettingKind::Integer { min, max } => value.as_i64().is_some_and(|n| (min..=max).contains(&n)),
        SettingKind::Decimal { min, max } => {
            value.as_f64().is_some_and(|n| n.is_finite() && (min as f64..=max as f64).contains(&n))
        }
        SettingKind::Text | SettingKind::Secret => value.as_str().is_some_and(|s| s.len() <= 1000),
        SettingKind::Choice { options } => value.as_str().is_some_and(|s| options.contains(&s)),
        SettingKind::List => value.as_array().is_some_and(|items| {
            items.len() <= 100 && items.iter().all(|item| item.as_str().is_some_and(|s| s.len() <= 100))
        }),
    };
    if ok { Ok(()) } else { Err(format!("{} has an invalid value", spec.key)) }
}

/// Sets or removes a dotted key in a JSON object, creating the objects on the way.
pub fn set_path(root: &mut Value, key: &str, value: Value) {
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    let mut parts: Vec<&str> = key.split('.').collect();
    let last = parts.pop().expect("keys are not empty");
    let mut current = root;
    for part in parts {
        let object = current.as_object_mut().expect("objects all the way down");
        current = object.entry(part).or_insert_with(|| Value::Object(Map::new()));
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
    }
    let object = current.as_object_mut().expect("objects all the way down");
    if value.is_null() {
        object.remove(last);
    } else {
        object.insert(last.to_owned(), value);
    }
}

pub fn get_path<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(root, |current, part| current.get(part))
}

/// The overlay the admin panel and the command line wrote, with its secrets opened. Secrets written
/// before they were sealed are read as they are and sealed the next time the overlay is saved.
pub async fn load_overlay(store: &Store) -> Result<Value, StoreError> {
    let mut overlay = store
        .setting(OVERLAY_KEY)
        .await?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    for spec in SETTINGS.iter().filter(|spec| matches!(spec.kind, SettingKind::Secret)) {
        let Some(sealed) = get_path(&overlay, spec.key).and_then(Value::as_str).and_then(|v| v.strip_prefix(SEALED))
        else {
            continue;
        };
        match store.unseal_secret(sealed).await {
            Ok(plain) => set_path(&mut overlay, spec.key, Value::String(plain)),
            Err(err) => tracing::warn!(key = spec.key, %err, "a sealed setting could not be opened"),
        }
    }
    Ok(overlay)
}

/// Stores the overlay with every secret in it sealed: a copy of the database alone does not give
/// away a relay's password or a directory's bind password.
pub async fn save_overlay(store: &Store, overlay: &Value) -> Result<(), StoreError> {
    let mut stored = overlay.clone();
    for spec in SETTINGS.iter().filter(|spec| matches!(spec.kind, SettingKind::Secret)) {
        let Some(plain) = get_path(&stored, spec.key).and_then(Value::as_str).filter(|v| !v.is_empty()) else {
            continue;
        };
        let sealed = format!("{SEALED}{}", store.seal_secret(plain).await?);
        set_path(&mut stored, spec.key, Value::String(sealed));
    }
    store.set_setting(OVERLAY_KEY, &stored.to_string()).await
}

/// Removes empty objects, and a relay without a host (it would not be usable), unless
/// the config file provides the host.
pub fn tidy(overlay: &mut Value, host_from_file: bool) {
    if !host_from_file
        && let Some(relay) = overlay.pointer_mut("/delivery/relay")
        && relay.get("host").and_then(Value::as_str).is_none_or(|host| host.trim().is_empty())
        && let Some(delivery) = overlay.pointer_mut("/delivery").and_then(Value::as_object_mut)
    {
        delivery.remove("relay");
    }
    fn prune(value: &mut Value) -> bool {
        if let Value::Object(map) = value {
            map.retain(|_, child| !prune(child));
            map.is_empty()
        } else {
            false
        }
    }
    prune(overlay);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn paths_and_tidying() {
        let mut overlay = json!({});
        set_path(&mut overlay, "delivery.relay.host", json!("relay.example.net"));
        set_path(&mut overlay, "delivery.relay.port", json!(587));
        set_path(&mut overlay, "tone.language", json!("en"));
        assert_eq!(get_path(&overlay, "delivery.relay.port"), Some(&json!(587)));

        set_path(&mut overlay, "delivery.relay.host", Value::Null);
        tidy(&mut overlay, false);
        assert_eq!(overlay, json!({ "tone": { "language": "en" } }), "a relay without host goes");
        set_path(&mut overlay, "tone.language", Value::Null);
        tidy(&mut overlay, false);
        assert_eq!(overlay, json!({}));
    }

    #[test]
    fn values_are_checked() {
        let port = spec_for("delivery.relay.port").unwrap();
        assert!(check_value(port, &json!(587)).is_ok());
        assert!(check_value(port, &json!(0)).is_err());
        assert!(check_value(port, &json!("587")).is_err());
        let security = spec_for("delivery.relay.security").unwrap();
        assert!(check_value(security, &json!("tls")).is_ok());
        assert!(check_value(security, &json!("ssl")).is_err());
        let junk = spec_for("spam.junk_score").unwrap();
        assert!(check_value(junk, &json!(6.5)).is_ok(), "spam scores may have decimals");
        assert!(check_value(junk, &json!(5)).is_ok());
        assert!(check_value(junk, &json!(0.5)).is_err());
        assert!(check_value(junk, &json!(100.5)).is_err());
        assert!(check_value(junk, &json!("6.5")).is_err());
        let relays = spec_for("smtp.trusted_relays").unwrap();
        assert!(check_value(relays, &json!(["192.0.2.1", "2001:db8::/32"])).is_ok());
        assert!(spec_for("hostname").is_none(), "the host name stays in the config file");
    }
}
