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

/// Puts `changes` into `overlay` the way the admin panel may: known settings, valid values, none the
/// config file holds. Returns what goes into the change log, passwords hidden.
fn merge_changes(
    backend: &dyn SettingsBackend,
    overlay: &mut Value,
    changes: &Map<String, Value>,
) -> ApiResult<Map<String, Value>> {
    let current = backend.view(overlay).map_err(|_| ApiError::Internal)?;
    let mut details = Map::new();
    for (key, value) in changes {
        let Some(spec) = spec_for(key) else {
            return Err(ApiError::Invalid(format!("unknown setting {key}")));
        };
        check_value(spec, value).map_err(ApiError::Invalid)?;
        if current.iter().any(|setting| setting.key == spec.key && setting.source == SettingSource::File) {
            return Err(ApiError::Rule("settingLocked", format!("{key} is set in the config file")));
        }
        set_path(overlay, spec.key, value.clone());
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
    tidy(overlay, host_from_file);
    Ok(details)
}

pub async fn update(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<Changes>,
) -> ApiResult<Json<Value>> {
    let backend = backend(&web)?;
    let mut overlay = load_overlay(&web).await?;
    let details = merge_changes(backend, &mut overlay, &request.changes)?;

    backend.apply(&overlay).map_err(|err| ApiError::Rule("settingsInvalid", err))?;
    web.store().set_setting(OVERLAY_KEY, &overlay.to_string()).await?;
    audit(&web, &session, "settings.update", "", Value::Object(details)).await;
    Ok(Json(view_json(&web, backend, &overlay)?))
}

/// Where a setting's value comes from right now, `None` on a server without settings.
pub(crate) async fn setting_source(web: &Web, key: &str) -> ApiResult<Option<SettingSource>> {
    let Some(backend) = web.settings().config.as_deref() else { return Ok(None) };
    let overlay = load_overlay(web).await?;
    let values = backend.view(&overlay).map_err(|_| ApiError::Internal)?;
    Ok(values.iter().find(|setting| setting.key == key).map(|setting| setting.source))
}

/// Changes settings on behalf of another page of the portal (the VPN page sets the proxy). Settings the
/// config file holds are left as they are; returns whether everything could be changed.
pub(crate) async fn change_settings(
    web: &Web,
    session: &crate::session::Session,
    changes: Map<String, Value>,
) -> ApiResult<bool> {
    let Some(backend) = web.settings().config.as_deref() else { return Ok(false) };
    let mut overlay = load_overlay(web).await?;
    let current = backend.view(&overlay).map_err(|_| ApiError::Internal)?;
    let locked = |key: &str| current.iter().any(|setting| setting.key == key && setting.source == SettingSource::File);
    let all = changes.keys().all(|key| !locked(key));
    let changes: Map<String, Value> = changes.into_iter().filter(|(key, _)| !locked(key)).collect();
    if changes.is_empty() {
        return Ok(all);
    }
    let details = merge_changes(backend, &mut overlay, &changes)?;
    backend.apply(&overlay).map_err(|err| ApiError::Rule("settingsInvalid", err))?;
    web.store().set_setting(OVERLAY_KEY, &overlay.to_string()).await?;
    audit(web, session, "settings.update", "", Value::Object(details)).await;
    Ok(all)
}

/// How sending the log to Loki goes.
pub async fn loki_status(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let loki = web.settings().loki.as_ref().ok_or_else(|| ApiError::NotFound("sending logs to Loki".into()))?;
    Ok(Json(serde_json::to_value(loki.status()).map_err(|_| ApiError::Internal)?))
}

/// Sends one test line with the saved settings and the changes not saved yet, so an address and its
/// credentials can be tried before anything is switched on. The line itself says nothing about anyone.
pub async fn loki_test(State(web): State<Web>, _admin: Admin, Json(request): Json<Changes>) -> ApiResult<Json<Value>> {
    let loki = web.settings().loki.clone().ok_or_else(|| ApiError::NotFound("sending logs to Loki".into()))?;
    let backend = backend(&web)?;
    let mut overlay = load_overlay(&web).await?;
    merge_changes(backend, &mut overlay, &request.changes)?;
    let target = backend.loki_connection(&overlay).map_err(|err| ApiError::Rule("lokiInvalid", err))?;
    loki.test(&target).await.map_err(|err| ApiError::Rule("lokiUnreachable", err))?;
    Ok(Json(json!({ "ok": true })))
}
