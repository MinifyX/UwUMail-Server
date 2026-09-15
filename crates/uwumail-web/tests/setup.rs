//! The setup assistant on a fresh server: the one-time code, the first admin and the test mail.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<&(String, String)>,
) -> (StatusCode, Value, Option<String>) {
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), cookie)
}

#[tokio::test]
async fn a_fresh_server_is_set_up_with_the_code_from_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    let settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
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
    let code = web.open_setup().await.expect("no admin yet, so there is a code");
    let app = web.router();

    let (_, status, _) = call(&app, "GET", "/api/setup", None, None).await;
    assert_eq!((status["open"].clone(), status["hostname"].clone()), (json!(true), json!("mail.example.de")));
    let (_, wrong, _) = call(&app, "POST", "/api/setup/code", Some(json!({ "code": "aaaa-bbbb-cccc" })), None).await;
    assert_eq!(wrong["code"], "setupCodeInvalid");
    let (status, _, _) =
        call(&app, "POST", "/api/setup/code", Some(json!({ "code": code.to_uppercase() })), None).await;
    assert_eq!(status, StatusCode::OK, "capitals and dashes do not matter");

    let first_admin = json!({
        "code": code,
        "domain": "Example.de",
        "localPart": "nyu",
        "name": "Nyu",
        "password": "katzenpfote-123",
    });
    let (status, session, cookie) = call(&app, "POST", "/api/setup", Some(first_admin.clone()), None).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(session["account"]["login"], "nyu@example.de");
    assert_eq!(session["account"]["role"], "admin");
    let auth = (cookie.expect("logged in right away"), session["csrfToken"].as_str().unwrap().to_owned());
    assert_eq!(store.dkim_keys("example.de").await.unwrap().len(), 2, "the domain got its keys");

    let (_, again, _) = call(&app, "POST", "/api/setup", Some(first_admin), None).await;
    assert_eq!(again["code"], "setupDone");
    let (_, status, _) = call(&app, "GET", "/api/setup", None, None).await;
    assert_eq!((status["open"].clone(), status["domains"].clone()), (json!(false), json!([])));

    // The test mail lands in the admin's own inbox; a reply to it is noticed.
    let (status, sent, _) = call(&app, "POST", "/api/admin/setup/test-mail", Some(json!({})), Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let id = sent["messageId"].as_str().unwrap().to_owned();
    let (_, progress, _) = call(&app, "GET", &format!("/api/admin/setup/test-mail/{id}"), None, Some(&auth)).await;
    assert_eq!((progress["arrived"].clone(), progress["replyFrom"].clone()), (json!(true), Value::Null));

    let nyu = store.account("nyu@example.de").await.unwrap().unwrap();
    let reply =
        format!("From: Nyu <nyu@elsewhere.example>\r\nIn-Reply-To: <{id}>\r\nSubject: Re: Test\r\n\r\nKlappt!\r\n");
    store
        .ingest(IngestRequest {
            account_id: nyu.id,
            raw: reply.into_bytes(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        })
        .await
        .unwrap();
    let (_, progress, _) = call(&app, "GET", &format!("/api/admin/setup/test-mail/{id}"), None, Some(&auth)).await;
    assert_eq!(progress["replyFrom"], "nyu@elsewhere.example");
}
