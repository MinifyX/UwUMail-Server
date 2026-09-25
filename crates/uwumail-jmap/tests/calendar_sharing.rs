//! Shared calendars and scheduling over JMAP Calendars, the way the webmail uses them: sharing a
//! calendar with someone of the server, what they may do with it, pushes reaching them, and
//! invitations and answers between two people with `sendSchedulingMessages`.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;
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
    Server { router: Jmap::new(smtp).router(), store, _dir: dir }
}

impl Server {
    async fn call(&self, login: &str, method: &str, mut arguments: Value) -> Value {
        arguments["accountId"] = json!(self.account_id(login).await);
        let body = json!({ "using": USING, "methodCalls": [[method, arguments, "0"]] }).to_string();
        let mut request = Request::builder()
            .method("POST")
            .uri("/jmap/api")
            .header(header::AUTHORIZATION, format!("Basic {}", BASE64.encode(format!("{login}:{PASSWORD}"))))
            .header(header::HOST, "mail.example.org")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1 << 24).await.unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        let reply = &response["methodResponses"][0];
        assert_eq!(reply[0], method, "{reply}");
        reply[1].clone()
    }

    async fn account_id(&self, login: &str) -> String {
        format!("a{}", self.store.account(login).await.unwrap().unwrap().id)
    }

    async fn calendars(&self, login: &str) -> Vec<Value> {
        self.call(login, "Calendar/get", json!({})).await["list"].as_array().unwrap().clone()
    }

    async fn default_calendar(&self, login: &str) -> String {
        let list = self.calendars(login).await;
        list.iter().find(|c| c["isDefault"] == true).unwrap()["id"].as_str().unwrap().to_owned()
    }
}

