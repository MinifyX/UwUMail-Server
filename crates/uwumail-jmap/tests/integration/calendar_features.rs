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
const USING: [&str; 4] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:calendars",
    "urn:ietf:params:jmap:principals",
    "urn:ietf:params:jmap:principals:availability",
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

    async fn principal_id(&self, login: &str) -> String {
        format!("p{}", self.store.account(login).await.unwrap().unwrap().id)
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

fn alert(offset: &str) -> Value {
    json!({ "@type": "Alert", "trigger": { "@type": "OffsetTrigger", "offset": offset }, "action": "display" })
}

#[tokio::test(flavor = "multi_thread")]
async fn default_alerts_reach_events_and_caldav() {
    let server = server().await;
    let calendar = server.default_calendar(MINI).await;
    let set = server
        .call(
            MINI,
            "Calendar/set",
            json!({ "update": { &calendar: { "defaultAlertsWithTime": { "d1": alert("-PT15M") } } } }),
        )
        .await;
    assert!(set["updated"].get(&calendar).is_some(), "{set}");
    assert_eq!(server.calendar(MINI, &calendar).await["defaultAlertsWithTime"]["d1"]["trigger"]["offset"], "-PT15M");

    // An event using them carries them, for phones to ring.
    let mut event = timed(&calendar, "Zahnarzt");
    event["useDefaultAlerts"] = json!(true);
    let id = server.create(MINI, event).await;
    let got = server.event(MINI, &id).await;
    assert_eq!(got["useDefaultAlerts"], true);
    assert_eq!(got["alerts"]["d1"]["trigger"]["offset"], "-PT15M", "{got}");
    let uid = got["uid"].as_str().unwrap().to_owned();
    assert!(server.caldav_object(MINI, &uid).await.contains("TRIGGER:-PT15M"));

    // New defaults reach the events that use them.
    server
        .call(
            MINI,
            "Calendar/set",
            json!({ "update": { &calendar: { "defaultAlertsWithTime": { "d1": alert("-PT30M") } } } }),
        )
        .await;
    let ics = server.caldav_object(MINI, &uid).await;
    assert!(ics.contains("TRIGGER:-PT30M") && !ics.contains("-PT15M"), "{ics}");

    // CalDAV clients see and set them as default alarms.
    let path = "/dav/calendars/mini@example.org/personal/";
    let body = "<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><c:default-alarm-vevent-datetime/></d:prop></d:propfind>";
    let reply = server.send(MINI, "PROPFIND", path, &[("depth", "0")], body.into()).await;
    assert!(reply.body.contains("BEGIN:VALARM") && reply.body.contains("TRIGGER:-PT30M"), "{}", reply.body);
    let patch = "<?xml version=\"1.0\"?><d:propertyupdate xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:set><d:prop><c:default-alarm-vevent-date>BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT12H\r\nEND:VALARM\r\n</c:default-alarm-vevent-date></d:prop></d:set></d:propertyupdate>";
    let reply = server.send(MINI, "PROPPATCH", path, &[], patch.into()).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS, "{}", reply.body);
    let without = &server.calendar(MINI, &calendar).await["defaultAlertsWithoutTime"];
    let alerts: Vec<&Value> = without.as_object().unwrap().values().collect();
    assert_eq!(alerts.len(), 1, "{without}");
    assert_eq!(alerts[0]["trigger"]["offset"], "-PT12H");

    // Ids are unique across one's calendars.
    let taken = server
        .call(
            MINI,
            "Calendar/set",
            json!({ "create": { "w": { "name": "Arbeit", "defaultAlertsWithTime": { "d1": alert("-PT5M") } } } }),
        )
        .await;
    assert_eq!(taken["notCreated"]["w"]["type"], "invalidProperties", "{taken}");

    // Someone the calendar is shared with has their own defaults.
    server.share_with_nyu(json!({ "mayReadItems": true })).await;
    server
        .call(
            NYU,
            "Calendar/set",
            json!({ "update": { &calendar: { "defaultAlertsWithTime": { "n1": alert("-PT1H") } } } }),
        )
        .await;
    server.call(NYU, "CalendarEvent/set", json!({ "update": { &id: { "useDefaultAlerts": true } } })).await;
    let theirs = server.event(NYU, &id).await;
    assert_eq!(theirs["alerts"]["n1"]["trigger"]["offset"], "-PT1H", "{theirs}");
    assert!(theirs["alerts"].get("d1").is_none());
    assert_eq!(server.event(MINI, &id).await["alerts"]["d1"]["trigger"]["offset"], "-PT30M");
}

