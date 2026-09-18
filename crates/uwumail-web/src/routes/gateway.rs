//! Server → Setup: the UwUMail Gateway and where this server stands on the internet.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use uwumail_smtp::reachability::Reachability;

use super::audit;
use super::security::confirm_identity;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::gateway::GatewayView;
use crate::session::Admin;

pub async fn show(State(web): State<Web>, _admin: Admin) -> Json<GatewayView> {
    Json(view(&web).await)
}

async fn view(web: &Web) -> GatewayView {
    let mut view = web.gateway().map(|gateway| gateway.view()).unwrap_or_default();
    // Which gateway there is to install. It comes from the release list rather than from the
    // gateway itself: the gateway has no idea what is newer than it is, and the server asks
    // GitHub once a day anyway.
    view.software_version = newer_gateway(web, view.software.as_deref()).await;
    view
}

/// The version to offer, or `None` when the gateway already runs the newest one.
///
/// The gateway calls itself `uwumail-gateway 0.2.2`; what comes after the space is its version.
async fn newer_gateway(web: &Web, software: Option<&str>) -> Option<String> {
    let newest = web.update_info().await.newest_release?;
    let running = software?.rsplit(' ').next()?.trim().to_owned();
    (running != newest).then_some(newest)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    /// `os-update`, `reboot` or `gateway-update`.
    verb: String,
    #[serde(default)]
    password: Option<String>,
}

/// Asks the VPS the gateway runs on for something. Each of these takes the gateway off the network
/// for a while, and with it every way into this server from outside -- so each needs the password
/// again, the same as pairing.
pub async fn ask(State(web): State<Web>, Admin(session): Admin, Json(ask): Json<Ask>) -> ApiResult<Json<GatewayView>> {
    let gateway = web.gateway().ok_or_else(|| ApiError::NotFound("the gateway".into()))?.clone();
    if !matches!(ask.verb.as_str(), "os-update" | "reboot" | "gateway-update") {
        return Err(ApiError::Invalid(format!("unknown job: {}", ask.verb)));
    }
    confirm_identity(&web, &session, ask.password.as_deref()).await?;
    // The version is never taken from the request: it is the newest release the server itself
    // found, or nothing. Whoever asks picks the button, not what gets installed.
    let version = match ask.verb.as_str() {
        "gateway-update" => Some(
            newer_gateway(&web, gateway.view().software.as_deref())
                .await
                .ok_or_else(|| ApiError::Rule("gatewayJobRefused", "there is no newer gateway to install".into()))?,
        ),
        _ => None,
    };
    let id = gateway
        .ask(&ask.verb, version.as_deref())
        .await
        .map_err(|message| ApiError::Rule("gatewayJobRefused", message))?;
    audit(&web, &session, "gateway.job", &ask.verb, json!({ "id": id, "version": version })).await;
    Ok(Json(view(&web).await))
}

#[derive(Deserialize)]
pub struct Pairing {
    code: String,
    #[serde(default)]
    password: Option<String>,
}

/// All mail and web traffic will flow through the gateway, so this needs the password again.
pub async fn pair(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(pairing): Json<Pairing>,
) -> ApiResult<Json<GatewayView>> {
    let gateway = web.gateway().ok_or_else(|| ApiError::NotFound("the gateway".into()))?.clone();
    confirm_identity(&web, &session, pairing.password.as_deref()).await?;
    gateway.pair(pairing.code.trim()).await.map_err(|message| ApiError::Rule("gatewayCodeInvalid", message))?;
    let view = gateway.view();
    audit(&web, &session, "gateway.pair", "", json!({ "fingerprint": view.fingerprint, "tunnel": view.tunnel })).await;
    Ok(Json(view))
}

#[derive(Deserialize, Default)]
pub struct Confirmation {
    #[serde(default)]
    password: Option<String>,
}

pub async fn forget(
    State(web): State<Web>,
    Admin(session): Admin,
    body: Option<Json<Confirmation>>,
) -> ApiResult<StatusCode> {
    let gateway = web.gateway().ok_or_else(|| ApiError::NotFound("the gateway".into()))?.clone();
    let confirmation = body.map(|Json(body)| body).unwrap_or_default();
    confirm_identity(&web, &session, confirmation.password.as_deref()).await?;
    let fingerprint = gateway.view().fingerprint;
    gateway.forget().await.map_err(|message| ApiError::Rule("gatewayForgetFailed", message))?;
    audit(&web, &session, "gateway.forget", "", json!({ "fingerprint": fingerprint })).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reachability(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Reachability>> {
    let Some(dns) = web.dns() else {
        return Err(ApiError::Rule("dnsUnavailable", "the server has no working DNS resolver".into()));
    };
    Ok(Json(web.smtp().check_reachability(dns).await))
}
