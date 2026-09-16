//! The spam filter in My account and for admins: what the Bayes filter learned, and learning from
//! mail that is already sorted.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

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
    let cookie =
        response.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let (Some(cookie), Value::Object(map)) = (cookie, &mut json) {
        map.insert("_cookie".into(), cookie.into());
    }
    (status, json)
}

async fn login(app: &Router, address: &str) -> (String, String) {
    let body = json!({ "login": address, "password": "katzenpfote-123" });
    let (_, login) = call(app, "POST", "/api/auth/login", Some(body), None).await;
    (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn people_and_admins_see_what_was_learned_and_can_learn_from_sorted_mail() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    let mut ids = Vec::new();
    for (address, role) in [("chef@example.de", Role::Admin), ("leni@example.de", Role::User)] {
        let account = NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: Some("katzenpfote-123".into()),
            role,
            quota_bytes: 0,
        };
        ids.push(store.create_account(account).await.unwrap().id);
    }
    let month_ago = now_secs() - 30 * 24 * 3600;
    for (raw, role, keywords, received_at) in [
        (&b"Subject: Gewinnspiel\r\n\r\ngratis\r\n"[..], MailboxRole::Junk, vec![], None),
        (
            &b"Subject: Elternabend\r\n\r\nDienstag\r\n"[..],
            MailboxRole::Inbox,
            vec!["$seen".to_owned()],
            Some(month_ago),
        ),
    ] {
        let request = IngestRequest {
            account_id: ids[1],
            raw: raw.to_vec(),
            mailboxes: vec![MailboxTarget::Role(role)],
            keywords,
            received_at,
        };
        store.ingest(request).await.unwrap();
    }

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
    let app = web.router();

    let leni = login(&app, "leni@example.de").await;
    let (status, overview) = call(&app, "GET", "/api/account/spam", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["bayes"]["own"], json!({ "spam": 0, "ham": 0 }));
    assert_eq!((overview["bayes"]["minimum"].as_i64(), overview["bayes"]["enabled"].as_bool()), (Some(50), Some(true)));

    let (status, learned) = call(&app, "POST", "/api/account/spam/learn-folders", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{learned}");
    assert_eq!(learned, json!({ "spam": 1, "ham": 1 }));
    let (status, _) = call(&app, "GET", "/api/admin/spam", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the server's numbers are for admins");

    let chef = login(&app, "chef@example.de").await;
    let (status, overview) = call(&app, "GET", "/api/admin/spam", None, Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["bayes"]["queued"].as_i64(), Some(4), "each message for the server and for Leni");
    let (status, learned) = call(&app, "POST", "/api/admin/spam/learn-folders", None, Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "{learned}");
    assert_eq!(learned, json!({ "spam": 1, "ham": 1, "people": 1 }));
    let audit = store.audit_log(10, None).await.unwrap();
    assert!(audit.iter().any(|entry| entry.action == "spam.learnFromFolders"));
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}
