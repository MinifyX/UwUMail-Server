//! Settings → VPN & proxy: the VPN is stored with its keys, shown without them, handed to the machine's
//! helper to start, and the way out then points at gluetun.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::egress::Egress;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::host::{HostBackend, HostFuture, HostMachine, HostView};
use uwumail_web::settings::{SETTINGS, SettingSource, SettingValue, SettingsBackend, get_path};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=";

/// A helper that remembers what it was handed and asked for.
struct FakeHost {
    verbs: Vec<String>,
    files: Mutex<Vec<(String, String)>>,
    asked: Mutex<Vec<String>>,
}

impl HostBackend for FakeHost {
    fn view(&self) -> HostView {
        let machine = HostMachine { verbs: self.verbs.clone(), helper: "2".into(), ..HostMachine::default() };
        HostView { available: true, machine: Some(machine), ..HostView::default() }
    }

    fn ask<'a>(&'a self, verb: &'a str) -> HostFuture<'a> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(verb.to_owned());
            Ok("abcd1234".to_owned())
        })
    }

    fn hand_over(&self, name: &'static str, contents: &str) -> Result<(), String> {
        self.files.lock().unwrap().push((name.to_owned(), contents.to_owned()));
        Ok(())
    }
}

/// Settings that are applied to the egress, the way the server does it.
struct FakeServer {
    egress: Egress,
}

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
        let proxy = get_path(overlay, "egress.proxy").and_then(Value::as_str).unwrap_or_default();
        let config = uwumail_smtp::egress::EgressConfig { proxy: proxy.into(), ..Default::default() };
        self.egress.reconfigure(&config)
    }

    fn config_file(&self) -> Option<String> {
        None
    }
}

async fn portal(verbs: &[&str]) -> (Router, Arc<FakeHost>, tempfile::TempDir) {
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
    let egress = Egress::direct();
    let web = Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: Some(Arc::new(FakeServer { egress: egress.clone() })),
            certificate: None,
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    web.set_egress(egress);
    let host = Arc::new(FakeHost {
        verbs: verbs.iter().map(|verb| verb.to_string()).collect(),
        files: Mutex::default(),
        asked: Mutex::default(),
    });
    web.set_host(host.clone());
    (web.router(), host, dir)
}

