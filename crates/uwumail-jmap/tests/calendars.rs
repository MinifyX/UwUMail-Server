//! JMAP Calendars end to end, next to CalDAV on the same store, the way the webmail, the UwUMail
//! apps and a phone would use them together.

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
const NYU: &str = "nyu@example.org";
const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"];

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

fn basic(login: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{login}:{PASSWORD}")))
}

struct Reply {
    status: StatusCode,
    etag: Option<String>,
    body: String,
}

impl Server {
    async fn send(&self, login: &str, method: &str, uri: &str, headers: &[(&str, &str)], body: String) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, basic(login))
            .header(header::HOST, "mail.example.org");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let etag = response.headers().get(header::ETAG).map(|v| v.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap();
        Reply { status, etag, body: String::from_utf8_lossy(&bytes).into_owned() }
    }

    async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        let body = json!({ "using": USING, "methodCalls": calls }).to_string();
        let reply = self.send(login, "POST", "/jmap/api", &[("content-type", "application/json")], body).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let response: Value = serde_json::from_str(&reply.body).unwrap();
        response["methodResponses"].as_array().unwrap().clone()
    }

    /// One call, its response arguments.
    async fn call(&self, login: &str, method: &str, arguments: Value) -> Value {
        let responses = self.api(login, json!([[method, arguments, "0"]])).await;
        assert_eq!(responses[0][0], method, "{}", responses[0]);
        responses[0][1].clone()
    }

    async fn account_id(&self, login: &str) -> String {
        format!("a{}", self.store.account(login).await.unwrap().unwrap().id)
    }

    /// The default calendar's id, made on the way.
    async fn default_calendar(&self, login: &str) -> String {
        let account = self.account_id(login).await;
        let calendars = self.call(login, "Calendar/get", json!({ "accountId": account })).await;
        calendars["list"].as_array().unwrap().iter().find(|c| c["isDefault"] == true).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn create_event(&self, login: &str, event: Value) -> String {
        let account = self.account_id(login).await;
        let set =
            self.call(login, "CalendarEvent/set", json!({ "accountId": account, "create": { "e": event } })).await;
        set["created"]["e"]["id"].as_str().unwrap_or_else(|| panic!("not created: {set}")).to_owned()
    }

    async fn get_event(&self, login: &str, id: &str, properties: Value) -> Value {
        let account = self.account_id(login).await;
        let got = self
            .call(login, "CalendarEvent/get", json!({ "accountId": account, "ids": [id], "properties": properties }))
            .await;
        got["list"][0].clone()
    }

    async fn state(&self, login: &str) -> String {
        let account = self.account_id(login).await;
        self.call(login, "Calendar/get", json!({ "accountId": account, "ids": [] })).await["state"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

fn timed(calendar: &str, title: &str) -> Value {
    json!({
        "calendarIds": { calendar: true },
        "title": title,
        "start": "2026-10-20T09:00:00",
        "timeZone": "Europe/Berlin",
        "duration": "PT1H",
        "locations": { "1": { "@type": "Location", "name": "Praxis am Markt" } },
        "description": "Impfung"
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_offers_calendars() {
    let server = server().await;
    let reply = server.send(MINI, "GET", "/jmap/session", &[], String::new()).await;
    let session: Value = serde_json::from_str(&reply.body).unwrap();
    let account = server.account_id(MINI).await;
    assert_eq!(session["capabilities"]["urn:ietf:params:jmap:calendars"], json!({}));
    assert_eq!(session["primaryAccounts"]["urn:ietf:params:jmap:calendars"], account);
    assert_eq!(
        session["accounts"][&account]["accountCapabilities"]["urn:ietf:params:jmap:calendars"],
        json!({
            "maxCalendarsPerEvent": 1, "minDateTime": "1900-01-01T00:00:00Z", "maxDateTime": "2200-01-01T00:00:00Z",
            "maxExpandedQueryDuration": "P400D", "maxParticipantsPerEvent": null, "mayCreateCalendar": true
        })
    );
    let identities = server.call(MINI, "ParticipantIdentity/get", json!({ "accountId": account })).await;
    assert_eq!(identities["list"][0]["calendarAddress"], "mailto:mini@example.org");
    assert_eq!(identities["list"][0]["name"], "MINI");
    assert_eq!(identities["list"][0]["isDefault"], true);
    let id = identities["list"][0]["id"].clone();
    let refused = server
        .call(
            MINI,
            "ParticipantIdentity/set",
            json!({ "accountId": account, "update": { id.as_str().unwrap(): { "name": "x" } } }),
        )
        .await;
    assert_eq!(refused["notUpdated"][id.as_str().unwrap()]["type"], "forbidden");
}

#[tokio::test(flavor = "multi_thread")]
async fn calendars_are_made_changed_and_removed() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendars = server.call(MINI, "Calendar/get", json!({ "accountId": account })).await;
    let list = calendars["list"].as_array().unwrap();
    assert_eq!(list.len(), 1, "the default calendar appears like it does over CalDAV");
    assert_eq!(list[0]["name"], "Kalender");
    assert_eq!(list[0]["color"], "#ff4d8d");
    assert_eq!((list[0]["isDefault"].clone(), list[0]["isVisible"].clone()), (json!(true), json!(true)));
    assert_eq!(list[0]["myRights"]["mayDelete"], false, "the only calendar stays");
    let personal = list[0]["id"].as_str().unwrap().to_owned();
    let before = calendars["state"].as_str().unwrap().to_owned();

    let set = server
        .call(
            MINI,
            "Calendar/set",
            json!({
                "accountId": account,
                "create": { "w": { "name": "Arbeit", "color": "#00AA00", "timeZone": "Europe/Berlin", "sortOrder": 2 } },
                "onSuccessSetIsDefault": "#w"
            }),
        )
        .await;
    let work = set["created"]["w"]["id"].as_str().unwrap_or_else(|| panic!("{set}")).to_owned();
    assert_eq!(set["created"]["w"]["isDefault"], true);
    assert_eq!(set["updated"][&personal], json!({ "isDefault": false }));

    let bad = server
        .call(
            MINI,
            "Calendar/set",
            json!({ "accountId": account, "create": { "x": { "name": "", "color": "green", "shareWith": { "p": {} } } } }),
        )
        .await;
    assert_eq!(bad["notCreated"]["x"]["type"], "invalidProperties");
    assert_eq!(bad["notCreated"]["x"]["properties"], json!(["color", "name", "shareWith"]));

    let update = json!({ &work: { "name": "Büro", "isVisible": false, "timeZone": null, "color": "#123" } });
    let updated = server.call(MINI, "Calendar/set", json!({ "accountId": account, "update": update })).await;
    assert!(updated["updated"].get(&work).is_some(), "{updated}");
    let got = server.call(MINI, "Calendar/get", json!({ "accountId": account, "ids": [&work, "c999999"] })).await;
    assert_eq!(got["list"][0]["name"], "Büro");
    assert_eq!(got["list"][0]["isVisible"], false);
    assert_eq!(got["list"][0]["color"], "#112233");
    assert_eq!(got["list"][0]["timeZone"], Value::Null);
    assert_eq!(got["notFound"], json!(["c999999"]));

    let changes = server.call(MINI, "Calendar/changes", json!({ "accountId": account, "sinceState": before })).await;
    assert_eq!(changes["created"], json!([&work]));
    assert_eq!(changes["updated"], json!([&personal]));

    server.create_event(MINI, timed(&work, "Planung")).await;
    let refused = server.call(MINI, "Calendar/set", json!({ "accountId": account, "destroy": [&work] })).await;
    assert_eq!(refused["notDestroyed"][&work]["type"], "calendarHasEvent");
    let gone = server
        .call(MINI, "Calendar/set", json!({ "accountId": account, "destroy": [&work], "onDestroyRemoveEvents": true }))
        .await;
    assert_eq!(gone["destroyed"], json!([&work]));
    let last = server.call(MINI, "Calendar/set", json!({ "accountId": account, "destroy": [&personal] })).await;
    assert_eq!(last["notDestroyed"][&personal]["type"], "forbidden");
    let left = server.call(MINI, "Calendar/get", json!({ "accountId": account })).await;
    assert_eq!(left["list"][0]["isDefault"], true, "the default moved back: {left}");
}

#[tokio::test(flavor = "multi_thread")]
async fn events_are_made_changed_and_deleted() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendar = server.default_calendar(MINI).await;
    let before = server.state(MINI).await;

    let set = server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "accountId": account, "create": { "e": timed(&calendar, "Tierarzt") } }),
        )
        .await;
    let created = &set["created"]["e"];
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["@type"], "Event");
    assert_eq!(created["sequence"], 0);
    assert_eq!(created["isOrigin"], true);
    assert!(created["uid"].as_str().unwrap().len() == 36, "{created}");
    assert!(created["created"].as_str().unwrap().ends_with('Z'));

    let event = server.get_event(MINI, &id, Value::Null).await;
    assert_eq!(event["title"], "Tierarzt");
    assert_eq!(event["start"], "2026-10-20T09:00:00");
    assert_eq!(event["timeZone"], "Europe/Berlin");
    assert_eq!(event["duration"], "PT1H");
    assert_eq!(event["calendarIds"], json!({ &calendar: true }));
    assert_eq!(event["baseEventId"], Value::Null);
    assert_eq!(event["isDraft"], false);
    assert_eq!(event["locations"]["1"]["name"], "Praxis am Markt");
    assert!(event.get("iCalendar").is_none(), "{event}");
    let utc = server.get_event(MINI, &id, json!(["utcStart", "utcEnd", "title"])).await;
    assert_eq!(
        (utc["utcStart"].as_str(), utc["utcEnd"].as_str()),
        (Some("2026-10-20T07:00:00Z"), Some("2026-10-20T08:00:00Z"))
    );
    assert!(utc.get("description").is_none());

    let changes =
        server.call(MINI, "CalendarEvent/changes", json!({ "accountId": account, "sinceState": before })).await;
    assert_eq!(changes["created"], json!([&id]));

    let middle = server.state(MINI).await;
    let patch =
        json!({ &id: { "title": "Tierarzt mit Nyu", "locations/1/name": "Praxis", "keywords": { "katze": true } } });
    let updated = server.call(MINI, "CalendarEvent/set", json!({ "accountId": account, "update": patch })).await;
    assert_eq!(updated["updated"][&id]["sequence"], 1, "{updated}");
    let event = server.get_event(MINI, &id, Value::Null).await;
    assert_eq!(event["title"], "Tierarzt mit Nyu");
    assert_eq!(event["locations"]["1"]["name"], "Praxis");
    assert_eq!(event["sequence"], 1);
    let changes =
        server.call(MINI, "CalendarEvent/changes", json!({ "accountId": account, "sinceState": middle })).await;
    assert_eq!(changes["updated"], json!([&id]));

    // Only the user's own things: no new version.
    let colour = server
        .call(MINI, "CalendarEvent/set", json!({ "accountId": account, "update": { &id: { "color": "orange" } } }))
        .await;
    assert!(colour["updated"][&id].get("sequence").is_none(), "{colour}");

    let bad_patches = server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "accountId": account, "update": {
                &id: { "uid": "other" },
                "v999999": { "title": "nobody" },
            } }),
        )
        .await;
    assert_eq!(bad_patches["notUpdated"][&id]["type"], "invalidProperties");
    assert_eq!(bad_patches["notUpdated"]["v999999"]["type"], "notFound");

    let destroyed =
        server.call(MINI, "CalendarEvent/set", json!({ "accountId": account, "destroy": [&id, "v999999"] })).await;
    assert_eq!(destroyed["destroyed"], json!([&id]));
    assert_eq!(destroyed["notDestroyed"]["v999999"]["type"], "notFound");
    let gone = server.call(MINI, "CalendarEvent/get", json!({ "accountId": account, "ids": [&id] })).await;
    assert_eq!(gone["notFound"], json!([&id]));
}

