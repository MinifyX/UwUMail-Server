//! Session cookies and the extractors that require a login.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method};
use uwumail_jmap::ClientInfo;
use uwumail_store::{Account, Role};

use crate::Web;
use crate::error::ApiError;

// The cookie names, the CSRF header and the session lifetime live in uwumail-jmap: the webmail
// signs in to JMAP with this very session, and one definition is easier to keep right than two.
pub use uwumail_jmap::auth::{
    CSRF_HEADER, PLAIN_SESSION_COOKIE as PLAIN_COOKIE, SECURE_SESSION_COOKIE as SECURE_COOKIE,
    WEB_SESSION_LIFETIME_SECS as SESSION_LIFETIME_SECS, constant_time_eq as same, session_cookie as token,
};

pub fn client(parts: &Parts) -> ClientInfo {
    parts.extensions.get::<ClientInfo>().copied().unwrap_or_default()
}

pub fn set_cookie(token: &str, client: ClientInfo) -> HeaderValue {
    let value = if client.https {
        format!("{SECURE_COOKIE}={token}; Path=/; Max-Age={SESSION_LIFETIME_SECS}; HttpOnly; Secure; SameSite=Strict")
    } else {
        format!("{PLAIN_COOKIE}={token}; Path=/; Max-Age={SESSION_LIFETIME_SECS}; HttpOnly; SameSite=Strict")
    };
    HeaderValue::from_str(&value).expect("hex tokens are valid header values")
}

pub fn clear_cookie(client: ClientInfo) -> HeaderValue {
    HeaderValue::from_static(if client.https {
        "__Host-uwumail=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict"
    } else {
        "uwumail=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict"
    })
}

/// A logged-in person. Requests that change something must carry the CSRF token.
pub struct Session {
    pub account: Account,
    pub csrf_token: String,
    pub token: String,
    pub client: ClientInfo,
    /// When the person logged in with this session.
    pub created_at: i64,
}

impl FromRequestParts<Web> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, web: &Web) -> Result<Self, Self::Rejection> {
        let client = client(parts);
        let token = token(&parts.headers, client.https).ok_or(ApiError::NotLoggedIn)?;
        let session = web.store().web_session(&token, SESSION_LIFETIME_SECS).await?.ok_or(ApiError::NotLoggedIn)?;
        if !matches!(parts.method, Method::GET | Method::HEAD) {
            let sent = parts.headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default();
            if !same(sent, &session.csrf_token) {
                return Err(ApiError::CsrfMismatch);
            }
        }
        Ok(Session {
            account: session.account,
            csrf_token: session.csrf_token,
            token,
            client,
            created_at: session.created_at,
        })
    }
}

/// A logged-in admin.
pub struct Admin(pub Session);

impl FromRequestParts<Web> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, web: &Web) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(parts, web).await?;
        if session.account.role != Role::Admin {
            return Err(ApiError::Forbidden);
        }
        Ok(Admin(session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_cookies_over_https() {
        let https = ClientInfo { https: true, ..ClientInfo::default() };
        let cookie = set_cookie("abc", https);
        assert!(cookie.to_str().unwrap().starts_with("__Host-uwumail=abc; Path=/;"));
        assert!(cookie.to_str().unwrap().contains("Secure"));
        assert!(!set_cookie("abc", ClientInfo::default()).to_str().unwrap().contains("Secure"));
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "ab"));
    }
}
