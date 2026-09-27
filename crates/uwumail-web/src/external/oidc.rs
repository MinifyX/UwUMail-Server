//! Logging in at another OpenID Connect provider: the authorization code flow with PKCE, state and
//! nonce (OpenID Connect Core 1.0 section 3.1). Every request to the provider leaves through the
//! egress like other requests of the server, over https to public addresses only.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use aws_lc_rs::{digest, hmac};
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use axum::http::{Method, Request};
use bytes::Bytes;
use data_encoding::{BASE64, BASE64URL_NOPAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uwumail_dav::client::{Transport, checked_url};

use super::OidcConfig;
use crate::jwt;

/// A login at the provider has this long to come back.
const PENDING_LIFETIME: i64 = 10 * 60;
/// The provider's discovery document and keys are asked for again after this long.
const CACHE_LIFETIME: Duration = Duration::from_secs(3600);
const MAX_ANSWER: usize = 1024 * 1024;
/// Clocks of two servers are never quite the same.
const CLOCK_SKEW: i64 = 120;

/// What the provider publishes about itself.
#[derive(Debug, Clone)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    pub userinfo_endpoint: Option<String>,
    /// How the provider wants the client to prove itself at the token endpoint.
    pub token_auth_methods: Vec<String>,
}

/// Someone who logged in at the provider, as its ID token (and userinfo) say.
#[derive(Debug, Clone)]
pub struct OidcIdentity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: String,
    /// The admin group claim has the admin group value.
    pub admin: bool,
    /// Where to go afterwards, as the login page asked.
    pub next: Option<String>,
}

/// A login on its way to the provider. It travels in a cookie of the browser that started it,
/// signed by this server, so nothing is kept here for logins that never come back.
#[derive(Serialize, Deserialize)]
struct Pending {
    state: String,
    nonce: String,
    verifier: String,
    next: Option<String>,
    created: i64,
}

struct Cached {
    at: Instant,
    issuer: String,
    discovery: Discovery,
    jwks: Value,
}

#[derive(Default)]
pub struct OidcState {
    cache: Mutex<Option<Cached>>,
}

impl OidcState {
    pub(super) fn forget(&self) {
        self.cache.lock().expect("oidc cache poisoned").take();
    }
}

fn random() -> String {
    BASE64URL_NOPAD.encode(&crate::login::random_bytes())
}

/// Why the login at the provider did not work, as a code the login page explains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OidcFailure {
    /// The login took too long, or the answer does not belong to a login started here.
    Expired,
    /// The provider could not be asked, or its answer did not check out.
    Failed(String),
}

impl std::fmt::Display for OidcFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OidcFailure::Expired => f.write_str("the login expired or was not started here"),
            OidcFailure::Failed(reason) => f.write_str(reason),
        }
    }
}

async fn send(transport: &dyn Transport, request: Request<Bytes>) -> Result<Value, String> {
    let answer = transport.send(request, MAX_ANSWER).await.map_err(|err| err.to_string())?;
    if answer.status != 200 {
        return Err(format!("the provider answered {}", answer.status));
    }
    serde_json::from_slice(&answer.body).map_err(|_| "the provider's answer is not JSON".to_owned())
}

