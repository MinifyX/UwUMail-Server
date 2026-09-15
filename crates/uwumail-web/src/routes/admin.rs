//! The "Server" area for admins.

use axum::Json;
use axum::extract::State;
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
