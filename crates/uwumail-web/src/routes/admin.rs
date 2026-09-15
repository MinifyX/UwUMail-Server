//! The "Server" area for admins: overview, health and the change log.

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Web;
use crate::error::ApiResult;
use crate::session::Admin;

pub async fn overview(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let counts = web.store().server_counts().await?;
    Ok(Json(json!({
        "counts": counts,
        "server": {
            "hostname": web.settings().hostname,
            "version": env!("CARGO_PKG_VERSION"),
            "uptimeSeconds": web.settings().started.elapsed().as_secs(),
        },
    })))
}

pub async fn health(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<crate::health::Health>> {
    Ok(Json(crate::health::health(&web, &session.account.login).await?))
}

/// Checks DNS and whether mail can leave right now, then answers like [`health`].
pub async fn check_health(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<crate::health::Health>> {
    web.check_health_now().await;
    Ok(Json(crate::health::health(&web, &session.account.login).await?))
}

#[derive(Deserialize)]
pub struct AuditPage {
    before: Option<i64>,
    limit: Option<usize>,
}

pub async fn audit(State(web): State<Web>, _admin: Admin, Query(page): Query<AuditPage>) -> ApiResult<Json<Value>> {
    let records = web.store().audit_log(page.limit.unwrap_or(100).clamp(1, 200), page.before).await?;
    Ok(Json(json!(records)))
}
