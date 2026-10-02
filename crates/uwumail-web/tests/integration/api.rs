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
        hostname: "mail.example.org".into(),
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
    store.create_domain("example.org").await.unwrap();
    for (address, role) in [("nyu@example.org", Role::Admin), ("leni@example.org", Role::User)] {
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
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
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
    assert_eq!(
        info,
        json!({
            "hostname": "mail.example.org",
            "setupRequired": false,
            "brand": { "name": "UwUMail", "custom": false, "color": null, "mascot": true, "logo": null },
            // No button for logging in elsewhere unless it is set up.
            "oidc": null,
        })
    );

    let (status, _, body) = call(&app, Call::get("/api/session")).await;
    assert_eq!((status, body), (StatusCode::OK, Value::Null), "not logged in is a normal answer");
    let (status, _, body) = call(&app, Call::get("/api/account")).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("notLoggedIn")));

    let wrong = json!({ "login": "nyu@example.org", "password": "falsch" });
    let (status, _, body) = call(&app, Call::send("POST", "/api/auth/login", wrong)).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalidCredentials")));

    let (cookie, csrf) = login(&app, "Nyu@Example.org").await;
    let (status, _, session) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/session") }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["account"]["login"], "nyu@example.org");
    assert_eq!(session["account"]["role"], "admin");
    assert_eq!(session["csrfToken"], csrf.as_str());

    let (status, _, profile) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/account") }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profile["addresses"], json!(["nyu@example.org"]));

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
    // The webmail's local data, its push worker and cached attachments go with the login.
    assert_eq!(response.headers()["clear-site-data"], "\"cache\", \"storage\"");
    let (status, _, body) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/session") }).await;
    assert_eq!((status, body), (StatusCode::OK, Value::Null));
}

#[tokio::test]
async fn ending_the_own_session_clears_the_browser_like_a_logout() {
    let (app, _dir) = setup().await;
    let (cookie, csrf) = login(&app, "leni@example.org").await;
    let (other_cookie, _) = login(&app, "leni@example.org").await;
    let (_, _, security) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/account/security") }).await;
    let id_of = |current: bool| {
        security["sessions"].as_array().unwrap().iter().find(|s| s["current"] == current).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (own, other) = (id_of(true), id_of(false));

    // Ending another browser's login leaves this one's data alone.
    let path = format!("/api/account/sessions/{other}");
    let (status, response, _) =
        call(&app, Call { cookie: Some(&cookie), csrf: Some(&csrf), ..Call::send("DELETE", &path, json!({})) }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(response.headers().get("clear-site-data").is_none());
    let (_, _, body) = call(&app, Call { cookie: Some(&other_cookie), ..Call::get("/api/session") }).await;
    assert_eq!(body, Value::Null);

    let path = format!("/api/account/sessions/{own}");
    let (status, response, _) =
        call(&app, Call { cookie: Some(&cookie), csrf: Some(&csrf), ..Call::send("DELETE", &path, json!({})) }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["clear-site-data"], "\"cache\", \"storage\"");
    assert!(response.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
}

#[tokio::test]
async fn preferences_need_csrf_and_valid_values() {
    let (app, _dir) = setup().await;
    let (cookie, csrf) = login(&app, "leni@example.org").await;
    let change = json!({ "mode": "pro", "language": "de", "mailConversations": "off" });

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
    assert_eq!(body, json!({ "mode": "pro", "language": "de", "mailConversations": "off" }));

    for invalid in [json!({ "mode": "expert" }), json!({ "colour": "pink" }), json!({ "mailConversations": true })] {
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
    let (leni, _) = login(&app, "leni@example.org").await;
    let (status, _, body) = call(&app, Call { cookie: Some(&leni), ..Call::get("/api/admin/overview") }).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));

    let (nyu, _) = login(&app, "nyu@example.org").await;
    let (status, _, body) = call(&app, Call { cookie: Some(&nyu), ..Call::get("/api/admin/overview") }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["counts"]["accounts"], 2);
    assert_eq!(body["counts"]["admins"], 1);
    assert_eq!(body["server"]["hostname"], "mail.example.org");

    let (status, _, body) = call(&app, Call::get("/api/nothing/here")).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("notFound")));
}

