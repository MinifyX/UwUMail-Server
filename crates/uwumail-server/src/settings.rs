//! Settings from the admin panel: merged with the config file and applied while the server runs.

use std::path::PathBuf;

use serde_json::{Value, json};
use uwumail_smtp::Smtp;
use uwumail_web::settings::{SETTINGS, SettingKind, SettingSource, SettingValue, SettingsBackend, get_path};

use crate::config::Config;

pub struct ServerSettings {
    pub path: Option<PathBuf>,
    pub smtp: Smtp,
}

impl SettingsBackend for ServerSettings {
    fn view(&self, overlay: &Value) -> Result<Vec<SettingValue>, String> {
        let config = Config::load_with_overlay(self.path.as_deref(), overlay).map_err(|err| format!("{err:#}"))?;
        let fixed = Config::file_and_environment(self.path.as_deref()).map_err(|err| format!("{err:#}"))?;
        let effective =
            json!({ "smtp": config.smtp, "spam": config.spam, "delivery": config.delivery, "tone": config.tone });
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

    fn apply(&self, overlay: &Value) -> Result<(), String> {
        let config = Config::load_with_overlay(self.path.as_deref(), overlay).map_err(|err| format!("{err:#}"))?;
        config.validate().map_err(|err| format!("{err:#}"))?;
        self.smtp
            .update_settings(config.smtp, config.spam, config.delivery, config.tone)
            .map_err(|err| err.to_string())?;
        tracing::info!("settings from the admin panel are in effect");
        Ok(())
    }

    fn config_file(&self) -> Option<String> {
        self.path.as_ref().map(|path| path.display().to_string())
    }
}
