//! HTTP authentication for JMAP: Basic with the account password, cached briefly.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::json;
use sha2::{Digest, Sha256};
use uwumail_store::{Account, Store};

const CACHE_LIFETIME: Duration = Duration::from_secs(300);
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_FAILURES: u32 = 10;

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
    secret: [u8; 32],
    cache: Mutex<HashMap<[u8; 32], (i64, Instant)>>,
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
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("the system RNG failed");
        Authenticator { store, secret, cache: Mutex::default(), failures: Mutex::default() }
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
        if let Some((account_id, since)) = cached
            && since.elapsed() < CACHE_LIFETIME
        {
            match self.store.account_by_id(account_id).await {
                Ok(Some(account)) if account.can_log_in() => return Ok(account),
                Ok(_) => {}
                Err(_) => return Err(AuthError::Internal),
            }
        }

        if self.blocked(client.ip) {
            return Err(AuthError::Blocked);
        }
        match self.store.authenticate(login, password).await {
            Ok(Some(account)) => {
                let mut cache = self.cache.lock().expect("auth cache poisoned");
                if cache.len() > 10_000 {
                    cache.retain(|_, (_, since)| since.elapsed() < CACHE_LIFETIME);
                }
                cache.insert(key, (account.id, Instant::now()));
                Ok(account)
            }
            Ok(None) => {
                self.record_failure(client.ip);
                tracing::warn!(%login, ip = %client.ip, "failed JMAP login");
                Err(AuthError::Invalid)
            }
            Err(err) => {
                tracing::error!(%err, "JMAP authentication failed internally");
                Err(AuthError::Internal)
            }
        }
    }
}
