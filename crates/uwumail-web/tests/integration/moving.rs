//! Moving from another provider, from My account: starting a move, pausing it, going on with it,
//! and ending it, which forgets the password. The copying itself is the server's worker, tested
//! there; here nothing connects anywhere.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const PASSWORD: &str = "katzenpfote-123";
const OLD_PASSWORD: &str = "altes-app-passwort";

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: &(String, String),
) -> (StatusCode, Value) {
    let body = body.map(|body| body.to_string());
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, &auth.0)
        .header(CSRF_HEADER, &auth.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map(Body::from).unwrap_or_else(Body::empty))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn login(app: &Router, address: &str) -> (String, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": address, "password": PASSWORD }).to_string()))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie =
        response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, json["csrfToken"].as_str().unwrap().to_owned())
}

async fn portal() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for address in ["mini@example.org", "nyu@example.org"] {
        let new = NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: Some(PASSWORD.into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        store.create_account(new).await.unwrap();
    }
    let settings = SmtpSettings {
        hostname: "mail.example.org".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let web = Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            certificate: None,
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    (web.router(), store, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_is_started_paused_resumed_and_ended() {
    let (app, store, _dir) = portal().await;
    let mini = login(&app, "mini@example.org").await;
    let nyu = login(&app, "nyu@example.org").await;

    let (status, empty) = call(&app, "GET", "/api/account/moving", None, &mini).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((empty["jobs"].as_array().map(Vec::len), empty["hasMailbox"].as_bool()), (Some(0), Some(true)));

    // With the server given, nothing is looked up: the job is queued for the worker.
    let body = json!({ "address": "Mini@Example.NET", "password": OLD_PASSWORD, "host": "imap.example.net" });
    let (status, job) = call(&app, "POST", "/api/account/moving", Some(body.clone()), &mini).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert_eq!((job["state"].as_str(), job["address"].as_str()), (Some("queued"), Some("mini@example.net")));
    assert_eq!((job["port"].as_u64(), job["login"].as_str()), (Some(993), Some("Mini@Example.NET")));
    assert!(!job.to_string().contains(OLD_PASSWORD), "the password stays on the server");
    let id = job["id"].as_i64().unwrap();
    let path = |rest: &str| format!("/api/account/moving/{id}{rest}");

    let (status, twice) = call(&app, "POST", "/api/account/moving", Some(body), &mini).await;
    assert_eq!((status, twice["code"].as_str()), (StatusCode::CONFLICT, Some("moveExists")));
    let here = json!({ "address": "nyu@example.org", "password": OLD_PASSWORD, "host": "imap.example.net" });
    let (status, refused) = call(&app, "POST", "/api/account/moving", Some(here), &mini).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("moveFromHere")));

    // Queued already: going on makes no sense yet. Paused, it does, with a new password.
    let (status, running) = call(&app, "POST", &path("/sync"), Some(json!({})), &mini).await;
    assert_eq!((status, running["code"].as_str()), (StatusCode::CONFLICT, Some("moveRunning")));
    let (status, paused) = call(&app, "POST", &path("/pause"), Some(json!({})), &mini).await;
    assert_eq!(
        (status, paused["state"].as_str(), paused["error"].as_str()),
        (StatusCode::OK, Some("paused"), Some("stopped"))
    );
    let (status, again) =
        call(&app, "POST", &path("/sync"), Some(json!({ "password": "neues-passwort" })), &mini).await;
    assert_eq!((status, again["state"].as_str()), (StatusCode::OK, Some("queued")));
    let mini_id = store.account("mini@example.org").await.unwrap().unwrap().id;
    assert_eq!(store.migration_password(mini_id, id).await.unwrap().as_deref(), Some("neues-passwort"));

    // Nobody else sees or ends it.
    let (_, theirs) = call(&app, "GET", "/api/account/moving", None, &nyu).await;
    assert_eq!(theirs["jobs"].as_array().map(Vec::len), Some(0));
    let (status, _) = call(&app, "DELETE", &path(""), None, &nyu).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "POST", &path("/pause"), Some(json!({})), &nyu).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Logging in at the old provider is limited: going on over and over is refused after a while.
    let mut limited = false;
    for _ in 0..12 {
        let (status, _) = call(&app, "POST", &path("/sync"), Some(json!({})), &mini).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(limited, "going on with a move is rate-limited");

    // Done: the job and its password are gone.
    let (status, _) = call(&app, "DELETE", &path(""), None, &mini).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, after) = call(&app, "GET", "/api/account/moving", None, &mini).await;
    assert_eq!(after["jobs"].as_array().map(Vec::len), Some(0));
    assert_eq!(store.migration_password(mini_id, id).await.unwrap(), None);
}

/// What a move left out: counted apart in the job, listed for its person only.
#[tokio::test(flavor = "multi_thread")]
async fn the_messages_a_move_left_out_are_counted_apart_and_listed() {
    use uwumail_store::{MigrationProgress, SkipReason, SkippedMessage, SkippedOf};
    let (app, store, _dir) = portal().await;
    let mini = login(&app, "mini@example.org").await;
    let nyu = login(&app, "nyu@example.org").await;
    let body = json!({ "address": "mini@example.net", "password": OLD_PASSWORD, "host": "imap.example.net" });
    let (status, job) = call(&app, "POST", "/api/account/moving", Some(body), &mini).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_i64().unwrap();
    assert_eq!(store.take_migration_job().await.unwrap().map(|job| job.id), Some(id));
    let progress = MigrationProgress {
        messages_done: 5,
        messages_total: 5,
        messages_skipped: 3,
        messages_known: 1,
        messages_too_large: 2,
        ..Default::default()
    };
    assert!(store.note_migration_progress(id, progress).await.unwrap());
    let large = |uid: u32| SkippedMessage {
        folder: "INBOX".into(),
        uid,
        reason: SkipReason::TooLarge,
        from: String::new(),
        subject: "Urlaubsfotos".into(),
        date: None,
        size: 80 * 1024 * 1024,
        recorded_at: 0,
    };
    store.note_skipped_messages(SkippedOf::MigrationJob(id), vec![large(1), large(2)]).await.unwrap();

    let (_, list) = call(&app, "GET", "/api/account/moving", None, &mini).await;
    let shown = &list["jobs"][0];
    assert_eq!(
        (shown["messagesSkipped"].as_i64(), shown["messagesKnown"].as_i64(), shown["messagesTooLarge"].as_i64()),
        (Some(3), Some(1), Some(2)),
        "{shown}"
    );
    assert_eq!(shown["messagesUnreadable"].as_i64(), Some(0));

    let path = format!("/api/account/moving/{id}/skipped");
    let (status, listed) = call(&app, "GET", &path, None, &mini).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let entries = listed["messages"].as_array().unwrap();
    assert_eq!(entries.iter().map(|entry| entry["uid"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2]);
    assert!(listed["maxSize"].as_u64().is_some_and(|size| size > 0), "{listed}");
    assert_eq!(
        (entries[0]["reason"].as_str(), entries[0]["subject"].as_str()),
        (Some("tooLarge"), Some("Urlaubsfotos"))
    );
    let (status, _) = call(&app, "GET", &path, None, &nyu).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nobody else's");
}
