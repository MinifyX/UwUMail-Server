//! How a message's remote pictures leave the server, in the admin panel: straight or through a VPN's
//! proxy, what happens while the proxy is away, and how it went since the server started.
//!
//! Only shown here. The proxy is set in the configuration or `.env`, because its address often carries
//! a login.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;

pub async fn show(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let egress = web.egress().ok_or_else(|| ApiError::NotFound("remote pictures are not fetched here".into()))?;
    Ok(Json(json!(egress.status())))
}

/// Asks a public service which address it sees, the same way the pictures go, so the admin can tell the
/// VPN's address from the server's. Only when asked: it is a request to someone else.
pub async fn test(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    let egress = web.egress().ok_or_else(|| ApiError::NotFound("remote pictures are not fetched here".into()))?;
    let outcome = egress.public_address().await;
    let (address, error) = match &outcome {
        Ok(address) => (Some(address.to_string()), None),
        Err(err) => (None, Some(err.to_string())),
    };
    let details = json!({ "proxied": egress.proxied(), "address": address, "error": error });
    audit(&web, &session, "egress.test", "server", details.clone()).await;
    Ok(Json(details))
}