#[tokio::test(flavor = "multi_thread")]
async fn all_day_events_float_and_last_whole_days() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendar = server.default_calendar(MINI).await;
    let id = server
        .create_event(
            MINI,
            json!({ "calendarIds": { &calendar: true }, "title": "Urlaub", "start": "2026-12-24T00:00:00",
                    "duration": "P3D", "showWithoutTime": true }),
        )
        .await;
    let event = server
        .get_event(MINI, &id, json!(["title", "start", "duration", "showWithoutTime", "timeZone", "utcStart"]))
        .await;
    assert_eq!(event["showWithoutTime"], true);
    assert_eq!(event["duration"], "P3D");
    assert!(event.get("timeZone").is_none_or(Value::is_null), "{event}");
    assert_eq!(event["utcStart"], "2026-12-24T00:00:00Z");

    // Floating: in Berlin the holidays start at midnight Berlin time.
    let query = |tz: &str, after: &str, before: &str| json!({ "accountId": account, "timeZone": tz, "filter": { "after": after, "before": before } });
    let found = server
        .call(MINI, "CalendarEvent/query", query("Europe/Berlin", "2026-12-26T12:00:00", "2026-12-28T00:00:00"))
        .await;
    assert_eq!(found["ids"], json!([&id]));
    let none = server
        .call(MINI, "CalendarEvent/query", query("Europe/Berlin", "2026-12-27T00:00:00", "2026-12-28T00:00:00"))
        .await;
    assert_eq!(none["ids"], json!([]));

    let timed_all_day = server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "accountId": account, "create": { "x": { "calendarIds": { &calendar: true }, "title": "x",
                "start": "2026-12-24T10:00:00", "showWithoutTime": true } } }),
        )
        .await;
    assert_eq!(timed_all_day["notCreated"]["x"]["properties"], json!(["start"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn recurring_events_expand_and_change_one_instance_at_a_time() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendar = server.default_calendar(MINI).await;
    let mut yoga = timed(&calendar, "Yoga");
    yoga["recurrenceRule"] = json!({ "@type": "RecurrenceRule", "frequency": "weekly", "count": 5 });
    let id = server.create_event(MINI, yoga).await;

    let window = json!({ "after": "2026-10-01T00:00:00", "before": "2026-11-12T00:00:00" });
    let expand = json!({ "accountId": account, "filter": window, "expandRecurrences": true, "timeZone": "Europe/Berlin",
                         "sort": [{ "property": "start" }] });
    let found = server.call(MINI, "CalendarEvent/query", expand.clone()).await;
    let instances: Vec<String> =
        found["ids"].as_array().unwrap().iter().map(|i| i.as_str().unwrap().to_owned()).collect();
    let expected: Vec<String> = ["20261020T090000", "20261027T090000", "20261103T090000", "20261110T090000"]
        .iter()
        .map(|r| format!("{id}_{r}"))
        .collect();
    assert_eq!(instances, expected);
    let plain = server
        .call(
            MINI,
            "CalendarEvent/query",
            json!({ "accountId": account, "filter": window, "timeZone": "Europe/Berlin" }),
        )
        .await;
    assert_eq!(plain["ids"], json!([&id]), "without expanding, the series once");

    let instance = server
        .get_event(
            MINI,
            &instances[1],
            json!(["title", "start", "recurrenceId", "recurrenceRule", "recurrenceOverrides"]),
        )
        .await;
    assert_eq!(instance["id"], instances[1]);
    assert_eq!(instance["baseEventId"], id);
    assert_eq!(instance["start"], "2026-10-27T09:00:00");
    assert_eq!(instance["recurrenceId"], "2026-10-27T09:00:00");
    assert_eq!(instance["recurrenceRule"], Value::Null);
    assert_eq!(instance["recurrenceOverrides"], Value::Null);
    let utc = server.get_event(MINI, &instances[1], json!(["utcStart"])).await;
    assert_eq!(utc["utcStart"], "2026-10-27T08:00:00Z", "winter time by then");
    let nowhere = server
        .call(MINI, "CalendarEvent/get", json!({ "accountId": account, "ids": [format!("{id}_20261028T090000")] }))
        .await;
    assert_eq!(nowhere["notFound"].as_array().unwrap().len(), 1, "not an instance of the series");

    // One instance moves and gets its own title; another one is taken out.
    let patch = json!({ &instances[1]: { "title": "Yoga im Park", "start": "2026-10-27T10:00:00" } });
    let set = server
        .call(MINI, "CalendarEvent/set", json!({ "accountId": account, "update": patch, "destroy": [&instances[2]] }))
        .await;
    assert!(set["updated"].get(&instances[1]).is_some(), "{set}");
    assert_eq!(set["destroyed"], json!([&instances[2]]));
    let series = server.get_event(MINI, &id, json!(["recurrenceOverrides", "sequence"])).await;
    assert_eq!(
        series["recurrenceOverrides"],
        json!({
            "2026-10-27T09:00:00": { "title": "Yoga im Park", "start": "2026-10-27T10:00:00" },
            "2026-11-03T09:00:00": { "excluded": true }
        })
    );
    let found = server.call(MINI, "CalendarEvent/query", expand.clone()).await;
    assert_eq!(found["ids"], json!([&instances[0], &instances[1], &instances[3]]));
    let moved = server.get_event(MINI, &instances[1], json!(["title", "start"])).await;
    assert_eq!((moved["title"].as_str(), moved["start"].as_str()), (Some("Yoga im Park"), Some("2026-10-27T10:00:00")));

    // A change to the series reaches every instance that did not change it itself.
    server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "accountId": account, "update": { &id: { "title": "Yoga!", "duration": "PT90M" } } }),
        )
        .await;
    let first = server.get_event(MINI, &instances[0], json!(["title", "duration"])).await;
    assert_eq!((first["title"].as_str(), first["duration"].as_str()), (Some("Yoga!"), Some("PT90M")));
    let moved = server.get_event(MINI, &instances[1], json!(["title", "duration"])).await;
    assert_eq!(moved["title"], "Yoga im Park");

    let too_long = server
        .api(
            MINI,
            json!([["CalendarEvent/query", { "accountId": account, "expandRecurrences": true,
                    "filter": { "after": "2026-01-01T00:00:00", "before": "2027-06-01T00:00:00" } }, "0"]]),
        )
        .await;
    assert_eq!(too_long[0][1]["type"], "expandDurationTooLarge", "{}", too_long[0]);
    let open = server
        .api(MINI, json!([["CalendarEvent/query", { "accountId": account, "expandRecurrences": true, "filter": { "after": "2026-01-01T00:00:00" } }, "0"]]))
        .await;
    assert_eq!(open[0][1]["type"], "invalidArguments");
    let cannot = server
        .api(MINI, json!([["CalendarEvent/queryChanges", { "accountId": account, "sinceQueryState": "1" }, "0"]]))
        .await;
    assert_eq!(cannot[0][1]["type"], "cannotCalculateChanges");
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_filter_sort_and_page() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let personal = server.default_calendar(MINI).await;
    let set = server
        .call(MINI, "Calendar/set", json!({ "accountId": account, "create": { "w": { "name": "Arbeit" } } }))
        .await;
    let work = set["created"]["w"]["id"].as_str().unwrap().to_owned();
    let mut ids = Vec::new();
    for (calendar, title, day, place) in [
        (&personal, "Zahnarzt", "2026-10-05", "Praxis Süd"),
        (&work, "Planung Q4", "2026-10-06", "Raum 3"),
        (&personal, "Kino mit Nyu", "2026-10-07", "Filmpalast"),
    ] {
        let mut event = timed(calendar, title);
        event["start"] = json!(format!("{day}T18:00:00"));
        event["locations"] = json!({ "l": { "@type": "Location", "name": place } });
        event["uid"] = json!(format!("{}@example.net", title.to_lowercase().replace(' ', "-")));
        ids.push(server.create_event(MINI, event).await);
    }
    let query = |filter: Value| json!({ "accountId": account, "filter": filter, "sort": [{ "property": "start" }] });
    let run = |filter: Value| {
        let server = &server;
        let query = query(filter);
        async move { server.call(MINI, "CalendarEvent/query", query).await["ids"].clone() }
    };
    assert_eq!(run(json!({ "inCalendar": &work })).await, json!([&ids[1]]));
    assert_eq!(run(json!({ "title": "kino" })).await, json!([&ids[2]]));
    assert_eq!(run(json!({ "location": "praxis" })).await, json!([&ids[0]]));
    assert_eq!(run(json!({ "text": "Impfung raum" })).await, json!([&ids[1]]));
    assert_eq!(run(json!({ "text": "\"mit Nyu\"" })).await, json!([&ids[2]]));
    assert_eq!(run(json!({ "uid": "zahnarzt@example.net" })).await, json!([&ids[0]]));
    assert_eq!(run(json!({ "after": "2026-10-06T00:00:00", "before": "2026-10-07T00:00:00" })).await, json!([&ids[1]]));
    assert_eq!(
        run(json!({ "operator": "OR", "conditions": [{ "title": "zahn" }, { "inCalendar": &work }] })).await,
        json!([&ids[0], &ids[1]])
    );
    assert_eq!(
        run(json!({ "operator": "NOT", "conditions": [{ "inCalendar": &work }] })).await,
        json!([&ids[0], &ids[2]])
    );

    let descending = json!({ "accountId": account, "sort": [{ "property": "start", "isAscending": false }],
                             "position": 1, "limit": 1, "calculateTotal": true });
    let page = server.call(MINI, "CalendarEvent/query", descending).await;
    assert_eq!(
        (page["ids"].clone(), page["total"].clone(), page["position"].clone()),
        (json!([&ids[1]]), json!(3), json!(1))
    );
    let unknown = server
        .api(MINI, json!([["CalendarEvent/query", { "accountId": account, "filter": { "colour": "red" } }, "0"]]))
        .await;
    assert_eq!(unknown[0][1]["type"], "unsupportedFilter");
    let unsorted = server
        .api(MINI, json!([["CalendarEvent/query", { "accountId": account, "sort": [{ "property": "title" }] }, "0"]]))
        .await;
    assert_eq!(unsorted[0][1]["type"], "unsupportedSort");
}