fn with_nyu(mut event: Value) -> Value {
    event["participants"] = json!({
        "mini": { "@type": "Participant", "calendarAddress": format!("mailto:{MINI}"), "roles": { "owner": true, "attendee": true }, "participationStatus": "accepted" },
        "nyu": { "@type": "Participant", "calendarAddress": format!("mailto:{NYU}"), "roles": { "attendee": true }, "participationStatus": "needs-action", "expectReply": true }
    });
    event
}

#[tokio::test(flavor = "multi_thread")]
async fn drafts_tell_nobody_until_they_are_events() {
    let server = server().await;
    let calendar = server.default_calendar(MINI).await;
    let mut draft = with_nyu(timed(&calendar, "Planung"));
    draft["isDraft"] = json!(true);
    let created = server
        .call(MINI, "CalendarEvent/set", json!({ "create": { "d": draft }, "sendSchedulingMessages": true }))
        .await;
    assert_eq!(created["created"]["d"]["isDraft"], true, "{created}");
    let id = created["created"]["d"]["id"].as_str().unwrap().to_owned();
    assert_eq!(server.event(MINI, &id).await["isDraft"], true);
    let invited =
        || async { server.call(NYU, "CalendarEvent/query", json!({ "filter": { "title": "Planung" } })).await };
    assert_eq!(invited().await["ids"], json!([]), "Nyu hears nothing of a draft");

    // CalDAV clients see it as an event; what they store sends nothing either.
    let uid = server.event(MINI, &id).await["uid"].as_str().unwrap().to_owned();
    let ics = server.caldav_object(MINI, &uid).await;
    assert!(ics.contains("SUMMARY:Planung"));
    let name = server.store.calendar_events(server.store.account(MINI).await.unwrap().unwrap().id, None).await.unwrap()
        [0]
    .name
    .clone();
    let changed = ics.replace("SUMMARY:Planung", "SUMMARY:Planung 2");
    let put = server
        .send(
            MINI,
            "PUT",
            &format!("/dav/calendars/{MINI}/personal/{name}"),
            &[("content-type", "text/calendar")],
            changed,
        )
        .await;
    assert!(put.status.is_success(), "{}", put.body);
    assert_eq!(invited().await["ids"], json!([]));
    assert_eq!(server.event(MINI, &id).await["isDraft"], true, "still a draft");

    let back = server.call(MINI, "CalendarEvent/set", json!({ "update": { &id: { "isDraft": true } } })).await;
    assert!(back["updated"].get(&id).is_some(), "a draft stays one: {back}");
    let published = server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "update": { &id: { "isDraft": false } }, "sendSchedulingMessages": true }),
        )
        .await;
    assert_eq!(published["updated"][&id]["isDraft"], false, "{published}");
    assert_eq!(invited().await["ids"].as_array().unwrap().len(), 1, "now Nyu is invited");
    let again = server.call(MINI, "CalendarEvent/set", json!({ "update": { &id: { "isDraft": true } } })).await;
    assert_eq!(again["notUpdated"][&id]["type"], "invalidProperties", "{again}");
}

