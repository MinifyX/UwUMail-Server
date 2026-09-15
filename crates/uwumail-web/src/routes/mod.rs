pub mod account;
pub mod admin;
pub mod auth;
pub mod links;
pub mod people;

use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use uwumail_store::AuditEntry;

use crate::Web;
use crate::assets;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

pub const MIN_PASSWORD_CHARS: usize = 10;
const MAX_PASSWORD_CHARS: usize = 256;

pub async fn not_found() -> ApiError {
    ApiError::NotFound("this API endpoint".into())
}

/// Every page of the app is the same HTML file; the app picks the page from the URL.
pub async fn app_page() -> Response {
    match assets::index() {
        Some(index) => assets::respond(index),
        None => ApiError::NotFound("the web app".into()).into_response(),
    }
}

pub async fn asset(Path(path): Path<String>) -> Response {
    match assets::find(&format!("/assets/{path}")) {
        Some(asset) => assets::respond(asset),
        None => ApiError::NotFound("this file".into()).into_response(),
    }
}

/// Long enough, and not just the address. Length beats rules about digits and symbols.
pub fn check_password(password: &str, login: &str) -> ApiResult<()> {
    let length = password.chars().count();
    let login = login.trim();
    let local_part = login.split('@').next().unwrap_or(login);
    if length < MIN_PASSWORD_CHARS {
        return Err(ApiError::Rule("weakPassword", format!("use at least {MIN_PASSWORD_CHARS} characters")));
    }
    if length > MAX_PASSWORD_CHARS {
        return Err(ApiError::Rule("weakPassword", format!("use at most {MAX_PASSWORD_CHARS} characters")));
    }
    if password.eq_ignore_ascii_case(login) || password.eq_ignore_ascii_case(local_part) {
        return Err(ApiError::Rule("weakPassword", "the password must not be the address".into()));
    }
    Ok(())
}

/// Writes an admin's change to the change log. A failure is logged, but does not undo the change.
pub async fn audit(web: &Web, session: &Session, action: &str, target: &str, details: Value) {
    tracing::info!(admin = %session.account.login, action, target, "admin change");
    let entry = AuditEntry {
        actor_id: Some(session.account.id),
        actor: session.account.login.clone(),
        action: action.into(),
        target: target.into(),
        details,
        ip: session.client.ip.to_string(),
    };
    if let Err(err) = web.store().record_audit(entry).await {
        tracing::error!(%err, "writing the change log failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_need_some_length() {
        assert!(check_password("kurz", "leni@example.de").is_err());
        assert!(check_password("leni@example.de", "Leni@Example.de").is_err());
        assert!(check_password("leni", "leni@example.de").is_err());
        assert!(check_password("Seifenblase-Wanderweg", "leni@example.de").is_ok());
        assert!(check_password(&"x".repeat(300), "leni@example.de").is_err());
    }
}
