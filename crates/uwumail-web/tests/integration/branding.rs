//! The server's own name, colours and logo, as the login page, the portal and the webmail get them.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{BrandConfig, Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

struct Response {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Response {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn send(app: &Router, method: &str, path: &str, body: Vec<u8>, auth: Option<&(String, String)>) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some((cookie, csrf)) = auth {
        request = request.header(header::COOKIE, cookie).header(CSRF_HEADER, csrf);
    }
    let mut request = request.body(Body::from(body)).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1 << 21).await.unwrap().to_vec();
    Response { status, headers, body }
}

async fn setup() -> (tempfile::TempDir, Smtp, Router, (String, String)) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    store
        .create_account(NewAccount {
            address: "nyu@example.org".into(),
            display_name: "Nyu".into(),
            password: Some("katzenpfote-123".into()),
            role: Role::Admin,
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
    let smtp = Smtp::new(store, settings).unwrap();
    let web = Web::new(
        smtp.clone(),
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
    let mut login = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": "nyu@example.org", "password": "katzenpfote-123" }).to_string()))
        .unwrap();
    login.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(login).await.unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let csrf = serde_json::from_slice::<Value>(&bytes).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    (dir, smtp, app, (cookie, csrf))
}

#[tokio::test]
async fn uwumail_as_it_comes_changes_nothing() {
    let (_dir, _smtp, app, _auth) = setup().await;
    let info = send(&app, "GET", "/api/info", vec![], None).await.json();
    assert_eq!(
        info["brand"],
        json!({ "name": "UwUMail", "custom": false, "color": null, "mascot": true, "logo": null })
    );
    let css = send(&app, "GET", "/branding.css", vec![], None).await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/css"));
    assert!(css.text().is_empty(), "the built-in pink stays untouched");
    assert_eq!(send(&app, "GET", "/branding/logo", vec![], None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_brand_reaches_the_login_page_the_session_and_the_stylesheet() {
    let (_dir, smtp, app, auth) = setup().await;
    smtp.set_brand(BrandConfig { name: "Post & Co".into(), color: "#0EA5E9".into(), mascot: false });

    let info = send(&app, "GET", "/api/info", vec![], None).await.json();
    assert_eq!(info["brand"]["name"], "Post & Co");
    assert_eq!(info["brand"]["color"], "#0ea5e9");
    assert_eq!(info["brand"]["mascot"], false);
    assert_eq!(info["brand"]["custom"], true);
    let session = send(&app, "GET", "/api/session", vec![], Some(&auth)).await.json();
    assert_eq!(session["server"]["brand"]["name"], "Post & Co");

    let css = send(&app, "GET", "/branding.css", vec![], None).await.text();
    assert!(css.contains("--uwu-pink: #0ea5e9;"), "{css}");
    assert!(css.contains("html:root[data-theme=\"dark\"]"));

    let preview = send(&app, "GET", "/api/admin/branding/palette?color=%2310b981", vec![], Some(&auth)).await;
    assert_eq!(preview.status, StatusCode::OK);
    assert_eq!(preview.json()["light"]["--uwu-pink"], "#10b981");
    let bad = send(&app, "GET", "/api/admin/branding/palette?color=pink", vec![], Some(&auth)).await;
    assert_eq!(bad.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn logos_are_checked_served_in_a_sandbox_and_removed() {
    let (_dir, _smtp, app, auth) = setup().await;
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><circle r="5"/></svg>"#.to_vec();

    // Only admins, and only pictures.
    assert_eq!(send(&app, "PUT", "/api/admin/branding/logo", svg.clone(), None).await.status, StatusCode::UNAUTHORIZED);
    let html = send(&app, "PUT", "/api/admin/branding/logo", b"<html>hi</html>".to_vec(), Some(&auth)).await;
    assert_eq!(html.status, StatusCode::CONFLICT);
    let huge = send(&app, "PUT", "/api/admin/branding/logo", vec![0x89; 600 * 1024], Some(&auth)).await;
    assert_eq!(huge.status, StatusCode::CONFLICT);

    let uploaded = send(&app, "PUT", "/api/admin/branding/logo", svg.clone(), Some(&auth)).await;
    assert_eq!(uploaded.status, StatusCode::OK);
    let url = uploaded.json()["logo"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/branding/logo?v="), "{url}");
    assert_eq!(send(&app, "GET", "/api/info", vec![], None).await.json()["brand"]["logo"], url.as_str());

    let logo = send(&app, "GET", &url, vec![], None).await;
    assert_eq!(logo.status, StatusCode::OK);
    assert_eq!(logo.body, svg);
    assert_eq!(logo.headers[header::CONTENT_TYPE], "image/svg+xml");
    assert!(logo.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("sandbox"));
    assert_eq!(logo.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

    let removed = send(&app, "DELETE", "/api/admin/branding/logo", vec![], Some(&auth)).await;
    assert_eq!(removed.status, StatusCode::OK);
    assert_eq!(removed.json()["logo"], Value::Null);
    assert_eq!(send(&app, "GET", "/branding/logo", vec![], None).await.status, StatusCode::NOT_FOUND);
}
