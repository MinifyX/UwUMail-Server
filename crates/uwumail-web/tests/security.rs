//! My account → Security through the API: authenticator app, login with a second factor,
//! recovery codes, app passwords, password change and the notices that come with them.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aws_lc_rs::hmac;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use data_encoding::BASE32_NOPAD;
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{AppScope, MailAuth, MailboxRole, NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const PASSWORD: &str = "katzenpfote-123";

struct Response {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<&(String, String)>,
) -> Response {
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
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|value| value.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    Response { status, body: serde_json::from_slice(&bytes).unwrap_or(Value::Null), cookie }
}

fn session(response: &Response) -> (String, String) {
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    (response.cookie.clone().expect("a session cookie"), response.body["csrfToken"].as_str().unwrap().to_owned())
}

fn totp(secret: &str) -> String {
    let secret = BASE32_NOPAD.decode(secret.as_bytes()).unwrap();
    let step = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() / 30;
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &secret), &step.to_be_bytes());
    let digest = tag.as_ref();
    let offset = usize::from(digest[19] & 0x0f);
    let value = u32::from_be_bytes(digest[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    format!("{:06}", value % 1_000_000)
}

async fn inbox_subjects(store: &Store, account_id: i64) -> Vec<String> {
    let inbox = store.mailboxes(account_id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
    let emails = store.emails_in_mailbox(inbox.unwrap().id, 50).await.unwrap();
    emails.into_iter().map(|email| email.subject).collect()
}

#[tokio::test]
async fn second_factors_app_passwords_and_notices() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    let nyu = store
        .create_account(NewAccount {
            address: "nyu@example.de".into(),
            display_name: "Nyu".into(),
            password: Some(PASSWORD.into()),
            role: Role::Admin,
            quota_bytes: 0,
        })
        .await
        .unwrap();
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
    let app = web.router();
    let credentials = json!({ "login": "nyu@example.de", "password": PASSWORD });

    // Without a second factor the password is enough.
    let auth = session(&call(&app, "POST", "/api/auth/login", Some(credentials.clone()), None).await);
    let security = call(&app, "GET", "/api/account/security", None, Some(&auth)).await.body;
    assert_eq!((security["totp"].clone(), security["appPasswordsRequired"].clone()), (json!(false), json!(false)));

    // Set up the authenticator app. A fresh login needs no password confirmation.
    let setup = call(&app, "POST", "/api/account/totp", Some(json!({})), Some(&auth)).await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);
    assert!(
        setup.body["uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/UwUMail%20%28mail.example.de%29:nyu%40example.de?")
    );
    assert!(setup.body["qr"]["size"].as_u64().unwrap() >= 21);
    let secret = setup.body["secret"].as_str().unwrap().to_owned();
    let wrong = call(&app, "POST", "/api/account/totp/confirm", Some(json!({ "code": "12345x" })), Some(&auth)).await;
    assert_eq!(wrong.body["code"], "codeInvalid");
    let confirmed =
        call(&app, "POST", "/api/account/totp/confirm", Some(json!({ "code": totp(&secret) })), Some(&auth)).await;
    let codes: Vec<String> =
        serde_json::from_value(confirmed.body["recoveryCodes"].clone()).expect("recovery codes on the first factor");
    assert_eq!(codes.len(), 10);
    assert!(inbox_subjects(&store, nyu.id).await.iter().any(|s| s == "Authenticator-App eingeschaltet"));

    // The main password no longer works in mail apps; an app password does.
    let denied = store.authenticate_mail("nyu@example.de", PASSWORD, AppScope::Smtp, "smtp", "192.0.2.1").await;
    assert!(matches!(denied.unwrap(), MailAuth::Denied(_)));
    let created = call(
        &app,
        "POST",
        "/api/account/app-passwords",
        Some(json!({ "name": "Handy", "scopes": ["mail", "smtp"] })),
        Some(&auth),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let app_password = created.body["secret"].as_str().unwrap();
    let allowed = store.authenticate_mail("nyu@example.de", app_password, AppScope::Smtp, "smtp", "192.0.2.1").await;
    assert!(matches!(allowed.unwrap(), MailAuth::Ok { app_password: Some(_), .. }));

    // Logging in again asks for the second factor before there is a session.
    call(&app, "POST", "/api/auth/logout", Some(json!({})), Some(&auth)).await;
    let first_step = call(&app, "POST", "/api/auth/login", Some(credentials.clone()), None).await;
    assert_eq!(first_step.status, StatusCode::OK);
    assert!(first_step.cookie.is_none(), "no session before the second factor");
    assert_eq!(first_step.body["secondFactor"]["totp"], true);
    let token = first_step.body["secondFactor"]["token"].as_str().unwrap().to_owned();
    let wrong =
        call(&app, "POST", "/api/auth/second-factor", Some(json!({ "token": token, "code": "abcdef" })), None).await;
    assert_eq!((wrong.status, wrong.body["code"].as_str()), (StatusCode::CONFLICT, Some("codeInvalid")));
    let recovered =
        call(&app, "POST", "/api/auth/second-factor", Some(json!({ "token": token, "code": codes[0] })), None).await;
    let auth = session(&recovered);
    let reused =
        call(&app, "POST", "/api/auth/second-factor", Some(json!({ "token": token, "code": codes[1] })), None).await;
    assert_eq!(reused.body["code"], "loginExpired", "a pending login finishes once");
    assert!(inbox_subjects(&store, nyu.id).await.iter().any(|s| s == "Wiederherstellungscode benutzt"));

    let security = call(&app, "GET", "/api/account/security", None, Some(&auth)).await.body;
    assert_eq!(security["recoveryCodesLeft"], 9);
    let kinds: Vec<&str> = security["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    for kind in ["login", "recoveryCodeUsed", "appPasswordCreated", "totpEnabled", "mainPasswordRefused"] {
        assert!(kinds.contains(&kind), "{kind} missing in {kinds:?}");
    }
    assert_eq!(security["sessions"].as_array().unwrap().iter().filter(|s| s["current"] == true).count(), 1);

    // Changing the password: the current one must be right, and other browsers are logged out.
    let other = call(&app, "POST", "/api/auth/login", Some(credentials.clone()), None).await;
    let other_token = other.body["secondFactor"]["token"].as_str().unwrap().to_owned();
    let other_auth = session(
        &call(&app, "POST", "/api/auth/second-factor", Some(json!({ "token": other_token, "code": codes[2] })), None)
            .await,
    );
    let change = |current: &str| Some(json!({ "current": current, "new": "Kirschbluete-Tastatur-42" }));
    let wrong = call(&app, "POST", "/api/account/password", change("falsch-falsch-falsch"), Some(&auth)).await;
    assert_eq!(wrong.body["code"], "wrongPassword");
    let changed = call(&app, "POST", "/api/account/password", change(PASSWORD), Some(&auth)).await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT, "{}", changed.body);
    assert_eq!(call(&app, "GET", "/api/account", None, Some(&other_auth)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "GET", "/api/account", None, Some(&auth)).await.status, StatusCode::OK);

    // The admin view shows the second factor, and a reset needs one to exist.
    let person = call(&app, "GET", "/api/admin/people/nyu@example.de", None, Some(&auth)).await.body;
    assert_eq!(person["security"]["secondFactor"], true);
    let health = call(&app, "GET", "/api/admin/health", None, Some(&auth)).await.body;
    let security_area = health["areas"].as_array().unwrap().iter().find(|a| a["area"] == "security").unwrap();
    assert_eq!(security_area["findings"][0]["code"], "adminsSecure");
}
