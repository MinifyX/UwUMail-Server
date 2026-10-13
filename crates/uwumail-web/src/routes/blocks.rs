//! The addresses the server keeps out after failed logins, for admins: who is blocked and why,
//! letting someone back in, and the addresses that are never blocked.

use std::net::IpAddr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;

/// How many blocks the admin is shown at once, the newest.
const BLOCKS_SHOWN: usize = 500;

pub async fn list(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let mut blocks = web.limiter().blocked();
    blocks.sort_by_key(|block| std::cmp::Reverse(block.since));
    let total = blocks.len();
    blocks.truncate(BLOCKS_SHOWN);
    Ok(Json(json!({
        "blocks": blocks.iter().map(|block| json!({
            "ip": block.ip,
            "reason": block.reason,
            "login": block.login,
            "protocol": block.protocol,
            "since": block.since,
            "until": block.until,
            "times": block.times,
        })).collect::<Vec<_>>(),
        "total": total,
        "exempt": web.store().block_exempt(),
    })))
}

#[derive(Deserialize)]
pub struct UnblockRequest {
    ip: String,
}

/// Lets an address back in, here and at the gateway.
pub async fn unblock(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<UnblockRequest>,
) -> ApiResult<StatusCode> {
    let ip: IpAddr = request.ip.trim().parse().map_err(|_| ApiError::Invalid("this is not an address".into()))?;
    if web.store().unblock(ip).await? {
        tracing::info!(%ip, admin = %session.account.login, "unblocked an address");
        audit(&web, &session, "blocks.unblock", &ip.to_string(), json!({})).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ExemptRequest {
    networks: Vec<String>,
}

/// Replaces the addresses and networks that are never blocked.
pub async fn set_exempt(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<ExemptRequest>,
) -> ApiResult<Json<Value>> {
    let exempt = web.store().set_block_exempt(&request.networks).await?;
    audit(&web, &session, "blocks.exempt", "", json!({ "networks": exempt })).await;
    Ok(Json(json!({ "exempt": exempt })))
}
