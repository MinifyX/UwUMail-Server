//! Taking calendars and contacts over from files, feeds and other providers, from My account.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use bytes::Bytes;
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_dav::client::{Answer, BoxFuture, RemoteError, Transport};
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{DavKind, NewAccount, NewDavCollection, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

const PASSWORD: &str = "katzenpfote-123";
const SECRET: &str = "private-7f3a9c";

/// "The internet": requests go to a router in this process, whatever host they name.
struct Internet(Router);

impl Transport for Internet {
    fn send(&self, request: Request<Bytes>, max_bytes: usize) -> BoxFuture<'_, Result<Answer, RemoteError>> {
        Box::pin(async move {
            let (mut parts, body) = request.into_parts();
            parts.uri = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").parse().unwrap();
            let mut request = Request::from_parts(parts, Body::from(body));
            request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
            let response = self.0.clone().oneshot(request).await.unwrap();
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body =
                axum::body::to_bytes(response.into_body(), max_bytes).await.map_err(|_| RemoteError::TooLarge)?;
            Ok(Answer { status, headers, body })
        })
    }
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Body>,
    auth: &(String, String),
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, &auth.0)
        .header(CSRF_HEADER, &auth.1)
        .header(header::CONTENT_TYPE, "application/json");
    if body.is_none() {
        request = request.header(header::CONTENT_LENGTH, "0");
    }
    let mut request = request.body(body.unwrap_or_else(Body::empty)).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn json_body(value: Value) -> Option<Body> {
    Some(Body::from(value.to_string()))
}

async fn login(app: &Router, address: &str) -> (String, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": address, "password": PASSWORD }).to_string()))
        .unwrap();
    let mut request = request;
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie =
        response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, json["csrfToken"].as_str().unwrap().to_owned())
}

