//! Shared folders in My account: sharing with people on the server and what others share.

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

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<(&str, &str)>,
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
    let cookie =
        response.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let (Some(cookie), Value::Object(map)) = (cookie, &mut json) {
        map.insert("_cookie".into(), cookie.into());
    }
    (status, json)
}

async fn login(app: &Router, address: &str) -> (String, String) {
    let (status, login) =
        call(app, "POST", "/api/auth/login", Some(json!({ "login": address, "password": "katzenpfote-123" })), None)
            .await;
    assert_eq!(status, StatusCode::OK, "{login}");
    (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn folders_are_shared_from_my_account() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for (address, name) in [("leni@example.org", "Leni"), ("ami@example.org", "Ami")] {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: name.into(),
                password: Some("katzenpfote-123".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
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
            webmail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    let app = web.router();
    let leni = login(&app, "leni@example.org").await;
    let ami = login(&app, "ami@example.org").await;

    let (status, view) = call(&app, "GET", "/api/account/sharing", None, Some((&leni.0, &leni.1))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["people"], json!([{ "login": "ami@example.org", "name": "Ami" }]), "everyone but oneself");
    let inbox = view["folders"].as_array().unwrap().iter().find(|f| f["role"] == "inbox").unwrap()["id"].clone();

    // Without the CSRF token nothing changes.
    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/account/sharing/{inbox}"),
        Some(json!({ "login": "ami@example.org", "level": "read" })),
        Some((&leni.0, "wrong")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, view) = call(
        &app,
        "PUT",
        &format!("/api/account/sharing/{inbox}"),
        Some(json!({ "login": "ami@example.org", "level": "write" })),
        Some((&leni.0, &leni.1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let folder = view["folders"].as_array().unwrap().iter().find(|f| f["id"] == inbox).unwrap();
    assert_eq!(folder["shares"][0]["login"], "ami@example.org");
    assert_eq!(folder["shares"][0]["level"], "write");

    let (_, bad) = call(
        &app,
        "PUT",
        &format!("/api/account/sharing/{inbox}"),
        Some(json!({ "login": "ami@example.org", "level": "everything" })),
        Some((&leni.0, &leni.1)),
    )
    .await;
    assert_eq!(bad["code"], "invalid", "{bad}");

    // Ami sees it; Ami cannot share Leni's folder on.
    let (_, view) = call(&app, "GET", "/api/account/sharing", None, Some((&ami.0, &ami.1))).await;
    assert_eq!(view["sharedWithMe"][0]["owner"], "leni@example.org");
    assert_eq!(view["sharedWithMe"][0]["path"], "Inbox");
    assert_eq!(view["sharedWithMe"][0]["level"], "write");
    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/account/sharing/{inbox}"),
        Some(json!({ "login": "leni@example.org", "level": "read" })),
        Some((&ami.0, &ami.1)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, view) =
        call(&app, "DELETE", &format!("/api/account/sharing/{inbox}/ami@example.org"), None, Some((&leni.0, &leni.1)))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert!(view["folders"].as_array().unwrap().iter().all(|f| f["shares"].as_array().unwrap().is_empty()));
    let (_, view) = call(&app, "GET", "/api/account/sharing", None, Some((&ami.0, &ami.1))).await;
    assert_eq!(view["sharedWithMe"], json!([]));
}
