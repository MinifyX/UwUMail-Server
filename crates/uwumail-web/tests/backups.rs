//! Backups through the portal API: settings, secrets that stay on the server, and the recovery key.

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
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let smtp = Smtp::new(store.clone(), settings).unwrap();
    let web = Web::new(
        smtp,
        WebSettings {
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            config: None,
            certificate: None,
        },
    );
    web.set_backups(uwumail_backup::Backups::new(store.clone(), "mail.example.de", "0.1.0"));
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

#[tokio::test]
async fn backup_settings_keep_their_secrets_on_the_server() {
    let (app, _store, _dir) = portal().await;
    let body = json!({ "login": "nyu@example.de", "password": "katzenpfote-123" });
    let (_, login) = call(&app, "POST", "/api/auth/login", Some(body), None).await;
    let auth = (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned());

    let (status, empty) = call(&app, "GET", "/api/admin/backups", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!((empty["enabled"].as_bool(), &empty["target"]), (Some(false), &Value::Null));

    let settings = json!({
        "enabled": true, "hour": 2, "retention": { "daily": 7, "weekly": 4, "monthly": 6 }, "encrypted": true,
        "target": { "host": "nas.example.de", "port": 22, "user": "backup", "path": "/volume1/uwumail", "method": "key" },
    });
    let (status, saved) = call(&app, "PUT", "/api/admin/backups", Some(settings.clone()), Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let key = saved["recoveryKey"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 64);
    let public = saved["target"]["publicKey"].as_str().unwrap();
    assert!(public.starts_with("ssh-ed25519 "), "{public}");
    assert!(!saved.to_string().contains("PRIVATE KEY"));

    let (_, again) = call(&app, "PUT", "/api/admin/backups", Some(settings), Some(&auth)).await;
    assert_eq!(again["recoveryKey"], Value::Null, "the key is shown once");
    assert_eq!(again["target"]["publicKey"].as_str(), Some(public), "and the SSH key stays");

    let with_password = json!({
        "enabled": true, "hour": 2, "retention": { "daily": 7, "weekly": 4, "monthly": 6 }, "encrypted": true,
        "target": { "host": "nas.example.de", "port": 22, "user": "backup", "path": "/volume1/uwumail",
                    "method": "password", "password": "Synology-geheim" },
    });
    let (_, changed) = call(&app, "PUT", "/api/admin/backups", Some(with_password), Some(&auth)).await;
    assert_eq!(
        (changed["target"]["method"].as_str(), changed["target"]["passwordSet"].as_bool()),
        (Some("password"), Some(true))
    );
    assert!(!changed.to_string().contains("Synology-geheim"));

    let (status, shown) = call(&app, "POST", "/api/admin/backups/recovery-key", Some(json!({})), Some(&auth)).await;
    assert_eq!(
        (status, shown["recoveryKey"].as_str()),
        (StatusCode::OK, Some(key.as_str())),
        "a fresh login needs no password"
    );
    let (status, _) = call(&app, "POST", "/api/admin/backups/run", Some(json!({})), Some(&auth)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}
