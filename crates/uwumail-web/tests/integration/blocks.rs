//! Logins nobody here could have meant keep their network out at once; the admin sees who is
//! kept out, lets them back in, and reads which outgoing mails other servers refused.

use std::net::IpAddr;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, NewQueueRecipient, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const SCANNER: &str = "203.0.113.7";
const ADMIN: &str = "198.51.100.20";

async fn open(dir: &std::path::Path) -> (Router, Store) {
    let store = Store::open(dir).await.unwrap();
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
    (web.router(), store)
}

async fn setup() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (app, store) = open(dir.path()).await;
    store.create_domain("example.org").await.unwrap();
    for (address, role) in [("nyu@example.org", Role::Admin), ("minifyx@example.org", Role::User)] {
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
    (app, store, dir)
}

/// The cookie and the CSRF token of a login, when there is one.
type Session<'a> = Option<(&'a str, &'a str)>;

async fn call(
    app: &Router,
    from: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    session: Session<'_>,
) -> (StatusCode, Value, Option<String>) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some((cookie, csrf)) = session {
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
    request.extensions_mut().insert(ClientInfo { ip: from.parse().unwrap(), https: true });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|value| value.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), cookie)
}

async fn try_login(app: &Router, from: &str, login: &str, password: &str) -> (StatusCode, Value, Option<String>) {
    call(app, from, "POST", "/api/auth/login", Some(json!({ "login": login, "password": password })), None).await
}

async fn admin(app: &Router) -> (String, String) {
    let (status, body, cookie) = try_login(app, ADMIN, "nyu@example.org", "katzenpfote-123").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (cookie.unwrap(), body["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn a_login_nobody_could_have_meant_blocks_at_once_and_an_admin_lets_back_in() {
    let (app, store, dir) = setup().await;

    // A slip of the keys and a wrong password are answered as before, and do not block.
    for (login, password) in [("minifx@example.org", "katzenpfote-123"), ("minifyx@example.org", "falsch")] {
        let (status, body, _) = try_login(&app, SCANNER, login, password).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalidCredentials")));
    }
    assert!(!store.auth_limiter().is_blocked(SCANNER.parse().unwrap()));

    // Nobody here is called anything like this: the same answer, and then the door is shut,
    // for the right password of a real account too.
    let (status, body, _) = try_login(&app, SCANNER, "administrator@example.net", "admin").await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalidCredentials")));
    let (status, body, _) = try_login(&app, SCANNER, "minifyx@example.org", "katzenpfote-123").await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("tooManyAttempts")));

    // Something that is no address at all, from an address that logged in here before: counted,
    // but never shut out at once.
    let (cookie, csrf) = admin(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));
    let (status, ..) = try_login(&app, ADMIN, "root", "toor").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!store.auth_limiter().is_blocked(ADMIN.parse().unwrap()));

    // From anywhere else it is.
    let (status, body, _) = try_login(&app, "2001:db8:7::1", "root", "toor").await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalidCredentials")));
    assert!(store.auth_limiter().is_blocked("2001:db8:7::2".parse().unwrap()), "the whole /64");

    let (status, view, _) = call(&app, ADMIN, "GET", "/api/admin/blocks", None, session).await;
    assert_eq!(status, StatusCode::OK);
    let blocks = view["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 2, "{view}");
    let by = |ip: &str| blocks.iter().find(|block| block["ip"] == ip).unwrap();
    let block = by("2001:db8:7::1");
    assert_eq!((block["reason"].as_str(), block["login"].as_str()), (Some("not_an_address"), Some("root")));
    let block = by(SCANNER);
    assert_eq!(
        (block["ip"].as_str(), block["reason"].as_str(), block["login"].as_str(), block["protocol"].as_str()),
        (Some(SCANNER), Some("unknown_login"), Some("administrator@example.net"), Some("web"))
    );
    assert_eq!(block["until"].as_u64().unwrap() - block["since"].as_u64().unwrap(), 24 * 3600);
    assert_eq!((block["times"].as_u64(), &view["exempt"]), (Some(1), &json!([])));

    // Only for admins.
    let (status, ..) = call(&app, ADMIN, "GET", "/api/admin/blocks", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The block is still there after a restart.
    drop((app, store));
    let (app, store) = open(dir.path()).await;
    let scanner: IpAddr = SCANNER.parse().unwrap();
    assert!(store.auth_limiter().is_blocked(scanner));

    let (cookie, csrf) = admin(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));
    let unblock = Some(json!({ "ip": "not an address" }));
    let (status, body, _) = call(&app, ADMIN, "POST", "/api/admin/blocks/unblock", unblock, session).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid")));
    let unblock = Some(json!({ "ip": SCANNER }));
    let (status, ..) = call(&app, ADMIN, "POST", "/api/admin/blocks/unblock", unblock, session).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, ..) = try_login(&app, SCANNER, "minifyx@example.org", "katzenpfote-123").await;
    assert_eq!(status, StatusCode::OK);

    // That login made the address one that is known here, so the next scanner comes from next door.
    let neighbour = "203.0.113.8";
    let (status, ..) = try_login(&app, neighbour, "postmaster@example.net", "x").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(store.auth_limiter().is_blocked(neighbour.parse().unwrap()));

    // An excepted network is let back in and never kept out again.
    for wrong in ["nonsense", "0.0.0.0/0", "2001:db8::/8"] {
        let exempt = Some(json!({ "networks": ["203.0.113.0/24", wrong] }));
        let (status, body, _) = call(&app, ADMIN, "PUT", "/api/admin/blocks/exempt", exempt, session).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid")), "{wrong}");
    }
    let exempt = Some(json!({ "networks": [" 203.0.113.0/24 ", "2001:db8::1"] }));
    let (status, body, _) = call(&app, ADMIN, "PUT", "/api/admin/blocks/exempt", exempt, session).await;
    assert_eq!((status, &body["exempt"]), (StatusCode::OK, &json!(["203.0.113.0/24", "2001:db8::1"])));
    assert!(!store.auth_limiter().is_blocked(neighbour.parse().unwrap()));
    let (status, ..) = try_login(&app, neighbour, "webmaster@example.net", "x").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!store.auth_limiter().is_blocked(neighbour.parse().unwrap()));
    let (_, view, _) = call(&app, ADMIN, "GET", "/api/admin/blocks", None, session).await;
    assert_eq!(view["blocks"].as_array().unwrap().len(), 1, "only the one outside the excepted networks: {view}");
    assert_eq!(view["exempt"], json!(["203.0.113.0/24", "2001:db8::1"]));
}

