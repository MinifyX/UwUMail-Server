//! The built-in holiday and week-number calendars (docs/holidays.md), turned on and off through
//! the settings, over JMAP and CalDAV on one store.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::{ClientInfo, Jmap};
use uwumail_smtp::{DeliveryConfig, Smtp, SmtpConfig, SmtpSettings, ToneConfig};
use uwumail_store::{NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";
const MINI: &str = "mini@example.org";
const USING: [&str; 4] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:contacts",
    "urn:ietf:params:jmap:calendars",
    "urn:uwumail:jmap:settings",
];

struct Server {
    router: Router,
    store: Store,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    store
        .create_account(NewAccount {
            address: MINI.into(),
            display_name: "Mini".into(),
            password: Some(PASSWORD.into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: "mail.example.org".into(),
            smtp: SmtpConfig::default(),
            spam: Default::default(),
            delivery: DeliveryConfig::default(),
            tone: ToneConfig::default(),
            server_tls: None,
        },
    )
    .unwrap();
    let dav =
        Dav::new(store.clone(), DavSettings { calendar_name: "Kalender".into(), addressbook_name: "Kontakte".into() });
    Server { router: Jmap::new(smtp).router().merge(dav.router()), store, _dir: dir }
}

impl Server {
    async fn send(&self, method: &str, uri: &str, headers: &[(&str, &str)], body: String) -> (StatusCode, String) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Basic {}", BASE64.encode(format!("{MINI}:{PASSWORD}"))))
            .header(header::HOST, "mail.example.org");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn call(&self, method: &str, mut arguments: Value) -> Value {
        arguments["accountId"] = json!(self.account_id().await);
        let body = json!({ "using": USING, "methodCalls": [[method, arguments, "0"]] }).to_string();
        let (status, body) = self.send("POST", "/jmap/api", &[("content-type", "application/json")], body).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let response: Value = serde_json::from_str(&body).unwrap();
        let reply = &response["methodResponses"][0];
        assert_eq!(reply[0], method, "{reply}");
        reply[1].clone()
    }

    async fn account_id(&self) -> String {
        format!("a{}", self.store.account(MINI).await.unwrap().unwrap().id)
    }

    async fn calendars(&self) -> Vec<Value> {
        self.call("Calendar/get", json!({})).await["list"].as_array().unwrap().clone()
    }

    async fn settings(&self, patch: Value) -> Value {
        self.call("UserSettings/set", json!({ "update": { "singleton": patch } })).await
    }

    async fn builtin(&self, kind: &str) -> Option<Value> {
        self.calendars().await.into_iter().find(|c| c["uwuBuiltin"] == kind)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_turn_the_built_in_calendars_on_and_off() {
    let server = server().await;
    assert!(server.builtin("holidays").await.is_none());

    let set = server
        .settings(json!({ "values/holidays": { "country": "DE", "region": "DE-NW" }, "values/weekNumbers": true }))
        .await;
    assert!(set["updated"].as_object().is_some_and(|u| u.contains_key("singleton")), "{set}");
    let holidays = server.builtin("holidays").await.expect("a holidays calendar");
    assert_eq!(holidays["name"], "Feiertage Nordrhein-Westfalen");
    assert_eq!(holidays["isDefault"], false);
    assert_eq!(holidays["includeInAvailability"], "none");
    assert_eq!(holidays["myRights"]["mayWriteAll"], false);
    assert_eq!(holidays["myRights"]["mayDelete"], false);
    let weeks = server.builtin("weeks").await.expect("a week-number calendar");
    assert_eq!(weeks["name"], "Kalenderwochen");
    let own =
        server.calendars().await.into_iter().find(|c| c["uwuBuiltin"].is_null()).expect("a calendar of one's own");
    assert_eq!(own["isDefault"], true);

    let id = holidays["id"].as_str().unwrap();
    let found = server
        .call(
            "CalendarEvent/query",
            json!({ "filter": { "inCalendar": id, "after": "2026-11-01T00:00:00", "before": "2027-01-01T00:00:00" },
                    "sort": [{ "property": "start" }] }),
        )
        .await;
    let got = server
        .call("CalendarEvent/get", json!({ "ids": found["ids"], "properties": ["title", "start", "alerts"] }))
        .await;
    let titles: Vec<(&str, &str)> = got["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["title"].as_str().unwrap(), e["start"].as_str().unwrap()))
        .collect();
    assert_eq!(
        titles,
        vec![
            ("Allerheiligen", "2026-11-01T00:00:00"),
            ("1. Weihnachtstag", "2026-12-25T00:00:00"),
            ("2. Weihnachtstag", "2026-12-26T00:00:00")
        ]
    );
    assert!(got["list"][0]["alerts"].is_null(), "no reminders");
    let event = got["list"][0]["id"].as_str().unwrap();
    let set = server.call("CalendarEvent/set", json!({ "update": { event: { "title": "Anders" } } })).await;
    assert!(set["notUpdated"][event].is_object(), "{set}");
    let set = server.call("Calendar/set", json!({ "destroy": [id], "onDestroyRemoveEvents": true })).await;
    assert!(set["notDestroyed"][id].is_object(), "{set}");
    // Its name and colour are the person's.
    let set = server.call("Calendar/set", json!({ "update": { id: { "name": "Frei", "color": "#00aa00" } } })).await;
    assert!(set["updated"].as_object().is_some_and(|u| u.contains_key(id)), "{set}");

    // CalDAV reads it and cannot write into it.
    let path = format!("/dav/calendars/{MINI}/holidays/");
    let propfind = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-privilege-set/></d:prop></d:propfind>"#;
    let (status, body) =
        server.send("PROPFIND", &path, &[("depth", "0"), ("content-type", "application/xml")], propfind.into()).await;
    assert_eq!(status.as_u16(), 207, "{body}");
    assert!(body.contains("read/>") && !body.contains("write-content/>") && !body.contains("bind/>"), "{body}");
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:party\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART;VALUE=DATE:20261020\r\nSUMMARY:Party\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let (status, _) =
        server.send("PUT", &format!("{path}party.ics"), &[("content-type", "text/calendar")], ics.into()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = server.send("GET", &format!("{path}h-20261225-18.ics"), &[], String::new()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("SUMMARY:1. Weihnachtstag") && !body.contains("VALARM"), "{body}");

    // Another state: the same calendar, written anew; the name the person chose stays.
    server.settings(json!({ "values/holidays": { "country": "DE", "region": "DE-SN" } })).await;
    let saxony = server.builtin("holidays").await.unwrap();
    assert_eq!((saxony["id"].as_str(), saxony["name"].as_str()), (Some(id), Some("Frei")));
    let (status, _) = server.send("GET", &format!("{path}h-20261118-17.ics"), &[], String::new()).await;
    assert_eq!(status, StatusCode::OK, "Buß- und Bettag");

    // Off again: the calendars go.
    server.settings(json!({ "values/holidays": null, "values/weekNumbers": false })).await;
    assert!(server.builtin("holidays").await.is_none());
    assert!(server.builtin("weeks").await.is_none());

    // A value that names no region is refused, and nothing is made.
    let set = server.settings(json!({ "values/holidays": { "country": "AT", "region": "DE-BY" } })).await;
    assert!(set["notUpdated"]["singleton"].is_object(), "{set}");
    assert!(server.builtin("holidays").await.is_none());
}
