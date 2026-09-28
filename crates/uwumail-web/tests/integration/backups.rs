//! Backups through the portal API: settings, secrets that stay on the server, and the recovery key.

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

async fn portal() -> (Router, Store, tempfile::TempDir) {
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
    let smtp = Smtp::new(store.clone(), settings).unwrap();
    let web = Web::new(
        smtp,
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
    web.set_backups(uwumail_backup::Backups::new(store.clone(), "mail.example.org", "0.1.0"));
    (web.router(), store, dir)
}

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
    let cookie = response.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let (Some(cookie), Value::Object(map)) = (cookie, &mut json) {
        map.insert("_cookie".into(), cookie.split(';').next().unwrap().into());
    }
    (status, json)
}

#[tokio::test]
async fn backup_settings_keep_their_secrets_on_the_server() {
    let (app, _store, _dir) = portal().await;
    let body = json!({ "login": "nyu@example.org", "password": "katzenpfote-123" });
    let (_, login) = call(&app, "POST", "/api/auth/login", Some(body), None).await;
    let auth = (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned());

    let (status, empty) = call(&app, "GET", "/api/admin/backups", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!((empty["enabled"].as_bool(), &empty["target"]), (Some(false), &Value::Null));

    let settings = json!({
        "enabled": true, "hour": 2, "retention": { "daily": 7, "weekly": 4, "monthly": 6 }, "encrypted": true,
        "target": { "host": "nas.example.org", "port": 22, "user": "backup", "path": "/volume1/uwumail", "method": "key" },
    });
    let (status, saved) = call(&app, "PUT", "/api/admin/backups", Some(settings.clone()), Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let key = saved["recoveryKey"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 64);
    let public = saved["target"]["publicKey"].as_str().unwrap();
    assert!(public.starts_with("ssh-ed25519 "), "{public}");
    assert!(!saved.to_string().contains("PRIVATE KEY"));

    let (_, again) = call(&app, "PUT", "/api/admin/backups", Some(settings), Some(&auth)).await;
    assert_eq!(again["recoveryKey"], Value::Null, "the key is shown once");
    assert_eq!(again["target"]["publicKey"].as_str(), Some(public), "and the SSH key stays");

    let with_password = json!({
        "enabled": true, "hour": 2, "retention": { "daily": 7, "weekly": 4, "monthly": 6 }, "encrypted": true,
        "target": { "host": "nas.example.org", "port": 22, "user": "backup", "path": "/volume1/uwumail",
                    "method": "password", "password": "Synology-geheim" },
    });
    let (_, changed) = call(&app, "PUT", "/api/admin/backups", Some(with_password), Some(&auth)).await;
    assert_eq!(
        (changed["target"]["method"].as_str(), changed["target"]["passwordSet"].as_bool()),
        (Some("password"), Some(true))
    );
    assert!(!changed.to_string().contains("Synology-geheim"));

    // Saving again keeps the password for the same server and user; another one asks for it again.
    let target = |host: &str, user: &str| {
        json!({
            "enabled": true, "hour": 2, "retention": { "daily": 7, "weekly": 4, "monthly": 6 }, "encrypted": true,
            "target": { "host": host, "port": 22, "user": user, "path": "/volume1/uwumail", "method": "password" },
        })
    };
    let (status, kept) =
        call(&app, "PUT", "/api/admin/backups", Some(target("nas.example.org", "backup")), Some(&auth)).await;
    assert_eq!((status, kept["target"]["passwordSet"].as_bool()), (StatusCode::OK, Some(true)), "{kept}");
    for (host, user) in [("other.example.net", "backup"), ("nas.example.org", "root")] {
        let (status, refused) = call(&app, "PUT", "/api/admin/backups", Some(target(host, user)), Some(&auth)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{host} {user}: {refused}");
    }
    let (_, unchanged) = call(&app, "GET", "/api/admin/backups", None, Some(&auth)).await;
    assert_eq!(unchanged["target"]["host"], "nas.example.org");

    let (status, shown) = call(&app, "POST", "/api/admin/backups/recovery-key", Some(json!({})), Some(&auth)).await;
    assert_eq!(
        (status, shown["recoveryKey"].as_str()),
        (StatusCode::OK, Some(key.as_str())),
        "a fresh login needs no password"
    );
    let (status, _) = call(&app, "POST", "/api/admin/backups/run", Some(json!({})), Some(&auth)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn backups_go_to_s3_or_a_folder_too() {
    let (app, store, dir) = portal().await;
    let body = json!({ "login": "nyu@example.org", "password": "katzenpfote-123" });
    let (_, login) = call(&app, "POST", "/api/auth/login", Some(body), None).await;
    let auth = (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned());
    let settings = |target: Value| {
        json!({
            "enabled": true, "hour": 2, "retention": { "daily": 7, "weekly": 4, "monthly": 6 }, "encrypted": true,
            "target": target,
        })
    };

    let s3 = json!({
        "kind": "s3", "endpoint": "https://s3.example.com/", "region": "", "bucket": "backups", "prefix": "/uwumail/",
        "accessKey": "AKIDEXAMPLE", "secretKey": "s3-geheim", "pathStyle": true,
    });
    let (status, saved) = call(&app, "PUT", "/api/admin/backups", Some(settings(s3)), Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let target = &saved["target"];
    assert_eq!(
        (target["kind"].as_str(), target["endpoint"].as_str(), target["region"].as_str(), target["prefix"].as_str()),
        (Some("s3"), Some("https://s3.example.com"), Some("us-east-1"), Some("uwumail"))
    );
    assert_eq!(target["secretKeySet"].as_bool(), Some(true));
    assert!(!saved.to_string().contains("s3-geheim"), "the secret stays on the server");

    // Saved again without the secret, it is kept for the same access key -- and needed for another.
    let again = json!({ "kind": "s3", "endpoint": "https://s3.example.com", "bucket": "backups",
                        "accessKey": "AKIDEXAMPLE", "pathStyle": false });
    let (status, kept) = call(&app, "PUT", "/api/admin/backups", Some(settings(again)), Some(&auth)).await;
    assert_eq!((status, kept["target"]["secretKeySet"].as_bool()), (StatusCode::OK, Some(true)), "{kept}");
    let raw = store.setting("backup.settings").await.unwrap().unwrap();
    assert!(raw.contains(r#""secretKey":"s3-geheim""#), "{raw}");
    let other =
        json!({ "kind": "s3", "endpoint": "https://s3.example.com", "bucket": "backups", "accessKey": "OTHER" });
    let (status, _) = call(&app, "PUT", "/api/admin/backups", Some(settings(other)), Some(&auth)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let broken = json!({ "kind": "s3", "endpoint": "https://s3.example.com/bucket", "bucket": "backups",
                         "accessKey": "AKIDEXAMPLE", "secretKey": "x" });
    let (status, _) = call(&app, "PUT", "/api/admin/backups", Some(settings(broken)), Some(&auth)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "the bucket does not go into the address");

    // A folder: not inside the data directory, and it has to be there to test it.
    let inside = json!({ "kind": "folder", "path": dir.path().join("backup").display().to_string() });
    let (status, refused) = call(&app, "PUT", "/api/admin/backups", Some(settings(inside)), Some(&auth)).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("backupFolderInData")));
    let outside = tempfile::tempdir().unwrap();
    let folder = json!({ "kind": "folder", "path": outside.path().join("nas").display().to_string() });
    let (status, saved) = call(&app, "PUT", "/api/admin/backups", Some(settings(folder)), Some(&auth)).await;
    assert_eq!((status, saved["target"]["kind"].as_str()), (StatusCode::OK, Some("folder")));
    let (status, _) = call(&app, "POST", "/api/admin/backups/test", Some(json!({})), Some(&auth)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "no such folder yet");
    std::fs::create_dir(outside.path().join("nas")).unwrap();
    let (status, tested) = call(&app, "POST", "/api/admin/backups/test", Some(json!({})), Some(&auth)).await;
    assert_eq!((status, tested["kind"].as_str()), (StatusCode::OK, Some("folder")), "{tested}");

    // Back up there, open the snapshot, and put a mailbox back.
    let (status, _) = call(
        &app,
        "POST",
        "/api/admin/backups/mailbox/restore",
        Some(json!({ "account": "nyu@example.org" })),
        Some(&auth),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "no snapshot is open");
    let backups = uwumail_backup::Backups::new(store.clone(), "mail.example.org", "0.1.0");
    backups.run_now().await.unwrap();
    let (status, opened) =
        call(&app, "POST", "/api/admin/backups/mailbox/open", Some(json!({ "snapshot": "latest" })), Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    let mut view = opened;
    for _ in 0..100 {
        if view["mailboxRestore"]["state"] != "opening" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        view = call(&app, "GET", "/api/admin/backups", None, Some(&auth)).await.1;
    }
    assert_eq!(view["mailboxRestore"]["state"], "open", "{view}");
    assert_eq!(view["mailboxRestore"]["people"][0]["login"], "nyu@example.org");
    let body = json!({ "account": "nyu@example.org", "folders": [] });
    let (status, _) = call(&app, "POST", "/api/admin/backups/mailbox/restore", Some(body), Some(&auth)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "no folders is nothing to do");
    let (status, restoring) = call(
        &app,
        "POST",
        "/api/admin/backups/mailbox/restore",
        Some(json!({ "account": "nyu@example.org" })),
        Some(&auth),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restoring}");
    for _ in 0..100 {
        view = call(&app, "GET", "/api/admin/backups", None, Some(&auth)).await.1;
        if view["mailboxRestore"]["state"] == "open" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(view["mailboxRestore"]["last"]["error"], "", "{view}");
    let (status, closed) = call(&app, "DELETE", "/api/admin/backups/mailbox", None, Some(&auth)).await;
    assert_eq!((status, closed["mailboxRestore"]["state"].as_str()), (StatusCode::OK, Some("")));
}