#[tokio::test]
async fn plain_http_gets_a_plain_cookie_and_logins_are_throttled() {
    let (app, _dir) = setup().await;
    let body = json!({ "login": "leni@example.org", "password": "katzenpfote-123" });
    let (status, response, _) = call(&app, Call { https: false, ..Call::send("POST", "/api/auth/login", body) }).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("uwumail=") && !cookie.contains("Secure"));

    let wrong = json!({ "login": "leni@example.org", "password": "falsch" });
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        (last, _, _) = call(&app, Call::send("POST", "/api/auth/login", wrong.clone())).await;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn logging_into_ones_own_account_does_not_reset_the_guesses_at_another() {
    // security-audit-0.8.0 W-1: a success used to clear its whole network's failures.
    let (app, _dir) = setup().await;
    let wrong = json!({ "login": "nyu@example.org", "password": "falsch" });
    let own = json!({ "login": "leni@example.org", "password": "katzenpfote-123" });
    for _ in 0..9 {
        let (status, _, _) = call(&app, Call::send("POST", "/api/auth/login", wrong.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = call(&app, Call::send("POST", "/api/auth/login", own.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, Call::send("POST", "/api/auth/login", wrong.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the tenth guess");
    let (status, _, _) = call(&app, Call::send("POST", "/api/auth/login", own)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "and the network waits, whoever it is");
}

#[tokio::test]
async fn signatures_per_domain_and_the_company_signature() {
    let (app, _dir) = setup().await;
    let (leni, leni_csrf) = login(&app, "leni@example.org").await;
    let (nyu, nyu_csrf) = login(&app, "nyu@example.org").await;
    let put =
        |cookie, csrf, path, body| Call { cookie: Some(cookie), csrf: Some(csrf), ..Call::send("PUT", path, body) };

    let (status, _, overview) = call(&app, Call { cookie: Some(&leni), ..Call::get("/api/account/signatures") }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview["domains"][0]["domain"], "example.org");
    assert_eq!(overview["domains"][0]["addressCount"], 1);
    assert_eq!(overview["limits"]["placeholders"][0], "name");
    let identity = overview["identities"][0]["id"].as_i64().unwrap();

    let body = json!({ "domains": { "example.org": { "text": "{name}, {domain}", "html": "<p>{name}</p>" } } });
    let (status, _, overview) = call(&app, put(&leni, &leni_csrf, "/api/account/signatures", body)).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["identities"][0]["effective"]["text"], "leni, example.org");
    assert_eq!(overview["identities"][0]["source"], "domain");
    // The old identity endpoint shows the effective signature too.
    let (_, _, list) = call(&app, Call { cookie: Some(&leni), ..Call::get("/api/account/identities") }).await;
    assert_eq!(list[0]["textSignature"], "leni, example.org");

    // Only her own domains and identities; nothing unknown; no CSRF, no change.
    let nyu_identity = {
        let (_, _, list) = call(&app, Call { cookie: Some(&nyu), ..Call::get("/api/account/signatures") }).await;
        list["identities"][0]["id"].as_i64().unwrap()
    };
    for (body, expected) in [
        (json!({ "domains": { "example.com": { "text": "x" } } }), StatusCode::UNPROCESSABLE_ENTITY),
        (json!({ "identities": { nyu_identity.to_string(): { "text": "x" } } }), StatusCode::NOT_FOUND),
        (json!({ "identities": { "abc": null } }), StatusCode::UNPROCESSABLE_ENTITY),
        (json!({ "domains": { "example.org": { "colour": "red" } } }), StatusCode::UNPROCESSABLE_ENTITY),
        (json!({ "domains": { "example.org": { "text": "x".repeat(300 * 1024) } } }), StatusCode::UNPROCESSABLE_ENTITY),
        (json!({ "other": 1 }), StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (status, _, _) = call(&app, put(&leni, &leni_csrf, "/api/account/signatures", body.clone())).await;
        assert!(status == expected || status == StatusCode::BAD_REQUEST, "{body}: {status}");
    }
    let (status, _, _) = call(
        &app,
        Call { cookie: Some(&leni), ..Call::send("PUT", "/api/account/signatures", json!({ "domains": {} })) },
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "without the CSRF token");
    let (_, _, overview) = call(&app, Call { cookie: Some(&nyu), ..Call::get("/api/account/signatures") }).await;
    assert!(overview["identities"][0]["signature"].is_null(), "Nyu's identity is untouched");

    // An address of her own, then back to the domain's.
    let body = json!({ "identities": { identity.to_string(): { "text": "Nur hier" } } });
    let (_, _, overview) = call(&app, put(&leni, &leni_csrf, "/api/account/signatures", body)).await;
    assert_eq!(overview["identities"][0]["effective"]["text"], "Nur hier");
    let body = json!({ "identities": { identity.to_string(): null } });
    let (_, _, overview) = call(&app, put(&leni, &leni_csrf, "/api/account/signatures", body)).await;
    assert_eq!(overview["identities"][0]["source"], "domain");

    // The company signature is the admin's: Leni may not, Nyu may.
    let path = "/api/admin/domains/example.org/signature";
    let template = json!({ "mode": "template", "text": "Beispiel AG, {name}", "html": "" });
    let (status, _, _) = call(&app, put(&leni, &leni_csrf, path, template.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(&app, Call { cookie: Some(&leni), ..Call::get(path) }).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, saved) = call(&app, put(&nyu, &nyu_csrf, path, template)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["mode"], "template");
    let (_, _, detail) = call(&app, Call { cookie: Some(&nyu), ..Call::get("/api/admin/domains/example.org") }).await;
    assert_eq!(detail["signature"]["text"], "Beispiel AG, {name}");
    for bad in
        [json!({ "mode": "sometimes" }), json!({ "mode": "footer", "text": " " }), json!({ "mode": "off", "x": 1 })]
    {
        let (status, _, _) = call(&app, put(&nyu, &nyu_csrf, path, bad.clone())).await;
        assert!(status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST, "{bad}: {status}");
    }
    let (status, _, _) =
        call(&app, put(&nyu, &nyu_csrf, "/api/admin/domains/unknown.example/signature", json!({ "mode": "off" })))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Without a signature of her own, Leni gets the template.
    let body = json!({ "domains": { "example.org": null } });
    let (_, _, overview) = call(&app, put(&leni, &leni_csrf, "/api/account/signatures", body)).await;
    assert_eq!(overview["identities"][0]["effective"]["text"], "Beispiel AG, leni");
    assert_eq!(overview["domains"][0]["company"]["mode"], "template");
}

#[tokio::test]
async fn signatures_and_the_undo_window_are_set_in_my_account() {
    let (app, _dir) = setup().await;
    let (cookie, csrf) = login(&app, "leni@example.org").await;

    let (status, _, list) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/account/identities") }).await;
    assert_eq!(status, StatusCode::OK);
    let identity = &list[0];
    assert_eq!(identity["email"], "leni@example.org");
    assert_eq!(identity["textSignature"], "");
    let path = format!("/api/account/identities/{}", identity["id"]);

    let change = json!({ "textSignature": "Leni\nexample.org", "htmlSignature": "<p>Leni</p>" });
    let (status, _, _) =
        call(&app, Call { cookie: Some(&cookie), csrf: Some(&csrf), ..Call::send("PATCH", &path, change) }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, list) = call(&app, Call { cookie: Some(&cookie), ..Call::get("/api/account/identities") }).await;
    assert_eq!(list[0]["textSignature"], "Leni\nexample.org");
    assert_eq!(list[0]["htmlSignature"], "<p>Leni</p>");

    // Only signatures and the name; far too long is refused; another id is not found.
    for (body, expected) in [
        (json!({ "email": "boss@example.org" }), StatusCode::UNPROCESSABLE_ENTITY),
        (json!({ "textSignature": "x".repeat(300 * 1024) }), StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (status, _, _) =
            call(&app, Call { cookie: Some(&cookie), csrf: Some(&csrf), ..Call::send("PATCH", &path, body) }).await;
        assert!(status == expected || status == StatusCode::BAD_REQUEST, "{status}");
    }
    let (status, _, _) = call(
        &app,
        Call {
            cookie: Some(&cookie),
            csrf: Some(&csrf),
            ..Call::send("PATCH", "/api/account/identities/999999", json!({ "name": "x" }))
        },
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The undo window is a preference with five choices.
    let (status, _, body) = call(
        &app,
        Call {
            cookie: Some(&cookie),
            csrf: Some(&csrf),
            ..Call::send("PATCH", "/api/account/preferences", json!({ "mailUndoSend": "20" }))
        },
    )
    .await;
    assert_eq!((status, &body["mailUndoSend"]), (StatusCode::OK, &json!("20")));
    let (status, _, _) = call(
        &app,
        Call {
            cookie: Some(&cookie),
            csrf: Some(&csrf),
            ..Call::send("PATCH", "/api/account/preferences", json!({ "mailUndoSend": "15" }))
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
