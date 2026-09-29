//! The birthdays calendar and moving birthdays out of calendars (docs/birthdays.md), end to end
//! over JMAP and CalDAV on one store.

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
    "urn:uwumail:jmap:birthdays",
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

    async fn birthdays_calendar(&self) -> Value {
        self.calendars().await.into_iter().find(|c| c["uwuBirthdays"] == true).expect("a birthdays calendar")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cards_fill_a_read_only_birthdays_calendar() {
    let server = server().await;
    let (status, session) = server.send("GET", "/jmap/session", &[], String::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(session.contains("urn:uwumail:jmap:birthdays"), "{session}");

    let created = server
        .call(
            "ContactCard/set",
            json!({ "create": { "max": {
                "name": { "full": "Max Müller" },
                "anniversaries": {
                    "b": { "kind": "birth", "date": { "@type": "PartialDate", "year": 1996, "month": 4, "day": 12 } },
                    "w": { "kind": "wedding", "date": { "@type": "PartialDate", "year": 2021, "month": 6, "day": 12 } }
                },
                "uwuReminders": [{ "daysBefore": 1, "time": "09:00" }]
            } } }),
        )
        .await;
    let card_id = created["created"]["max"]["id"].as_str().unwrap_or_else(|| panic!("{created}")).to_owned();
    let card = server.call("ContactCard/get", json!({ "ids": [&card_id] })).await;
    assert_eq!(card["list"][0]["uwuReminders"], json!([{ "daysBefore": 1, "time": "09:00" }]));

    let calendar = server.birthdays_calendar().await;
    assert_eq!(calendar["name"], "Geburtstage");
    assert_eq!(calendar["isDefault"], false);
    assert_eq!(calendar["myRights"]["mayWriteAll"], false);
    assert_eq!(calendar["myRights"]["mayDelete"], false);
    assert_eq!(calendar["includeInAvailability"], "none");
    let calendar_id = calendar["id"].as_str().unwrap().to_owned();

    let events = server.call("CalendarEvent/get", json!({})).await;
    let events = events["list"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    let birthday = events.iter().find(|e| e["uwuBirthday"]["kind"] == "birth").unwrap();
    assert_eq!(birthday["uwuBirthday"]["contactId"], json!(card_id));
    assert_eq!(birthday["uwuBirthday"]["year"], 1996);
    assert_eq!(birthday["title"], "Max Müller (*1996)");
    assert_eq!(birthday["showWithoutTime"], true);
    assert_eq!(birthday["calendarIds"], json!({ &calendar_id: true }));
    let alert = birthday["alerts"].as_object().unwrap().values().next().unwrap().clone();
    assert_eq!(alert["trigger"]["offset"], "-PT15H", "a day before at nine: {alert}");

    // Each year's instance says the age.
    let window = json!({ "after": "2026-01-01T00:00:00", "before": "2027-01-01T00:00:00" });
    let found = server
        .call(
            "CalendarEvent/query",
            json!({ "filter": { "inCalendar": &calendar_id, "after": window["after"], "before": window["before"] },
                    "expandRecurrences": true, "sort": [{ "property": "start" }] }),
        )
        .await;
    let ids = found["ids"].as_array().unwrap().clone();
    assert_eq!(ids.len(), 2, "{found}");
    let got =
        server.call("CalendarEvent/get", json!({ "ids": ids, "properties": ["title", "start", "uwuBirthday"] })).await;
    let titles: Vec<&str> = got["list"].as_array().unwrap().iter().map(|e| e["title"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["Max Müller (30)", "Hochzeitstag von Max Müller (5 Jahre)"]);

    // All day means the day itself, whatever time zone the calendar has.
    let set =
        server.call("Calendar/set", json!({ "update": { &calendar_id: { "timeZone": "Pacific/Kiritimati" } } })).await;
    assert!(set["updated"].as_object().is_some_and(|u| u.contains_key(&calendar_id)), "{set}");
    let got = server
        .call("CalendarEvent/get", json!({ "ids": &ids, "properties": ["start", "timeZone", "showWithoutTime"] }))
        .await;
    let days: Vec<(&str, bool, bool)> = got["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["start"].as_str().unwrap(), e["timeZone"].is_null(), e["showWithoutTime"] == true))
        .collect();
    assert_eq!(days, vec![("2026-04-12T00:00:00", true, true), ("2026-06-12T00:00:00", true, true)]);

    // Nothing writes into it but the cards.
    let birthday_id = birthday["id"].as_str().unwrap();
    let set = server
        .call(
            "CalendarEvent/set",
            json!({
                "create": { "x": { "calendarIds": { &calendar_id: true }, "title": "Party", "start": "2026-10-20T09:00:00", "duration": "PT1H" } },
                "update": { birthday_id: { "title": "Anders" } },
                "destroy": [birthday_id]
            }),
        )
        .await;
    assert!(set["notCreated"]["x"].is_object(), "{set}");
    assert!(set["notUpdated"][birthday_id].is_object(), "{set}");
    assert!(set["notDestroyed"][birthday_id].is_object(), "{set}");
    let set = server.call("Calendar/set", json!({ "destroy": [&calendar_id], "onDestroyRemoveEvents": true })).await;
    assert!(set["notDestroyed"][&calendar_id].is_object(), "{set}");
    // Its colour and whether it shows are the person's.
    let set = server
        .call("Calendar/set", json!({ "update": { &calendar_id: { "color": "#00aa00", "isVisible": false } } }))
        .await;
    assert!(set["updated"].as_object().is_some_and(|u| u.contains_key(&calendar_id)), "{set}");
    let hidden = server.birthdays_calendar().await;
    assert_eq!((hidden["color"].as_str(), hidden["isVisible"].as_bool()), (Some("#00aa00"), Some(false)));
    let slug = "/dav/calendars/mini@example.org/birthdays/party.ics";
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:party\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART;VALUE=DATE:20261020\r\nSUMMARY:Party\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let (status, _) = server.send("PUT", slug, &[("content-type", "text/calendar")], ics.into()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A changed card changes the event; the calendar's state moves with it.
    let before = server.call("CalendarEvent/get", json!({ "ids": [] })).await["state"].clone();
    let update = json!({ "update": { &card_id: { "anniversaries/b/date": { "@type": "PartialDate", "month": 4, "day": 13 } } } });
    let set = server.call("ContactCard/set", update).await;
    assert!(set["updated"].as_object().is_some_and(|u| u.contains_key(&card_id)), "{set}");
    let changes = server.call("CalendarEvent/changes", json!({ "sinceState": before })).await;
    assert_eq!(changes["updated"].as_array().unwrap().len(), 1, "{changes}");
    let got = server.call("CalendarEvent/get", json!({ "ids": [birthday_id] })).await;
    assert_eq!(got["list"][0]["title"], "Max Müller");
    assert_eq!(got["list"][0]["start"], "1970-04-13T00:00:00");
}

#[tokio::test(flavor = "multi_thread")]
async fn birthdays_move_out_of_other_calendars() {
    let server = server().await;
    let calendars = server.calendars().await;
    let personal = calendars.iter().find(|c| c["isDefault"] == true).unwrap()["id"].as_str().unwrap().to_owned();
    let card = server.call("ContactCard/set", json!({ "create": { "c": { "name": { "full": "Änne Groß" } } } })).await
        ["created"]["c"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let yearly = |title: &str, start: &str| {
        json!({ "calendarIds": { &personal: true }, "title": title, "start": start, "duration": "P1D",
                "showWithoutTime": true,
                "recurrenceRule": { "@type": "RecurrenceRule", "frequency": "yearly" } })
    };
    let set = server
        .call(
            "CalendarEvent/set",
            json!({ "create": {
                "anne": yearly("Geburtstag von Anne Gross (*1950)", "2010-02-28T00:00:00"),
                "leni": yearly("🎂 Leni", "2012-02-29T00:00:00"),
                "dentist": yearly("Zahnarzt", "2012-03-01T00:00:00")
            } }),
        )
        .await;
    let anne = set["created"]["anne"]["id"].as_str().unwrap_or_else(|| panic!("{set}")).to_owned();
    let leni = set["created"]["leni"]["id"].as_str().unwrap().to_owned();
    let dentist = set["created"]["dentist"]["id"].as_str().unwrap().to_owned();

    let scan = server.call("Birthdays/scan", json!({})).await;
    let candidates = scan["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2, "{scan}");
    let of = |id: &str| candidates.iter().find(|c| c["eventId"] == id).unwrap().clone();
    let found = of(&anne);
    assert_eq!(found["match"], "matched", "{found}");
    assert_eq!(found["contacts"][0]["contactId"], json!(card));
    assert_eq!(found["birthday"], json!({ "month": 2, "day": 28, "year": 1950 }));
    assert_eq!(found["mayDeleteEvent"], true);
    let found = of(&leni);
    assert_eq!((found["match"].as_str(), found["name"].as_str()), (Some("unmatched"), Some("Leni")));
    assert_eq!(found["birthday"], json!({ "month": 2, "day": 29, "year": null }));

    let imported = server
        .call(
            "Birthdays/import",
            json!({ "entries": {
                &anne: { "contactId": &card },
                &leni: { "newContact": { "name": "Leni Muster" } },
                "v999999": { "contactId": &card },
                &dentist: { "contactId": &card, "newContact": { "name": "Beides" } }
            } }),
        )
        .await;
    assert_eq!(imported["imported"][&anne]["eventDeleted"], true, "{imported}");
    assert_eq!(imported["imported"][&leni]["created"], true, "{imported}");
    assert_eq!(imported["notImported"]["v999999"]["type"], "notFound", "{imported}");
    assert_eq!(imported["notImported"][&dentist]["type"], "invalidProperties", "{imported}");
    let gone = server.call("CalendarEvent/get", json!({ "ids": [&anne, &leni] })).await;
    assert_eq!(gone["notFound"].as_array().unwrap().len(), 2, "{gone}");
    let card = server.call("ContactCard/get", json!({ "ids": [&card] })).await;
    let dates = card["list"][0]["anniversaries"].to_string();
    assert!(dates.contains("1950") && dates.contains("birth"), "{card}");
    // The birthdays calendar has both now, Leni on the last day of February.
    let events = server.call("CalendarEvent/get", json!({})).await;
    let titles: Vec<String> = events["list"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["uwuBirthday"].is_object())
        .map(|e| e["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
    assert!(
        titles.contains(&"Änne Groß (*1950)".to_owned()) && titles.contains(&"Leni Muster".to_owned()),
        "{titles:?}"
    );
    assert!(server.call("Birthdays/scan", json!({})).await["candidates"].as_array().unwrap().is_empty());
}
