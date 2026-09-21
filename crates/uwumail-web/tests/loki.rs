//! Sending the log to Loki through the portal API: the status, and a test line that tries the
//! changes before they are saved.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::loki::LokiTarget;
use uwumail_web::settings::{SETTINGS, SettingSource, SettingValue, SettingsBackend, get_path};
use uwumail_web::{CSRF_HEADER, Loki, LokiConfig, Web, WebSettings};

/// Takes the overlay as the whole configuration, the way the server merges it underneath its file.
struct FakeServer;

impl SettingsBackend for FakeServer {
    fn view(&self, overlay: &Value) -> Result<Vec<SettingValue>, String> {
        Ok(SETTINGS
            .iter()
            .map(|spec| {
                let value = get_path(overlay, spec.key).cloned().unwrap_or(Value::Null);
                let source = if value.is_null() { SettingSource::Default } else { SettingSource::Database };
                SettingValue { key: spec.key, set: !value.is_null(), value, source }
            })
            .collect())
    }

    fn apply(&self, overlay: &Value) -> Result<(), String> {
        self.config(overlay)?.target("mail.example.de").map(|_| ())
    }

    fn config_file(&self) -> Option<String> {
        None
    }

    fn loki_connection(&self, overlay: &Value) -> Result<LokiTarget, String> {
        self.config(overlay)?.connection("mail.example.de")
    }
}

impl FakeServer {
    fn config(&self, overlay: &Value) -> Result<LokiConfig, String> {
        let loki = get_path(overlay, "log.loki").cloned().unwrap_or_else(|| json!({}));
        serde_json::from_value(loki).map_err(|err| err.to_string())
    }
}

async fn portal(dir: &std::path::Path) -> (Router, (String, String), Store) {
    let store = Store::open(dir).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    store
        .create_account(NewAccount {
            address: "nyu@example.de".into(),
            display_name: "Nyu".into(),
            password: Some("katzenpfote-123".into()),
            role: Role::Admin,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    let smtp_settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let web = Web::new(
        Smtp::new(store.clone(), smtp_settings).unwrap(),
        WebSettings {
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            loki: Some(Loki::new()),
            config: Some(Arc::new(FakeServer)),
            certificate: None,
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    let app = web.router();
    let mut login = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": "nyu@example.de", "password": "katzenpfote-123" }).to_string()))
        .unwrap();
    login.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(login).await.unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let csrf = serde_json::from_slice::<Value>(&bytes).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    (app, (cookie, csrf), store)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: &(String, String),
) -> (StatusCode, Value) {
    let mut request =
        Request::builder().method(method).uri(path).header(header::COOKIE, &auth.0).header(CSRF_HEADER, &auth.1);
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A Loki that takes one push and hands over what it got.
async fn one_push() -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let got = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 8192];
        while !String::from_utf8_lossy(&request).contains("\"streams\"") || !request.ends_with(b"}") {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        socket.write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n").await.unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (url, got)
}

#[tokio::test]
async fn a_test_line_tries_the_changes_without_saving_them() {
    let dir = tempfile::tempdir().unwrap();
    let (app, auth, store) = portal(dir.path()).await;

    let (status, loki) = call(&app, "GET", "/api/admin/logs/loki", None, &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((loki["enabled"].clone(), loki["sent"].clone()), (json!(false), json!(0)));

    let (url, got) = one_push().await;
    let changes = json!({ "changes": { "log.loki.url": url, "log.loki.token": "t0ken" } });
    let (status, body) = call(&app, "POST", "/api/admin/logs/loki/test", Some(changes), &auth).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let request = got.await.unwrap();
    assert!(request.starts_with("POST /loki/api/v1/push "), "{request}");
    assert!(request.to_ascii_lowercase().contains("authorization: bearer t0ken"));
    assert!(request.contains("a test line from the UwUMail admin panel"));
    assert!(store.setting("config.overlay").await.unwrap().is_none(), "trying saves nothing");

    let (status, body) = call(
        &app,
        "POST",
        "/api/admin/logs/loki/test",
        Some(json!({ "changes": { "log.loki.url": "loki.example.net" } })),
        &auth,
    )
    .await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("lokiInvalid")));

    // Nothing listens here any more.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let (status, body) = call(
        &app,
        "POST",
        "/api/admin/logs/loki/test",
        Some(json!({ "changes": { "log.loki.url": format!("http://{closed}") } })),
        &auth,
    )
    .await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("lokiUnreachable")));
}

#[tokio::test]
async fn switching_on_needs_the_privacy_consent() {
    let dir = tempfile::tempdir().unwrap();
    let (app, auth, _store) = portal(dir.path()).await;
    let on = json!({ "log.loki.enabled": true, "log.loki.url": "http://loki.example.net:3100" });
    let (status, body) = call(&app, "PATCH", "/api/admin/settings", Some(json!({ "changes": on })), &auth).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("settingsInvalid")));
    assert!(body["detail"].as_str().unwrap().contains("privacy_consent"), "{body}");

    let mut agreed = on.clone();
    agreed["log.loki.privacy_consent"] = json!(true);
    let (status, body) = call(&app, "PATCH", "/api/admin/settings", Some(json!({ "changes": agreed })), &auth).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, log) = call(&app, "GET", "/api/admin/audit", None, &auth).await;
    assert_eq!(log[0]["details"]["log.loki.privacy_consent"], true, "who agreed is in the change log");
}
