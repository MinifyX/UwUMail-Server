//! The health overview: which findings show up before any background check ran.

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, CertificateStatus, Web, WebSettings};

fn codes(area: &Value) -> Vec<&str> {
    area["findings"].as_array().unwrap().iter().map(|finding| finding["code"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn health_lists_every_area_with_findings() {
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
            protocols: None,
        })
        .await
        .unwrap();
    let mini = store
        .create_account(NewAccount {
            address: "mini@example.de".into(),
            display_name: "Mini".into(),
            password: None,
            role: Role::User,
            quota_bytes: 100,
            protocols: None,
        })
        .await
        .unwrap();
    let raw = "From: a@example.org\r\nTo: mini@example.de\r\nSubject: Fast voll\r\n\r\nMiau miau miau miau miau\r\n";
    store
        .ingest(IngestRequest {
            account_id: mini.id,
            raw: raw.as_bytes().to_vec(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        })
        .await
        .unwrap();

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: "mail.example.de".into(),
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
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            config: None,
            certificate: Some(Arc::new(move || {
                Some(CertificateStatus {
                    not_after: now + 10 * 86_400,
                    names: vec!["mail.example.de".into()],
                    self_signed: false,
                    automatic: true,
                })
            })),
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

    let mut request = Request::builder()
        .uri("/api/admin/health")
        .header(header::COOKIE, &cookie)
        .header(CSRF_HEADER, &csrf)
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let health: Value = serde_json::from_slice(&bytes).unwrap();

    let area = |name: &str| health["areas"].as_array().unwrap().iter().find(|a| a["area"] == name).unwrap().clone();
    assert_eq!(codes(&area("dns")), ["dnsPending"]);
    assert_eq!(area("dns")["level"], "unknown");
    assert_eq!(codes(&area("certificate")), ["certExpiresSoon"], "Let's Encrypt should have renewed by now");
    assert_eq!(area("certificate")["level"], "warning");
    assert_eq!(codes(&area("delivery")), ["deliveryNotChecked"]);
    let storage = area("storage");
    assert!(codes(&storage).contains(&"mailboxesNearlyFull"), "{storage}");
    let full = storage["findings"].as_array().unwrap().iter().find(|f| f["code"] == "mailboxesNearlyFull").unwrap();
    assert_eq!(full["link"], "/admin/people/mini@example.de");
    assert_eq!(health["level"], "warning");
    assert_eq!(health["checkedAt"], Value::Null);
}
