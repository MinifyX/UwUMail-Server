//! HTTP authentication for JMAP: Basic with an app password or the account password, or an app
//! password or OAuth access token (docs/oauth.md) alone as a bearer token. Logins with the account password are cached briefly, because
//! checking it is slow on purpose.

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
use uwumail_store::{ALL_SCOPES, Account, AppScope, LiveLogin, MailAuth, MailAuthDenied, Store};

const CACHE_LIFETIME: Duration = Duration::from_secs(300);
/// Wrong second factors at the token endpoint, per account, before it waits out the window.
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
///
/// Transport-aware on purpose: over HTTPS only the `__Host-`-prefixed cookie is read, over plain
/// HTTP only the un-prefixed one. Reading both on every request undid the `__Host-` prefix — a
/// cookie a sibling host or a plain-HTTP answer planted as `uwumail=` would shadow the real
/// `__Host-uwumail`, binding the victim to the attacker's session (security-audit-0.5.2 S-7).
pub fn session_cookie(headers: &HeaderMap, https: bool) -> Option<String> {
    let name = if https { SECURE_SESSION_COOKIE } else { PLAIN_SESSION_COOKIE };
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(cookie, _)| *cookie == name)
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

/// Who logged in, and with what.
#[derive(Debug, Clone)]
pub struct Login {
    pub account: Account,
    /// The credential, as push subscriptions record it: `session:<hash>`, `app:<id>`, `oauth:<grant>`
    /// or `password`.
    pub credential: String,
    /// What the credential may be used for. An app password or an OAuth app limited to `mail`
    /// reads and sends mail; calendars and address books need `dav`, over JMAP as over CalDAV and
    /// CardDAV. The account password and the webmail's session may do everything.
    pub scopes: Vec<AppScope>,
}

impl Login {
    fn password(account: Account) -> Login {
        Login { account, credential: uwumail_store::PUSH_CREDENTIAL_PASSWORD.to_owned(), scopes: ALL_SCOPES.to_vec() }
    }

    /// Whether this login may reach calendars and address books.
    pub fn may_use_dav(&self) -> bool {
        self.scopes.contains(&AppScope::Dav)
    }

    /// The login as a connection that stays open keeps it, to check it again later.
    pub fn live(&self) -> LiveLogin {
        LiveLogin::new(&self.account, self.credential.clone())
    }
}

