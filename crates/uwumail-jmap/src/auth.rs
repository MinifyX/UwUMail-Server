//! HTTP authentication for JMAP: Basic with an app password or the account password. Logins with
//! the account password are cached briefly, because checking it is slow on purpose.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::json;
use sha2::{Digest, Sha256};
use uwumail_store::{Account, AppScope, MailAuth, MailAuthDenied, Store};

const CACHE_LIFETIME: Duration = Duration::from_secs(300);
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_FAILURES: u32 = 10;

/// The portal's session cookie and CSRF header, which the webmail signs in with. Defined here
/// because this is the layer that reads them; the portal uses the same names from this module.
/// Over HTTPS the `__Host-` prefix pins the cookie to this exact host.
pub const SECURE_SESSION_COOKIE: &str = "__Host-uwumail";
pub const PLAIN_SESSION_COOKIE: &str = "uwumail";
pub const CSRF_HEADER: &str = "x-csrf-token";
/// A session ends after this long without use.
pub const WEB_SESSION_LIFETIME_SECS: i64 = 14 * 24 * 3600;

/// The session token from a request's cookies, if any.
pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SECURE_SESSION_COOKIE || *name == PLAIN_SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty() && value.len() <= 128)
}

/// Compares two tokens without letting the time taken say how much of them matched.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Who is connecting, as seen by the HTTP layer (after trusted reverse proxies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientInfo {
    pub ip: IpAddr,
    /// The client used HTTPS, directly or through a proxy.
    pub https: bool,
}

impl Default for ClientInfo {
    fn default() -> Self {
        ClientInfo { ip: IpAddr::V4(Ipv4Addr::LOCALHOST), https: false }
    }
}

#[derive(Debug)]
pub enum AuthError {
    Missing,
    Invalid,
    Blocked,
    Internal,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, detail) = match self {
            AuthError::Missing => (StatusCode::UNAUTHORIZED, "Log in with your address and password."),
            AuthError::Invalid => (StatusCode::UNAUTHORIZED, "The address or password is wrong."),
            AuthError::Blocked => (StatusCode::TOO_MANY_REQUESTS, "Too many failed logins, try again later."),
            AuthError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server."),
        };
        let body = json!({ "type": "about:blank", "status": status.as_u16(), "detail": detail });
        let mut response =
            (status, [(header::CONTENT_TYPE, "application/problem+json")], body.to_string()).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"UwUMail\""));
        }
        response
    }
}

pub struct Authenticator {
    store: Store,
    /// Whether the webmail is switched on for the whole server. Signing in with the portal's
    /// session is only for the webmail, so it stops here too when an admin switches it off —
    /// not just at the page. Basic auth is untouched by this.
    webmail: Arc<AtomicBool>,
    /// What app passwords must allow, and the protocol name for the activity list and the log.
    scope: AppScope,
    protocol: &'static str,
    secret: [u8; 32],
    /// Account id, when it was cached, and the same as a Unix time to compare with password changes.
    cache: Mutex<HashMap<[u8; 32], (i64, Instant, i64)>>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

fn network(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            IpAddr::V6(segments.into())
        }
        v4 => v4,
    }
}

impl Authenticator {
    pub fn new(store: Store) -> Authenticator {
        Authenticator::for_protocol(store, AppScope::Mail, "jmap")
    }

    /// Basic authentication for another HTTP protocol, like CalDAV with its own app password scope.
    pub fn for_protocol(store: Store, scope: AppScope, protocol: &'static str) -> Authenticator {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("the system RNG failed");
        Authenticator {
            store,
            // Without a server saying otherwise the webmail is on; a build without one has no
            // page to reach anyway.
            webmail: Arc::new(AtomicBool::new(true)),
            scope,
            protocol,
            secret,
            cache: Mutex::default(),
            failures: Mutex::default(),
        }
    }

    /// Hands the authenticator the server's webmail switch, so it sees changes at once.
    pub fn watch_webmail(&mut self, webmail: Arc<AtomicBool>) {
        self.webmail = webmail;
    }