const PHONE_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example Phone//EN\r\nBEGIN:VEVENT\r\n\
UID:phone-1@example.net\r\nDTSTAMP:20260917T080000Z\r\nDTSTART;TZID=Europe/Berlin:20261012T150000\r\n\
DTEND;TZID=Europe/Berlin:20261012T160000\r\nSUMMARY:Friseur\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const SYNC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:sync-collection xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:sync-token>TOKEN</d:sync-token>
<d:sync-level>1</d:sync-level><d:prop><d:getetag/><c:calendar-data/></d:prop></d:sync-collection>"#;

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap_or_else(|| panic!("{start} not in {text}")) + start.len();
    let to = text[from..].find(end).unwrap_or_else(|| panic!("{end} not after {start} in {text}")) + from;
    &text[from..to]
}

#[tokio::test(flavor = "multi_thread")]
async fn caldav_and_jmap_see_each_others_changes() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendar = server.default_calendar(MINI).await;
    let collection = "/dav/calendars/mini@example.org/personal/";
    let before = server.state(MINI).await;

    // A phone stores an event over CalDAV: JMAP clients hear about it and can read it.
    let router = server.router.clone();
    let push = tokio::spawn(async move {
        let mut request = Request::get("/jmap/eventsource/?types=CalendarEvent&closeafter=state&ping=0")
            .header(header::AUTHORIZATION, basic(MINI))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = router.oneshot(request).await.unwrap();
        String::from_utf8(to_bytes(response.into_body(), 1024 * 1024).await.unwrap().to_vec()).unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let put = server
        .send(
            MINI,
            "PUT",
            &format!("{collection}phone-1.ics"),
            &[("content-type", "text/calendar")],
            PHONE_EVENT.into(),
        )
        .await;
    assert_eq!(put.status, StatusCode::CREATED, "{}", put.body);
    let pushed = tokio::time::timeout(std::time::Duration::from_secs(10), push).await.unwrap().unwrap();
    assert!(pushed.contains("\"CalendarEvent\""), "{pushed}");

    let changes =
        server.call(MINI, "CalendarEvent/changes", json!({ "accountId": account, "sinceState": before })).await;
    let phone_id = changes["created"][0].as_str().unwrap_or_else(|| panic!("{changes}")).to_owned();
    let event = server.get_event(MINI, &phone_id, Value::Null).await;
    assert_eq!(event["title"], "Friseur");
    assert_eq!(event["uid"], "phone-1@example.net");
    assert_eq!(event["calendarIds"], json!({ &calendar: true }));

    // The webmail makes one: the phone's next sync brings it along, with data CalDAV accepts.
    let sync = server.send(MINI, "REPORT", collection, &[("depth", "1")], SYNC.replace("TOKEN", "")).await;
    let token = between(&sync.body, "<d:sync-token>", "</d:sync-token>").to_owned();
    let web_id = server.create_event(MINI, timed(&calendar, "Elternabend")).await;
    let sync = server.send(MINI, "REPORT", collection, &[("depth", "1")], SYNC.replace("TOKEN", &token)).await;
    assert_eq!(sync.status, StatusCode::MULTI_STATUS, "{}", sync.body);
    assert!(sync.body.contains("SUMMARY:Elternabend"), "{}", sync.body);
    assert!(sync.body.contains("TZID:Europe/Berlin"), "the time zone comes along: {}", sync.body);
    assert!(!sync.body.contains("Friseur"), "only what changed: {}", sync.body);
    let href = between(&sync.body, "<d:href>", "</d:href>").to_owned();
    let fetched = server.send(MINI, "GET", &href, &[], String::new()).await;
    assert_eq!(fetched.status, StatusCode::OK);
    let etag = fetched.etag.unwrap();
    let content = fetched.body;
    // The phone can store it back unchanged: CalDAV accepts what JMAP wrote.
    let again = server
        .send(MINI, "PUT", &href, &[("content-type", "text/calendar"), ("if-match", &etag)], content.clone())
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT, "{}", again.body);

    // A JMAP change moves the ETag and the sync token.
    let token = between(&sync.body, "<d:sync-token>", "</d:sync-token>").to_owned();
    server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "accountId": account, "update": { &web_id: { "title": "Elternabend 2b" } } }),
        )
        .await;
    let fetched = server.send(MINI, "GET", &href, &[], String::new()).await;
    assert_ne!(fetched.etag.as_deref(), Some(etag.as_str()));
    assert!(fetched.body.contains("SUMMARY:Elternabend 2b"));
    let sync = server.send(MINI, "REPORT", collection, &[("depth", "1")], SYNC.replace("TOKEN", &token)).await;
    assert!(sync.body.contains("Elternabend 2b"), "{}", sync.body);

    // The phone deletes, a new calendar appears: JMAP sees both.
    let middle = server.state(MINI).await;
    let deleted = server.send(MINI, "DELETE", &format!("{collection}phone-1.ics"), &[], String::new()).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let made = server.send(MINI, "MKCALENDAR", "/dav/calendars/mini@example.org/sport/", &[], String::new()).await;
    assert_eq!(made.status, StatusCode::CREATED);
    let changes =
        server.call(MINI, "CalendarEvent/changes", json!({ "accountId": account, "sinceState": &middle })).await;
    assert_eq!(changes["destroyed"], json!([&phone_id]));
    let calendars = server.call(MINI, "Calendar/changes", json!({ "accountId": account, "sinceState": &middle })).await;
    assert_eq!(calendars["created"].as_array().unwrap().len(), 1, "{calendars}");

    // A list of reminders from an iPhone holds tasks, not events: it is not a calendar here.
    let reminders = r#"<?xml version="1.0" encoding="utf-8"?>
