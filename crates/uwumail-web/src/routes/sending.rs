//! Outbound abuse for admins (docs/outbound-abuse.md): accounts that may not send to other
//! servers for now, lifting that early, and the complaints that came in for abuse@.

use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use super::audit;
use crate::Web;
use crate::error::ApiResult;
use crate::health::unix_now;
use crate::session::Admin;

/// How many complaints the admin is shown, the newest.
const REPORTS_SHOWN: usize = 500;

pub async fn suspensions(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let now = unix_now();
    Ok(Json(json!({ "suspensions": web.store().send_suspensions(now).await?, "now": now })))
}

/// An admin lets an account send again before its time is up, e.g. after its password was changed.
pub async fn lift(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(account_id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let now = unix_now();
    let lifted = web.store().lift_send_suspension(account_id, now).await?;
    audit(&web, &session, "sending.lift", &lifted.login, json!({ "reason": lifted.reason, "until": lifted.until }))
        .await;
    Ok(Json(json!({ "suspensions": web.store().send_suspensions(now).await?, "now": now })))
}

pub async fn abuse_reports(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "reports": web.store().abuse_reports(REPORTS_SHOWN).await? })))
}
