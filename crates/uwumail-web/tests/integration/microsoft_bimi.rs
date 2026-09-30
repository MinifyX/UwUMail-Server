//! Delivery to Microsoft and BIMI per domain, end to end through the router (docs/microsoft.md,
//! docs/bimi.md).

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{MailboxRole, MicrosoftRefusal, NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, CertificateStatus, Web, WebSettings};

struct Server {
    web: Web,
    app: Router,
    store: Store,
    /// Days the certificate has left; starts with ten.
    certificate_days: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for (address, role) in [("nyu@example.org", Role::Admin), ("leni@example.org", Role::User)] {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: address.split('@').next().unwrap().into(),
                password: Some("katzenpfote-123".into()),
                role,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let certificate_days = Arc::new(AtomicI64::new(10));
    let days = certificate_days.clone();
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: "mail.example.org".into(),
            smtp: Default::default(),
            spam: Default::default(),
            delivery: Default::default(),
            tone: Default::default(),
            server_tls: None,
        },
    )
    .unwrap();
    let web = Web::new(
        smtp,
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            // Ten days left of a Let's Encrypt certificate: it should have been renewed, a warning.
            certificate: Some(Arc::new(move || {
                Some(CertificateStatus {
                    not_after: unix_now() + days.load(Ordering::SeqCst) * 86_400 + 3600,
                    names: vec!["mail.example.org".into()],
                    self_signed: false,
                    automatic: true,
                    lets_encrypt_account: None,
                    chain: Vec::new(),
                })
            })),
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    Server { app: web.router(), web, store, certificate_days, _dir: dir }
}

async fn send(app: &Router, request: Request<Body>, ip: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = request;
    let ip: IpAddr = ip.parse().unwrap();
    request.extensions_mut().insert(ClientInfo { ip, https: true });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn login(app: &Router, login: &str) -> (String, String) {
    let request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": login, "password": "katzenpfote-123" }).to_string()))
        .unwrap();
    let mut request = request;
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let csrf = serde_json::from_slice::<Value>(&bytes).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    (cookie, csrf)
}

async fn api(app: &Router, method: &str, path: &str, session: &(String, String)) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, &session.0)
        .header(CSRF_HEADER, &session.1)
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(app, request, "127.0.0.1").await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn call(app: &Router, method: &str, path: &str, session: &(String, String), body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, &session.0)
        .header(CSRF_HEADER, &session.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, body) = send(app, request, "127.0.0.1").await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn public(app: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    send(app, Request::get(path).body(Body::empty()).unwrap(), "198.51.100.20").await
}

async fn inbox(store: &Store, login: &str) -> Vec<String> {
    let account = store.account(login).await.unwrap().unwrap();
    let inbox = store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
    let emails = store.emails_in_mailbox(inbox.unwrap().id, 50).await.unwrap();
    let mut texts = Vec::new();
    for email in emails {
        let hash = uwumail_store::BlobHash::parse(&email.blob).unwrap();
        texts.push(String::from_utf8_lossy(&store.blob(&hash).await.unwrap()).into_owned());
    }
    texts
}

fn blocked(ip: &str) -> MicrosoftRefusal {
    MicrosoftRefusal {
        scope: "ip",
        subject: ip.into(),
        group: "blockList",
        code: "S3150".into(),
        ip: ip.into(),
        domain: "example.org".into(),
        reply: format!("550 5.7.1 Unfortunately, messages from [{ip}] weren't sent (S3150)."),
    }
}

