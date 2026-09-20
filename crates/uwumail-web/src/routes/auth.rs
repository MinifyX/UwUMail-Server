//! Logging in and out, and what the app needs to know on start.

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_jmap::ClientInfo;
use uwumail_store::{Account, CodeCheck, Role, SecurityEvent};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::notices::{Notice, Origin, notify};
use crate::session::{self, SESSION_LIFETIME_SECS, Session};
use crate::webauthn::{self, RelyingParty};

/// Public facts for the login page.
pub async fn info(State(web): State<Web>) -> ApiResult<Json<Value>> {
    let counts = web.store().server_counts().await?;
    Ok(Json(json!({
        "hostname": web.settings().hostname,
        "setupRequired": counts.admins == 0,
    })))
}

pub(crate) fn session_body(web: &Web, account: &Account, csrf_token: &str, preferences: Value) -> Value {
    json!({
        "account": {
            "id": account.id,
            "login": account.login,
            "name": account.display_name,
            "role": account.role,
        },
        "csrfToken": csrf_token,
        "preferences": preferences,
        // Whether this person has a mailbox in the browser, so the portal knows where to send
        // them after they sign in and whether to offer the button at all.
        "webmail": super::webmail::allowed_for(web, account),
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
    // A service account has no password here at all, but the answer must not say which of the
    // two it was: the same refusal, and the same time spent, as a wrong password.
    let found = web.store().authenticate(request.login.trim(), &request.password).await?;
    let Some(account) = found.filter(Account::can_use_portal) else {
        web.limiter().record_failure(client.ip);
        tracing::warn!(login = %request.login, ip = %client.ip, "failed web login");
        return Err(ApiError::InvalidCredentials);
    };
    begin_login(&web, account, client, &headers).await
}

/// The password was right: log in, or ask for the second factor first.
pub(crate) async fn begin_login(
    web: &Web,
    account: Account,
    client: ClientInfo,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let security = web.store().security_overview(account.id).await?;
    if !security.second_factor {
        // Failed attempts only reset once the whole login succeeded, so codes cannot be guessed endlessly.
        web.limiter().record_success(client.ip);
        return complete_login(web, &account, client, headers, "password").await;
    }
    let token = web.login_state().start(account.id);
    let body = json!({
        "secondFactor": {
            "token": token,
            "totp": security.totp,
            "passkey": security.passkeys > 0,
            "recoveryCodes": security.recovery_codes_left > 0,
        }
    });
    Ok(no_store(Json(body).into_response()))
}

/// Creates the browser session. `method` is how the login was confirmed, for the activity list.
pub(crate) async fn complete_login(
    web: &Web,
    account: &Account,
    client: ClientInfo,
    headers: &HeaderMap,
    method: &str,
) -> ApiResult<Response> {
    let user_agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let created =
        web.store().create_web_session(account.id, SESSION_LIFETIME_SECS, &client.ip.to_string(), user_agent).await?;
    tracing::info!(login = %account.login, ip = %client.ip, admin = account.role == Role::Admin, method, "web login");
    let event = SecurityEvent {
        kind: "login".into(),
        actor: String::new(),
        ip: client.ip.to_string(),
        details: json!({ "method": method }),
    };
    if let Err(err) = web.store().record_security_event(account.id, event).await {
        tracing::error!(%err, "writing the security activity failed");
    }

    let preferences = web.store().preferences(account.id).await?;
    let body = session_body(web, account, &created.csrf_token, Value::Object(preferences));
    let mut response = Json(body).into_response();
    response.headers_mut().insert(header::SET_COOKIE, session::set_cookie(&created.token, client));
    Ok(no_store(response))
}

#[derive(Deserialize)]
pub struct SecondFactorCode {
    token: String,
    code: String,
}

#[derive(Deserialize)]
pub struct PendingToken {
    token: String,
}

/// What `navigator.credentials.get()` needs to log in with a passkey.
pub async fn passkey_options(State(web): State<Web>, Json(request): Json<PendingToken>) -> ApiResult<Json<Value>> {
    let expired = || ApiError::Rule("loginExpired", "start the login again".into());
    let pending = web.login_state().get(&request.token).ok_or_else(expired)?;
    let passkeys = web.store().passkeys(pending.account_id).await?;
    let challenge = crate::login::random_bytes().to_vec();
    if passkeys.is_empty() || !web.login_state().set_challenge(&request.token, challenge.clone()) {
        return Err(expired());
    }
    Ok(Json(json!({
        "challenge": webauthn::encode(&challenge),
        "rpId": web.settings().hostname,
        "timeout": 120_000,
        "userVerification": "preferred",
        "allowCredentials": passkeys.iter().map(|passkey| json!({
            "type": "public-key",
            "id": webauthn::encode(&passkey.credential_id),
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assertion {
    id: String,
    client_data_json: String,
    authenticator_data: String,
    signature: String,
}

#[derive(Deserialize)]
pub struct PasskeyLogin {
    token: String,
    credential: Assertion,
}

pub async fn passkey_login(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    Json(request): Json<PasskeyLogin>,
) -> ApiResult<Response> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    if web.limiter().is_blocked(client.ip) {
        return Err(ApiError::TooManyAttempts);
    }
    let expired = || ApiError::Rule("loginExpired", "start the login again".into());
    let pending = web.login_state().get(&request.token).ok_or_else(expired)?;
    let challenge = pending.challenge.clone().ok_or_else(expired)?;
    let account =
        web.store().account_by_id(pending.account_id).await?.filter(Account::can_use_portal).ok_or_else(expired)?;

    let checked = async {
        let credential_id = webauthn::decode(&request.credential.id)?;
        let passkey = web
            .store()
            .passkey_by_credential(&credential_id)
            .await
            .map_err(|err| err.to_string())?
            .filter(|passkey| passkey.account_id == account.id)
            .ok_or("this passkey does not belong to the account")?;
        let rp = RelyingParty::for_hostname(&web.settings().hostname);
        let count = webauthn::verify_assertion(
            &rp,
            &challenge,
            &passkey.public_key,
            u32::try_from(passkey.sign_count).unwrap_or(0),
            &webauthn::decode(&request.credential.client_data_json)?,
            &webauthn::decode(&request.credential.authenticator_data)?,
            &webauthn::decode(&request.credential.signature)?,
        )?;
        Ok::<_, String>((passkey, count))
    }
    .await;
    let (passkey, count) = match checked {
        Ok(ok) => ok,
        Err(detail) => {
            web.limiter().record_failure(client.ip);
            web.login_state().failed(&request.token);
            tracing::warn!(login = %account.login, ip = %client.ip, %detail, "passkey login refused");
            return Err(ApiError::Rule("passkeyInvalid", detail));
        }
    };
    web.login_state().finish(&request.token).ok_or_else(expired)?;
    web.store().touch_passkey(passkey.id, i64::from(count)).await?;
    web.limiter().record_success(client.ip);
    complete_login(&web, &account, client, &headers, "passkey").await
}

pub async fn second_factor(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    Json(request): Json<SecondFactorCode>,
) -> ApiResult<Response> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    if web.limiter().is_blocked(client.ip) {
        return Err(ApiError::TooManyAttempts);
    }
    let expired = || ApiError::Rule("loginExpired", "start the login again".into());
    let pending = web.login_state().get(&request.token).ok_or_else(expired)?;
    let account =
        web.store().account_by_id(pending.account_id).await?.filter(Account::can_use_portal).ok_or_else(expired)?;
    let check = web.store().check_second_factor_code(account.id, &request.code).await?;
    if check == CodeCheck::Invalid {
        web.limiter().record_failure(client.ip);
        web.login_state().failed(&request.token);
        tracing::warn!(login = %account.login, ip = %client.ip, "wrong second factor code");
        return Err(ApiError::Rule("codeInvalid", "the code is wrong or was used before".into()));
    }
    web.login_state().finish(&request.token).ok_or_else(expired)?;
    web.limiter().record_success(client.ip);
    let method = match check {
        CodeCheck::RecoveryCode { left } => {
            let ip = client.ip.to_string();
            notify(&web, &account, Notice::RecoveryCodeUsed { left }, Origin { actor: "", ip: &ip }).await;
            "recoveryCode"
        }
        _ => "totp",
    };
    complete_login(&web, &account, client, &headers, method).await
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
