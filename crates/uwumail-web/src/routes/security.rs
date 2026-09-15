//! "Security" in My account: password, authenticator app, recovery codes, app passwords,
//! browser sessions and the activity list.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{AppScope, NewAppPassword, SecurityEvent, Store, StoreError};

use super::check_password;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::health::unix_now;
use crate::login::CONFIRMATION_LIFETIME;
use crate::notices::{Notice, Origin, notify};
use crate::session::Session;
use crate::webauthn::{self, RelyingParty};

/// Sensitive changes need the password again, unless the login or the last confirmation is fresh.
pub(crate) async fn confirm_identity(web: &Web, session: &Session, password: Option<&str>) -> ApiResult<()> {
    let id = Store::web_session_id(&session.token);
    let fresh = session.created_at > unix_now() - CONFIRMATION_LIFETIME.as_secs() as i64;
    if fresh || web.login_state().recently_confirmed(&id) {
        return Ok(());
    }
    let Some(password) = password.filter(|password| !password.is_empty()) else {
        return Err(ApiError::Rule("confirmPassword", "confirm this with your password".into()));
    };
    let ip = session.client.ip;
    if web.limiter().is_blocked(ip) {
        return Err(ApiError::TooManyAttempts);
    }
    match web.store().authenticate(&session.account.login, password).await? {
        Some(account) if account.id == session.account.id => {
            web.limiter().record_success(ip);
            web.login_state().confirm(&id);
            Ok(())
        }
        _ => {
            web.limiter().record_failure(ip);
            Err(ApiError::Rule("wrongPassword", "the password is wrong".into()))
        }
    }
}

fn origin(session: &Session) -> (String, String) {
    (String::new(), session.client.ip.to_string())
}

async fn event(web: &Web, session: &Session, kind: &str, details: Value) {
    let event = SecurityEvent { kind: kind.into(), actor: String::new(), ip: session.client.ip.to_string(), details };
    if let Err(err) = web.store().record_security_event(session.account.id, event).await {
        tracing::error!(%err, "writing the security activity failed");
    }
}

/// The QR code as rows of modules, drawn by the portal: no image library, no inline images.
fn qr_code(data: &str) -> Value {
    match qrcode::QrCode::with_error_correction_level(data, qrcode::EcLevel::M) {
        Ok(code) => {
            let modules: String =
                code.to_colors().iter().map(|color| if *color == qrcode::Color::Dark { '1' } else { '0' }).collect();
            json!({ "size": code.width(), "modules": modules })
        }
        Err(_) => Value::Null,
    }
}

pub async fn overview(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let id = session.account.id;
    let store = web.store();
    let security = store.security_overview(id).await?;
    let current = Store::web_session_id(&session.token);
    let sessions: Vec<Value> = store
        .web_sessions(id)
        .await?
        .into_iter()
        .map(|info| {
            let mut value = json!(info);
            value["current"] = json!(info.id == current);
            value
        })
        .collect();
    Ok(Json(json!({
        "totp": security.totp,
        "passkeys": store.passkeys(id).await?,
        "recoveryCodesLeft": security.recovery_codes_left,
        "secondFactor": security.second_factor,
        "appsNeedAppPassword": security.apps_need_app_password,
        "appPasswordsRequired": security.app_passwords_required(),
        "appPasswords": store.app_passwords(id).await?,
        "sessions": sessions,
        "events": store.security_events(id, 50).await?,
    })))
}

#[derive(Deserialize, Default)]
pub struct Confirmation {
    #[serde(default)]
    password: Option<String>,
}

#[derive(Deserialize)]
pub struct PasswordChange {
    current: String,
    new: String,
}

