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
    Json(web.gateway().map(|gateway| gateway.view()).unwrap_or_default())
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
