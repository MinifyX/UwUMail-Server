//! The way out for remote pictures in the admin panel: shown without the proxy's login, and tested only
//! when an admin asks.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::egress::{Egress, EgressConfig, Fallback};
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

async fn portal() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    for (user, role) in [("nyu", Role::Admin), ("mini", Role::User)] {
        store
            .create_account(NewAccount {
                address: format!("{user}@example.de"),
                display_name: user.into(),
                password: Some("katzenpfote-123".into()),
                role,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
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
            loki: None,
            config: None,
            certificate: None,
            webmail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    // Nothing listens on the discard port, so the proxy is away.
    let config = EgressConfig {
        proxy: "socks5://vpn:geheim@127.0.0.1:9".into(),
        fallback: Fallback::Block,
        ..EgressConfig::default()
    };
    web.set_egress(Egress::new(&config).unwrap());
    (web.router(), dir)
}

async fn call(app: &Router, method: &str, path: &str, auth: Option<&(String, String)>) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some((cookie, csrf)) = auth {
        request = request.header(header::COOKIE, cookie).header(CSRF_HEADER, csrf);
    }
    let mut request = request.body(Body::empty()).unwrap();
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

async fn login(app: &Router, user: &str) -> (String, String) {
    let body = json!({ "login": format!("{user}@example.de"), "password": "katzenpfote-123" });
    let request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ClientInfo { https: true, ..ClientInfo::default() })
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie = response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap();
    let cookie = cookie.to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, json["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn the_way_out_is_shown_to_admins_without_the_proxy_login() {
    let (app, _dir) = portal().await;
    let admin = login(&app, "nyu").await;

    let (status, view) = call(&app, "GET", "/api/admin/egress", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["proxy"], "socks5://127.0.0.1:9");
    assert_eq!(view["fallback"], "block");
    assert_eq!((view["fetched"].as_u64(), view["proxyFailures"].as_u64()), (Some(0), Some(0)));
    assert!(!view.to_string().contains("geheim"), "{view}");

    let (status, tested) = call(&app, "POST", "/api/admin/egress/test", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!((tested["proxied"].as_bool(), &tested["address"]), (Some(true), &Value::Null));
    assert!(tested["error"].is_string(), "the proxy is away and nothing goes out directly: {tested}");

    let person = login(&app, "mini").await;
    assert_eq!(call(&app, "GET", "/api/admin/egress", Some(&person)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "POST", "/api/admin/egress/test", None).await.0, StatusCode::UNAUTHORIZED);
}