fn event(calendar: &str, title: &str) -> Value {
    json!({
        "calendarIds": { calendar: true },
        "title": title,
        "start": "2026-10-20T09:00:00",
        "timeZone": "Europe/Berlin",
        "duration": "PT1H"
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn calendars_are_shared_with_people_of_the_server() {
    let server = server().await;
    let personal = server.default_calendar(MINI).await;
    let created =
        server.call(MINI, "CalendarEvent/set", json!({ "create": { "e": event(&personal, "Tierarzt") } })).await;
    let event_id = created["created"]["e"]["id"].as_str().unwrap().to_owned();
    let nyu_id = server.account_id(NYU).await;
    let nyu_state = server.call(NYU, "Calendar/get", json!({ "ids": [] })).await["state"].as_str().unwrap().to_owned();
    let mut pushes = server.store.subscribe_changes();

    // Shared by address, read only; the answer names Nyu by principal id.
    let shared = server
        .call(
            MINI,
            "Calendar/set",
            json!({ "update": { &personal: { "shareWith": { NYU: { "mayReadItems": true, "mayReadFreeBusy": true } } } } }),
        )
        .await;
    assert!(shared["updated"].get(&personal).is_some(), "{shared}");
    let pushed = pushes.recv().await.unwrap();
    let nyu_account = server.store.account(NYU).await.unwrap().unwrap().id;
    let mut heard = pushed.account_id == nyu_account;
    while let Ok(change) = pushes.try_recv() {
        heard |= change.account_id == nyu_account;
    }
    assert!(heard, "Nyu hears about the new calendar");
    let mine = server.calendars(MINI).await;
    assert_eq!(mine[0]["shareWith"][&nyu_id]["mayReadItems"], true);
    assert_eq!(mine[0]["shareWith"][&nyu_id]["mayWriteAll"], false);

    let theirs = server.calendars(NYU).await;
    let seen = theirs.iter().find(|c| c["id"] == personal.as_str()).expect("the shared calendar is listed");
    assert_eq!(seen["myRights"]["mayWriteAll"], false);
    assert_eq!(seen["myRights"]["mayReadItems"], true);
    assert_eq!(seen["isDefault"], false);
    assert_eq!(seen["uwuSharedBy"]["email"], MINI);
    assert_eq!(seen["shareWith"], Value::Null);
    let changes = server.call(NYU, "Calendar/changes", json!({ "sinceState": nyu_state })).await;
    assert_eq!(changes["created"], json!([&personal]));
    let events = server.call(NYU, "CalendarEvent/get", json!({ "ids": [&event_id] })).await;
    assert_eq!(events["list"][0]["title"], "Tierarzt");

    let refused =
        server.call(NYU, "CalendarEvent/set", json!({ "create": { "n": event(&personal, "Nyus Termin") } })).await;
    assert_eq!(refused["notCreated"]["n"]["type"], "forbidden", "{refused}");
    let renamed = server.call(NYU, "Calendar/set", json!({ "update": { &personal: { "name": "Meins" } } })).await;
    assert_eq!(renamed["notUpdated"][&personal]["type"], "forbidden");

    // With writing allowed, Nyu's events land in Mini's calendar.
    server
        .call(
            MINI,
            "Calendar/set",
            json!({ "update": { &personal: { format!("shareWith/{nyu_id}"): { "mayReadItems": true, "mayWriteAll": true } } } }),
        )
        .await;
    let written =
        server.call(NYU, "CalendarEvent/set", json!({ "create": { "n": event(&personal, "Nyus Termin") } })).await;
    let written_id = written["created"]["n"]["id"].as_str().unwrap_or_else(|| panic!("{written}")).to_owned();
    let for_mini = server.call(MINI, "CalendarEvent/get", json!({ "ids": [&written_id] })).await;
    assert_eq!(for_mini["list"][0]["title"], "Nyus Termin");

    // Leaving a shared calendar is destroying it for oneself.
    let left = server.call(NYU, "Calendar/set", json!({ "destroy": [&personal] })).await;
    assert_eq!(left["destroyed"], json!([&personal]), "{left}");
    assert!(server.calendars(NYU).await.iter().all(|c| c["id"] != personal.as_str()));
    assert_eq!(server.calendars(MINI).await[0]["shareWith"], Value::Null);
    let nobody = server
        .call(
            MINI,
            "Calendar/set",
            json!({ "update": { &personal: { "shareWith": { "nobody@example.org": { "mayReadItems": true } } } } }),
        )
        .await;
    assert_eq!(nobody["notUpdated"][&personal]["type"], "invalidProperties");
}

#[tokio::test(flavor = "multi_thread")]
async fn invitations_and_answers_with_scheduling_messages() {
    let server = server().await;
    let personal = server.default_calendar(MINI).await;
    let mut invite = event(&personal, "Kaffee");
    invite["participants"] = json!({
        "mini": { "@type": "Participant", "calendarAddress": format!("mailto:{MINI}"), "roles": { "owner": true, "attendee": true }, "participationStatus": "accepted" },
        "nyu": { "@type": "Participant", "calendarAddress": format!("mailto:{NYU}"), "roles": { "attendee": true }, "participationStatus": "needs-action", "expectReply": true }
    });
    let created = server
        .call(MINI, "CalendarEvent/set", json!({ "create": { "k": invite }, "sendSchedulingMessages": true }))
        .await;
    let mini_event = created["created"]["k"]["id"].as_str().unwrap_or_else(|| panic!("{created}")).to_owned();
    assert_eq!(created["created"]["k"]["organizerCalendarAddress"], format!("mailto:{MINI}"));

    // The invitation is in Nyu's own calendar, waiting for an answer.
    let found = server.call(NYU, "CalendarEvent/query", json!({ "filter": { "title": "Kaffee" } })).await;
    let nyu_event = found["ids"][0].as_str().unwrap_or_else(|| panic!("{found}")).to_owned();
    assert_ne!(nyu_event, mini_event);
    let got = server.call(NYU, "CalendarEvent/get", json!({ "ids": [&nyu_event] })).await;
    let copy = &got["list"][0];
    assert_eq!(copy["isOrigin"], false);
    let participants = copy["participants"].as_object().unwrap();
    let (key, me) = participants
        .iter()
        .find(|(_, p)| p["calendarAddress"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&format!("mailto:{NYU}"))))
        .unwrap();
    assert_eq!(me["participationStatus"], "needs-action");

    // Nyu accepts; Mini's copy knows.
    let answered = server
        .call(
            NYU,
            "CalendarEvent/set",
            json!({ "update": { &nyu_event: { format!("participants/{key}/participationStatus"): "accepted" } }, "sendSchedulingMessages": true }),
        )
        .await;
    assert!(answered["updated"].get(&nyu_event).is_some(), "{answered}");
    let organizer = server.call(MINI, "CalendarEvent/get", json!({ "ids": [&mini_event] })).await;
    let accepted = organizer["list"][0]["participants"]
        .as_object()
        .unwrap()
        .values()
        .find(|p| p["calendarAddress"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&format!("mailto:{NYU}"))))
        .unwrap()["participationStatus"]
        .clone();
    assert_eq!(accepted, "accepted");

    // Without sendSchedulingMessages nothing is sent: Mini's change stays Mini's.
    server
        .call(MINI, "CalendarEvent/set", json!({ "update": { &mini_event: { "title": "Kaffee und Kuchen" } } }))
        .await;
    let unchanged = server.call(NYU, "CalendarEvent/get", json!({ "ids": [&nyu_event] })).await;
    assert_eq!(unchanged["list"][0]["title"], "Kaffee");

    // Deleting with sendSchedulingMessages cancels.
    server.call(MINI, "CalendarEvent/set", json!({ "destroy": [&mini_event], "sendSchedulingMessages": true })).await;
    let cancelled = server.call(NYU, "CalendarEvent/get", json!({ "ids": [&nyu_event] })).await;
    assert_eq!(cancelled["list"][0]["status"], "cancelled", "{cancelled}");
}

async fn contacts_call(server: &Server, login: &str, method: &str, mut arguments: Value) -> Value {
    arguments["accountId"] = json!(server.account_id(login).await);
    let using = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:contacts"];
    let body = json!({ "using": using, "methodCalls": [[method, arguments, "0"]] }).to_string();
    let mut request = Request::builder()
        .method("POST")
        .uri("/jmap/api")
        .header(header::AUTHORIZATION, format!("Basic {}", BASE64.encode(format!("{login}:{PASSWORD}"))))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = server.router.clone().oneshot(request).await.unwrap();
    let bytes = to_bytes(response.into_body(), 1 << 24).await.unwrap();
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    response["methodResponses"][0][1].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn address_books_are_shared_the_same_way() {
    let server = server().await;
    let books = contacts_call(&server, MINI, "AddressBook/get", json!({})).await;
    let book = books["list"][0]["id"].as_str().unwrap().to_owned();
    let card = json!({ "@type": "Card", "addressBookIds": { &book: true }, "name": { "full": "Leni Katze" } });
    let created = contacts_call(&server, MINI, "ContactCard/set", json!({ "create": { "c": card } })).await;
    let card_id = created["created"]["c"]["id"].as_str().unwrap_or_else(|| panic!("{created}")).to_owned();
    let update = json!({ "update": { &book: { "shareWith": { NYU: { "mayRead": true } } } } });
    let shared = contacts_call(&server, MINI, "AddressBook/set", update).await;
    assert!(shared["updated"].get(&book).is_some(), "{shared}");

    let theirs = contacts_call(&server, NYU, "AddressBook/get", json!({})).await;
    let seen = theirs["list"].as_array().unwrap().iter().find(|b| b["id"] == book.as_str()).expect("listed");
    assert_eq!(seen["myRights"]["mayWrite"], false);
    let cards = contacts_call(&server, NYU, "ContactCard/get", json!({ "ids": [&card_id] })).await;
    assert_eq!(cards["list"][0]["name"]["full"], "Leni Katze");
    let refused = contacts_call(&server, NYU, "ContactCard/set", json!({ "destroy": [&card_id] })).await;
    assert_eq!(refused["notDestroyed"][&card_id]["type"], "forbidden", "{refused}");
    let left = contacts_call(&server, NYU, "AddressBook/set", json!({ "destroy": [&book] })).await;
    assert_eq!(left["destroyed"], json!([&book]));
}
