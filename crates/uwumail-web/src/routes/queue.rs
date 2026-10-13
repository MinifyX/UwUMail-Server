//! The outgoing queue and the server log, for admins.
//!
//! The queue shows who sends to whom, sizes and errors, but never subjects or content.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::logs::level_rank;
use crate::session::Admin;

pub async fn list(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let entries = web.store().queue_entries().await?;
    Ok(Json(Value::Array(
        entries
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.message.id,
                    "from": entry.message.return_path,
                    "size": entry.message.size,
                    "createdAt": entry.message.created_at,
                    "expiresAt": entry.message.expires_at,
                    "recipients": entry.recipients.iter().map(|recipient| json!({
                        "address": recipient.address,
                        "status": recipient.status,
                        "attempts": recipient.attempts,
                        "nextAttemptAt": recipient.next_attempt_at,
                        "lastError": recipient.last_error,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )))
}

pub async fn retry(State(web): State<Web>, Admin(session): Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().retry_queue_message(id).await?;
    audit(&web, &session, "queue.retry", &format!("#{id}"), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn drop(State(web): State<Web>, Admin(session): Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().delete_queue_message(id).await?;
    audit(&web, &session, "queue.drop", &format!("#{id}"), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// How many refused mails the admin is shown at once.
const REFUSED_SHOWN: usize = 200;

/// Outgoing mail other servers did not take: who wrote to whom and what the other side said.
pub async fn refused(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "refused": web.store().refused_deliveries(REFUSED_SHOWN).await? })))
}

pub async fn clear_refused(State(web): State<Web>, Admin(session): Admin) -> ApiResult<StatusCode> {
    let cleared = web.store().clear_refused_deliveries().await?;
    audit(&web, &session, "queue.refused.clear", "", json!({ "cleared": cleared })).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct LogQuery {
    after: Option<u64>,
    level: Option<String>,
    /// Kinds of lines, separated by commas; all of them when missing.
    category: Option<String>,
    search: Option<String>,
    limit: Option<usize>,
}

pub async fn logs(State(web): State<Web>, _admin: Admin, Query(query): Query<LogQuery>) -> ApiResult<Json<Value>> {
    let Some(buffer) = web.settings().logs.as_ref() else {
        return Err(ApiError::NotFound("the server log".into()));
    };
    let max_rank = query.level.as_deref().map(level_rank).unwrap_or(4);
    let categories: Vec<&str> =
        query.category.as_deref().unwrap_or("").split(',').map(str::trim).filter(|kind| !kind.is_empty()).collect();
    let (lines, latest) = buffer.lines(
        query.after.unwrap_or(0),
        max_rank,
        &categories,
        query.search.as_deref(),
        query.limit.unwrap_or(500).min(2000),
    );
    Ok(Json(json!({ "lines": lines, "latest": latest })))
}