    fn cache_key(&self, login: &str, password: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.secret);
        hasher.update(login.to_lowercase().as_bytes());
        hasher.update([0]);
        hasher.update(password.as_bytes());
        hasher.finalize().into()
    }

    fn blocked(&self, ip: IpAddr) -> bool {
        let failures = self.failures.lock().expect("auth failures poisoned");
        failures
            .get(&network(ip))
            .is_some_and(|(count, since)| *count >= MAX_FAILURES && since.elapsed() < FAILURE_WINDOW)
    }

    fn record_failure(&self, ip: IpAddr) {
        let mut failures = self.failures.lock().expect("auth failures poisoned");
        if failures.len() > 100_000 {
            failures.retain(|_, (_, since)| since.elapsed() < FAILURE_WINDOW);
        }
        let entry = failures.entry(network(ip)).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= FAILURE_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    /// Who is signed in, by `Authorization` or — for the webmail — by the portal's session.
    ///
    /// `changes` says whether this request would change something: those have to carry the CSRF
    /// token as well, exactly like the portal's own JSON API. Reading with the cookie alone is
    /// safe because the server sends no CORS headers and the cookie is `SameSite=Strict`, so no
    /// other site can read an answer or even get the cookie sent.
    pub async fn account_for(
        &self,
        headers: &HeaderMap,
        client: ClientInfo,
        changes: bool,
    ) -> Result<Account, AuthError> {
        if headers.get(header::AUTHORIZATION).is_none() {
            return self.session_account(headers, changes).await;
        }
        self.account(headers, client).await
    }

    /// The portal's session as a JMAP login: only for people whose webmail is switched on.
    ///
    /// Deliberately independent of the JMAP protocol switch, which decides what *other* mail
    /// programs may do with this account's password. The webmail is part of the server itself.
    ///
    /// The same three conditions the portal shows the way in by. An account that is told it has no
    /// webmail must not get one by asking for it directly — an answer that only the button knows
    /// about is not a rule, it is a decoration.
    async fn session_account(&self, headers: &HeaderMap, changes: bool) -> Result<Account, AuthError> {
        if !self.webmail.load(Ordering::Relaxed) {
            return Err(AuthError::Missing);
        }
        let token = session_cookie(headers).ok_or(AuthError::Missing)?;
        let session = match self.store.web_session(&token, WEB_SESSION_LIFETIME_SECS).await {
            Ok(Some(session)) => session,
            Ok(None) => return Err(AuthError::Invalid),
            Err(_) => return Err(AuthError::Internal),
        };
        if changes {
            let sent = headers.get(CSRF_HEADER).and_then(|value| value.to_str().ok()).unwrap_or_default();
            if !constant_time_eq(sent, &session.csrf_token) {
                return Err(AuthError::Invalid);
            }
        }
        if !session.account.can_use_portal() || !session.account.webmail || !session.account.has_mailbox() {
            return Err(AuthError::Invalid);
        }
        Ok(session.account)
    }

    pub async fn account(&self, headers: &HeaderMap, client: ClientInfo) -> Result<Account, AuthError> {
        let value = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).ok_or(AuthError::Missing)?;
        let (scheme, credentials) = value.split_once(' ').ok_or(AuthError::Invalid)?;
        if !scheme.eq_ignore_ascii_case("basic") {
            return Err(AuthError::Invalid);
        }
        let decoded = BASE64.decode(credentials.trim()).map_err(|_| AuthError::Invalid)?;
        let decoded = String::from_utf8(decoded).map_err(|_| AuthError::Invalid)?;
        let (login, password) = decoded.split_once(':').ok_or(AuthError::Invalid)?;

        let key = self.cache_key(login, password);
        let cached = self.cache.lock().expect("auth cache poisoned").get(&key).copied();
        if let Some((account_id, since, cached_at)) = cached
            && since.elapsed() < CACHE_LIFETIME
        {
            match self.store.account_by_id(account_id).await {
                // A new password, app password rule or second factor since then ends the cached
                // login, and so does a protocol switched off in the meantime: the switch has to
                // hold here too, or it would only hold for five minutes.
                Ok(Some(account))
                    if account.can_log_in()
                        && account.credentials_changed_at < cached_at
                        && account.may_use(self.protocol) =>
                {
                    return Ok(account);
                }
                Ok(_) => {
                    self.cache.lock().expect("auth cache poisoned").remove(&key);
                }
                Err(_) => return Err(AuthError::Internal),
            }
        }

        if self.blocked(client.ip) {
            return Err(AuthError::Blocked);
        }
        let ip = client.ip.to_string();
        match self.store.authenticate_mail(login, password, self.scope, self.protocol, &ip).await {
            Ok(MailAuth::Ok { account, app_password }) => {
                // App passwords are a quick lookup; only the slow account password is worth caching.
                if app_password.is_none() {
                    let mut cache = self.cache.lock().expect("auth cache poisoned");
                    if cache.len() > 10_000 {
                        cache.retain(|_, (_, since, _)| since.elapsed() < CACHE_LIFETIME);
                    }
                    let unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                    cache.insert(key, (account.id, Instant::now(), unix));
                }
                Ok(account)
            }
            Ok(MailAuth::Denied(reason)) => {
                // A phone still using the right account password should not lock out its network.
                if reason != MailAuthDenied::AppPasswordRequired {
                    self.record_failure(client.ip);
                }
                tracing::warn!(%login, ip = %client.ip, %reason, protocol = self.protocol, "failed login");
                Err(AuthError::Invalid)
            }
            Err(err) => {
                tracing::error!(%err, protocol = self.protocol, "authentication failed internally");
                Err(AuthError::Internal)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, session_cookie};
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn reads_either_cookie_name() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("theme=dark; __Host-uwumail=abc123"));
        assert_eq!(session_cookie(&headers).as_deref(), Some("abc123"));
        headers.insert(header::COOKIE, HeaderValue::from_static("uwumail=def456"));
        assert_eq!(session_cookie(&headers).as_deref(), Some("def456"));
        headers.insert(header::COOKIE, HeaderValue::from_static("uwumail="));
        assert_eq!(session_cookie(&headers), None);
    }

    #[test]
    fn compares_tokens_without_leaking_their_length_in_time() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
