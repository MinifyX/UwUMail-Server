//! The portal API end to end: login, session cookie, CSRF, preferences, admin rights, logout.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

fn smtp(store: Store) -> Smtp {
    let settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    Smtp::new(store, settings).unwrap()
}

async fn setup() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    for (address, role) in [("nyu@example.de", Role::Admin), ("leni@example.de", Role::User)] {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: address.split('@').next().unwrap().into(),
                password: Some("katzenpfote-123".into()),
                role,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let web = Web::new(
        smtp(store),
        WebSettings {
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            config: None,
            certificate: None,
            webmail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    (web.router(), dir)
}

struct Call<'a> {
    method: &'a str,
    path: &'a str,
    body: Option<Value>,
    cookie: Option<&'a str>,
    csrf: Option<&'a str>,
    https: bool,
}

impl<'a> Call<'a> {
    fn get(path: &'a str) -> Self {
        Call { method: "GET", path, body: None, cookie: None, csrf: None, https: true }
    }

    fn send(method: &'a str, path: &'a str, body: Value) -> Self {
        Call { method, path, body: Some(body), ..Call::get(path) }
    }
}

async fn call(app: &Router, call: Call<'_>) -> (StatusCode, Response<Body>, Value) {
    let mut request = Request::builder().method(call.method).uri(call.path);
    if let Some(cookie) = call.cookie {
        request = request.header(header::COOKIE, cookie);
    }
    if let Some(csrf) = call.csrf {
        request = request.header(CSRF_HEADER, csrf);
    }
    let body = match call.body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request.extensions_mut().insert(ClientInfo { https: call.https, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, Response::from_parts(parts, Body::empty()), json)
}

/// Logs in and returns the cookie pair and the CSRF token.
async fn login(app: &Router, login: &str) -> (String, String) {
    let (status, response, body) =
        call(app, Call::send("POST", "/api/auth/login", json!({ "login": login, "password": "katzenpfote-123" })))
            .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().to_owned();
    assert!(set_cookie.starts_with("__Host-uwumail="), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    (cookie, body["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn login_session_and_logout() {
    let (app, _dir) = setup().await;

    let (status, _, info) = call(&app, Call::get("/api/info")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info, json!({ "hostname": "mail.example.de", "setupRequired": false }));

    let (status, _, body) = call(&app, Call::get("/api/session")).await;
    assert_eq!((status, body), (StatusCode::OK, Value::Null), "not logged in is a normal answer");
    let (status, _, body) = call(&app, Call::get("/api/account")).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("notLoggedIn")));

    let wrong = json!({ "login": "nyu@example.de", "password": "falsch" });
    let (status, _, body) = call(&app, Call::send("POST", "/api/auth/login", wrong)).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalidCredentials")));

    let (cookie, csrf) = login(&app, "Nyu@Example.de").await;
    let (status, _, session) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/session") }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["account"]["login"], "nyu@example.de");
    assert_eq!(session["account"]["role"], "admin");
    assert_eq!(session["csrfToken"], csrf.as_str());

    let (status, _, profile) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/account") }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profile["addresses"], json!(["nyu@example.de"]));

    // Logging out needs the CSRF token, then the cookie is gone for good.
    let (status, _, _) =
        call(&app, Call { cookie: Some(&cookie), ..Call::send("POST", "/api/auth/logout", json!({})) }).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, response, _) = call(
        &app,
        Call { cookie: Some(&cookie), csrf: Some(&csrf), ..Call::send("POST", "/api/auth/logout", json!({})) },
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(response.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    let (status, _, body) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/session") }).await;
    assert_eq!((status, body), (StatusCode::OK, Value::Null));
}

#[tokio::test]
async fn preferences_need_csrf_and_valid_values() {
    let (app, _dir) = setup().await;
    let (cookie, csrf) = login(&app, "leni@example.de").await;
    let change = json!({ "mode": "pro", "language": "de" });

    let (status, _, body) = call(
        &app,
        Call {
            cookie: Some(&cookie),
            csrf: Some("wrong"),
            ..Call::send("PATCH", "/api/account/preferences", change.clone())
        },
    )
    .await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("csrfMismatch")));

    let (status, _, body) = call(
        &app,
        Call { cookie: Some(&cookie), csrf: Some(&csrf), ..Call::send("PATCH", "/api/account/preferences", change) },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "mode": "pro", "language": "de" }));

    for invalid in [json!({ "mode": "expert" }), json!({ "colour": "pink" })] {
        let (status, _, _) = call(
            &app,
            Call {
                cookie: Some(&cookie),
                csrf: Some(&csrf),
                ..Call::send("PATCH", "/api/account/preferences", invalid)
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    let (_, _, session) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/session") }).await;
    assert_eq!(session["preferences"]["mode"], "pro");
}

#[tokio::test]
async fn only_admins_see_the_server_area() {
    let (app, _dir) = setup().await;
    let (leni, _) = login(&app, "leni@example.de").await;
    let (status, _, body) = call(&app, Call { cookie: Some(&leni), ..Call::get("/api/admin/overview") }).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));

    let (nyu, _) = login(&app, "nyu@example.de").await;
    let (status, _, body) = call(&app, Call { cookie: Some(&nyu), ..Call::get("/api/admin/overview") }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["counts"]["accounts"], 2);
    assert_eq!(body["counts"]["admins"], 1);
    assert_eq!(body["server"]["hostname"], "mail.example.de");

    let (status, _, body) = call(&app, Call::get("/api/nothing/here")).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("notFound")));
}

#[tokio::test]
async fn plain_http_gets_a_plain_cookie_and_logins_are_throttled() {
    let (app, _dir) = setup().await;
    let body = json!({ "login": "leni@example.de", "password": "katzenpfote-123" });
    let (status, response, _) = call(&app, Call { https: false, ..Call::send("POST", "/api/auth/login", body) }).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("uwumail=") && !cookie.contains("Secure"));

    let wrong = json!({ "login": "leni@example.de", "password": "falsch" });
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        (last, _, _) = call(&app, Call::send("POST", "/api/auth/login", wrong.clone())).await;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}