async fn login(app: &Router) -> (String, String) {
    let mut request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": "nyu@example.org", "password": "katzenpfote-123" }).to_string()))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let csrf = serde_json::from_slice::<Value>(&bytes).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    (cookie, csrf)
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
async fn the_vpn_is_stored_shown_without_its_key_and_started_by_the_helper() {
    let (app, host, _dir) = portal(&["os-update", "reboot", "vpn-apply", "vpn-stop"]).await;
    let auth = login(&app).await;

    let (status, view) = call(&app, "GET", "/api/admin/vpn", None, &auth).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!((view["saved"].as_bool(), view["helper"]["canVpn"].as_bool()), (Some(false), Some(true)));

    let nord =
        json!({ "provider": "nordvpn", "kind": "wireguard", "countries": "Switzerland", "wireguardPrivateKey": KEY });
    let (status, view) = call(&app, "PUT", "/api/admin/vpn", Some(nord), &auth).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(!view.to_string().contains(KEY), "the key never comes back: {view}");
    assert_eq!(view["secrets"]["wireguardPrivateKey"], true);
    assert_eq!(view["complete"], Value::Null, "nothing missing");

    // Changing the country keeps the key, since it was not sent again.
    let change = json!({ "provider": "nordvpn", "kind": "wireguard", "countries": "Netherlands" });
    let (_, view) = call(&app, "PUT", "/api/admin/vpn", Some(change), &auth).await;
    assert_eq!(
        (view["config"]["countries"].as_str(), view["secrets"]["wireguardPrivateKey"].as_bool()),
        (Some("Netherlands"), Some(true))
    );

    let (status, view) = call(&app, "POST", "/api/admin/vpn/apply", None, &auth).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let (name, contents) = host.files.lock().unwrap()[0].clone();
    assert_eq!(name, "vpn.json");
    let handed: Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(handed["env"]["WIREGUARD_PRIVATE_KEY"], KEY);
    assert_eq!(handed["env"]["SERVER_COUNTRIES"], "Netherlands");
    assert_eq!(host.asked.lock().unwrap().as_slice(), ["vpn-apply"]);
    assert_eq!(view["proxy"]["current"], "http://gluetun:8888", "the way out points at gluetun now");

    let (status, view) = call(&app, "POST", "/api/admin/vpn/stop", None, &auth).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["proxy"]["current"], Value::Null, "straight again while the VPN is off");
    assert_eq!(host.asked.lock().unwrap().last().unwrap(), "vpn-stop");

    let (status, files) = call(&app, "POST", "/api/admin/vpn/files", None, &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert!(files["envFile"].as_str().unwrap().contains(&format!("WIREGUARD_PRIVATE_KEY='{KEY}'")));
}

#[tokio::test]
async fn an_old_helper_or_an_incomplete_vpn_starts_nothing() {
    let (app, host, _dir) = portal(&["os-update", "reboot"]).await;
    let auth = login(&app).await;
    let nord = json!({ "provider": "nordvpn", "kind": "wireguard", "wireguardPrivateKey": KEY });
    call(&app, "PUT", "/api/admin/vpn", Some(nord), &auth).await;
    let (status, body) = call(&app, "POST", "/api/admin/vpn/apply", None, &auth).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("vpnHelperOld")), "{body}");

    let (app, host2, _dir) = portal(&["vpn-apply", "vpn-stop"]).await;
    let auth = login(&app).await;
    let mullvad = json!({ "provider": "mullvad", "kind": "wireguard", "wireguardPrivateKey": KEY });
    let (_, view) = call(&app, "PUT", "/api/admin/vpn", Some(mullvad), &auth).await;
    assert!(view["complete"].as_str().unwrap().contains("address"), "{view}");
    let (status, body) = call(&app, "POST", "/api/admin/vpn/apply", None, &auth).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("vpnInvalid")), "{body}");
    assert!(host.asked.lock().unwrap().is_empty() && host2.asked.lock().unwrap().is_empty());
    let (status, _) = call(&app, "PUT", "/api/admin/vpn", Some(json!({ "provider": "evilvpn" })), &auth).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_vpn_can_always_be_switched_off_and_a_named_server_is_looked_up() {
    // A helper too old for the VPN: switching off still sends everything straight again.
    let (app, host, _dir) = portal(&["os-update", "reboot"]).await;
    let auth = login(&app).await;
    let (status, view) = call(&app, "POST", "/api/admin/vpn/use-gluetun", None, &auth).await;
    assert_eq!((status, view["proxy"]["current"].as_str()), (StatusCode::OK, Some("http://gluetun:8888")));
    let (status, view) = call(&app, "POST", "/api/admin/vpn/stop", None, &auth).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["proxy"]["current"], Value::Null);
    assert!(host.asked.lock().unwrap().is_empty(), "nothing asked of a helper that cannot do it");

    // An own WireGuard server named in its file (Endpoint = name:port) is stored by address.
    let custom = json!({
        "provider": "custom",
        "kind": "wireguard",
        "wireguardPrivateKey": KEY,
        "wireguardAddresses": "10.5.0.2/16",
        "wireguardPublicKey": KEY,
        "wireguardEndpointIp": "localhost",
        "wireguardEndpointPort": 51820,
    });
    let (status, view) = call(&app, "PUT", "/api/admin/vpn", Some(custom), &auth).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let address = view["config"]["wireguardEndpointIp"].as_str().unwrap();
    assert!(address.parse::<std::net::IpAddr>().unwrap().is_loopback(), "{address}");
    assert_eq!(view["complete"], Value::Null);
    let unknown = json!({ "provider": "custom", "kind": "wireguard", "wireguardEndpointIp": "vpn.invalid" });
    let (status, body) = call(&app, "PUT", "/api/admin/vpn", Some(unknown), &auth).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("vpnInvalid")), "{body}");
}
