//! The parts of JMAP Calendars beyond events and sharing: per-user properties of shared calendars,
//! default alerts, drafts, custom time zones, single instances, availability, notifications,
//! copying and parsing events, and query changes of expanded queries — each next to CalDAV on the
//! same store.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::{ClientInfo, Jmap};
use uwumail_store::{NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";
const MINI: &str = "mini@example.org";
const NYU: &str = "nyu@example.org";
const USING: [&str; 3] =
    ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars", "urn:ietf:params:jmap:principals"];

struct Server {
    router: Router,
    store: Store,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for user in ["mini", "nyu"] {
        store
            .create_account(NewAccount {
                address: format!("{user}@example.org"),
                display_name: user.to_uppercase(),
                password: Some(PASSWORD.into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let smtp = crate::common::smtp(&store);
    let dav = Dav::new(store.clone(), DavSettings { calendar_name: "Kalender".into(), addressbook_name: "K".into() })
        .with_scheduling(smtp.clone());
    Server { router: Jmap::new(smtp).router().merge(dav.router()), store, _dir: dir }
}

struct Reply {
    status: StatusCode,
    body: String,
}

impl Server {
    async fn send(&self, login: &str, method: &str, uri: &str, headers: &[(&str, &str)], body: String) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Basic {}", BASE64.encode(format!("{login}:{PASSWORD}"))))
            .header(header::HOST, "mail.example.org");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap();
        Reply { status, body: String::from_utf8_lossy(&bytes).into_owned() }
    }

    /// Method calls in one request, each with the login's account filled in.
    async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        let account = self.account_id(login).await;
        let mut calls = calls.as_array().unwrap().clone();
        for call in &mut calls {
            if call[1].get("accountId").is_none() {
                call[1]["accountId"] = json!(account);
            }
        }
        let body = json!({ "using": USING, "methodCalls": calls }).to_string();
        let reply = self.send(login, "POST", "/jmap/api", &[("content-type", "application/json")], body).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let response: Value = serde_json::from_str(&reply.body).unwrap();
        response["methodResponses"].as_array().unwrap().clone()
    }

    /// One call; its response arguments, or the error.
    async fn call(&self, login: &str, method: &str, arguments: Value) -> Value {
        let responses = self.api(login, json!([[method, arguments, "0"]])).await;
        responses[0][1].clone()
    }

    async fn account_id(&self, login: &str) -> String {
        format!("a{}", self.store.account(login).await.unwrap().unwrap().id)
    }

    async fn calendar(&self, login: &str, id: &str) -> Value {
        self.call(login, "Calendar/get", json!({ "ids": [id] })).await["list"][0].clone()
    }

    async fn default_calendar(&self, login: &str) -> String {
        let list = self.call(login, "Calendar/get", json!({})).await["list"].as_array().unwrap().clone();
        list.iter().find(|c| c["isDefault"] == true).unwrap()["id"].as_str().unwrap().to_owned()
    }

    async fn create(&self, login: &str, event: Value) -> String {
        let set = self.call(login, "CalendarEvent/set", json!({ "create": { "e": event } })).await;
        set["created"]["e"]["id"].as_str().unwrap_or_else(|| panic!("not created: {set}")).to_owned()
    }

    async fn event(&self, login: &str, id: &str) -> Value {
        self.call(login, "CalendarEvent/get", json!({ "ids": [id] })).await["list"][0].clone()
    }

    async fn state(&self, login: &str) -> String {
        self.call(login, "CalendarEvent/get", json!({ "ids": [] })).await["state"].as_str().unwrap().to_owned()
    }

    /// Shares Mini's default calendar with Nyu.
    async fn share_with_nyu(&self, rights: Value) -> String {
        let calendar = self.default_calendar(MINI).await;
        let shared =
            self.call(MINI, "Calendar/set", json!({ "update": { &calendar: { "shareWith": { NYU: rights } } } })).await;
        assert!(shared["updated"].get(&calendar).is_some(), "{shared}");
        calendar
    }

    /// The CalDAV object of an event, found by its uid.
    async fn caldav_object(&self, login: &str, uid: &str) -> String {
        let account = self.store.account(login).await.unwrap().unwrap().id;
        let event = self.store.calendar_events(account, None).await.unwrap();
        event.into_iter().find(|e| e.uid == uid).map(|e| e.content).unwrap_or_default()
    }
}

fn timed(calendar: &str, title: &str) -> Value {
    json!({
        "calendarIds": { calendar: true },
        "title": title,
        "start": "2026-10-20T09:00:00",
        "timeZone": "Europe/Berlin",
        "duration": "PT1H"
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_calendars_and_events_keep_per_user_properties_apart() {
    let server = server().await;
    let calendar = server.share_with_nyu(json!({ "mayReadItems": true })).await;
    let owners = server.calendar(MINI, &calendar).await;

    // Nyu's own name, colour, order, visibility and availability, which Mini never sees.
    let own = json!({ "name": "Minis", "color": "#00ff00", "sortOrder": 3, "isVisible": false, "includeInAvailability": "all" });
    let set = server.call(NYU, "Calendar/set", json!({ "update": { &calendar: own } })).await;
    assert!(set["updated"].get(&calendar).is_some(), "{set}");
    let theirs = server.calendar(NYU, &calendar).await;
    assert_eq!(
        (
            &theirs["name"],
            &theirs["color"],
            &theirs["sortOrder"],
            &theirs["isVisible"],
            &theirs["includeInAvailability"]
        ),
        (&json!("Minis"), &json!("#00ff00"), &json!(3), &json!(false), &json!("all"))
    );
    assert_eq!(server.calendar(MINI, &calendar).await, owners, "the owner's calendar is the same");
    let shared = format!("/dav/calendars/{NYU}/shared~{}/", &calendar[1..]);
    let body = "<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:displayname/></d:prop></d:propfind>";
    let reply = server.send(NYU, "PROPFIND", &shared, &[("depth", "0")], body.into()).await;
    assert!(reply.body.contains("Minis"), "Nyu's CalDAV clients see Nyu's name: {}", reply.body);

    // Per-user properties of an event: Mini's stay in the event, Nyu's apart.
    let mut event = timed(&calendar, "Yoga");
    event["color"] = json!("red");
    event["keywords"] = json!({ "sport": true });
    event["alerts"] = json!({ "a": { "@type": "Alert", "trigger": { "@type": "OffsetTrigger", "offset": "-PT15M" } } });
    let id = server.create(MINI, event).await;
    let seen = server.event(NYU, &id).await;
    assert_eq!((seen.get("color"), seen.get("alerts"), seen.get("keywords")), (None, None, None), "{seen}");
    let mini_state = server.state(MINI).await;
    let patch = json!({ "color": "blue", "keywords": { "mine": true }, "alerts": { "n": { "@type": "Alert", "trigger": { "@type": "OffsetTrigger", "offset": "-PT1H" } } } });
    let set = server.call(NYU, "CalendarEvent/set", json!({ "update": { &id: patch } })).await;
    assert!(set["updated"].get(&id).is_some(), "a reader changes their own properties: {set}");
    let seen = server.event(NYU, &id).await;
    assert_eq!((&seen["color"], &seen["keywords"]), (&json!("blue"), &json!({ "mine": true })));
    assert_eq!(seen["alerts"]["n"]["trigger"]["offset"], "-PT1H");
    let owners = server.event(MINI, &id).await;
    assert_eq!((&owners["color"], &owners["keywords"]), (&json!("red"), &json!({ "sport": true })));
    assert_eq!(owners["alerts"]["a"]["trigger"]["offset"], "-PT15M");
    assert_eq!(server.state(MINI).await, mini_state, "Mini hears nothing of it");
    let ics = server.caldav_object(MINI, owners["uid"].as_str().unwrap()).await;
    assert!(ics.contains("COLOR:red") && !ics.contains("blue"), "{ics}");

    // Everything else stays the owner's to change.
    let refused = server.call(NYU, "CalendarEvent/set", json!({ "update": { &id: { "title": "Nyus Yoga" } } })).await;
    assert_eq!(refused["notUpdated"][&id]["type"], "forbidden", "{refused}");

    // Private events show only their times to others, and cannot be changed by them; secret ones
    // are not there for them.
    let mut private = timed(&calendar, "Arzt");
    private["privacy"] = json!("private");
    private["description"] = json!("Befund");
    let private = server.create(MINI, private).await;
    let seen = server.event(NYU, &private).await;
    assert_eq!((seen.get("title"), seen.get("description")), (None, None), "{seen}");
    assert_eq!(seen["start"], "2026-10-20T09:00:00");
    let refused = server.call(NYU, "CalendarEvent/set", json!({ "update": { &private: { "color": "blue" } } })).await;
    assert_eq!(refused["notUpdated"][&private]["type"], "forbidden", "{refused}");
    let found = server.call(NYU, "CalendarEvent/query", json!({ "filter": { "text": "Befund" } })).await;
    assert_eq!(found["ids"], json!([]));
    let mut secret = timed(&calendar, "Geheim");
    secret["privacy"] = json!("secret");
    let secret = server.create(MINI, secret).await;
    let got = server.call(NYU, "CalendarEvent/get", json!({ "ids": [&secret] })).await;
    assert_eq!(got["notFound"], json!([&secret]));

    // Leaving the calendar forgets what Nyu kept.
    server.call(NYU, "Calendar/set", json!({ "destroy": [&calendar] })).await;
    server.share_with_nyu(json!({ "mayReadItems": true })).await;
    assert_eq!(server.calendar(NYU, &calendar).await["name"], "Kalender");
    assert!(server.event(NYU, &id).await.get("color").is_none());
}