async fn get_json(transport: &dyn Transport, url: &str, bearer: Option<&str>) -> Result<Value, String> {
    let url = checked_url(url).map_err(|err| err.to_string())?;
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(url.as_str())
        .header(USER_AGENT, "UwUMail")
        .header(ACCEPT, "application/json");
    if let Some(token) = bearer {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    send(transport, request.body(Bytes::new()).map_err(|err| err.to_string())?).await
}

fn endpoint(document: &Value, name: &str) -> Result<String, String> {
    let url = document.get(name).and_then(Value::as_str).ok_or(format!("the provider names no {name}"))?;
    checked_url(url).map_err(|err| format!("{name}: {err}"))?;
    Ok(url.to_owned())
}

/// Reads the provider's discovery document (OpenID Connect Discovery 1.0) and checks it names
/// itself the way the settings do.
pub async fn discover(transport: &dyn Transport, issuer: &str) -> Result<Discovery, String> {
    let issuer = issuer.trim();
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let document = get_json(transport, &url, None).await?;
    // The provider's own spelling counts, give or take the slash at the end that settings often
    // add or leave out; ID tokens are then checked against exactly that spelling.
    let named = document.get("issuer").and_then(Value::as_str).unwrap_or_default();
    if named.is_empty() || named.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(format!("the provider calls itself {named}, not {issuer}"));
    }
    Ok(Discovery {
        issuer: named.to_owned(),
        authorization_endpoint: endpoint(&document, "authorization_endpoint")?,
        token_endpoint: endpoint(&document, "token_endpoint")?,
        jwks_uri: endpoint(&document, "jwks_uri")?,
        userinfo_endpoint: endpoint(&document, "userinfo_endpoint").ok(),
        token_auth_methods: document
            .get("token_endpoint_auth_methods_supported")
            .and_then(Value::as_array)
            .map(|methods| methods.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
    })
}

/// Tries the settings: discovery and keys. Returns what the provider is, for the admin.
pub async fn test(transport: &dyn Transport, config: &OidcConfig) -> Result<String, String> {
    checked_url(config.issuer.trim()).map_err(|err| err.to_string())?;
    let discovery = discover(transport, &config.issuer).await?;
    let jwks = get_json(transport, &discovery.jwks_uri, None).await?;
    let keys = jwks.get("keys").and_then(Value::as_array).map_or(0, Vec::len);
    if keys == 0 {
        return Err("the provider publishes no signing keys".into());
    }
    Ok(format!("{} answers, with {keys} signing keys", discovery.issuer))
}

impl super::ExternalLogin {
    async fn discovery(&self, transport: &dyn Transport, fresh_keys: bool) -> Result<(Discovery, Value), String> {
        let config = self.config();
        let issuer = config.oidc.issuer.trim().to_owned();
        if !fresh_keys {
            let cache = self.oidc().cache.lock().expect("oidc cache poisoned");
            if let Some(cached) = cache.as_ref().filter(|c| c.issuer == issuer && c.at.elapsed() < CACHE_LIFETIME) {
                return Ok((cached.discovery.clone(), cached.jwks.clone()));
            }
        }
        let discovery = discover(transport, &issuer).await?;
        let jwks = get_json(transport, &discovery.jwks_uri, None).await?;
        *self.oidc().cache.lock().expect("oidc cache poisoned") =
            Some(Cached { at: Instant::now(), issuer, discovery: discovery.clone(), jwks: jwks.clone() });
        Ok((discovery, jwks))
    }

    /// Signs what a pending login carries, with this server's secret of the moment.
    fn seal_pending(&self, pending: &Pending) -> String {
        let payload = BASE64URL_NOPAD.encode(&serde_json::to_vec(pending).expect("pending logins are JSON"));
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.secret);
        let tag = hmac::sign(&key, payload.as_bytes());
        format!("{payload}.{}", BASE64URL_NOPAD.encode(tag.as_ref()))
    }

    /// The pending login from a cookie, if this server signed it and it is not too old.
    fn open_pending(&self, cookie: &str) -> Option<Pending> {
        let (payload, tag) = cookie.split_once('.')?;
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.secret);
        hmac::verify(&key, payload.as_bytes(), &BASE64URL_NOPAD.decode(tag.as_bytes()).ok()?).ok()?;
        let pending: Pending = serde_json::from_slice(&BASE64URL_NOPAD.decode(payload.as_bytes()).ok()?).ok()?;
        let age = crate::health::unix_now() - pending.created;
        (0..PENDING_LIFETIME).contains(&age).then_some(pending)
    }

    /// Starts a login at the provider: the address to send the browser to, and the signed cookie
    /// value the browser keeps until it comes back.
    pub async fn oidc_start(
        &self,
        transport: &dyn Transport,
        redirect_uri: &str,
        next: Option<String>,
    ) -> Result<(String, String), String> {
        let config = self.config();
        if !config.oidc.enabled {
            return Err("logging in with OpenID Connect is switched off".into());
        }
        let (discovery, _) = self.discovery(transport, false).await?;
        let (state, nonce, verifier) = (random(), random(), random());
        let challenge = BASE64URL_NOPAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()).as_ref());
        let mut url = Url::parse(&discovery.authorization_endpoint).map_err(|err| err.to_string())?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", config.oidc.client_id.trim())
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", "openid email profile")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        let pending = Pending { state, nonce, verifier, next, created: crate::health::unix_now() };
        Ok((url.to_string(), self.seal_pending(&pending)))
    }

    /// Finishes a login when the provider sends the browser back: checks the state against the
    /// browser's cookie, trades the code in, checks the ID token, and says who logged in.
    pub async fn oidc_finish(
        &self,
        transport: &dyn Transport,
        redirect_uri: &str,
        state: &str,
        cookie: &str,
        code: &str,
    ) -> Result<OidcIdentity, OidcFailure> {
        // The state has to come back to the browser that started the login: otherwise someone
        // could send their own login's answer to somebody else and log them in as themselves.
        let pending = self.open_pending(cookie).ok_or(OidcFailure::Expired)?;
        if state.is_empty() || !crate::session::same(state, &pending.state) {
            return Err(OidcFailure::Expired);
        }
        let config = self.config();
        let failed = OidcFailure::Failed;
        let (discovery, jwks) = self.discovery(transport, false).await.map_err(failed)?;

        let client_id = config.oidc.client_id.trim();
        let secret = config.oidc.client_secret.as_str();
        // client_secret_basic unless the provider only takes the secret in the form.
        let basic = discovery.token_auth_methods.is_empty()
            || discovery.token_auth_methods.iter().any(|method| method == "client_secret_basic");
        let mut pairs = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pending.verifier.as_str()),
        ];
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(checked_url(&discovery.token_endpoint).map_err(|err| failed(err.to_string()))?.as_str())
            .header(USER_AGENT, "UwUMail")
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
        if secret.is_empty() {
            pairs.push(("client_id", client_id));
        } else if basic {
            let encode = |value: &str| url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
            let credentials = BASE64.encode(format!("{}:{}", encode(client_id), encode(secret)).as_bytes());
            request = request.header(AUTHORIZATION, format!("Basic {credentials}"));
        } else {
            pairs.extend([("client_id", client_id), ("client_secret", secret)]);
        }
        let form = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
        let request = request.body(Bytes::from(form)).map_err(|err| failed(err.to_string()))?;
        let tokens = send(transport, request).await.map_err(|err| failed(format!("the token endpoint: {err}")))?;
        let id_token = tokens.get("id_token").and_then(Value::as_str).ok_or(failed("no ID token came back".into()))?;

        let token = jwt::parse(id_token).map_err(failed)?;
        let jwks = if token.key_known(&jwks) {
            jwks
        } else {
            // The provider may have a new key since the keys were fetched.
            self.discovery(transport, true).await.map_err(failed)?.1
        };
        if !token.verify(&jwks) {
            return Err(failed("the ID token's signature does not check out".into()));
        }
        let claims = &token.claims;
        let text = |name: &str| claims.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
        if text("iss") != discovery.issuer {
            return Err(failed("the ID token comes from another issuer".into()));
        }
        let audience_ok = match claims.get("aud") {
            Some(Value::String(aud)) => aud == client_id,
            Some(Value::Array(auds)) => {
                auds.iter().any(|aud| aud.as_str() == Some(client_id))
                    && (auds.len() == 1 || claims.get("azp").and_then(Value::as_str) == Some(client_id))
            }
            _ => false,
        };
        if !audience_ok {
            return Err(failed("the ID token is meant for another app".into()));
        }
        let now = crate::health::unix_now();
        let exp = claims.get("exp").and_then(Value::as_i64).unwrap_or(0);
        if exp + CLOCK_SKEW < now {
            return Err(failed("the ID token has expired".into()));
        }
        if claims.get("iat").and_then(Value::as_i64).is_some_and(|iat| iat > now + CLOCK_SKEW) {
            return Err(failed("the ID token was issued in the future".into()));
        }
        let nonce = text("nonce");
        if !crate::session::same(&nonce, &pending.nonce) {
            return Err(failed("the ID token belongs to another login".into()));
        }
        let subject = text("sub");
        if subject.is_empty() {
            return Err(failed("the ID token names nobody".into()));
        }

        // Some providers (Authelia, for one) keep the address out of the ID token; userinfo has it.
        let mut facts = claims.clone();
        if (!facts.contains_key("email") || !has_claim(&facts, &config.oidc.admin_group_claim))
            && let (Some(userinfo), Some(access)) =
                (&discovery.userinfo_endpoint, tokens.get("access_token").and_then(Value::as_str))
            && let Ok(Value::Object(info)) = get_json(transport, userinfo, Some(access)).await
            && info.get("sub").and_then(Value::as_str) == Some(subject.as_str())
        {
            for (name, value) in info {
                facts.entry(name).or_insert(value);
            }
        }
        let email = facts
            .get("email")
            .and_then(Value::as_str)
            .map(|email| email.trim().to_lowercase())
            .filter(|email| email.contains('@'));
        let email_verified = match facts.get("email_verified") {
            Some(Value::Bool(verified)) => *verified,
            Some(Value::String(verified)) => verified.eq_ignore_ascii_case("true"),
            _ => false,
        };
        let name = ["name", "preferred_username", "nickname"]
            .iter()
            .find_map(|claim| facts.get(*claim).and_then(Value::as_str).filter(|name| !name.trim().is_empty()))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let group_claim = config.oidc.admin_group_claim.trim();
        let wanted = config.oidc.admin_group_value.trim();
        let admin = !group_claim.is_empty()
            && !wanted.is_empty()
            && match facts.get(group_claim) {
                Some(Value::String(value)) => value == wanted,
                Some(Value::Array(values)) => values.iter().any(|value| value.as_str() == Some(wanted)),
                Some(Value::Bool(value)) => *value && wanted == "true",
                _ => false,
            };
        Ok(OidcIdentity { issuer: discovery.issuer, subject, email, email_verified, name, admin, next: pending.next })
    }
}

fn has_claim(facts: &serde_json::Map<String, Value>, claim: &str) -> bool {
    claim.trim().is_empty() || facts.contains_key(claim.trim())
}
