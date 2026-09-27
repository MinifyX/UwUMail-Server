//! The OAuth 2.0 / OpenID Connect provider for mail apps (docs/oauth.md): discovery, dynamic
//! registration, the consent in the portal, the code flow with PKCE, refresh token rotation with
//! reuse detection, revocation, userinfo and the signed ID token.

use std::sync::Arc;
use std::time::Instant;

use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use data_encoding::BASE64URL_NOPAD;
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const PASSWORD: &str = "katzenpfote-123";
/// RFC 7636 appendix B.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT: &str = "http://127.0.0.1/callback";

fn web(store: &Store) -> Web {
    let settings = SmtpSettings {
        hostname: "mail.example.org".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            certificate: None,
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    )
}

async fn setup() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    store
        .create_account(NewAccount {
            address: "mini@example.org".into(),
            display_name: "Mini".into(),
            password: Some(PASSWORD.into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    (web(&store).router(), store, dir)
}

fn request(method: &str, path: &str, content_type: &str, body: String) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    request
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn get(app: &Router, path: &str) -> (StatusCode, Value) {
    send(app, request("GET", path, "application/json", String::new())).await
}

/// The portal session of `mini`: cookie and CSRF token.
async fn login(app: &Router) -> (String, String) {
    let body = json!({ "login": "mini@example.org", "password": PASSWORD }).to_string();
    let response = app.clone().oneshot(request("POST", "/api/auth/login", "application/json", body)).await.unwrap();
    let cookie =
        response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, json["csrfToken"].as_str().unwrap().to_owned())
}

async fn portal(app: &Router, method: &str, path: &str, body: Value, auth: &(String, String)) -> (StatusCode, Value) {
    let mut request =
        request(method, path, "application/json", if body.is_null() { String::new() } else { body.to_string() });
    request.headers_mut().insert(header::COOKIE, auth.0.parse().unwrap());
    request.headers_mut().insert(CSRF_HEADER, auth.1.parse().unwrap());
    send(app, request).await
}

async fn form(app: &Router, path: &str, pairs: &[(&str, &str)]) -> (StatusCode, Value) {
    let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
    send(app, request("POST", path, "application/x-www-form-urlencoded", body)).await
}

async fn bearer(app: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let mut request = request("GET", path, "application/json", String::new());
    request.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
    send(app, request).await
}

async fn register(app: &Router, redirect_uris: Value) -> (StatusCode, Value) {
    let body = json!({ "client_name": "Thunderbird", "redirect_uris": redirect_uris, "token_endpoint_auth_method": "client_secret_basic" });
    send(app, request("POST", "/oauth/register", "application/json", body.to_string())).await
}

fn authorize_query(client_id: &str, extra: &[(&str, &str)]) -> String {
    let mut pairs = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", "http://127.0.0.1:41234/callback"),
        ("scope", "openid email profile mail smtp"),
        ("state", "zustand-1"),
        ("nonce", "nonce-1"),
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
    ];
    for (name, value) in extra {
        pairs.retain(|(known, _)| known != name);
        pairs.push((name, value));
    }
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()
}

/// Asks for the consent page's facts, allows, and returns the code the app is sent.
async fn allow(app: &Router, auth: &(String, String), query: &str) -> String {
    let decision: Value = url::form_urlencoded::parse(query.as_bytes())
        .map(|(name, value)| (name.into_owned(), Value::String(value.into_owned())))
        .chain([("approve".to_owned(), Value::Bool(true))])
        .collect::<serde_json::Map<_, _>>()
        .into();
    let (status, answer) = portal(app, "POST", "/api/oauth/authorize", decision, auth).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let redirect = Url::parse(answer["redirect"].as_str().unwrap()).unwrap();
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.port(), Some(41234), "loopback apps keep the port they asked for");
    let pairs: std::collections::HashMap<String, String> = redirect.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], "zustand-1");
    assert_eq!(pairs["iss"], "https://mail.example.org", "RFC 9207");
    pairs["code"].clone()
}

async fn redeem(app: &Router, client_id: &str, code: &str, verifier: &str, redirect: &str) -> (StatusCode, Value) {
    form(
        app,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", verifier),
        ],
    )
    .await
}