<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop><d:displayname>Erinnerungen</d:displayname>
<c:supported-calendar-component-set><c:comp name="VTODO"/></c:supported-calendar-component-set></d:prop></d:set></c:mkcalendar>"#;
    let made = server.send(MINI, "MKCALENDAR", "/dav/calendars/mini@example.org/tasks/", &[], reminders.into()).await;
    assert_eq!(made.status, StatusCode::CREATED);
    let list = server.call(MINI, "Calendar/get", json!({ "accountId": account })).await;
    let names: Vec<&str> = list["list"].as_array().unwrap().iter().filter_map(|c| c["name"].as_str()).collect();
    assert_eq!(names.len(), 2, "{list}");
    assert!(!names.contains(&"Erinnerungen"), "{list}");
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_reaches_into_another_account() {
    let server = server().await;
    let mini = server.account_id(MINI).await;
    let nyu = server.account_id(NYU).await;
    let calendar = server.default_calendar(MINI).await;
    let id = server.create_event(MINI, timed(&calendar, "Geheim")).await;
    let mut series = timed(&calendar, "Serie");
    series["recurrenceRule"] = json!({ "frequency": "daily", "count": 3 });
    let series_id = server.create_event(MINI, series).await;
    let instance = format!("{series_id}_20261021T090000");
    server.default_calendar(NYU).await;

    let foreign = server.api(NYU, json!([["CalendarEvent/get", { "accountId": &mini, "ids": [&id] }, "0"]])).await;
    assert_eq!(foreign[0][1]["type"], "accountNotFound");
    let got = server.call(NYU, "CalendarEvent/get", json!({ "accountId": &nyu, "ids": [&id, &instance] })).await;
    assert_eq!(got["list"], json!([]));
    assert_eq!(got["notFound"], json!([&id, &instance]));
    let all = server.call(NYU, "CalendarEvent/get", json!({ "accountId": &nyu, "ids": null })).await;
    assert_eq!(all["list"], json!([]));
    let found = server.call(NYU, "CalendarEvent/query", json!({ "accountId": &nyu })).await;
    assert_eq!(found["ids"], json!([]));

    let set = server
        .call(
            NYU,
            "CalendarEvent/set",
            json!({
                "accountId": &nyu,
                "create": { "x": timed(&calendar, "Kuckucksei") },
                "update": { &id: { "title": "gehackt" }, &instance: { "title": "gehackt" } },
                "destroy": [&id, &instance]
            }),
        )
        .await;
    assert_eq!(set["notCreated"]["x"]["properties"], json!(["calendarIds"]));
    assert_eq!(set["notUpdated"][&id]["type"], "notFound");
    assert_eq!(set["notUpdated"][&instance]["type"], "notFound");
    assert_eq!(set["notDestroyed"][&id]["type"], "notFound");
    assert_eq!(set["notDestroyed"][&instance]["type"], "notFound");
    let calendars = server
        .call(
            NYU,
            "Calendar/set",
            json!({ "accountId": &nyu, "update": { &calendar: { "name": "meins" } }, "destroy": [&calendar] }),
        )
        .await;
    assert_eq!(calendars["notUpdated"][&calendar]["type"], "notFound");
    assert_eq!(calendars["notDestroyed"][&calendar]["type"], "notFound");
    let theirs = server.call(NYU, "Calendar/get", json!({ "accountId": &nyu, "ids": [&calendar] })).await;
    assert_eq!(theirs["notFound"], json!([&calendar]));

    // Over CalDAV, too, the other account's home stays closed.
    let dav = server
        .send(NYU, "PROPFIND", "/dav/calendars/mini@example.org/personal/", &[("depth", "1")], String::new())
        .await;
    assert_eq!(dav.status, StatusCode::FORBIDDEN);
    let still = server.get_event(MINI, &id, json!(["title"])).await;
    assert_eq!(still["title"], "Geheim");
}

