//! Admin alerts in the portal: what is open, what was fine again lately, and acknowledging.

use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use crate::Web;
use crate::error::ApiResult;
use crate::session::Admin;

/// How many resolved alerts the history shows.
const HISTORY: usize = 50;

pub async fn list(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let alerts = web.store().alerts(HISTORY).await?;
    let (open, resolved): (Vec<_>, Vec<_>) = alerts.into_iter().partition(|alert| alert.resolved_at.is_none());
    Ok(Json(json!({ "open": open, "resolved": resolved })))
}

/// Someone knows: no more reminders for this one.
pub async fn acknowledge(State(web): State<Web>, Admin(session): Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let alert = web.store().acknowledge_alert(id, &session.account.login).await?;
    let details = json!({ "kind": alert.kind, "code": alert.code });
    crate::routes::audit(&web, &session, "alert.acknowledge", &alert.key, details).await;
    Ok(Json(json!(alert)))
}