#[derive(Debug)]
pub enum AuthError {
    Missing,
    Invalid,
    Blocked,
    /// Too many password checks at once on the whole server.
    Busy,
    Internal,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, detail) = match self {
            AuthError::Missing => (StatusCode::UNAUTHORIZED, "Log in with your address and password."),
            AuthError::Invalid => (StatusCode::UNAUTHORIZED, "The address or password is wrong."),
            AuthError::Blocked => (StatusCode::TOO_MANY_REQUESTS, "Too many failed logins, try again later."),
            AuthError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "The server is busy, try again in a moment."),
            AuthError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server."),
        };
        let body = json!({ "type": "about:blank", "status": status.as_u16(), "detail": detail });
        let mut response =
            (status, [(header::CONTENT_TYPE, "application/problem+json")], body.to_string()).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"UwUMail\""));
            // Apps signed in with OAuth learn from the second challenge that a token works too.
            response
                .headers_mut()
                .append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"UwUMail\""));
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
    /// Wrong second factors at the token endpoint, per account.
    second_factor_failures: Mutex<HashMap<i64, (u32, Instant)>>,
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
            second_factor_failures: Mutex::default(),
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

    /// The failed-login counts of the whole server, shared with the portal, IMAP, ManageSieve and
    /// SMTP. JMAP, DAV and `/jmap/token` used to keep one of their own each, per network only, so
    /// guesses at one login from many networks were never slowed down (security-audit-0.16.0
    /// PROTOCOLS-8).
    fn limiter(&self) -> &Arc<uwumail_store::AuthLimiter> {
        self.store.auth_limiter()
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
        self.login_for(headers, client, changes).await.map(|login| login.account)
    }

    /// The same, and which credential it was: push subscriptions belong to that (RFC 8620, 7.2).
    pub async fn login_for(&self, headers: &HeaderMap, client: ClientInfo, changes: bool) -> Result<Login, AuthError> {
        if headers.get(header::AUTHORIZATION).is_none() {
            let account = self.session_account(headers, client.https, changes).await?;
            // session_account only answers with a cookie there.
            let token = session_cookie(headers, client.https).unwrap_or_default();
            return Ok(Login {
                account,
                credential: uwumail_store::push_credential_for_session(&token),
                scopes: ALL_SCOPES.to_vec(),
            });
        }
        self.login(headers, client).await
    }

    /// The portal's session as a JMAP login: only for people whose webmail is switched on.
    ///
    /// Deliberately independent of the JMAP protocol switch, which decides what *other* mail
    /// programs may do with this account's password. The webmail is part of the server itself.
    ///
    /// The same three conditions the portal shows the way in by. An account that is told it has no
    /// webmail must not get one by asking for it directly — an answer that only the button knows
    /// about is not a rule, it is a decoration.
    async fn session_account(&self, headers: &HeaderMap, https: bool, changes: bool) -> Result<Account, AuthError> {
        if !self.webmail.load(Ordering::Relaxed) {
            return Err(AuthError::Missing);
        }
        let token = session_cookie(headers, https).ok_or(AuthError::Missing)?;
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
        self.login(headers, client).await.map(|login| login.account)
    }

    async fn login(&self, headers: &HeaderMap, client: ClientInfo) -> Result<Login, AuthError> {
        let value = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).ok_or(AuthError::Missing)?;
        let (scheme, credentials) = value.split_once(' ').ok_or(AuthError::Invalid)?;
        if scheme.eq_ignore_ascii_case("bearer") {
            return self.bearer(credentials.trim(), client).await;
        }
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
                    return Ok(Login::password(account));
                }
                Ok(_) => {
                    self.cache.lock().expect("auth cache poisoned").remove(&key);
                }
                Err(_) => return Err(AuthError::Internal),
            }
        }

        let Some(attempt) = self.limiter().begin(client.ip, login) else {
            return Err(AuthError::Blocked);
        };
        let ip = client.ip.to_string();
        // Stamp the cache from before the slow check runs, not after: a credential change that
        // lands while argon2 is verifying must invalidate the entry, not be masked for the cache
        // lifetime (security-audit-0.5.2 S-21).
        let started = Instant::now();
        let started_unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let checked = self.store.authenticate_mail(login, password, self.scope, self.protocol, &ip).await;
        drop(attempt);
        match checked {
            Ok(MailAuth::Ok { account, app_password, credential, scopes }) => {
                self.limiter().record_success(client.ip, login);
                // App passwords are a quick lookup; only the slow account password is worth caching.
                if app_password.is_none() {
                    let mut cache = self.cache.lock().expect("auth cache poisoned");
                    if cache.len() > 10_000 {
                        cache.retain(|_, (_, since, _)| since.elapsed() < CACHE_LIFETIME);
                    }
                    cache.insert(key, (account.id, started, started_unix));
                }
                Ok(Login { account, credential, scopes })
            }
            Ok(MailAuth::Denied(reason)) => {
                match reason {
                    // A phone still using the right account password should not lock out its network.
                    MailAuthDenied::AppPasswordRequired => {}
                    MailAuthDenied::UnknownLogin => self.limiter().record_unknown_login(client.ip),
                    _ => self.limiter().record_failure(client.ip, login),
                }
                tracing::warn!(%login, ip = %client.ip, %reason, protocol = self.protocol, "failed login");
                Err(AuthError::Invalid)
            }
            Err(uwumail_store::StoreError::Busy) => Err(AuthError::Busy),
            Err(err) => {
                tracing::error!(%err, protocol = self.protocol, "authentication failed internally");
                Err(AuthError::Internal)
            }
        }
    }

    /// `Authorization: Bearer <app password or OAuth access token>`: the secret alone, without the
    /// login. Wrong tokens count against the network like wrong passwords.
    async fn bearer(&self, token: &str, client: ClientInfo) -> Result<Login, AuthError> {
        if self.limiter().is_blocked(client.ip) {
            return Err(AuthError::Blocked);
        }
        let ip = client.ip.to_string();
        let checked = if uwumail_store::is_oauth_access_token(token) {
            self.store.authenticate_oauth_grant(token, self.scope, self.protocol, &ip).await
        } else {
            self.store.authenticate_bearer(token, self.scope, self.protocol, &ip).await.map(|auth| (auth, None))
        };
        match checked {
            Ok((MailAuth::Ok { account, credential, scopes, .. }, _)) => Ok(Login { account, credential, scopes }),
            Ok((MailAuth::Denied(reason), _)) => {
                self.limiter().record_wrong_token(client.ip);
                tracing::warn!(ip = %client.ip, %reason, protocol = self.protocol, "failed bearer login");
                Err(AuthError::Invalid)
            }
            Err(err) => {
                tracing::error!(%err, protocol = self.protocol, "authentication failed internally");
                Err(AuthError::Internal)
            }
        }
    }

    /// The account behind a login made earlier, as it is now, while that login still holds: the
    /// credential is still there (app password, OAuth app, webmail session, unchanged password),
    /// the account may still log in and still use JMAP, or for the webmail's session still has its
    /// webmail, and the webmail is still on. A WebSocket and an event stream ask this before each
    /// request and each event, so they end with the login instead of outliving it.
    pub async fn still_valid(&self, login: &LiveLogin) -> Option<Account> {
        let webmail = login.credential.starts_with("session:");
        if webmail && !self.webmail.load(Ordering::Relaxed) {
            return None;
        }
        let protocol = (!webmail).then_some(self.protocol);
        let account = match self.store.live_login(login, protocol).await {
            Ok(account) => account?,
            Err(err) => {
                tracing::error!(%err, protocol = self.protocol, "checking a login again failed");
                return None;
            }
        };
        if webmail && (!account.can_use_portal() || !account.webmail || !account.has_mailbox()) {
            return None;
        }
        Some(account)
    }

    /// Lets one password check for `login` begin, or says no: see
    /// [`uwumail_store::AuthLimiter::begin`].
    pub fn begin(&self, client: ClientInfo, login: &str) -> Option<uwumail_store::Attempt> {
        self.limiter().begin(client.ip, login)
    }

    /// Counts a wrong password (or second factor) for `login` from this client's network.
    pub fn failed(&self, client: ClientInfo, login: &str) {
        self.limiter().record_failure(client.ip, login);
    }

    /// Counts a login that does not exist here, from this client's network.
    pub fn unknown_login(&self, client: ClientInfo) {
        self.limiter().record_unknown_login(client.ip);
    }

    /// Forgives `login` its own failures, once it got all the way in.
    pub fn succeeded(&self, client: ClientInfo, login: &str) {
        self.limiter().record_success(client.ip, login);
    }

    /// Whether this account's second factor is locked after too many wrong codes, from any network.
    pub fn second_factor_locked(&self, account_id: i64) -> bool {
        let failures = self.second_factor_failures.lock().expect("second factor failures poisoned");
        failures
            .get(&account_id)
            .is_some_and(|(count, since)| *count >= MAX_FAILURES && since.elapsed() < FAILURE_WINDOW)
    }

    /// Counts a wrong second factor for an account.
    pub fn second_factor_failed(&self, account_id: i64) {
        let mut failures = self.second_factor_failures.lock().expect("second factor failures poisoned");
        failures.retain(|_, (_, since)| since.elapsed() < FAILURE_WINDOW);
        let entry = failures.entry(account_id).or_insert((0, Instant::now()));
        entry.0 += 1;
    }

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    pub(crate) fn scope(&self) -> AppScope {
        self.scope
    }
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, session_cookie};
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn reads_the_cookie_the_transport_allows() {
        let mut headers = HeaderMap::new();
        // Over HTTPS the __Host- cookie is read and a planted plain cookie riding ahead of it is
        // ignored, so it cannot shadow the real session.
        headers.insert(header::COOKIE, HeaderValue::from_static("uwumail=attacker; __Host-uwumail=victim"));
        assert_eq!(session_cookie(&headers, true).as_deref(), Some("victim"));
        // Over plain HTTP only the un-prefixed cookie exists.
        assert_eq!(session_cookie(&headers, false).as_deref(), Some("attacker"));
        // A plain cookie alone over HTTPS is not accepted.
        headers.insert(header::COOKIE, HeaderValue::from_static("uwumail=attacker"));
        assert_eq!(session_cookie(&headers, true), None);
        // An empty value is no cookie.
        headers.insert(header::COOKIE, HeaderValue::from_static("__Host-uwumail="));
        assert_eq!(session_cookie(&headers, true), None);
    }

    #[test]
    fn compares_tokens_without_leaking_their_length_in_time() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