#[tokio::test]
async fn microsoft_refusals_show_up_are_mailed_once_and_can_be_closed() {
    let server = server().await;
    // Keep the certificate out of the way: this test is about Microsoft.
    server.certificate_days.store(80, Ordering::SeqCst);
    let admin = login(&server.app, "nyu@example.org").await;
    let person = login(&server.app, "leni@example.org").await;
    let (status, body) = api(&server.app, "GET", "/api/admin/microsoft/issues", &admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "issues": [], "delistUrl": "https://sender.office.com" }));
    for path in ["/api/admin/microsoft/issues", "/api/admin/microsoft/checklist"] {
        assert_eq!(api(&server.app, "GET", path, &person).await.0, StatusCode::FORBIDDEN);
    }

    server.store.record_microsoft_refusal(blocked("203.0.113.5"), unix_now()).await.unwrap();
    server.store.record_microsoft_refusal(blocked("203.0.113.5"), unix_now()).await.unwrap();
    let (_, body) = api(&server.app, "GET", "/api/admin/microsoft/issues", &admin).await;
    let issue = &body["issues"][0];
    assert_eq!(
        (issue["code"].as_str(), issue["subject"].as_str(), issue["count"].as_i64()),
        (Some("S3150"), Some("203.0.113.5"), Some(2))
    );
    // The portal's banner and list go by the kind.
    assert_eq!((issue["group"].as_str(), issue["kind"].as_str()), (Some("blockList"), Some("blocked")));
    assert_eq!(issue["resolvedAt"], Value::Null);

    // The overview names it, with the address and where to fix it.
    let (_, health) = api(&server.app, "GET", "/api/admin/health", &admin).await;
    let delivery = health["areas"].as_array().unwrap().iter().find(|area| area["area"] == "delivery").unwrap();
    let finding =
        delivery["findings"].as_array().unwrap().iter().find(|f| f["code"] == "microsoftBlocked").expect("{delivery}");
    assert_eq!((finding["level"].as_str(), finding["params"]["ip"].as_str()), (Some("problem"), Some("203.0.113.5")));
    assert_eq!(finding["link"], "/admin/microsoft");

    // The admins hear about it once, with the address and Microsoft's delisting form.
    server.web.check_alerts().await;
    server.web.check_alerts().await;
    let mails: Vec<String> = inbox(&server.store, "nyu@example.org").await;
    let about: Vec<&String> = mails.iter().filter(|mail| mail.contains("203.0.113.5")).collect();
    assert_eq!(about.len(), 1, "{mails:?}");
    assert!(about[0].contains("S3150") && about[0].contains("https://sender.office.com"), "{}", about[0]);

    // An admin says it is fixed.
    let path = format!("/api/admin/microsoft/issues/{}/resolve", issue["id"]);
    assert_eq!(api(&server.app, "POST", &path, &person).await.0, StatusCode::FORBIDDEN);
    let (status, body) = api(&server.app, "POST", &path, &admin).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["issues"][0]["resolvedBy"], "nyu@example.org");
    let (_, health) = api(&server.app, "GET", "/api/admin/health", &admin).await;
    assert!(!health.to_string().contains("microsoftBlocked"), "{health}");
    assert_eq!(
        api(&server.app, "POST", "/api/admin/microsoft/issues/999/resolve", &admin).await.0,
        StatusCode::NOT_FOUND
    );
    let log = server.store.audit_log(10, None).await.unwrap();
    assert!(log.iter().any(|entry| entry.action == "microsoft.resolve" && entry.target == "203.0.113.5"));
}

const LOGO: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><script>alert(1)</script><circle cx="16" cy="16" r="12" fill="#ff66aa"/></svg>"##;

fn mark_certificate() -> String {
    use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};
    let mut params = CertificateParams::new(vec!["example.org".to_owned()]).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::Other(vec![1, 3, 6, 1, 5, 5, 7, 3, 31])];
    params.self_signed(&KeyPair::generate().unwrap()).unwrap().pem()
}

