//! Setting up mail apps: autoconfig, Autodiscover and Apple configuration profiles.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{AppScope, MailAuth, NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const PASSWORD: &str = "katzenpfote-123";

struct Reply {
    status: StatusCode,
    content_type: String,
    text: String,
    cookie: Option<String>,
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let mut request = request;
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let header_text = |name| response.headers().get(name).map(|v: &header::HeaderValue| v.to_str().unwrap().to_owned());
    let content_type = header_text(header::CONTENT_TYPE).unwrap_or_default();
    let cookie = header_text(header::SET_COOKIE).map(|value| value.split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    Reply { status, content_type, text: String::from_utf8_lossy(&bytes).into_owned(), cookie }
}

fn json_request(method: &str, path: &str, body: Value, auth: Option<&(String, String)>) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some((cookie, csrf)) = auth {
        request = request.header(header::COOKIE, cookie).header(CSRF_HEADER, csrf);
    }
    request.body(Body::from(body.to_string())).unwrap()
}

async fn setup() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    store
        .create_account(NewAccount {
            address: "mini@example.de".into(),
            display_name: "Mini".into(),
            password: Some(PASSWORD.into()),
            role: Role::User,
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
    let web = Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            config: None,
            certificate: None,
        },
    );
    (web.router(), store, dir)
}

#[tokio::test]
async fn apps_find_their_settings() {
    let (app, _store, _dir) = setup().await;

    let get = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
    let config = send(&app, get("/mail/config-v1.1.xml?emailaddress=mini%40example.de")).await;
    assert_eq!(config.status, StatusCode::OK);
    assert!(config.content_type.starts_with("application/xml"));
    assert!(config.text.contains("<domain>example.de</domain>"), "{}", config.text);
    assert!(
        config.text.contains(
            "<hostname>mail.example.de</hostname>\n      <port>993</port>\n      <socketType>SSL</socketType>"
        )
    );
    let config = send(&app, get("/.well-known/autoconfig/mail/config-v1.1.xml")).await;
    assert!(config.text.contains("<domain>%EMAILDOMAIN%</domain>"));
    let unknown = send(&app, get("/mail/config-v1.1.xml?emailaddress=someone%40example.org")).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    let outlook = |address: &str| {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006"><Request><EMailAddress>{address}</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema></Request></Autodiscover>"#
        );
        Request::builder().method("POST").uri("/autodiscover/autodiscover.xml").body(Body::from(body)).unwrap()
    };
    let found = send(&app, outlook("anyone@example.de")).await;
    assert_eq!(found.status, StatusCode::OK);
    assert!(
        found.text.contains("<Type>IMAP</Type>\n        <Server>mail.example.de</Server>\n        <Port>993</Port>")
    );
    assert!(found.text.contains("<LoginName>anyone@example.de</LoginName>"), "no difference for unknown people");
    let elsewhere = send(&app, outlook("someone@example.org")).await;
    assert!(elsewhere.text.contains("<ErrorCode>600</ErrorCode>"));
}

#[tokio::test]
async fn apple_profiles_carry_a_new_app_password_and_download_once() {
    let (app, store, _dir) = setup().await;
    let login = send(
        &app,
        json_request("POST", "/api/auth/login", json!({ "login": "mini@example.de", "password": PASSWORD }), None),
    )
    .await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.text);
    let csrf = serde_json::from_str::<Value>(&login.text).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    let auth = (login.cookie.unwrap(), csrf);

    let refused =
        send(&app, json_request("POST", "/api/account/apple-profiles", json!({ "device": "" }), Some(&auth))).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", refused.text);
    let created =
        send(&app, json_request("POST", "/api/account/apple-profiles", json!({ "device": "iPhone" }), Some(&auth)))
            .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let created: Value = serde_json::from_str(&created.text).unwrap();
    assert_eq!(created["appPassword"]["name"], "iPhone");
    let url = created["url"].as_str().unwrap();

    // The download needs no session: the link itself is the secret, and it works once.
    let download = send(&app, Request::builder().uri(url).body(Body::empty()).unwrap()).await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.content_type, "application/x-apple-aspen-config");
    let secret = download
        .text
        .split("<key>IncomingPassword</key>\n      <string>")
        .nth(1)
        .and_then(|rest| rest.split("</string>").next())
        .unwrap();
    let auth_result = store.authenticate_mail("mini@example.de", secret, AppScope::Mail, "imap", "").await.unwrap();
    assert!(
        matches!(auth_result, MailAuth::Ok { app_password: Some(_), .. }),
        "the profile's password is an app password"
    );
    let again = send(&app, Request::builder().uri(url).body(Body::empty()).unwrap()).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}