const CUSTOM_ZONE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nBEGIN:VTIMEZONE\r\nTZID:Büro\r\n\
BEGIN:STANDARD\r\nDTSTART:16011028T030000\r\nRRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\r\nTZOFFSETFROM:+0200\r\n\
TZOFFSETTO:+0100\r\nTZNAME:Winter\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010325T020000\r\n\
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:Sommer\r\nEND:DAYLIGHT\r\n\
END:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:zone@example.org\r\nDTSTAMP:20260901T080000Z\r\n\
DTSTART;TZID=Büro:20261020T090000\r\nDURATION:PT1H\r\nSUMMARY:Standup\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn custom_time_zones_travel_between_caldav_and_jmap() {
    let server = server().await;
    let calendar = server.default_calendar(MINI).await;
    let put =
        server.send(MINI, "PUT", &format!("/dav/calendars/{MINI}/personal/zone.ics"), &[], CUSTOM_ZONE.into()).await;
    assert_eq!(put.status, StatusCode::CREATED, "{}", put.body);
    let found = server.call(MINI, "CalendarEvent/query", json!({ "filter": { "uid": "zone@example.org" } })).await;
    let id = found["ids"][0].as_str().unwrap().to_owned();
    let got = server
        .call(MINI, "CalendarEvent/get", json!({ "ids": [&id], "properties": ["timeZone", "timeZones", "utcStart"] }))
        .await;
    let event = &got["list"][0];
    assert_eq!(event["timeZone"], "/Büro", "{event}");
    assert_eq!(event["timeZones"]["/Büro"]["daylight"][0]["offsetTo"], "+0200");
    assert_eq!(event["utcStart"], "2026-10-20T07:00:00Z");

    // Expanded, the instance after the clocks changed is an hour later in UTC.
    let expanded = server
        .call(
            MINI,
            "CalendarEvent/query",
            json!({ "filter": { "after": "2026-10-26T00:00:00", "before": "2026-11-01T00:00:00" }, "expandRecurrences": true }),
        )
        .await;
    let instance = expanded["ids"][0].as_str().unwrap().to_owned();
    let got = server.call(MINI, "CalendarEvent/get", json!({ "ids": [&instance], "properties": ["utcStart"] })).await;
    assert_eq!(got["list"][0]["utcStart"], "2026-10-27T08:00:00Z", "{got}");

    // Changed over JMAP, CalDAV clients still find their zone.
    let set = server.call(MINI, "CalendarEvent/set", json!({ "update": { &id: { "title": "Daily" } } })).await;
    assert!(set["updated"].get(&id).is_some(), "{set}");
    let ics = server.caldav_object(MINI, "zone@example.org").await;
    assert_eq!(ics.matches("BEGIN:VTIMEZONE").count(), 1, "{ics}");
    assert!(ics.contains("TZID:Büro") && ics.contains("TZNAME:Sommer") && ics.contains("SUMMARY:Daily"), "{ics}");

    // A JMAP client brings its own zone.
    let mut event = timed(&calendar, "Eigene Zone");
    event["timeZone"] = json!("/Mars");
    event["timeZones"] = json!({ "/Mars": { "@type": "TimeZone", "tzId": "Mars",
        "standard": [{ "@type": "TimeZoneRule", "start": "2000-01-01T00:00:00", "offsetFrom": "+0300", "offsetTo": "+0300" }] } });
    let created = server.create(MINI, event).await;
    let got =
        server.call(MINI, "CalendarEvent/get", json!({ "ids": [&created], "properties": ["utcStart", "uid"] })).await;
    assert_eq!(got["list"][0]["utcStart"], "2026-10-20T06:00:00Z", "{got}");
    let ics = server.caldav_object(MINI, got["list"][0]["uid"].as_str().unwrap()).await;
    assert!(ics.contains("TZID:Mars") && ics.contains("TZOFFSETTO:+0300"), "{ics}");
}

