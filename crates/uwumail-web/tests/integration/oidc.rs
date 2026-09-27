//! Logging in to the portal at another OpenID Connect provider (docs/login-oidc-ldap.md), against a
//! small provider in this process: discovery, keys, the token endpoint and signed ID tokens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use aws_lc_rs::{digest, hmac};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bytes::Bytes;
use data_encoding::{BASE32_NOPAD, BASE64, BASE64URL_NOPAD};
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;
use uwumail_dav::client::{Answer, BoxFuture, RemoteError, Transport};
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{AuthConfig, OidcConfig, Web, WebSettings};

const ISSUER: &str = "https://idp.example.net/application/o/uwumail/";
const CLIENT_ID: &str = "uwumail-portal";
const CLIENT_SECRET: &str = "geheimnis-der-anwendung";

/// "The internet": every request goes to the provider in this process.
struct Internet(Router);

impl Transport for Internet {
    fn send(&self, request: Request<Bytes>, max_bytes: usize) -> BoxFuture<'_, Result<Answer, RemoteError>> {
        Box::pin(async move {
            let (mut parts, body) = request.into_parts();
            assert_eq!(parts.uri.host(), Some("idp.example.net"), "only the provider is asked");
            parts.uri = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").parse().unwrap();
            let response = self.0.clone().oneshot(Request::from_parts(parts, Body::from(body))).await.unwrap();
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body =
                axum::body::to_bytes(response.into_body(), max_bytes).await.map_err(|_| RemoteError::TooLarge)?;
            Ok(Answer { status, headers, body })
        })
    }
}

/// What the provider will say in the next ID token, and what it saw.
struct Provider {
    key: EcdsaKeyPair,
    /// Signs tokens whose claims ask for it (`_forged`), under the published key's name.
    rogue: EcdsaKeyPair,
    /// The claims of the next ID token; `nonce` is filled in from the login unless set.
    claims: Mutex<Value>,
    /// PKCE challenges of the logins it saw start, by code.
    challenges: Mutex<HashMap<String, (String, String)>>,
}

impl Provider {
    fn jwk(&self) -> Value {
        let point = self.key.public_key().as_ref();
        json!({ "kty": "EC", "crv": "P-256", "kid": "k1", "use": "sig", "alg": "ES256",
            "x": BASE64URL_NOPAD.encode(&point[1..33]), "y": BASE64URL_NOPAD.encode(&point[33..65]) })
    }

    fn sign(&self, claims: &Value) -> String {
        let mut claims = claims.clone();
        let key = match claims.as_object_mut().and_then(|claims| claims.remove("_forged")) {
            Some(_) => &self.rogue,
            None => &self.key,
        };
        let header =
            BASE64URL_NOPAD.encode(json!({ "alg": "ES256", "kid": "k1", "typ": "JWT" }).to_string().as_bytes());
        let body = BASE64URL_NOPAD.encode(claims.to_string().as_bytes());
        let input = format!("{header}.{body}");
        let signature = key.sign(&SystemRandom::new(), input.as_bytes()).unwrap();
        format!("{input}.{}", BASE64URL_NOPAD.encode(signature.as_ref()))
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

async fn discovery() -> impl IntoResponse {
    axum::Json(json!({
        "issuer": ISSUER,
        "authorization_endpoint": "https://idp.example.net/authorize",
        "token_endpoint": "https://idp.example.net/token",
        "jwks_uri": "https://idp.example.net/jwks",
        "userinfo_endpoint": "https://idp.example.net/userinfo",
        "token_endpoint_auth_methods_supported": ["client_secret_basic"],
    }))
}

async fn token(State(provider): State<Arc<Provider>>, headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    let expected = format!("Basic {}", BASE64.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}").as_bytes()));
    if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "invalid_client" })));
    }
    let form: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    let Some((challenge, nonce)) = provider.challenges.lock().unwrap().remove(&form["code"]) else {
        return (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "invalid_grant" })));
    };
    let verifier = digest::digest(&digest::SHA256, form["code_verifier"].as_bytes());
    assert_eq!(BASE64URL_NOPAD.encode(verifier.as_ref()), challenge, "PKCE with the verifier behind the challenge");
    assert_eq!(form["redirect_uri"], "https://mail.example.org/api/auth/oidc/callback");
    let mut claims = provider.claims.lock().unwrap().clone();
    if claims.get("nonce").is_none() {
        claims["nonce"] = json!(nonce);
    }
    (
        StatusCode::OK,
        axum::Json(json!({ "access_token": "at-1", "token_type": "Bearer", "id_token": provider.sign(&claims) })),
    )
}