pub async fn change_password(
    State(web): State<Web>,
    session: Session,
    Json(change): Json<PasswordChange>,
) -> ApiResult<StatusCode> {
    check_password(&change.new, &session.account.login)?;
    if change.new == change.current {
        return Err(ApiError::Rule("samePassword", "choose a password you did not use just now".into()));
    }
    let ip = session.client.ip;
    if web.limiter().is_blocked(ip) {
        return Err(ApiError::TooManyAttempts);
    }
    match web.store().change_password(session.account.id, &change.current, &change.new, &session.token).await {
        Ok(()) => {}
        Err(StoreError::Rule { code: "wrongPassword", message }) => {
            web.limiter().record_failure(ip);
            return Err(ApiError::Rule("wrongPassword", message));
        }
        Err(err) => return Err(err.into()),
    }
    tracing::info!(login = %session.account.login, "password changed");
    let (actor, ip) = origin(&session);
    notify(&web, &session.account, Notice::PasswordChanged, Origin { actor: &actor, ip: &ip }).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn start_totp(
    State(web): State<Web>,
    session: Session,
    body: Option<Json<Confirmation>>,
) -> ApiResult<Json<Value>> {
    let confirmation = body.map(|Json(body)| body).unwrap_or_default();
    confirm_identity(&web, &session, confirmation.password.as_deref()).await?;
    let issuer = format!("UwUMail ({})", web.settings().hostname);
    let setup = web.store().begin_totp(session.account.id, &issuer, &session.account.login).await?;
    Ok(Json(json!({ "secret": setup.secret, "uri": setup.uri, "qr": qr_code(&setup.uri) })))
}

#[derive(Deserialize)]
pub struct Code {
    code: String,
}

pub async fn confirm_totp(State(web): State<Web>, session: Session, Json(body): Json<Code>) -> ApiResult<Json<Value>> {
    let codes = web.store().confirm_totp(session.account.id, &body.code).await?;
    let (actor, ip) = origin(&session);
    notify(&web, &session.account, Notice::TotpEnabled, Origin { actor: &actor, ip: &ip }).await;
    Ok(Json(json!({ "recoveryCodes": codes })))
}

pub async fn disable_totp(
    State(web): State<Web>,
    session: Session,
    body: Option<Json<Confirmation>>,
) -> ApiResult<StatusCode> {
    let confirmation = body.map(|Json(body)| body).unwrap_or_default();
    confirm_identity(&web, &session, confirmation.password.as_deref()).await?;
    if !web.store().security_overview(session.account.id).await?.totp {
        return Err(ApiError::NotFound("authenticator app".into()));
    }
    web.store().disable_totp(session.account.id).await?;
    let (actor, ip) = origin(&session);
    notify(&web, &session.account, Notice::TotpDisabled, Origin { actor: &actor, ip: &ip }).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn new_recovery_codes(
    State(web): State<Web>,
    session: Session,
    body: Option<Json<Confirmation>>,
) -> ApiResult<Json<Value>> {
    let confirmation = body.map(|Json(body)| body).unwrap_or_default();
    confirm_identity(&web, &session, confirmation.password.as_deref()).await?;
    let codes = web.store().regenerate_recovery_codes(session.account.id).await?;
    let (actor, ip) = origin(&session);
    notify(&web, &session.account, Notice::RecoveryCodesCreated, Origin { actor: &actor, ip: &ip }).await;
    Ok(Json(json!({ "recoveryCodes": codes })))
}

#[derive(Deserialize)]
pub struct AppsSwitch {
    on: bool,
    #[serde(default)]
    password: Option<String>,
}

pub async fn set_apps_need_app_password(
    State(web): State<Web>,
    session: Session,
    Json(switch): Json<AppsSwitch>,
) -> ApiResult<StatusCode> {
    // Turning it on only makes the account safer; turning it off needs the password.
    if !switch.on {
        confirm_identity(&web, &session, switch.password.as_deref()).await?;
    }
    web.store().set_apps_need_app_password(session.account.id, switch.on).await?;
    if switch.on {
        event(&web, &session, "appsNeedAppPassword", json!({})).await;
    } else {
        let (actor, ip) = origin(&session);
        notify(&web, &session.account, Notice::AppsMayUseMainPassword, Origin { actor: &actor, ip: &ip }).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAppPasswordRequest {
    name: String,
    scopes: Vec<AppScope>,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    password: Option<String>,
}

pub async fn create_app_password(
    State(web): State<Web>,
    session: Session,
    Json(request): Json<NewAppPasswordRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    confirm_identity(&web, &session, request.password.as_deref()).await?;
    let created = web
        .store()
        .create_app_password(
            session.account.id,
            NewAppPassword { name: request.name, scopes: request.scopes, expires_at: request.expires_at },
        )
        .await?;
    let (actor, ip) = origin(&session);
    let notice = Notice::AppPasswordCreated { name: created.app_password.name.clone() };
    notify(&web, &session.account, notice, Origin { actor: &actor, ip: &ip }).await;
    Ok((StatusCode::CREATED, Json(json!({ "appPassword": created.app_password, "secret": created.secret }))))
}

pub async fn revoke_app_password(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let revoked = web.store().revoke_app_password(session.account.id, id).await?;
    event(&web, &session, "appPasswordRevoked", json!({ "name": revoked.name })).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn end_session(State(web): State<Web>, session: Session, Path(id): Path<String>) -> ApiResult<StatusCode> {
    web.store().end_web_session(session.account.id, &id).await?;
    event(&web, &session, "sessionEnded", json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn end_other_sessions(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let ended = web.store().end_other_web_sessions(session.account.id, &session.token).await?;
    if ended > 0 {
        event(&web, &session, "sessionsEnded", json!({ "count": ended })).await;
    }
    Ok(Json(json!({ "ended": ended })))
}

/// What `navigator.credentials.create()` needs to add a passkey.
pub async fn passkey_options(
    State(web): State<Web>,
    session: Session,
    body: Option<Json<Confirmation>>,
) -> ApiResult<Json<Value>> {
    let confirmation = body.map(|Json(body)| body).unwrap_or_default();
    confirm_identity(&web, &session, confirmation.password.as_deref()).await?;
    let challenge = web.login_state().start_registration(&Store::web_session_id(&session.token));
    let existing = web.store().passkeys(session.account.id).await?;
    let account = &session.account;
    let display_name = if account.display_name.trim().is_empty() { &account.login } else { &account.display_name };
    Ok(Json(json!({
        "challenge": webauthn::encode(&challenge),
        "rp": { "id": web.settings().hostname, "name": "UwUMail" },
        // The user handle only needs to be stable and private: the account number, not the address.
        "user": { "id": webauthn::encode(&account.id.to_be_bytes()), "name": account.login, "displayName": display_name },
        "pubKeyCredParams": [
            { "type": "public-key", "alg": -8 },
            { "type": "public-key", "alg": -7 },
            { "type": "public-key", "alg": -257 },
        ],
        "timeout": 120_000,
        "attestation": "none",
        "authenticatorSelection": { "residentKey": "discouraged", "userVerification": "preferred" },
        "excludeCredentials": existing.iter().map(|passkey| json!({
            "type": "public-key",
            "id": webauthn::encode(&passkey.credential_id),
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedCredential {
    client_data_json: String,
    attestation_object: String,
}

#[derive(Deserialize)]
pub struct NewPasskey {
    #[serde(default)]
    name: String,
    credential: CreatedCredential,
}

pub async fn add_passkey(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewPasskey>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let challenge = web
        .login_state()
        .finish_registration(&Store::web_session_id(&session.token))
        .ok_or_else(|| ApiError::Rule("passkeyExpired", "start adding the passkey again".into()))?;
    let invalid = |detail: String| {
        tracing::warn!(login = %session.account.login, %detail, "refused a new passkey");
        ApiError::Rule("passkeyInvalid", detail)
    };
    let client_data = webauthn::decode(&new.credential.client_data_json).map_err(invalid)?;
    let attestation = webauthn::decode(&new.credential.attestation_object).map_err(invalid)?;
    let rp = RelyingParty::for_hostname(&web.settings().hostname);
    let credential = webauthn::verify_registration(&rp, &challenge, &client_data, &attestation).map_err(invalid)?;
    let (passkey, codes) = web
        .store()
        .add_passkey(
            session.account.id,
            credential.credential_id,
            credential.public_key,
            i64::from(credential.sign_count),
            &new.name,
        )
        .await?;
    let (actor, ip) = origin(&session);
    notify(
        &web,
        &session.account,
        Notice::PasskeyAdded { name: passkey.name.clone() },
        Origin { actor: &actor, ip: &ip },
    )
    .await;
    Ok((StatusCode::CREATED, Json(json!({ "passkey": passkey, "recoveryCodes": codes }))))
}

pub async fn remove_passkey(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    body: Option<Json<Confirmation>>,
) -> ApiResult<StatusCode> {
    let confirmation = body.map(|Json(body)| body).unwrap_or_default();
    confirm_identity(&web, &session, confirmation.password.as_deref()).await?;
    let removed = web.store().remove_passkey(session.account.id, id).await?;
    let (actor, ip) = origin(&session);
    notify(&web, &session.account, Notice::PasskeyRemoved { name: removed.name }, Origin { actor: &actor, ip: &ip })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_codes_are_square_module_strings() {
        let qr = qr_code("otpauth://totp/UwUMail:leni%40example.de?secret=AAAABBBBCCCCDDDD");
        let size = qr["size"].as_u64().unwrap() as usize;
        assert!(size >= 21);
        assert_eq!(qr["modules"].as_str().unwrap().len(), size * size);
    }
}
