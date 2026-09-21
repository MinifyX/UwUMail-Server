//! The gateway in the portal: showing, pairing and forgetting it, and its place in the health overview.

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::gateway::{GatewayBackend, GatewayFuture, GatewayState, GatewayView};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

/// Stands in for the server's tunnel.
#[derive(Default)]
struct FakeGateway {
    view: Mutex<GatewayView>,
    codes: Mutex<Vec<String>>,
    /// Everything the portal asked the gateway's machine for, in order.
    asked: Mutex<Vec<(String, Option<String>)>>,
}

impl GatewayBackend for FakeGateway {
    fn view(&self) -> GatewayView {
        self.view.lock().unwrap().clone()
    }

    fn pair<'a>(&'a self, code: &'a str) -> GatewayFuture<'a> {
        Box::pin(async move {
            if !code.starts_with("uwugw1") {
                return Err("this is not a pairing code of a UwUMail Gateway".into());
            }
            self.codes.lock().unwrap().push(code.to_owned());
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
            *self.view.lock().unwrap() = GatewayView {
                state: GatewayState::Connecting,
                tunnel: vec!["192.0.2.10:443".into()],
                down_since: Some(now),
                ..GatewayView::default()
            };
            Ok(())
        })
    }

    fn forget(&self) -> GatewayFuture<'_> {
        Box::pin(async move {
            *self.view.lock().unwrap() = GatewayView::default();
            Ok(())
        })
    }

    fn ask<'a>(
        &'a self,
        verb: &'a str,
        version: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            if !self.view.lock().unwrap().can_install {
                return Err("the gateway is not listening for this right now".into());
            }
            self.asked.lock().unwrap().push((verb.to_owned(), version.map(str::to_owned)));
            Ok("job1".to_owned())
        })
    }
}

struct Portal {
    app: Router,
    cookie: String,
    csrf: String,
}

impl Portal {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, &self.cookie)
            .header(CSRF_HEADER, &self.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

async fn portal(gateway: Arc<FakeGateway>) -> Portal {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    std::mem::forget(dir);
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
    let settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let web = Web::new(
        Smtp::new(store, settings).unwrap(),
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
    web.set_gateway(gateway);
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
    Portal { app, cookie, csrf }
}

#[tokio::test]
async fn admins_pair_and_forget_the_gateway() {
    let gateway = Arc::new(FakeGateway::default());
    let portal = portal(gateway.clone()).await;

    let (status, view) = portal.call("GET", "/api/admin/gateway", None).await;
    assert_eq!((status, view["state"].as_str()), (StatusCode::OK, Some("none")));

    let (status, error) = portal.call("POST", "/api/admin/gateway", Some(json!({ "code": "hello" }))).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("gatewayCodeInvalid")));

    // Right after logging in, the password counts as confirmed.
    let (status, view) = portal.call("POST", "/api/admin/gateway", Some(json!({ "code": " uwugw1abc " }))).await;
    assert_eq!((status, view["state"].as_str()), (StatusCode::OK, Some("connecting")), "{view}");
    assert_eq!(*gateway.codes.lock().unwrap(), ["uwugw1abc"]);

    let (_, health) = portal.call("GET", "/api/admin/health", None).await;
    let area = health["areas"].as_array().unwrap().iter().find(|area| area["area"] == "gateway").unwrap().clone();
    assert_eq!(area["findings"][0]["code"], "gatewayDown");
    assert_eq!(area["level"], "warning", "a tunnel that just went down gets a moment");

    let (_, audit) = portal.call("GET", "/api/admin/audit", None).await;
    assert!(audit.to_string().contains("gateway.pair"), "{audit}");

    let (status, _) = portal.call("DELETE", "/api/admin/gateway", Some(json!({}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, view) = portal.call("GET", "/api/admin/gateway", None).await;
    assert_eq!(view["state"], "none");
    let (_, health) = portal.call("GET", "/api/admin/health", None).await;
    assert!(!health.to_string().contains("\"gateway\""), "no gateway area without a gateway");
}