const TWO_INSTANCES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nBEGIN:VEVENT\r\n\
UID:only-some@example.org\r\nDTSTAMP:20260901T080000Z\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261027T090000\r\n\
DTSTART;TZID=Europe/Berlin:20261027T100000\r\nDURATION:PT1H\r\nSUMMARY:One\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\n\
UID:only-some@example.org\r\nDTSTAMP:20260901T080000Z\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261103T090000\r\n\
DTSTART;TZID=Europe/Berlin:20261103T090000\r\nDURATION:PT1H\r\nSUMMARY:Two\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn single_instances_without_their_series() {
    let server = server().await;
    let calendar = server.default_calendar(MINI).await;
    let put =
        server.send(MINI, "PUT", &format!("/dav/calendars/{MINI}/personal/some.ics"), &[], TWO_INSTANCES.into()).await;
    assert_eq!(put.status, StatusCode::CREATED, "{}", put.body);
    let found = server.call(MINI, "CalendarEvent/query", json!({ "filter": { "uid": "only-some@example.org" } })).await;
    assert_eq!(found["ids"].as_array().unwrap().len(), 1, "{found}");
    let id = found["ids"][0].as_str().unwrap().to_owned();
    let first = server.event(MINI, &id).await;
    assert_eq!((&first["title"], &first["recurrenceId"]), (&json!("One"), &json!("2026-10-27T09:00:00")), "{first}");
    let other = format!("{id}_20261103T090000");
    let second = server.event(MINI, &other).await;
    assert_eq!((&second["title"], &second["baseEventId"]), (&json!("Two"), &json!(&id)), "{second}");
    let expanded = server
        .call(
            MINI,
            "CalendarEvent/query",
            json!({ "filter": { "after": "2026-10-01T00:00:00", "before": "2026-12-01T00:00:00" }, "expandRecurrences": true }),
        )
        .await;
    assert_eq!(expanded["ids"], json!([&id, &other]));

    let set = server
        .call(MINI, "CalendarEvent/set", json!({ "update": { &id: { "title": "Eins" }, &other: { "title": "Zwei" } } }))
        .await;
    assert!(set["updated"].get(&id).is_some() && set["updated"].get(&other).is_some(), "{set}");
    let ics = server.caldav_object(MINI, "only-some@example.org").await;
    assert!(ics.contains("SUMMARY:Eins") && ics.contains("SUMMARY:Zwei"), "{ics}");
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2, "no series appeared: {ics}");
    assert_eq!(ics.matches("RECURRENCE-ID").count(), 2, "{ics}");

    let gone = server.call(MINI, "CalendarEvent/set", json!({ "destroy": [&other] })).await;
    assert_eq!(gone["destroyed"], json!([&other]), "{gone}");
    let ics = server.caldav_object(MINI, "only-some@example.org").await;
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 1, "{ics}");
    assert_eq!(server.event(MINI, &id).await["title"], "Eins");

    // A JMAP client may keep a single instance too.
    let mut single = timed(&calendar, "Einmal");
    single["uid"] = json!("single@example.org");
    single["recurrenceId"] = json!("2026-10-20T09:00:00");
    single["recurrenceIdTimeZone"] = json!("Europe/Berlin");
    let created = server.create(MINI, single).await;
    assert_eq!(server.event(MINI, &created).await["recurrenceId"], "2026-10-20T09:00:00");
    let ics = server.caldav_object(MINI, "single@example.org").await;
    assert!(ics.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261020T090000"), "{ics}");
}

fn at(calendar: &str, title: &str, start: &str, extra: Value) -> Value {
    let mut event = timed(calendar, title);
    event["start"] = json!(start);
    event["timeZone"] = json!("Etc/UTC");
    for (key, value) in extra.as_object().unwrap() {
        event[key] = value.clone();
    }
    event
}

