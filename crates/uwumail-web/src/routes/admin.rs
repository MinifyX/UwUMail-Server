//! The "Server" area for admins: overview and the change log.

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

#[derive(Deserialize)]
pub struct AuditPage {
    before: Option<i64>,
    limit: Option<usize>,
}

pub async fn audit(State(web): State<Web>, _admin: Admin, Query(page): Query<AuditPage>) -> ApiResult<Json<Value>> {
    let records = web.store().audit_log(page.limit.unwrap_or(100).clamp(1, 200), page.before).await?;
    Ok(Json(json!(records)))
}
