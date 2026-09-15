//! Domains through the portal API: add, catch-all, DKIM rotation, remove.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

async fn portal() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    store
        .create_account(NewAccount {
            address: "nyu@example.de".into(),
            display_name: "Nyu".into(),
            password: Some("katzenpfote-123".into()),
            role: Role::Admin,
            quota_bytes: 0,
        })
        .await
        .unwrap();
    let settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let smtp = Smtp::new(store.clone(), settings).unwrap();
    let web = Web::new(
        smtp,
        WebSettings { hostname: "mail.example.de".into(), started: Instant::now(), logs: None, config: None },
    );
    (web.router(), store, dir)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<&(String, String)>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some((cookie, csrf)) = auth {
        request = request.header(header::COOKIE, cookie).header(CSRF_HEADER, csrf);
    }
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let (Some(cookie), Value::Object(map)) = (cookie, &mut json) {
        map.insert("_cookie".into(), cookie.split(';').next().unwrap().into());
    }
    (status, json)
}

fn states(domain: &Value) -> Vec<(String, String)> {
    let mut states: Vec<_> = domain["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| (key["selector"].as_str().unwrap().to_owned(), key["state"].as_str().unwrap().to_owned()))
        .collect();
    states.sort();
    states
}

#[tokio::test]
async fn domains_with_catch_all_and_key_rotation() {
    let (app, store, _dir) = portal().await;
    let (_, login) = call(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({ "login": "nyu@example.de", "password": "katzenpfote-123" })),
        None,
    )
    .await;
    let auth = (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned());

    let (status, created) =
        call(&app, "POST", "/api/admin/domains", Some(json!({ "name": "Verein.DE" })), Some(&auth)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "verein.de");
    let first = states(&created);
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|(_, state)| state == "active"));
    assert!(created["keys"][0]["dnsName"].as_str().unwrap().ends_with("._domainkey.verein.de"));
    assert_eq!(created["setup"]["hostname"], "mail.example.de");

    let (status, _) = call(&app, "POST", "/api/admin/domains", Some(json!({ "name": "verein.de" })), Some(&auth)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (_, list) = call(&app, "GET", "/api/admin/domains", None, Some(&auth)).await;
    let names: Vec<_> = list.as_array().unwrap().iter().map(|d| (d["name"].clone(), d["people"].clone())).collect();
    assert_eq!(names, vec![(json!("example.de"), json!(1)), (json!("verein.de"), json!(0))]);

    // Catch-all to a person, then off again.
    let (status, domain) = call(
        &app,
        "PUT",
        "/api/admin/domains/verein.de/catch-all",
        Some(json!({ "login": "nyu@example.de" })),
        Some(&auth),
    )
    .await;
    assert_eq!((status, domain["catchAll"].as_str()), (StatusCode::OK, Some("nyu@example.de")));
    assert!(store.resolve_recipient("irgendwer@verein.de").await.unwrap().is_some());
    let (_, domain) =
        call(&app, "PUT", "/api/admin/domains/verein.de/catch-all", Some(json!({ "login": null })), Some(&auth)).await;
    assert_eq!(domain["catchAll"], Value::Null);

    // Rotation: new keys wait, switch over (forced, no DNS in tests), the old ones retire and can go.
    let (_, rotating) =
        call(&app, "POST", "/api/admin/domains/verein.de/dkim/rotate", Some(json!({})), Some(&auth)).await;
    let pending: Vec<_> = states(&rotating).into_iter().filter(|(_, state)| state == "pending").collect();
    assert_eq!(pending.len(), 2, "{rotating}");
    let (_, again) = call(&app, "POST", "/api/admin/domains/verein.de/dkim/rotate", Some(json!({})), Some(&auth)).await;
    assert_eq!(states(&again).len(), 4, "preparing twice keeps the same new keys");

    let (status, switched) =
        call(&app, "POST", "/api/admin/domains/verein.de/dkim/activate", Some(json!({ "force": true })), Some(&auth))
            .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    let after = states(&switched);
    assert_eq!(after.iter().filter(|(_, state)| state == "active").count(), 2);
    let retired: Vec<_> =
        after.iter().filter(|(_, state)| state == "retired").map(|(selector, _)| selector.clone()).collect();
    assert_eq!(retired.len(), 2);
    let active = after.iter().find(|(_, state)| state == "active").unwrap().0.clone();
    let (status, body) =
        call(&app, "DELETE", &format!("/api/admin/domains/verein.de/dkim/{active}"), None, Some(&auth)).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("keyActive")));
    let (status, trimmed) =
        call(&app, "DELETE", &format!("/api/admin/domains/verein.de/dkim/{}", retired[0]), None, Some(&auth)).await;
    assert_eq!((status, states(&trimmed).len()), (StatusCode::OK, 3));

    // Domains with addresses stay.
    let (status, body) = call(&app, "DELETE", "/api/admin/domains/example.de", None, Some(&auth)).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("domainInUse")));
    let (status, _) = call(&app, "DELETE", "/api/admin/domains/verein.de", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, log) = call(&app, "GET", "/api/admin/audit?limit=20", None, Some(&auth)).await;
    let actions: Vec<_> = log.as_array().unwrap().iter().map(|r| r["action"].as_str().unwrap().to_owned()).collect();
    assert_eq!(
        actions,
        [
            "domain.remove",
            "domain.dkimRemove",
            "domain.dkimActivate",
            "domain.dkimPrepare",
            "domain.dkimPrepare",
            "domain.catchAll",
            "domain.catchAll",
            "domain.create"
        ]
    );
}