#[tokio::test(flavor = "multi_thread")]
async fn events_stay_within_limits() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendar = server.default_calendar(MINI).await;
    let with = |change: Value| {
        let mut event = timed(&calendar, "Grenze");
        for (key, value) in change.as_object().unwrap() {
            event[key] = value.clone();
        }
        event
    };
    let cases = [
        ("zone", with(json!({ "timeZone": "Europe/Atlantis" })), "invalidProperties"),
        ("early", with(json!({ "start": "1850-01-01T00:00:00" })), "invalidProperties"),
        ("late", with(json!({ "start": "2199-12-31T23:00:00", "duration": "P2D" })), "invalidProperties"),
        ("title", with(json!({ "title": "x".repeat(1025) })), "invalidProperties"),
        ("method", with(json!({ "method": "request" })), "invalidProperties"),
        ("big", with(json!({ "description": "Katzen ".repeat(200_000) })), "tooLarge"),
        ("rule", with(json!({ "recurrenceRule": { "frequency": "daily", "interval": 0 } })), "invalidProperties"),
        ("draft", with(json!({ "isDraft": true })), "invalidProperties"),
        ("two", with(json!({ "calendarIds": { &calendar: true, "c999999": true } })), "invalidProperties"),
        ("none", with(json!({ "calendarIds": {} })), "invalidProperties"),
    ];
    let create: serde_json::Map<String, Value> = cases.iter().map(|(k, v, _)| ((*k).to_owned(), v.clone())).collect();
    let set = server.call(MINI, "CalendarEvent/set", json!({ "accountId": account, "create": create })).await;
    for (key, _, kind) in cases {
        assert_eq!(set["notCreated"][key]["type"], kind, "{key}: {}", set["notCreated"][key]);
    }
    assert_eq!(set["created"], Value::Null, "{set}");

    let invite = with(json!({ "participants": {
        "a": { "@type": "Participant", "calendarAddress": "mailto:someone@example.net", "roles": { "attendee": true } }
    } }));
    let refused = server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "accountId": account, "create": { "i": invite.clone() }, "sendSchedulingMessages": true }),
        )
        .await;
    assert_eq!(refused["notCreated"]["i"]["type"], "noSupportedScheduleMethods");
    let stored =
        server.call(MINI, "CalendarEvent/set", json!({ "accountId": account, "create": { "i": invite } })).await;
    assert!(stored["created"]["i"]["id"].is_string(), "without scheduling it is just data: {stored}");

    let bad_zone = server
        .api(MINI, json!([["CalendarEvent/query", { "accountId": account, "timeZone": "Mars/Base" }, "0"]]))
        .await;
    assert_eq!(bad_zone[0][1]["type"], "invalidArguments");

    // Accounts without calendars have no calendar methods either.
    server
        .store
        .update_account(
            MINI,
            uwumail_store::AccountUpdate {
                protocols: Some(uwumail_store::Protocols { caldav: false, ..Default::default() }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let off = server.api(MINI, json!([["Calendar/get", { "accountId": account }, "0"]])).await;
    assert_eq!(off[0][1]["type"], "accountNotSupportedByMethod");
    let reply = server.send(MINI, "GET", "/jmap/session", &[], String::new()).await;
    assert!(!reply.body.contains("urn:ietf:params:jmap:calendars"), "{}", reply.body);
}

/// security-audit-0.7.0 S-45: checking an event looked at every changed instance against a whole
/// copy of the series, overrides included, so the work grew with the square of their number. Their
/// number is bounded now, and a series at the bound is still taken.
#[tokio::test(flavor = "multi_thread")]
async fn a_series_has_a_bounded_number_of_changed_instances() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendar = server.default_calendar(MINI).await;
    let series = |instances: usize| {
        let mut event = timed(&calendar, "Täglich");
        event["recurrenceRule"] = json!({ "frequency": "daily", "count": 5000 });
        let start = chrono::NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let overrides: serde_json::Map<String, Value> = (0..instances)
            .map(|day| {
                let date = start + chrono::Duration::days(day as i64);
                (format!("{}T09:00:00", date.format("%Y-%m-%d")), json!({ "title": format!("Tag {day}") }))
            })
            .collect();
        event["recurrenceOverrides"] = Value::Object(overrides);
        event
    };
    let set = server
        .call(
            MINI,
            "CalendarEvent/set",
            json!({ "accountId": account, "create": { "many": series(1001), "enough": series(1000) } }),
        )
        .await;
    assert_eq!(set["notCreated"]["many"]["type"], "invalidProperties", "{}", set["notCreated"]);
    assert_eq!(set["notCreated"]["many"]["properties"], json!(["recurrenceOverrides"]));
    let enough = set["created"]["enough"]["id"].as_str().unwrap_or_else(|| panic!("{set}"));
    let found = server
        .call(MINI, "CalendarEvent/query", json!({ "accountId": account, "filter": { "title": "Tag 999" } }))
        .await;
    assert_eq!(found["ids"], json!([enough]), "{found}");
}

/// Hours are exact, days nominal (RFC 8984 section 1.4.6): an event over the night the clocks go
/// back ends five real hours after it starts, in /get, in query windows and for each instance.
#[tokio::test(flavor = "multi_thread")]
async fn exact_durations_end_on_time_when_the_clocks_change() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let calendar = server.default_calendar(MINI).await;
    let mut night = timed(&calendar, "Nachtschicht");
    night["start"] = json!("2026-10-24T23:30:00");
    night["duration"] = json!("PT5H");
    let id = server.create_event(MINI, night.clone()).await;
    let got = server.get_event(MINI, &id, json!(["utcStart", "utcEnd"])).await;
    assert_eq!(got["utcStart"], "2026-10-24T21:30:00Z");
    assert_eq!(got["utcEnd"], "2026-10-25T02:30:00Z", "{got}");

    let query = |after: &str, expand: bool| {
        json!({
            "accountId": account,
            "filter": { "after": after, "before": "2026-10-26T00:00:00" },
            "expandRecurrences": expand
        })
    };
    let before_end = server.call(MINI, "CalendarEvent/query", query("2026-10-25T02:15:00", false)).await;
    assert_eq!(before_end["ids"], json!([id]), "{before_end}");
    let after_end = server.call(MINI, "CalendarEvent/query", query("2026-10-25T02:45:00", false)).await;
    assert_eq!(after_end["ids"], json!([]), "{after_end}");

    // The same for one instance of a weekly series.
    let destroyed = server.call(MINI, "CalendarEvent/set", json!({ "accountId": account, "destroy": [id] })).await;
    assert_eq!(destroyed["destroyed"], json!([id]));
    let mut series = night;
    series["start"] = json!("2026-10-17T23:30:00");
    series["recurrenceRule"] = json!({ "frequency": "weekly", "count": 3 });
    let base = server.create_event(MINI, series).await;
    let instance = format!("{base}_20261024T233000");
    let inside = server.call(MINI, "CalendarEvent/query", query("2026-10-25T02:15:00", true)).await;
    assert_eq!(inside["ids"], json!([instance]), "{inside}");
    let outside = server.call(MINI, "CalendarEvent/query", query("2026-10-25T02:45:00", true)).await;
    assert_eq!(outside["ids"], json!([]), "{outside}");
    let got = server.get_event(MINI, &instance, json!(["utcEnd"])).await;
    assert_eq!(got["utcEnd"], "2026-10-25T02:30:00Z", "{got}");
}