#[tokio::test(flavor = "multi_thread")]
async fn availability_shows_when_people_are_busy_and_no_more() {
    let server = server().await;
    let calendar = server.default_calendar(MINI).await;
    for (title, start, extra) in [
        ("A", "2026-10-20T09:00:00", json!({})),
        ("A2", "2026-10-20T09:30:00", json!({})),
        ("B", "2026-10-20T11:00:00", json!({ "status": "tentative" })),
        ("Frei", "2026-10-20T13:00:00", json!({ "freeBusyStatus": "free" })),
        ("Privat", "2026-10-20T15:00:00", json!({ "privacy": "private" })),
        ("Geheim", "2026-10-20T17:00:00", json!({ "privacy": "secret" })),
        ("Abgesagt", "2026-10-20T19:00:00", json!({ "status": "cancelled" })),
        ("Serie", "2026-10-19T07:00:00", json!({ "recurrenceRule": { "frequency": "daily", "count": 3 } })),
    ] {
        server.create(MINI, at(&calendar, title, start, extra)).await;
    }
    let mini = server.principal_id(MINI).await;
    let principal = server.call(NYU, "Principal/get", json!({ "ids": [&mini] })).await;
    let capability = &principal["list"][0]["capabilities"]["urn:ietf:params:jmap:calendars"];
    assert_eq!(capability["mayGetAvailability"], true, "{principal}");
    assert_eq!(capability["calendarAddress"], format!("mailto:{MINI}"));
    assert_eq!(capability["accountId"], Value::Null);

    let window = json!({ "id": &mini, "utcStart": "2026-10-20T00:00:00Z", "utcEnd": "2026-10-21T00:00:00Z" });
    let busy = server.call(NYU, "Principal/getAvailability", window.clone()).await;
    let periods: Vec<(String, String, String)> = busy["list"]
        .as_array()
        .unwrap_or_else(|| panic!("{busy}"))
        .iter()
        .map(|p| {
            assert_eq!(p["event"], Value::Null);
            (
                p["utcStart"].as_str().unwrap().into(),
                p["utcEnd"].as_str().unwrap().into(),
                p["busyStatus"].as_str().unwrap().into(),
            )
        })
        .collect();
    let period =
        |a: &str, b: &str, s: &str| (format!("2026-10-20T{a}:00Z"), format!("2026-10-20T{b}:00Z"), s.to_owned());
    assert_eq!(
        periods,
        vec![
            period("07:00", "08:00", "confirmed"),
            period("09:00", "10:30", "confirmed"),
            period("11:00", "12:00", "tentative"),
            period("15:00", "16:00", "confirmed"),
        ]
    );

    // With the calendar shared, the events come along, but not a private one.
    server.share_with_nyu(json!({ "mayReadItems": true })).await;
    let mut details = window.clone();
    details["showDetails"] = json!(true);
    details["eventProperties"] = json!(["title"]);
    let busy = server.call(NYU, "Principal/getAvailability", details).await;
    let titles: Vec<Value> = busy["list"].as_array().unwrap().iter().map(|p| p["event"]["title"].clone()).collect();
    assert_eq!(titles, vec![json!("Serie"), json!("A"), json!("A2"), json!("B"), Value::Null], "{busy}");
    assert_eq!(busy["list"][0]["accountId"], server.account_id(NYU).await);

    // Mini's calendar can stop making her busy; Nyu's copy of it can start making him busy.
    server.call(MINI, "Calendar/set", json!({ "update": { &calendar: { "includeInAvailability": "none" } } })).await;
    assert_eq!(server.call(NYU, "Principal/getAvailability", window.clone()).await["list"], json!([]));
    server.call(NYU, "Calendar/set", json!({ "update": { &calendar: { "includeInAvailability": "all" } } })).await;
    let mut nyus = window.clone();
    nyus["id"] = json!(server.principal_id(NYU).await);
    assert_eq!(server.call(MINI, "Principal/getAvailability", nyus).await["list"].as_array().unwrap().len(), 4);

    let mut long = window.clone();
    long["utcEnd"] = json!("2028-01-01T00:00:00Z");
    assert_eq!(server.call(NYU, "Principal/getAvailability", long).await["type"], "tooLarge");
    let mut nobody = window.clone();
    nobody["id"] = json!("p999999");
    assert_eq!(server.call(NYU, "Principal/getAvailability", nobody).await["type"], "notFound");

    // A masked address leads to nobody, neither here nor over CalDAV (security-audit-0.16.0
    // PROTOCOLS-L4).
    let policy = uwumail_store::DomainMaskedPolicy { mode: uwumail_store::MaskedMode::Own, ..Default::default() };
    server.store.set_domain_masked_policy("example.org", policy).await.unwrap();
    let mini_id = server.store.account(MINI).await.unwrap().unwrap().id;
    let new = uwumail_store::NewMaskedAddress {
        domain: None,
        state: Some(uwumail_store::MaskedState::Enabled),
        for_domain: String::new(),
        description: "Shop".into(),
        url: None,
        email_prefix: None,
        created_by: "test".into(),
    };
    let masked = server.store.create_masked_address(mini_id, new).await.unwrap().email;
    let found = server
        .call(NYU, "Principal/query", json!({ "filter": { "calendarAddress": format!("mailto:{masked}") } }))
        .await;
    assert_eq!(found["ids"], json!([]), "{found}");
    let found =
        server.call(NYU, "Principal/query", json!({ "filter": { "calendarAddress": format!("mailto:{MINI}") } })).await;
    assert_eq!(found["ids"], json!([&mini]));
    let request = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nMETHOD:REQUEST\r\nBEGIN:VFREEBUSY\r\nUID:fb\r\n\
DTSTAMP:20260917T080000Z\r\nDTSTART:20261020T000000Z\r\nDTEND:20261021T000000Z\r\nORGANIZER:mailto:{NYU}\r\n\
ATTENDEE:mailto:{masked}\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n"
    );
    let outbox = format!("/dav/calendars/{NYU}/outbox/");
    let answer = server.send(NYU, "POST", &outbox, &[("content-type", "text/calendar")], request).await;
    assert!(answer.body.contains("3.7;Invalid calendar user") && !answer.body.contains("FREEBUSY"), "{}", answer.body);
}