fn key() -> EcdsaKeyPair {
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new()).unwrap();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref()).unwrap()
}

fn provider() -> (Arc<Provider>, Router) {
    let provider =
        Arc::new(Provider { key: key(), rogue: key(), claims: Mutex::new(Value::Null), challenges: Mutex::default() });
    let jwks = json!({ "keys": [provider.jwk()] });
    let router = Router::new()
        .route("/application/o/uwumail/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(move || async move { axum::Json(jwks.clone()) }))
        .route("/token", post(token))
        .route("/userinfo", get(|| async { (StatusCode::UNAUTHORIZED, "") }))
        .with_state(provider.clone());
    (provider, router)
}

struct Setup {
    app: Router,
    web: Web,
    store: Store,
    provider: Arc<Provider>,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    store
        .create_account(NewAccount {
            address: "mini@example.org".into(),
            display_name: "Mini".into(),
            password: Some("katzenpfote-123".into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    let settings = SmtpSettings {
        hostname: "mail.example.org".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let web = Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            certificate: None,
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );
    let (provider, router) = provider();
    web.set_oidc_transport(Arc::new(Internet(router)));
    web.external_login().configure(AuthConfig {
        oidc: OidcConfig {
            enabled: true,
            // Settings often leave out the slash the provider has at the end.
            issuer: ISSUER.trim_end_matches('/').into(),
            client_id: CLIENT_ID.into(),
            client_secret: CLIENT_SECRET.into(),
            button_label: "Authentik".into(),
            auto_create: true,
            allowed_domains: vec!["example.org".into()],
            admin_group_claim: "groups".into(),
            admin_group_value: "uwumail-admins".into(),
        },
        ..AuthConfig::default()
    });
    Setup { app: web.router(), web, store, provider, _dir: dir }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn get_page(app: &Router, path: &str, cookie: Option<&str>) -> Reply {
    let mut request = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let mut request = request.body(Body::empty()).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    Reply { status, headers, body: String::from_utf8_lossy(&bytes).into_owned() }
}

fn cookies(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().split(';').next().unwrap().to_owned())
        .collect()
}

/// Where the page after the provider sends the browser on.
fn onward(body: &str) -> String {
    let start = body.find("url=").expect("a page that moves on") + 4;
    body[start..body[start..].find('"').unwrap() + start].replace("&amp;", "&")
}

/// Starts a login at the portal and plays the provider's part up to the way back: returns the
/// callback address and the browser's cookie.
async fn start(setup: &Setup, next: &str) -> (String, String) {
    let reply = get_page(&setup.app, &format!("/api/auth/oidc/start?next={next}"), None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let location = Url::parse(reply.headers[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(location.host_str(), Some("idp.example.net"));
    let query: HashMap<String, String> = location.query_pairs().into_owned().collect();
    assert_eq!((query["client_id"].as_str(), query["code_challenge_method"].as_str()), (CLIENT_ID, "S256"));
    assert_eq!(query["scope"], "openid email profile");
    let code = format!("code-{}", query["state"]);
    setup
        .provider
        .challenges
        .lock()
        .unwrap()
        .insert(code.clone(), (query["code_challenge"].clone(), query["nonce"].clone()));
    let cookie = cookies(&reply.headers).into_iter().find(|c| c.starts_with("__Host-uwumail-oidc=")).unwrap();
    (format!("/api/auth/oidc/callback?code={code}&state={}", query["state"]), cookie)
}

fn identity(sub: &str, email: &str) -> Value {
    json!({ "iss": ISSUER, "aud": CLIENT_ID, "sub": sub, "email": email, "email_verified": true, "name": "Leni Lindwurm",
        "groups": ["staff"], "iat": now(), "exp": now() + 300 })
}

async fn finish(setup: &Setup, claims: Value) -> Reply {
    *setup.provider.claims.lock().unwrap() = claims;
    let (callback, cookie) = start(setup, "/account/security").await;
    get_page(&setup.app, &callback, Some(&cookie)).await
}

#[tokio::test]
async fn logging_in_at_the_provider() {
    let setup = setup().await;
    let reply = get_page(&setup.app, "/api/info", None).await;
    let info: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(info["oidc"]["label"], "Authentik");

    // The first login of someone the provider vouches for makes their account.
    let reply = finish(&setup, identity("sub-leni", "Leni@Example.org")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(onward(&reply.body), "/account/security");
    let session = cookies(&reply.headers).into_iter().find(|c| c.starts_with("__Host-uwumail=")).expect("logged in");
    let me: Value = serde_json::from_str(&get_page(&setup.app, "/api/session", Some(&session)).await.body).unwrap();
    assert_eq!(me["account"]["login"], "leni@example.org", "{me}");
    let leni = setup.store.account("leni@example.org").await.unwrap().unwrap();
    assert_eq!((leni.display_name.as_str(), leni.role), ("Leni Lindwurm", Role::User));
    assert_eq!(setup.store.auth_source(leni.id).await.unwrap(), "oidc");
    assert!(!setup.store.has_password(leni.id).await.unwrap());

    // The next time the provider's lasting subject counts, not the address.
    let reply = finish(&setup, identity("sub-leni", "leni.neu@example.org")).await;
    assert_eq!(onward(&reply.body), "/account/security");
    assert!(setup.store.account("leni.neu@example.org").await.unwrap().is_none());

    // The admin group makes admins of new accounts.
    let mut boss = identity("sub-boss", "boss@example.org");
    boss["groups"] = json!(["staff", "uwumail-admins"]);
    finish(&setup, boss).await;
    assert_eq!(setup.store.account("boss@example.org").await.unwrap().unwrap().role, Role::Admin);

    // What does not check out.
    let refusals = [
        ("nonce", json!("some-other-login"), "failed"),
        ("aud", json!("another-app"), "failed"),
        ("iss", json!("https://idp.example.com/"), "failed"),
        ("exp", json!(now() - 3600), "failed"),
        ("email_verified", json!(false), "emailNotVerified"),
        ("email", json!("someone@example.net"), "domainNotAllowed"),
    ];
    for (claim, value, code) in refusals {
        let mut claims = identity("sub-new", "new@example.org");
        claims[claim] = value;
        let reply = finish(&setup, claims).await;
        assert_eq!(onward(&reply.body), format!("/login?oidcError={code}"), "{claim}");
        assert!(cookies(&reply.headers).iter().all(|c| !c.starts_with("__Host-uwumail=")), "{claim}: no session");
    }
    assert!(setup.store.account("new@example.org").await.unwrap().is_none());

    // A token signed with a key the provider does not publish, claiming to be its key.
    let mut forged = identity("sub-new", "new@example.org");
    forged["_forged"] = json!(true);
    let reply = finish(&setup, forged).await;
    assert_eq!(onward(&reply.body), "/login?oidcError=failed");
    assert!(setup.store.account("new@example.org").await.unwrap().is_none());

    // The answer has to come back to the browser that started the login.
    *setup.provider.claims.lock().unwrap() = identity("sub-mallory", "mallory@example.org");
    let (callback, _) = start(&setup, "/").await;
    let reply = get_page(&setup.app, &callback, None).await;
    assert_eq!(onward(&reply.body), "/login?oidcError=expired");
    let (_, cookie) = start(&setup, "/").await;
    let reply = get_page(&setup.app, &callback, Some(&cookie)).await;
    assert_eq!(onward(&reply.body), "/login?oidcError=expired", "another login's state");
    assert!(setup.store.account("mallory@example.org").await.unwrap().is_none());

    // Only paths on this server come next.
    *setup.provider.claims.lock().unwrap() = identity("sub-leni", "leni@example.org");
    let (callback, cookie) = start(&setup, "https://elsewhere.example.com/").await;
    let reply = get_page(&setup.app, &callback, Some(&cookie)).await;
    assert_eq!(onward(&reply.body), "/account");
    assert!(setup.web.external_login().config().oidc.enabled);

    // Leni's mailbox becomes a service, then a shared mailbox: the provider opens neither.
    setup.store.set_account_role("leni@example.org", Role::Service).await.unwrap();
    let reply = finish(&setup, identity("sub-leni", "leni@example.org")).await;
    assert_eq!(onward(&reply.body), "/login?oidcError=noAccount");
    assert!(cookies(&reply.headers).iter().all(|c| !c.starts_with("__Host-uwumail=")), "no session");
    let me: Value = serde_json::from_str(&get_page(&setup.app, "/api/session", Some(&session)).await.body).unwrap();
    assert!(me["account"].is_null(), "the old session went: {me}");
    setup.store.make_shared_mailbox("leni@example.org", Vec::new()).await.unwrap();
    let reply = finish(&setup, identity("sub-leni", "leni@example.org")).await;
    assert_eq!(onward(&reply.body), "/login?oidcError=noAccount");
    assert_eq!(setup.store.account("leni@example.org").await.unwrap().map(|a| a.shared_mailbox), Some(true));
}

fn totp(secret: &str) -> String {
    let secret = BASE32_NOPAD.decode(secret.as_bytes()).unwrap();
    let step = (now() / 30) as u64;
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &secret), &step.to_be_bytes());
    let digest = tag.as_ref();
    let offset = usize::from(digest[19] & 0x0f);
    let value = u32::from_be_bytes(digest[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    format!("{:06}", value % 1_000_000)
}

#[tokio::test]
async fn an_existing_account_links_once_and_keeps_its_second_factor() {
    let setup = setup().await;
    let mini = setup.store.account("mini@example.org").await.unwrap().unwrap();
    let totp_setup = setup.store.begin_totp(mini.id, "UwUMail", &mini.login).await.unwrap();
    let recovery = setup.store.confirm_totp(mini.id, &totp(&totp_setup.secret)).await.unwrap().expect("TOTP on");

    // The verified address finds the account; the second factor is still asked for.
    let reply = finish(&setup, identity("sub-mini", "mini@example.org")).await;
    let target = onward(&reply.body);
    assert!(target.starts_with("/login?pending="), "{target}");
    assert!(cookies(&reply.headers).iter().all(|c| !c.starts_with("__Host-uwumail=")), "no session before the code");
    let query: HashMap<String, String> =
        Url::parse(&format!("https://mail.example.org{target}")).unwrap().query_pairs().into_owned().collect();
    assert_eq!(query["methods"], "totp,recovery");
    assert_eq!(query["next"], "/account/security");
    // The code of this very moment went into setting it up; a recovery code does as well.
    let body = json!({ "token": query["pending"], "code": recovery[0] }).to_string();
    let mut request = Request::post("/api/auth/second-factor")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = setup.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::SET_COOKIE).is_some());
    let events = setup.store.security_events(mini.id, 10).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "oidcLinked"));
    let login = events.iter().find(|event| event.kind == "login").expect("the login is listed");
    assert_eq!(login.details["method"], "oidc", "no password was typed here");

    // Another login at the provider that now claims the same address gets nothing.
    let reply = finish(&setup, identity("sub-mallory", "mini@example.org")).await;
    assert_eq!(onward(&reply.body), "/login?oidcError=alreadyLinked");

    // Without an account and without making one, there is nothing to log in to.
    let mut config = (*setup.web.external_login().config()).clone();
    config.oidc.auto_create = false;
    setup.web.external_login().configure(config);
    let reply = finish(&setup, identity("sub-nobody", "nobody@example.org")).await;
    assert_eq!(onward(&reply.body), "/login?oidcError=noAccount");
}
