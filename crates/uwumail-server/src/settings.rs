//! Settings from the admin panel: merged with the config file and applied while the server runs.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use uwumail_smtp::Smtp;
use uwumail_web::Loki;
use uwumail_web::loki::LokiTarget;
use uwumail_web::settings::{SETTINGS, SettingKind, SettingSource, SettingValue, SettingsBackend, get_path};

use crate::config::Config;

pub struct ServerSettings {
    pub path: Option<PathBuf>,
    pub smtp: Smtp,
    /// Sends the log to Loki; switched on, over and off from here.
    pub loki: Arc<Loki>,
    /// The web portal's copy of `http.webmail`, so a change in the admin panel is in effect
    /// before the answer is written.
    pub webmail: Arc<AtomicBool>,
    /// The way out for pictures, update checks and fetching; changed in place.
    pub egress: uwumail_smtp::egress::Egress,
}

/// The effective value and origin of every setting, for an overlay. Used by the admin panel and
/// by the command line, which has no running server behind it.
pub fn view_settings(path: Option<&std::path::Path>, overlay: &Value) -> Result<Vec<SettingValue>, String> {
    let config = Config::load_with_overlay(path, overlay).map_err(|err| format!("{err:#}"))?;
    let fixed = Config::file_and_environment(path).map_err(|err| format!("{err:#}"))?;
    let effective = json!({
        "smtp": config.smtp,
        "spam": config.spam,
        "delivery": config.delivery,
        "tone": config.tone,
        "http": { "webmail": config.http.webmail },
        "log": { "loki": config.log.loki },
        "egress": config.egress,
    });
    Ok(SETTINGS
        .iter()
        .map(|spec| {
            let value = get_path(&effective, spec.key).cloned().unwrap_or(Value::Null);
            let source = if fixed.contains(spec.key) {
                SettingSource::File
            } else if get_path(overlay, spec.key).is_some() {
                SettingSource::Database
            } else {
                SettingSource::Default
            };
            let set = !value.is_null() && value != json!("");
            let value = if matches!(spec.kind, SettingKind::Secret) { Value::Null } else { value };
            SettingValue { key: spec.key, value, set, source }
        })
        .collect())
}

impl SettingsBackend for ServerSettings {
    fn view(&self, overlay: &Value) -> Result<Vec<SettingValue>, String> {
        view_settings(self.path.as_deref(), overlay)
    }

    fn apply(&self, overlay: &Value) -> Result<(), String> {
        let config = Config::load_with_overlay(self.path.as_deref(), overlay).map_err(|err| format!("{err:#}"))?;
        config.validate().map_err(|err| format!("{err:#}"))?;
        self.webmail.store(config.http.webmail, Ordering::Relaxed);
        self.smtp
            .update_settings(config.smtp, config.spam, config.delivery, config.tone)
            .map_err(|err| err.to_string())?;
        self.loki.set_target(config.log.loki.target(&config.hostname)?);
        self.egress.reconfigure(&config.egress)?;
        tracing::info!("settings from the admin panel are in effect");
        Ok(())
    }

    fn config_file(&self) -> Option<String> {
        self.path.as_ref().map(|path| path.display().to_string())
    }

    fn loki_connection(&self, overlay: &Value) -> Result<LokiTarget, String> {
        let config = Config::load_with_overlay(self.path.as_deref(), overlay).map_err(|err| format!("{err:#}"))?;
        config.log.loki.connection(&config.hostname)
    }
}
