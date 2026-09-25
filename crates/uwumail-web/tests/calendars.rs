//! Sharing calendars and address books from My account.

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

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>, auth: Option<&(String, String)>) -> (StatusCode, Value) {
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
    let (_, login) =
        call(app, "POST", "/api/auth/login", Some(json!({ "login": address, "password": "katzenpfote-123" })), None)
            .await;
    (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn calendars_are_shared_and_left_from_my_account() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for address in ["leni@example.org", "ami@example.org"] {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: address[..4].to_owned(),
                password: Some("katzenpfote-123".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
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
            webmail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    let app = web.router();
    let leni = login(&app, "leni@example.org").await;
    let ami = login(&app, "ami@example.org").await;

    let (status, view) = call(&app, "GET", "/api/account/calendars", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let own = view["own"].as_array().unwrap();
    assert_eq!(own.len(), 2, "the default calendar and address book: {view}");
    let calendar = own.iter().find(|c| c["kind"] == "calendar").unwrap()["id"].as_i64().unwrap();

    let share = |address: &str, rights: &str| Some(json!({ "address": address, "rights": rights }));
    let path = format!("/api/account/calendars/{calendar}/shares");
    let (status, view) = call(&app, "PUT", &path, share("Ami@Example.org", "write"), Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let shares = &view["own"].as_array().unwrap().iter().find(|c| c["id"] == calendar).unwrap()["shares"];
    assert_eq!(shares[0]["address"], "ami@example.org");
    assert_eq!(shares[0]["rights"], "write");
    let (status, _) = call(&app, "PUT", &path, share("nobody@example.org", "read"), Some(&leni)).await;
    assert_eq!(status, StatusCode::CONFLICT, "nobody has that address here");
    let (status, _) = call(&app, "PUT", &path, share("ami@example.org", "everything"), Some(&leni)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(&app, "PUT", &path, share("leni@example.org", "read"), Some(&ami)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "only the owner shares from here");

    let (_, theirs) = call(&app, "GET", "/api/account/calendars", None, Some(&ami)).await;
    assert_eq!(theirs["shared"][0]["owner"], "leni@example.org");
    assert_eq!(theirs["shared"][0]["rights"], "write");

    let (status, theirs) =
        call(&app, "DELETE", &format!("/api/account/shared-calendars/{calendar}"), None, Some(&ami)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(theirs["shared"], json!([]));
    call(&app, "PUT", &path, share("ami@example.org", "read"), Some(&leni)).await;
    let ami_id = store.account("ami@example.org").await.unwrap().unwrap().id;
    let (status, view) = call(&app, "DELETE", &format!("{path}/{ami_id}"), None, Some(&leni)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(view["own"].as_array().unwrap().iter().all(|c| c["shares"] == json!([])));
}
