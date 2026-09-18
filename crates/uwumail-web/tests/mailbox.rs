//! Forwarding and away messages through the API, with the confirmation link for other servers.

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

#[tokio::test]
async fn forwarding_needs_confirmation_elsewhere_and_away_messages_need_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    for (address, role) in [("leni@example.de", Role::User), ("ami@example.de", Role::User)] {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: String::new(),
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
    let (_, login) = call(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({ "login": "leni@example.de", "password": "katzenpfote-123" })),
        None,
    )
    .await;
    let auth = (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned());

    let target = |address: &str| Some(json!({ "address": address }));
    let (status, forwarding) =
        call(&app, "POST", "/api/account/forwarding/targets", target("Ami@Example.de"), Some(&auth)).await;
    assert_eq!(status, StatusCode::CREATED, "{forwarding}");
    assert!(forwarding["targets"][0]["confirmedAt"].is_number(), "people here need no confirmation");

    let (status, forwarding) =
        call(&app, "POST", "/api/account/forwarding/targets", target("oma@elsewhere.example"), Some(&auth)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(forwarding["targets"][1]["confirmedAt"].is_null());

    // The confirmation goes out through the queue with a link in it.
    let entries = store.queue_entries().await.unwrap();
    let entry =
        entries.iter().find(|e| e.recipients[0].address == "oma@elsewhere.example").expect("a confirmation mail");
    let raw = String::from_utf8(store.blob(&entry.message.blob).await.unwrap()).unwrap();
    // Quoted-printable breaks long lines; the link is whole again without the soft breaks.
    let raw = raw.replace("=\r\n", "").replace("=\n", "");
    let token = raw.split("https://mail.example.de/forwarding/").nth(1).unwrap()[..64].to_owned();
    assert!(raw.contains("DKIM-Signature"), "{raw}");

    let (status, link) = call(&app, "GET", &format!("/api/forwarding-links/{token}"), None, None).await;
    assert_eq!((status, link["from"].as_str()), (StatusCode::OK, Some("leni@example.de")));
    let (status, _) =
        call(&app, "POST", &format!("/api/forwarding-links/{token}/confirm"), Some(json!({})), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, again) = call(&app, "POST", &format!("/api/forwarding-links/{token}/confirm"), Some(json!({})), None).await;
    assert_eq!(again["code"], "linkInvalid");
    let (_, forwarding) = call(&app, "GET", "/api/account/forwarding", None, Some(&auth)).await;
    assert!(forwarding["targets"][1]["confirmedAt"].is_number());

    // Removing and adding again must not send the same address another confirmation right away.
    let id = forwarding["targets"][1]["id"].as_i64().unwrap();
    call(&app, "DELETE", &format!("/api/account/forwarding/targets/{id}"), None, Some(&auth)).await;
    let (status, error) =
        call(&app, "POST", "/api/account/forwarding/targets", target("oma@elsewhere.example"), Some(&auth)).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("forwardingThrottled")));
    for n in 0..4 {
        let (status, _) = call(
            &app,
            "POST",
            "/api/account/forwarding/targets",
            target(&format!("n{n}@elsewhere.example")),
            Some(&auth),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, list) = call(&app, "GET", "/api/account/forwarding", None, Some(&auth)).await;
        let last = list["targets"].as_array().unwrap().last().unwrap()["id"].as_i64().unwrap();
        call(&app, "DELETE", &format!("/api/account/forwarding/targets/{last}"), None, Some(&auth)).await;
    }
    let (_, error) =
        call(&app, "POST", "/api/account/forwarding/targets", target("sixth@elsewhere.example"), Some(&auth)).await;
    assert_eq!(error["code"], "forwardingThrottled", "five confirmations an hour per person");

    let (_, forwarding) =
        call(&app, "PUT", "/api/account/forwarding/keep-copy", Some(json!({ "keep": false })), Some(&auth)).await;
    assert_eq!(forwarding["keepCopy"], false);

    let (status, error) = call(
        &app,
        "PUT",
        "/api/account/vacation",
        Some(json!({ "isEnabled": true, "fromDate": null, "toDate": null, "subject": "Urlaub", "textBody": " " })),
        Some(&auth),
    )
    .await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("vacationText")));
    let (status, vacation) = call(
        &app,
        "PUT",
        "/api/account/vacation",
        Some(json!({ "isEnabled": true, "fromDate": null, "toDate": null, "subject": "Urlaub", "textBody": "Ab Montag wieder da." })),
        Some(&auth),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{vacation}");
    let (_, vacation) = call(&app, "GET", "/api/account/vacation", None, Some(&auth)).await;
    assert_eq!(vacation["textBody"], "Ab Montag wieder da.");
}
