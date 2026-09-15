//! Server settings through the portal API, with a stand-in for the server's config handling.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::settings::{SETTINGS, SettingSource, SettingValue, SettingsBackend, get_path};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

/// Pretends the config file sets `tone.language`; remembers what was applied.
#[derive(Default)]
struct FakeServer {
    applied: Mutex<Option<Value>>,
}

impl SettingsBackend for FakeServer {
    fn view(&self, overlay: &Value) -> Result<Vec<SettingValue>, String> {
        Ok(SETTINGS
            .iter()
            .map(|spec| {
                let (value, source) = if spec.key == "tone.language" {
                    (json!("de"), SettingSource::File)
                } else if let Some(value) = get_path(overlay, spec.key) {
                    (value.clone(), SettingSource::Database)
                } else {
                    (Value::Null, SettingSource::Default)
                };
                let secret = spec.key.ends_with("password");
                SettingValue {
                    key: spec.key,
                    set: !value.is_null(),
                    value: if secret { Value::Null } else { value },
                    source,
                }
            })
            .collect())
    }

    fn apply(&self, overlay: &Value) -> Result<(), String> {
        if get_path(overlay, "smtp.trusted_relays").is_some_and(|relays| relays.to_string().contains("nonsense")) {
            return Err("smtp.trusted_relays: nonsense is not a network".into());
        }
        *self.applied.lock().unwrap() = Some(overlay.clone());
        Ok(())
    }

    fn config_file(&self) -> Option<String> {
        Some("/etc/uwumail/uwumail.toml".into())
    }
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

#[tokio::test]
async fn settings_are_checked_locked_stored_and_logged() {
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
    let smtp_settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let server = Arc::new(FakeServer::default());
    let web = Web::new(
        Smtp::new(store.clone(), smtp_settings).unwrap(),
        WebSettings {
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            config: Some(server.clone()),
        },
    );
    let app = web.router();

    // Log in.
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
    let auth = (cookie, csrf);

    let (status, view) = call(&app, "GET", "/api/admin/settings", None, &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["configFile"], "/etc/uwumail/uwumail.toml");
    assert!(
        view["specs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|spec| spec["key"] == "delivery.relay.port" && spec["type"] == "integer")
    );

    let change = |changes: Value| Some(json!({ "changes": changes }));
    let (status, body) =
        call(&app, "PATCH", "/api/admin/settings", change(json!({ "tone.language": "en" })), &auth).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("settingLocked")));
    let (status, _) =
        call(&app, "PATCH", "/api/admin/settings", change(json!({ "delivery.relay.port": 99999 })), &auth).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) =
        call(&app, "PATCH", "/api/admin/settings", change(json!({ "smtp.trusted_relays": ["nonsense"] })), &auth).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("settingsInvalid")));

    let relay = json!({
        "delivery.relay.host": "relay.example.net",
        "delivery.relay.port": 465,
        "delivery.relay.password": "geheim-und-lang",
        "tone.external": "light",
    });
    let (status, view) = call(&app, "PATCH", "/api/admin/settings", change(relay), &auth).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let password = view["settings"].as_array().unwrap().iter().find(|s| s["key"] == "delivery.relay.password").unwrap();
    assert_eq!(
        (password["value"].clone(), password["set"].clone()),
        (Value::Null, json!(true)),
        "secrets never come back"
    );
    let stored: Value = serde_json::from_str(&store.setting("config.overlay").await.unwrap().unwrap()).unwrap();
    assert_eq!(stored["delivery"]["relay"]["port"], 465);
    assert_eq!(server.applied.lock().unwrap().as_ref(), Some(&stored));

    // Removing the host removes the whole relay.
    let (_, _) =
        call(&app, "PATCH", "/api/admin/settings", change(json!({ "delivery.relay.host": null })), &auth).await;
    let stored: Value = serde_json::from_str(&store.setting("config.overlay").await.unwrap().unwrap()).unwrap();
    assert_eq!(stored, json!({ "tone": { "external": "light" } }));

    let (_, log) = call(&app, "GET", "/api/admin/audit", None, &auth).await;
    assert_eq!(log[1]["action"], "settings.update");
    assert_eq!(log[1]["details"]["delivery.relay.password"], "•••");
    assert!(!log.to_string().contains("geheim"), "the change log never contains passwords");
}
