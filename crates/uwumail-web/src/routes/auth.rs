//! Logging in and out, and what the app needs to know on start.

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_jmap::ClientInfo;
use uwumail_store::{Account, Role};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::{self, SESSION_LIFETIME_SECS, Session};

/// Public facts for the login page.
pub async fn info(State(web): State<Web>) -> ApiResult<Json<Value>> {
    let counts = web.store().server_counts().await?;
    Ok(Json(json!({
        "hostname": web.settings().hostname,
        "setupRequired": counts.admins == 0,
    })))
}

fn session_body(web: &Web, account: &Account, csrf_token: &str, preferences: Value) -> Value {
    json!({
        "account": {
            "id": account.id,
            "login": account.login,
            "name": account.display_name,
            "role": account.role,
        },
        "csrfToken": csrf_token,
        "preferences": preferences,
        "server": {
            "hostname": web.settings().hostname,
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Who is logged in, or `null`. Also renews the cookie, so an active session never runs out.
///
/// Not being logged in is an ordinary answer here (the app asks on every start), not an error.
pub async fn session(State(web): State<Web>, session: Result<Session, ApiError>) -> ApiResult<Response> {
    let session = match session {
        Ok(session) => session,
        Err(ApiError::NotLoggedIn) => return Ok(no_store(Json(Value::Null).into_response())),
        Err(err) => return Err(err),
    };
    let preferences = web.store().preferences(session.account.id).await?;
    let body = session_body(&web, &session.account, &session.csrf_token, Value::Object(preferences));
    let mut response = Json(body).into_response();
    response.headers_mut().insert(header::SET_COOKIE, session::set_cookie(&session.token, session.client));
    Ok(no_store(response))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    login: String,
    password: String,
}

pub async fn login(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> ApiResult<Response> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    if web.limiter().is_blocked(client.ip) {
        return Err(ApiError::TooManyAttempts);
    }
    let Some(account) = web.store().authenticate(request.login.trim(), &request.password).await? else {
        web.limiter().record_failure(client.ip);
        tracing::warn!(login = %request.login, ip = %client.ip, "failed web login");
        return Err(ApiError::InvalidCredentials);
    };
    web.limiter().record_success(client.ip);

    let user_agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let created =
        web.store().create_web_session(account.id, SESSION_LIFETIME_SECS, &client.ip.to_string(), user_agent).await?;
    tracing::info!(login = %account.login, ip = %client.ip, admin = account.role == Role::Admin, "web login");

    let preferences = web.store().preferences(account.id).await?;
    let body = session_body(&web, &account, &created.csrf_token, Value::Object(preferences));
    let mut response = Json(body).into_response();
    response.headers_mut().insert(header::SET_COOKIE, session::set_cookie(&created.token, client));
    Ok(no_store(response))
}

/// Logs out. Works without a valid session too, so a stale cookie can always be cleared.
pub async fn logout(State(web): State<Web>, parts: Parts) -> ApiResult<Response> {
    let mut parts = parts;
    let client = session::client(&parts);
    if let Some(token) = session::token(&parts.headers) {
        // Only a request from the app itself may end a valid session.
        match Session::from_request_parts(&mut parts, &web).await {
            Ok(_) | Err(ApiError::NotLoggedIn) => web.store().delete_web_session(&token).await?,
            Err(err) => return Err(err),
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::SET_COOKIE, session::clear_cookie(client));
    Ok(response)
}