#[tokio::test]
async fn refused_mail_can_be_read_after_the_queue_forgot_it() {
    let (app, store, _dir) = setup().await;
    let recipients = ["leni@example.net", "mini@example.com"].map(|address| NewQueueRecipient {
        address: address.into(),
        notify_flags: 0,
        orcpt: None,
    });
    let id =
        store.enqueue("minifyx@example.org", recipients.into(), b"Subject: hi\r\n\r\nhi\r\n", None, None, 3600).await;
    let id = id.unwrap();
    let claimed = store.claim_due_deliveries(10, 600).await.unwrap();
    let (first, second) = (&claimed[0].recipients[0], &claimed[0].recipients[1]);
    store.mark_recipient_failed(first.id, "550 5.7.1 not from this network").await.unwrap();
    store.mark_recipient_given_up(second.id, "451 4.3.0 try again later").await.unwrap();
    assert!(store.complete_queue_message(id).await.unwrap());

    let (cookie, csrf) = admin(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));
    let (status, ..) = call(&app, ADMIN, "GET", "/api/admin/queue/refused", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, _) = call(&app, ADMIN, "GET", "/api/admin/queue/refused", None, session).await;
    assert_eq!(status, StatusCode::OK);
    let refused = body["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 2, "{body}");
    let given_up = refused.iter().find(|entry| entry["gaveUp"] == true).unwrap();
    assert_eq!(
        (given_up["from"].as_str(), given_up["to"].as_str(), given_up["error"].as_str()),
        (Some("minifyx@example.org"), Some("mini@example.com"), Some("451 4.3.0 try again later"))
    );
    assert!(given_up["at"].as_i64().unwrap() > 0 && given_up["id"].is_i64());

    let (status, ..) = call(&app, ADMIN, "DELETE", "/api/admin/queue/refused", None, session).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body, _) = call(&app, ADMIN, "GET", "/api/admin/queue/refused", None, session).await;
    assert_eq!(body["refused"], json!([]));
}