async fn notifications(server: &Server, login: &str) -> Vec<Value> {
    let got = server.call(login, "CalendarEventNotification/get", json!({})).await;
    got["list"].as_array().unwrap_or_else(|| panic!("{got}")).clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn others_changes_leave_notifications() {
    let server = server().await;
    let calendar = server.share_with_nyu(json!({ "mayReadItems": true, "mayWriteAll": true })).await;
    let mini_state = server.call(MINI, "CalendarEventNotification/get", json!({ "ids": [] })).await["state"].clone();

    // Nyu adds an event to Mini's calendar: Mini hears who did what, Nyu hears nothing.
    let id = server.create(NYU, timed(&calendar, "Von Nyu")).await;
    let mine = notifications(&server, MINI).await;
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0]["type"], "created");
    assert_eq!(mine[0]["calendarEventId"], id);
    assert_eq!(mine[0]["event"]["title"], "Von Nyu");
    assert_eq!(mine[0]["changedBy"]["principalId"], server.principal_id(NYU).await);
    assert_eq!(mine[0]["changedBy"]["name"], "NYU");
    assert_eq!(mine[0]["isDraft"], false);
    assert!(notifications(&server, NYU).await.is_empty());
    let changes = server.call(MINI, "CalendarEventNotification/changes", json!({ "sinceState": mini_state })).await;
    assert_eq!(changes["created"], json!([&mine[0]["id"]]), "{changes}");

    // Mini changes it: Nyu gets the patch.
    server.call(MINI, "CalendarEvent/set", json!({ "update": { &id: { "title": "Von Mini" } } })).await;
    let theirs = notifications(&server, NYU).await;
    assert_eq!(theirs.len(), 1);
    assert_eq!((&theirs[0]["type"], &theirs[0]["event"]["title"]), (&json!("updated"), &json!("Von Nyu")));
    assert_eq!(theirs[0]["eventPatch"]["title"], "Von Mini", "{}", theirs[0]);

    // Nyu deletes it over CalDAV.
    let uid = server.event(MINI, &id).await["uid"].as_str().unwrap().to_owned();
    let mini_id = server.store.account(MINI).await.unwrap().unwrap().id;
    let name =
        server.store.calendar_events(mini_id, None).await.unwrap().into_iter().find(|e| e.uid == uid).unwrap().name;
    let path = format!("/dav/calendars/{NYU}/shared~{}/{name}", &calendar[1..]);
    assert_eq!(server.send(NYU, "DELETE", &path, &[], String::new()).await.status, StatusCode::NO_CONTENT);
    let mine = notifications(&server, MINI).await;
    assert_eq!(mine.last().unwrap()["type"], "destroyed");
    assert_eq!(mine.last().unwrap()["event"]["title"], "Von Mini");

    // What Mini keeps private is nobody else's news.
    let mut private = timed(&calendar, "Privat");
    private["privacy"] = json!("private");
    server.create(MINI, private).await;
    assert_eq!(notifications(&server, NYU).await.len(), 1);

    // Scheduling: the invitation and the answer are news too.
    let invite = with_nyu(timed(&calendar, "Kaffee"));
    let created = server
        .call(MINI, "CalendarEvent/set", json!({ "create": { "k": invite }, "sendSchedulingMessages": true }))
        .await;
    assert!(created["created"]["k"]["id"].is_string(), "{created}");
    let own_calendar = server.default_calendar(NYU).await;
    let found = server
        .call(NYU, "CalendarEvent/query", json!({ "filter": { "title": "Kaffee", "inCalendar": own_calendar } }))
        .await;
    let copy = found["ids"][0].as_str().unwrap_or_else(|| panic!("{found}")).to_owned();
    let invitation = notifications(&server, NYU)
        .await
        .into_iter()
        .find(|n| n["calendarEventId"] == copy.as_str())
        .expect("Nyu hears of the invitation");
    assert_eq!(invitation["type"], "created");
    assert_eq!(invitation["changedBy"]["calendarAddress"], format!("mailto:{MINI}"));
    assert_eq!(invitation["changedBy"]["principalId"], server.principal_id(MINI).await);
    let participants = server.event(NYU, &copy).await["participants"].as_object().unwrap().clone();
    let key = participants
        .iter()
        .find(|(_, p)| p["calendarAddress"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&format!("mailto:{NYU}"))))
        .map(|(k, _)| k.clone())
        .unwrap();
    server
        .call(
            NYU,
            "CalendarEvent/set",
            json!({ "update": { &copy: { format!("participants/{key}/participationStatus"): "accepted" } }, "sendSchedulingMessages": true }),
        )
        .await;
    let answer = notifications(&server, MINI).await.pop().unwrap();
    assert_eq!((&answer["type"], &answer["changedBy"]["email"]), (&json!("updated"), &json!(NYU)), "{answer}");

    // Queries and dismissing.
    let query = server
        .call(
            NYU,
            "CalendarEventNotification/query",
            json!({ "filter": { "type": "updated" }, "sort": [{ "property": "created", "isAscending": false }] }),
        )
        .await;
    assert_eq!(query["ids"].as_array().unwrap().len(), 1, "{query}");
    let dismissed = server.call(NYU, "CalendarEventNotification/set", json!({ "destroy": [&query["ids"][0]] })).await;
    assert_eq!(dismissed["destroyed"], json!([&query["ids"][0]]), "{dismissed}");
    let refused = server.call(NYU, "CalendarEventNotification/set", json!({ "create": { "x": {} } })).await;
    assert_eq!(refused["notCreated"]["x"]["type"], "forbidden");
}
