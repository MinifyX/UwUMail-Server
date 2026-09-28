//! Profile pictures in the portal: one's own, those admins set for services, shared mailboxes,
//! groups and domains, and the switches for public pictures (docs/profile-pictures.md).

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::profile_pictures::sample;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, NewGroup, Role, Store, WhoMaySend};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const PASSWORD: &str = "katzenpfote-123";

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

type Auth = (String, String);

async fn send(app: &Router, method: &str, path: &str, body: Vec<u8>, auth: &Auth) -> Answer {
    let (cookie, csrf) = auth;
    let mut request =
        Request::builder().method(method).uri(path).header(header::COOKIE, cookie).header(CSRF_HEADER, csrf);
    // JSON for changes and switches; an upload is the file itself.
    if body.first() == Some(&b'{') {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let mut request = request.body(Body::from(body)).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 32 << 20).await.unwrap().to_vec();
    Answer { status, headers, body }
}

async fn login(app: &Router, address: &str) -> Auth {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": address, "password": PASSWORD }).to_string()))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let csrf = serde_json::from_slice::<Value>(&bytes).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    (cookie, csrf)
}

async fn setup() -> (tempfile::TempDir, Store, Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for (address, role) in
        [("nyu@example.org", Role::Admin), ("mini@example.org", Role::User), ("bot@example.org", Role::Service)]
    {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: String::new(),
                password: Some(PASSWORD.into()),
                role,
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
    (dir, store, web.router())
}

#[tokio::test(flavor = "multi_thread")]
async fn my_own_picture() {
    let (_dir, _store, app) = setup().await;
    let mini = login(&app, "mini@example.org").await;

    let fresh = send(&app, "GET", "/api/account/picture", vec![], &mini).await.json();
    assert_eq!(fresh, json!({ "picture": null, "visibility": "server", "sendFace": false, "mayBePublic": true }));

    let uploaded = send(&app, "PUT", "/api/account/picture", sample(700, 500, "png"), &mini).await;
    assert_eq!(uploaded.status, StatusCode::OK);
    let picture = &uploaded.json()["picture"];
    assert_eq!(picture["type"], "image/jpeg");
    let url = picture["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/api/account/picture/file?v="), "{url}");
    let file = send(&app, "GET", &url, vec![], &mini).await;
    assert_eq!((file.status, file.headers[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/jpeg"));

    let changed =
        send(&app, "PATCH", "/api/account/picture", br#"{"visibility":"public","sendFace":true}"#.to_vec(), &mini)
            .await;
    assert_eq!(
        (changed.json()["visibility"].clone(), changed.json()["sendFace"].clone()),
        (json!("public"), json!(true))
    );
    let bad = send(&app, "PATCH", "/api/account/picture", br#"{"visibility":"everyone"}"#.to_vec(), &mini).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

    let refused = send(&app, "PUT", "/api/account/picture", b"<svg/>".to_vec(), &mini).await;
    assert_eq!(refused.json()["code"], "pictureType");
    let mut huge = sample(8, 8, "png");
    huge.resize(10 * 1024 * 1024 + 1, 0);
    assert_eq!(send(&app, "PUT", "/api/account/picture", huge, &mini).await.status, StatusCode::PAYLOAD_TOO_LARGE);

    let removed = send(&app, "DELETE", "/api/account/picture", vec![], &mini).await.json();
    assert_eq!(removed["picture"], Value::Null);
    assert_eq!(send(&app, "GET", "/api/account/picture/file", vec![], &mini).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn admins_set_pictures_for_services_groups_and_domains() {
    let (_dir, store, app) = setup().await;
    let nyu = login(&app, "nyu@example.org").await;
    let mini = login(&app, "mini@example.org").await;
    store
        .create_group(NewGroup {
            address: "info@example.org".into(),
            name: "Info".into(),
            who_may_send: WhoMaySend::Anyone,
            members_may_send_as: false,
            members: vec!["mini@example.org".into()],
        })
        .await
        .unwrap();

    // A service's picture is its admins' to choose; a person's is their own.
    let set = send(&app, "PUT", "/api/admin/people/bot@example.org/picture", sample(64, 64, "png"), &nyu).await;
    assert_eq!(set.status, StatusCode::OK);
    assert!(
        set.json()["picture"]["url"].as_str().unwrap().starts_with("/api/admin/people/bot@example.org/picture/file")
    );
    let person = send(&app, "PUT", "/api/admin/people/mini@example.org/picture", sample(64, 64, "png"), &nyu).await;
    assert_eq!(person.json()["code"], "picturePerson");
    let face =
        send(&app, "PATCH", "/api/admin/people/bot@example.org/picture", br#"{"sendFace":true}"#.to_vec(), &nyu).await;
    assert_eq!(face.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        send(&app, "PUT", "/api/admin/people/bot@example.org/picture", sample(64, 64, "png"), &mini).await.status,
        StatusCode::FORBIDDEN
    );

    let group =
        send(&app, "PUT", "/api/admin/domains/example.org/groups/info/picture", sample(64, 64, "gif"), &nyu).await;
    assert_eq!(group.status, StatusCode::OK);
    let visible = send(
        &app,
        "PATCH",
        "/api/admin/domains/example.org/groups/info/picture",
        br#"{"visibility":"public"}"#.to_vec(),
        &nyu,
    )
    .await;
    assert_eq!(visible.json()["visibility"], "public");
    assert_eq!(
        send(&app, "GET", "/api/admin/domains/example.org/groups/nobody/picture", vec![], &nyu).await.status,
        StatusCode::NOT_FOUND
    );

    let logo = send(&app, "PUT", "/api/admin/domains/example.org/logo", sample(90, 60, "webp"), &nyu).await.json();
    assert!(logo["picture"]["url"].is_string());
    assert_eq!((logo["publicPictures"].clone(), logo["serverAllowsPublic"].clone()), (json!(true), json!(true)));

    // The switches: per domain and for the whole server.
    let off =
        send(&app, "PUT", "/api/admin/domains/example.org/public-pictures", br#"{"allowed":false}"#.to_vec(), &nyu)
            .await
            .json();
    assert_eq!(off["publicPictures"], false);
    assert_eq!(send(&app, "GET", "/api/account/picture", vec![], &mini).await.json()["mayBePublic"], false);
    let refused = send(&app, "PATCH", "/api/account/picture", br#"{"visibility":"public"}"#.to_vec(), &mini).await;
    assert_eq!(refused.json()["code"], "publicNotAllowed");
    send(&app, "PUT", "/api/admin/domains/example.org/public-pictures", br#"{"allowed":true}"#.to_vec(), &nyu).await;
    let server = send(&app, "PUT", "/api/admin/pictures", br#"{"allowed":false}"#.to_vec(), &nyu).await.json();
    assert_eq!(server["publicAllowed"], false);
    assert_eq!(
        send(&app, "GET", "/api/admin/domains/example.org/groups/info/picture", vec![], &nyu).await.json()["visibility"],
        "server"
    );

    // Every change is in the change log.
    let audit = store.audit_log(50, None).await.unwrap();
    let actions: Vec<&str> = audit.iter().map(|entry| entry.action.as_str()).collect();
    for action in [
        "person.picture",
        "group.picture",
        "group.pictureVisibility",
        "domain.logo",
        "domain.publicPictures",
        "pictures.public",
    ] {
        assert!(actions.contains(&action), "{action} missing in {actions:?}");
    }
}
