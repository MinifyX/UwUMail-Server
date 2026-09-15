//! Session cookies and the extractors that require a login.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use uwumail_jmap::ClientInfo;
use uwumail_store::{Account, Role};

use crate::Web;
use crate::error::ApiError;

/// A session ends after this long without use.
pub const SESSION_LIFETIME_SECS: i64 = 14 * 24 * 3600;
/// Over HTTPS the `__Host-` prefix pins the cookie to this exact host.
const SECURE_COOKIE: &str = "__Host-uwumail";
const PLAIN_COOKIE: &str = "uwumail";
pub const CSRF_HEADER: &str = "x-csrf-token";

pub fn client(parts: &Parts) -> ClientInfo {
    parts.extensions.get::<ClientInfo>().copied().unwrap_or_default()
}

/// The session token from the request's cookies, if any.
pub fn token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SECURE_COOKIE || *name == PLAIN_COOKIE)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty() && value.len() <= 128)
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

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
        let token = token(&parts.headers).ok_or(ApiError::NotLoggedIn)?;
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
            client: client(parts),
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
    fn reads_either_cookie_name() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("theme=dark; __Host-uwumail=abc123"));
        assert_eq!(token(&headers).as_deref(), Some("abc123"));
        headers.insert(header::COOKIE, HeaderValue::from_static("uwumail=def456"));
        assert_eq!(token(&headers).as_deref(), Some("def456"));
        headers.insert(header::COOKIE, HeaderValue::from_static("uwumail="));
        assert_eq!(token(&headers), None);
    }

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
