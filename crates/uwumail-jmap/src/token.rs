//! `POST /jmap/token`: a program trades the login, the account password and — with two-factor
//! authentication — a code for a new app password that it then sends as a bearer token
//! (docs/jmap-tokens.md). The app password is an ordinary one: it shows up in My account → Security
//! and can be revoked there.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use uwumail_store::{CodeCheck, NewAppPassword, SecurityEvent, StoreError};

use crate::auth::ClientInfo;
use crate::session::base_url;
use crate::{Jmap, ids};

/// Longest a token may live, when the program asks for an expiry at all.
const MAX_EXPIRY_DAYS: u64 = 3650;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TokenRequest {
    username: String,
    password: String,
    /// The six digits of the authenticator app, or a recovery code.
    #[serde(default)]
    code: Option<String>,
    /// What the person sees in the list of app passwords, e.g. "Thunderbird on the laptop".
    name: String,
    #[serde(default)]
    expires_in_days: Option<u64>,
}

fn problem(status: StatusCode, kind: &str, detail: &str) -> Response {
    let body = json!({
        "type": format!("urn:uwumail:jmap:token:{kind}"),
        "status": status.as_u16(),
        "detail": detail,
    });
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json"), (header::CACHE_CONTROL, "no-store")],
        body.to_string(),
    )
        .into_response()
}

fn wrong_login() -> Response {
    problem(StatusCode::UNAUTHORIZED, "invalidCredentials", "The address or password is wrong.")
}

pub async fn handle(
    State(jmap): State<Jmap>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let auth = &jmap.inner.auth;
    if auth.is_blocked(client) {
        return problem(StatusCode::TOO_MANY_REQUESTS, "tooManyAttempts", "Too many failed logins, try again later.");
    }
    let request: TokenRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(err) => return problem(StatusCode::BAD_REQUEST, "invalidRequest", &err.to_string()),
    };
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return problem(StatusCode::BAD_REQUEST, "invalidName", "The name must have 1 to 60 characters.");
    }
    let expires_in_days = request.expires_in_days.filter(|days| *days > 0);
    if expires_in_days.is_some_and(|days| days > MAX_EXPIRY_DAYS) {
        return problem(StatusCode::BAD_REQUEST, "invalidExpiry", "expiresInDays may be at most 3650.");
    }

    let store = auth.store();
    let account = match store.authenticate(&request.username, &request.password).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            auth.failed(client);
            tracing::warn!(login = %request.username, ip = %client.ip, "failed JMAP token login");
            return wrong_login();
        }
        Err(err) => {
            tracing::error!(%err, "JMAP token login failed internally");
            return problem(StatusCode::INTERNAL_SERVER_ERROR, "serverFail", "Something went wrong on the server.");
        }
    };
    // The same switch as for Basic authentication: an account that may not use JMAP gets no token.
    if !account.may_use("jmap") {
        return problem(StatusCode::FORBIDDEN, "protocolOff", "This account may not use JMAP.");
    }

    let security = match store.security_overview(account.id).await {
        Ok(security) => security,
        Err(err) => {
            tracing::error!(%err, "reading the second factors failed");
            return problem(StatusCode::INTERNAL_SERVER_ERROR, "serverFail", "Something went wrong on the server.");
        }
    };
    let mut method = "password";
    if security.second_factor {
        if auth.second_factor_locked(account.id) {
            return problem(StatusCode::TOO_MANY_REQUESTS, "tooManyAttempts", "Too many wrong codes, try again later.");
        }
        let Some(code) = request.code.as_deref().map(str::trim).filter(|code| !code.is_empty()) else {
            // The password was right; the program asks for the code and tries again.
            return problem(
                StatusCode::UNAUTHORIZED,
                "secondFactorRequired",
                "Enter the code from your authenticator app or a recovery code.",
            );
        };
        match store.check_second_factor_code(account.id, code).await {
            Ok(CodeCheck::Invalid) => {
                auth.failed(client);
                auth.second_factor_failed(account.id);
                tracing::warn!(login = %account.login, ip = %client.ip, "wrong second factor for a JMAP token");
                return problem(StatusCode::UNAUTHORIZED, "invalidCode", "The code is wrong or was used before.");
            }
            Ok(CodeCheck::Totp) => method = "totp",
            Ok(CodeCheck::RecoveryCode { .. }) => method = "recoveryCode",
            Err(err) => {
                tracing::error!(%err, "checking a second factor failed");
                return problem(StatusCode::INTERNAL_SERVER_ERROR, "serverFail", "Something went wrong on the server.");
            }
        }
    }

    let expires_at = expires_in_days.map(|days| crate::methods::unix_now() + days as i64 * 86_400);
    let new = NewAppPassword { name: name.to_owned(), scopes: vec![auth.scope()], expires_at };
    let created = match store.create_app_password(account.id, new).await {
        Ok(created) => created,
        Err(StoreError::Rule { code, message }) => return problem(StatusCode::CONFLICT, code, &message),
        Err(StoreError::Invalid(message)) => return problem(StatusCode::BAD_REQUEST, "invalidRequest", &message),
        Err(err) => {
            tracing::error!(%err, "creating a JMAP token failed");
            return problem(StatusCode::INTERNAL_SERVER_ERROR, "serverFail", "Something went wrong on the server.");
        }
    };
    let ip = client.ip.to_string();
    let login_event = SecurityEvent {
        kind: "login".into(),
        actor: String::new(),
        ip: ip.clone(),
        details: json!({ "method": method, "protocol": "jmapToken" }),
    };
    if let Err(err) = store.record_security_event(account.id, login_event).await {
        tracing::error!(%err, "writing the security activity failed");
    }
    match &jmap.inner.notice {
        // The server tells the person by mail, the way the portal does, and writes the activity.
        Some(notice) => notice(account.clone(), created.app_password.name.clone(), ip).await,
        None => {
            let event = SecurityEvent {
                kind: "appPasswordCreated".into(),
                actor: String::new(),
                ip,
                details: json!({ "name": created.app_password.name }),
            };
            if let Err(err) = store.record_security_event(account.id, event).await {
                tracing::error!(%err, "writing the security activity failed");
            }
        }
    }
    tracing::info!(login = %account.login, ip = %client.ip, "JMAP token created");

    let base = base_url(&headers, client);
    let body = json!({
        "token": created.secret,
        "tokenType": "Bearer",
        "id": created.app_password.id,
        "name": created.app_password.name,
        "expiresAt": created.app_password.expires_at.map(crate::dates::format),
        "accountId": ids::account(account.id),
        "username": account.login,
        "sessionUrl": format!("{base}/jmap/session"),
    });
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")],
        body.to_string(),
    )
        .into_response()
}