#[tokio::test]
async fn bimi_logos_are_cleaned_hosted_and_described() {
    let server = server().await;
    let admin = login(&server.app, "nyu@example.org").await;
    let person = login(&server.app, "leni@example.org").await;
    let base = "/api/admin/domains/example.org/bimi";
    assert_eq!(api(&server.app, "GET", base, &person).await.0, StatusCode::FORBIDDEN);
    let (status, view) = api(&server.app, "GET", base, &admin).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!((view["enabled"].as_bool(), view["hasSvg"].as_bool()), (Some(false), Some(false)));
    assert_eq!(view["logoUrl"], "https://mail.example.org/bimi/example.org.svg");
    assert_eq!(
        view["record"],
        json!({ "name": "default._bimi.example.org", "value": "v=BIMI1; l=https://mail.example.org/bimi/example.org.svg" })
    );
    assert_eq!(view["dmarc"]["status"], "unknown");
    assert_eq!(view["published"], Value::Null);

    // Switching on needs a logo; one that Tiny PS cannot draw is refused by name.
    let (status, body) = call(&server.app, "PUT", base, &admin, json!({ "enabled": true })).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("bimiNoSvg")));
    let clipped = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><clipPath id="c"/></svg>"#;
    let (status, body) = call(&server.app, "PUT", &format!("{base}/svg"), &admin, json!({ "svg": clipped })).await;
    assert_eq!(
        (status, body["code"].as_str(), body["detail"].as_str()),
        (StatusCode::CONFLICT, Some("bimiSvgUnsupported"), Some("clipPath"))
    );

    let upload = json!({ "svg": LOGO, "title": "Example Club", "background": "#ffffff" });
    let (status, view) = call(&server.app, "PUT", &format!("{base}/svg"), &admin, upload).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!((view["hasSvg"].as_bool(), view["title"].as_str()), (Some(true), Some("Example Club")));
    // The admin sees it; nobody else does while BIMI is off.
    let request =
        Request::get(format!("{base}/logo.svg")).header(header::COOKIE, &admin.0).body(Body::empty()).unwrap();
    let (status, headers, preview) = send(&server.app, request, "127.0.0.1").await;
    assert_eq!((status, headers[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/svg+xml"));
    assert!(preview.contains(r#"baseProfile="tiny-ps""#) && preview.contains("<title>Example Club</title>"));
    assert!(!preview.contains("script"), "{preview}");
    assert_eq!(public(&server.app, "/bimi/example.org.svg").await.0, StatusCode::NOT_FOUND);

    let (status, view) = call(&server.app, "PUT", base, &admin, json!({ "enabled": true })).await;
    assert_eq!((status, view["enabled"].as_bool()), (StatusCode::OK, Some(true)));
    let (status, headers, served) = public(&server.app, "/bimi/example.org.svg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(served, preview);
    assert!(headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("sandbox"));
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    for missing in ["/bimi/example.org.pem", "/bimi/example.net.svg", "/bimi/example.org.png", "/bimi/..svg"] {
        assert_eq!(public(&server.app, missing).await.0, StatusCode::NOT_FOUND, "{missing}");
    }

    // A new title writes the logo again with it.
    let (_, view) = call(&server.app, "PUT", base, &admin, json!({ "title": "Club & Friends" })).await;
    assert_eq!(view["title"], "Club & Friends");
    assert!(public(&server.app, "/bimi/example.org.svg").await.2.contains("<title>Club &amp; Friends</title>"));

    // A mark certificate is checked, kept and named in the record.
    let (status, body) =
        call(&server.app, "PUT", &format!("{base}/certificate"), &admin, json!({ "pem": "nope" })).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("bimiCertificateInvalid")));
    let (status, view) =
        call(&server.app, "PUT", &format!("{base}/certificate"), &admin, json!({ "pem": mark_certificate() })).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        (view["certificate"]["coversDomain"].as_bool(), view["certificate"]["expired"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(view["certificateUrl"], "https://mail.example.org/bimi/example.org.pem");
    assert!(view["record"]["value"].as_str().unwrap().ends_with("; a=https://mail.example.org/bimi/example.org.pem"));
    let (status, _, pem) = public(&server.app, "/bimi/example.org.pem").await;
    assert!(status == StatusCode::OK && pem.starts_with("-----BEGIN CERTIFICATE-----"));

    let (_, view) = call(&server.app, "DELETE", &format!("{base}/certificate"), &admin, json!({})).await;
    assert_eq!(view["certificate"], Value::Null);
    let (_, view) = call(&server.app, "DELETE", &format!("{base}/svg"), &admin, json!({})).await;
    assert_eq!((view["enabled"].as_bool(), view["hasSvg"].as_bool()), (Some(false), Some(false)));
    assert_eq!(public(&server.app, "/bimi/example.org.svg").await.0, StatusCode::NOT_FOUND);
    let log = server.store.audit_log(20, None).await.unwrap();
    for action in [
        "domain.bimiLogo",
        "domain.bimi",
        "domain.bimiCertificate",
        "domain.bimiCertificateRemoved",
        "domain.bimiLogoRemoved",
    ] {
        assert!(log.iter().any(|entry| entry.action == action), "{action}");
    }
}
