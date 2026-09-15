//! The public page behind invitation and reset links: choose a password, then you are logged in.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_jmap::ClientInfo;
use uwumail_store::{AuditEntry, PasswordLink};

use super::check_password;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::{self, SESSION_LIFETIME_SECS};

async fn valid_link(web: &Web, token: &str) -> ApiResult<PasswordLink> {
    if token.len() > 128 {
        return Err(ApiError::Rule("linkInvalid", "this link is not valid (anymore)".into()));
    }
    web.store()
        .password_link(token)
        .await?
        .ok_or_else(|| ApiError::Rule("linkInvalid", "this link is not valid (anymore)".into()))
}

pub async fn show(State(web): State<Web>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    let link = valid_link(&web, &token).await?;
    Ok(Json(json!({
        "login": link.account.login,
        "name": link.account.display_name,
        "purpose": link.purpose,
        "expiresAt": link.expires_at,
    })))
}

#[derive(Deserialize)]
pub struct Choice {
    password: String,
}

pub async fn choose(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(choice): Json<Choice>,
) -> ApiResult<Response> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let link = valid_link(&web, &token).await?;
    check_password(&choice.password, &link.account.login)?;
    let account = web.store().use_password_link(&token, &choice.password).await?;
    let _ = web
        .store()
        .record_audit(AuditEntry {
            actor_id: Some(account.id),
            actor: account.login.clone(),
            action: "account.passwordChosen".into(),
            target: account.login.clone(),
            details: json!({ "purpose": link.purpose }),
            ip: client.ip.to_string(),
        })
        .await;
    tracing::info!(login = %account.login, purpose = ?link.purpose, "password chosen through a link");

    let user_agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let created =
        web.store().create_web_session(account.id, SESSION_LIFETIME_SECS, &client.ip.to_string(), user_agent).await?;
    let preferences = web.store().preferences(account.id).await?;
    let body = super::auth::session_body(&web, &account, &created.csrf_token, Value::Object(preferences));
    let mut response = Json(body).into_response();
    response.headers_mut().insert(header::SET_COOKIE, session::set_cookie(&created.token, client));
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    Ok(response)
}