fn claims(id_token: &str, jwks: &Value) -> Value {
    let parts: Vec<&str> = id_token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: Value = serde_json::from_slice(&BASE64URL_NOPAD.decode(parts[0].as_bytes()).unwrap()).unwrap();
    assert_eq!(header["alg"], "ES256");
    let key =
        jwks["keys"].as_array().unwrap().iter().find(|key| key["kid"] == header["kid"]).expect("the key is published");
    let mut point = vec![4u8];
    point.extend(BASE64URL_NOPAD.decode(key["x"].as_str().unwrap().as_bytes()).unwrap());
    point.extend(BASE64URL_NOPAD.decode(key["y"].as_str().unwrap().as_bytes()).unwrap());
    let signature = BASE64URL_NOPAD.decode(parts[2].as_bytes()).unwrap();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("the ID token is signed with the published key");
    serde_json::from_slice(&BASE64URL_NOPAD.decode(parts[1].as_bytes()).unwrap()).unwrap()
}

#[tokio::test]
async fn discovery_and_registration() {
    let (app, _store, _dir) = setup().await;
    for path in ["/.well-known/oauth-authorization-server", "/.well-known/openid-configuration"] {
        let (status, metadata) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(metadata["issuer"], "https://mail.example.org");
        assert_eq!(metadata["token_endpoint"], "https://mail.example.org/oauth/token");
        assert_eq!(metadata["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(metadata["token_endpoint_auth_methods_supported"], json!(["none"]));
        assert_eq!(metadata["id_token_signing_alg_values_supported"], json!(["ES256"]));
    }
    let (status, jwks) = get(&app, "/oauth/jwks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(jwks["keys"][0]["crv"], "P-256");
    assert!(jwks["keys"][0].get("d").is_none(), "only the public key");

    // Always a public client, whatever it asked for; loopback and app schemes are fine.
    let (status, client) = register(&app, json!([REDIRECT, "com.example.mail:/oauth2redirect"])).await;
    assert_eq!(status, StatusCode::CREATED, "{client}");
    assert_eq!(client["token_endpoint_auth_method"], "none");
    assert!(client.get("client_secret").is_none());
    assert!(client["client_id"].as_str().unwrap().starts_with("uwu-"));
    for bad in [
        json!(["http://app.example.com/cb"]),
        json!(["javascript:alert(1)"]),
        json!([]),
        json!(["https://app.example.com/cb#x"]),
    ] {
        let (status, refused) = register(&app, bad.clone()).await;
        assert_eq!(
            (status, refused["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_redirect_uri")),
            "{bad}"
        );
    }
    let body = json!({ "redirect_uris": [REDIRECT], "grant_types": ["client_credentials"] });
    let (status, refused) = send(&app, request("POST", "/oauth/register", "application/json", body.to_string())).await;
    assert_eq!((status, refused["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_client_metadata")));
}

#[tokio::test]
async fn the_code_flow_with_pkce_rotation_and_revocation() {
    let (app, store, _dir) = setup().await;
    let (_, client) = register(&app, json!([REDIRECT])).await;
    let client_id = client["client_id"].as_str().unwrap().to_owned();
    let auth = login(&app).await;

    // Without a login the consent page has nothing to show.
    let query = authorize_query(&client_id, &[]);
    let (status, _) = get(&app, &format!("/api/oauth/authorize?{query}")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, page) = portal(&app, "GET", &format!("/api/oauth/authorize?{query}"), Value::Null, &auth).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["client"]["name"], "Thunderbird");
    assert_eq!(page["client"]["redirectHost"], "127.0.0.1");
    assert_eq!(page["scopes"], json!(["openid", "email", "profile", "mail", "smtp"]));
    assert_eq!(page["consented"], false);

    // Asking not to be asked, before anything was allowed, gets an answer for the app right away.
    let silent = authorize_query(&client_id, &[("prompt", "none")]);
    let (_, answer) = portal(&app, "GET", &format!("/api/oauth/authorize?{silent}"), Value::Null, &auth).await;
    assert!(answer["redirect"].as_str().unwrap().contains("error=consent_required"), "{answer}");

    // The consent is a change: it needs the CSRF token.
    let mut forged =
        request("POST", "/api/oauth/authorize", "application/json", json!({ "approve": true }).to_string());
    forged.headers_mut().insert(header::COOKIE, auth.0.parse().unwrap());
    assert_eq!(send(&app, forged).await.0, StatusCode::FORBIDDEN);

    // PKCE is required; a request without it goes back to the app with an error.
    let plain = authorize_query(&client_id, &[("code_challenge_method", "plain")]);
    let (_, answer) = portal(&app, "GET", &format!("/api/oauth/authorize?{plain}"), Value::Null, &auth).await;
    assert!(answer["redirect"].as_str().unwrap().contains("error=invalid_request"), "{answer}");
    // Somewhere the app did not register is never sent anything.
    let elsewhere = authorize_query(&client_id, &[("redirect_uri", "https://attacker.example.net/cb")]);
    let (status, refused) = portal(&app, "GET", &format!("/api/oauth/authorize?{elsewhere}"), Value::Null, &auth).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("oauthRedirectInvalid")));
    let other_path = authorize_query(&client_id, &[("redirect_uri", "http://127.0.0.1:41234/other")]);
    let (status, _) = portal(&app, "GET", &format!("/api/oauth/authorize?{other_path}"), Value::Null, &auth).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A wrong verifier spends the code; so does using it twice.
    let code = allow(&app, &auth, &query).await;
    let redirect = "http://127.0.0.1:41234/callback";
    let (status, refused) = redeem(&app, &client_id, &code, &"x".repeat(43), redirect).await;
    assert_eq!((status, refused["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));
    let (status, _) = redeem(&app, &client_id, &code, VERIFIER, redirect).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the code was spent by the wrong try");
    // The redirect address has to be the very one the code was made for.
    let code = allow(&app, &auth, &query).await;
    let (status, _) = redeem(&app, &client_id, &code, VERIFIER, "http://127.0.0.1:41235/callback").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // And the app the very one that asked.
    let (_, other) = register(&app, json!([REDIRECT])).await;
    let code = allow(&app, &auth, &query).await;
    let (status, _) = redeem(&app, other["client_id"].as_str().unwrap(), &code, VERIFIER, redirect).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, refused) = redeem(&app, "uwu-unknown", &code, VERIFIER, redirect).await;
    assert_eq!((status, refused["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalid_client")));

    let code = allow(&app, &auth, &query).await;
    let (status, tokens) = redeem(&app, &client_id, &code, VERIFIER, redirect).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3600);
    assert_eq!(tokens["scope"], "openid email profile mail smtp");
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_owned();
    let (_, jwks) = get(&app, "/oauth/jwks").await;
    let id = claims(tokens["id_token"].as_str().unwrap(), &jwks);
    assert_eq!(id["iss"], "https://mail.example.org");
    assert_eq!(id["aud"], json!(client_id));
    assert_eq!(id["nonce"], "nonce-1");
    assert_eq!(id["email"], "mini@example.org");
    assert_eq!(id["name"], "Mini");
    assert!(id["exp"].as_i64().unwrap() > id["iat"].as_i64().unwrap());

    // Only hashes are kept, in the database and its write-ahead log.
    for file in std::fs::read_dir(_dir.path()).unwrap().flatten().filter(|entry| entry.path().is_file()) {
        let dump = std::fs::read(file.path()).unwrap();
        for token in [&access, &refresh] {
            assert!(
                !dump.windows(token.len()).any(|window| window == token.as_bytes()),
                "a token in {:?}",
                file.path()
            );
        }
    }

    let (status, info) = bearer(&app, "/oauth/userinfo", &access).await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!((info["sub"].clone(), info["email"].clone()), (id["sub"].clone(), json!("mini@example.org")));
    assert_eq!(bearer(&app, "/oauth/userinfo", "uwu_at_nothing").await.0, StatusCode::UNAUTHORIZED);

    // Now allowed, the page does not ask again.
    let (_, page) = portal(&app, "GET", &format!("/api/oauth/authorize?{query}"), Value::Null, &auth).await;
    assert_eq!(page["consented"], true);

    // Rotation: the new refresh token works once; the old one coming back ends the whole grant.
    let (status, next) = form(
        &app,
        "/oauth/token",
        &[("grant_type", "refresh_token"), ("client_id", &client_id), ("refresh_token", &refresh)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    let next_access = next["access_token"].as_str().unwrap().to_owned();
    assert_ne!(next["refresh_token"].as_str().unwrap(), refresh);
    assert_eq!(bearer(&app, "/oauth/userinfo", &next_access).await.0, StatusCode::OK);
    let (status, reused) = form(
        &app,
        "/oauth/token",
        &[("grant_type", "refresh_token"), ("client_id", &client_id), ("refresh_token", &refresh)],
    )
    .await;
    assert_eq!((status, reused["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));
    assert_eq!(bearer(&app, "/oauth/userinfo", &next_access).await.0, StatusCode::UNAUTHORIZED, "the grant ended");
    let account = store.account("mini@example.org").await.unwrap().unwrap();
    let events = store.security_events(account.id, 20).await.unwrap();
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    // One entry per sign-in, with what the app may do.
    let granted: Vec<_> = events.iter().filter(|event| event.kind == "oauthGranted").collect();
    assert_eq!(granted.len(), 1, "{kinds:?}");
    assert!(
        granted[0].details["scopes"].as_array().is_some_and(|scopes| !scopes.is_empty()),
        "{:?}",
        granted[0].details
    );

    // The person sees the app under Security and can sign it out.
    let code = allow(&app, &auth, &query).await;
    let (_, tokens) = redeem(&app, &client_id, &code, VERIFIER, redirect).await;
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let (_, security) = portal(&app, "GET", "/api/account/security", Value::Null, &auth).await;
    let grants = security["oauthGrants"].as_array().unwrap();
    assert_eq!(grants.len(), 1, "{security}");
    assert_eq!(grants[0]["clientName"], "Thunderbird");
    let id = grants[0]["id"].as_i64().unwrap();
    let (status, _) = portal(&app, "DELETE", &format!("/api/account/oauth-grants/{id}"), Value::Null, &auth).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(bearer(&app, "/oauth/userinfo", &access).await.0, StatusCode::UNAUTHORIZED);
    let (status, _) = portal(&app, "DELETE", &format!("/api/account/oauth-grants/{id}"), Value::Null, &auth).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The app signs itself out (RFC 7009); unknown tokens are no error.
    let code = allow(&app, &auth, &query).await;
    let (_, tokens) = redeem(&app, &client_id, &code, VERIFIER, redirect).await;
    let refresh = tokens["refresh_token"].as_str().unwrap();
    let (status, _) = form(&app, "/oauth/revoke", &[("client_id", &client_id), ("token", refresh)]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = form(&app, "/oauth/revoke", &[("client_id", &client_id), ("token", "uwu_rt_unknown")]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = form(
        &app,
        "/oauth/token",
        &[("grant_type", "refresh_token"), ("client_id", &client_id), ("refresh_token", refresh)],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Saying no sends the app away empty-handed.
    let decision = json!({ "response_type": "code", "client_id": client_id, "redirect_uri": redirect, "state": "zustand-1",
        "code_challenge": CHALLENGE, "code_challenge_method": "S256", "scope": "mail", "approve": false });
    let (_, answer) = portal(&app, "POST", "/api/oauth/authorize", decision, &auth).await;
    assert!(answer["redirect"].as_str().unwrap().contains("error=access_denied"), "{answer}");
}

#[tokio::test]
async fn refused_codes_and_tokens_are_counted_per_network() {
    let (app, _store, _dir) = setup().await;
    let (_, client) = register(&app, json!([REDIRECT])).await;
    let client_id = client["client_id"].as_str().unwrap();
    for _ in 0..30 {
        let (status, _) = redeem(&app, client_id, "uwu_ac_guess", VERIFIER, REDIRECT).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, refused) = redeem(&app, client_id, "uwu_ac_guess", VERIFIER, REDIRECT).await;
    assert_eq!((status, refused["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("temporarily_unavailable")));
    // Logging in to the portal from the same network is another matter.
    login(&app).await;
}