async fn account(store: &Store, address: &str) -> i64 {
    store.create_domain("example.org").await.ok();
    store
        .create_account(NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: Some(PASSWORD.into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap()
        .id
}

fn web(store: &Store) -> Web {
    let settings = SmtpSettings {
        hostname: "mail.example.org".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    Web::new(
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
    )
}

fn events(count: usize, padding: usize) -> String {
    let mut ics =
        String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nX-WR-CALNAME:Vereinstermine\r\n");
    for n in 0..count {
        ics.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:termin-{n}@example.org\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261001T{:02}0000Z\r\n\
SUMMARY:Termin {n}\r\nDESCRIPTION:{}\r\nEND:VEVENT\r\n",
            n % 24,
            "x".repeat(padding)
        ));
    }
    ics.push_str("END:VCALENDAR\r\n");
    ics
}

#[tokio::test(flavor = "multi_thread")]
async fn files_feeds_and_other_providers_come_in() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    account(&store, "mini@example.org").await;

    // Another provider, where Leni keeps a calendar and a card; and a feed with a secret address.
    let other_dir = tempfile::tempdir().unwrap();
    let other = Store::open(other_dir.path()).await.unwrap();
    let leni = account(&other, "leni@example.org").await;
    let dav =
        Dav::new(other.clone(), DavSettings { calendar_name: "Privat".into(), addressbook_name: "Adressen".into() });
    let calendar =
        other.dav_collections(leni, DavKind::Calendar, dav.default_collection(DavKind::Calendar)).await.unwrap();
    let split = uwumail_store::split_ics(&events(3, 0).replace("termin-", "leni-"), false);
    other.dav_import(leni, calendar[0].id, split.objects, uwumail_store::DavImportMode::Merge).await.unwrap();
    other
        .dav_collections(leni, DavKind::Addressbook, NewDavCollection::default_address_book("Adressen"))
        .await
        .unwrap();
    let feed = events(2, 0).replace("termin-", "ferien-");
    let internet = dav.router().route(&format!("/{SECRET}/basic.ics"), get(move || async move { feed.clone() }));

    let web = web(&store);
    web.set_dav_transport(Arc::new(Internet(internet)));
    let app = web.router();
    let mini = login(&app, "mini@example.org").await;

    // A file of about 3 MB: more than requests usually may be.
    let big = events(1500, 2000);
    assert!(big.len() > 3_000_000);
    let (status, view) =
        call(&app, "POST", "/api/account/calendars/import?kind=calendar", Some(Body::from(big)), &mini).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["report"]["created"], 1500);
    assert_eq!(view["collection"]["name"], "Vereinstermine");
    let imported = view["collection"]["id"].as_i64().unwrap();
    let path = format!("/api/account/calendars/import?kind=calendar&target={imported}&mode=onlyNew");
    let (_, again) = call(&app, "POST", &path, Some(Body::from(events(2, 0))), &mini).await;
    assert_eq!(again["report"]["skipped"], 2, "different, and only new ones were asked for: {again}");
    assert_eq!(again["report"]["problems"][0]["reason"], "exists");

    let (status, _) = call(
        &app,
        "POST",
        "/api/account/calendars/import?kind=calendar",
        Some(Body::from(vec![b'x'; 21 * 1024 * 1024])),
        &mini,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, refused) =
        call(&app, "POST", "/api/account/calendars/import?kind=addressbook", Some(Body::from(events(1, 0))), &mini)
            .await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("notVCard")));
    let card = "BEGIN:VCARD\r\nVERSION:2.1\r\nFN:Nyu\r\nTEL;CELL:+49 30 1234567\r\nEND:VCARD\r\n";
    let (status, view) =
        call(&app, "POST", "/api/account/calendars/import?kind=addressbook&name=Handy", Some(Body::from(card)), &mini)
            .await;
    assert_eq!((status, &view["report"]["created"]), (StatusCode::OK, &json!(1)), "{view}");

    // A subscription: fetched at once, its address kept to itself.
    let url = format!("webcal://calendar.example.net/{SECRET}/basic.ics");
    let (status, view) = call(
        &app,
        "POST",
        "/api/account/calendar-subscriptions",
        json_body(json!({ "url": url, "name": "Ferien" })),
        &mini,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["report"]["entries"], 2);
    assert!(!view.to_string().contains(SECRET), "the secret address never comes back");
    let subscribed = view["own"].as_array().unwrap().iter().find(|c| c["name"] == "Ferien").unwrap().clone();
    assert_eq!(subscribed["subscription"]["source"], "calendar.example.net/…");
    let sub_id = subscribed["subscription"]["id"].as_i64().unwrap();
    let feed_calendar = subscribed["id"].as_i64().unwrap();
    let (status, refused) = call(
        &app,
        "POST",
        &format!("/api/account/calendars/import?kind=calendar&target={feed_calendar}"),
        Some(Body::from(events(1, 0))),
        &mini,
    )
    .await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("readOnly")));
    let (status, again) =
        call(&app, "POST", "/api/account/calendar-subscriptions", json_body(json!({ "url": url })), &mini).await;
    assert_eq!((status, again["code"].as_str()), (StatusCode::CONFLICT, Some("subscriptionExists")));
    let (status, _) = call(
        &app,
        "POST",
        "/api/account/calendar-subscriptions",
        json_body(json!({ "url": "https://192.168.0.1/cal.ics" })),
        &mini,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "no addresses inside the network");
    let (status, paused) =
        call(&app, "POST", &format!("/api/account/calendar-subscriptions/{sub_id}/refresh"), None, &mini).await;
    assert_eq!((status, paused["code"].as_str()), (StatusCode::CONFLICT, Some("refreshPause")));
    let (status, view) = call(
        &app,
        "PATCH",
        &format!("/api/account/calendar-subscriptions/{sub_id}"),
        json_body(json!({ "intervalSecs": 21600, "name": "Schulferien" })),
        &mini,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let changed = view["own"].as_array().unwrap().iter().find(|c| c["id"] == feed_calendar).unwrap().clone();
    assert_eq!(
        (changed["name"].as_str(), &changed["subscription"]["intervalSecs"]),
        (Some("Schulferien"), &json!(21600))
    );

    // Moving over from the other provider, both kinds in one go.
    let (status, moved) = call(
        &app,
        "POST",
        "/api/account/calendars/remote",
        json_body(json!({
            "address": "leni@example.org",
            "password": PASSWORD,
            "server": "https://dav.example.net/",
            "kinds": ["calendar", "addressbook"],
        })),
        &mini,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let results = moved["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "{moved}");
    let calendar = results.iter().find(|r| r["kind"] == "calendar").unwrap();
    assert_eq!((calendar["name"].as_str(), &calendar["report"]["created"]), (Some("Privat"), &json!(3)));
    let (status, wrong) = call(
        &app,
        "POST",
        "/api/account/calendars/remote",
        json_body(json!({ "address": "leni@example.org", "password": "falsch", "server": "dav.example.net", "kinds": ["calendar"] })),
        &mini,
    )
    .await;
    assert_eq!((status, wrong["code"].as_str()), (StatusCode::CONFLICT, Some("wrongPassword")));
    assert!(!wrong.to_string().contains("falsch"));

    let (status, view) =
        call(&app, "DELETE", &format!("/api/account/calendar-subscriptions/{sub_id}?keep=true"), None, &mini).await;
    assert_eq!(status, StatusCode::OK);
    let kept = view["own"].as_array().unwrap().iter().find(|c| c["id"] == feed_calendar).unwrap().clone();
    assert!(kept["subscription"].is_null() && kept["entries"] == 2, "{kept}");
}
