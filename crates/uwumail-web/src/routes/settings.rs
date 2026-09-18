//! Server settings in the admin panel.

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;
use crate::settings::{SETTINGS, SettingKind, SettingSource, SettingsBackend, check_value, set_path, spec_for, tidy};

/// The settings key the database overlay is stored under.
pub const OVERLAY_KEY: &str = "config.overlay";

fn backend(web: &Web) -> ApiResult<&dyn SettingsBackend> {
    web.settings().config.as_deref().ok_or_else(|| ApiError::NotFound("server settings".into()))
}

pub async fn load_overlay(web: &Web) -> ApiResult<Value> {
    Ok(web
        .store()
        .setting(OVERLAY_KEY)
        .await?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({})))
}

/// Whether a gateway is paired. Outgoing mail leaves through it from that moment, whatever the
/// sending route says, so the page has to be able to say so.
fn through_gateway(web: &Web) -> bool {
    web.gateway().is_some_and(|gateway| gateway.view().state != crate::gateway::GatewayState::None)
}

fn view_json(web: &Web, backend: &dyn SettingsBackend, overlay: &Value) -> ApiResult<Value> {
    let values = backend.view(overlay).map_err(|err| {
        tracing::error!(%err, "reading the effective settings failed");
        ApiError::Internal
    })?;
    Ok(json!({
        "settings": values,
        "specs": SETTINGS,
        "configFile": backend.config_file(),
        "gateway": { "paired": through_gateway(web) },
    }))
}

pub async fn show(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let backend = backend(&web)?;
    let overlay = load_overlay(&web).await?;
    Ok(Json(view_json(&web, backend, &overlay)?))
}

#[derive(Deserialize)]
pub struct Changes {
    changes: Map<String, Value>,
}

pub async fn update(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<Changes>,
) -> ApiResult<Json<Value>> {
    let backend = backend(&web)?;
    let mut overlay = load_overlay(&web).await?;
    let current = backend.view(&overlay).map_err(|_| ApiError::Internal)?;

    let mut details = Map::new();
    for (key, value) in &request.changes {
        let Some(spec) = spec_for(key) else {
            return Err(ApiError::Invalid(format!("unknown setting {key}")));
        };
        check_value(spec, value).map_err(ApiError::Invalid)?;
        if current.iter().any(|setting| setting.key == spec.key && setting.source == SettingSource::File) {
            return Err(ApiError::Rule("settingLocked", format!("{key} is set in the config file")));
        }
        set_path(&mut overlay, spec.key, value.clone());
        // Passwords never go into the change log.
        let logged = if matches!(spec.kind, SettingKind::Secret) && !value.is_null() {
            json!("•••")
        } else {
            value.clone()
        };
        details.insert(key.clone(), logged);
    }
    let host_from_file =
        current.iter().any(|setting| setting.key == "delivery.relay.host" && setting.source == SettingSource::File);
    tidy(&mut overlay, host_from_file);

    backend.apply(&overlay).map_err(|err| ApiError::Rule("settingsInvalid", err))?;
    web.store().set_setting(OVERLAY_KEY, &overlay.to_string()).await?;
    audit(&web, &session, "settings.update", "", Value::Object(details)).await;
    Ok(Json(view_json(&web, backend, &overlay)?))
}
