//! Shipment/get, /changes, /query, /set and /rescan (docs/jmap-shipments.md). Mails, names and
//! numbers are made up.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_labels::{Mail, shipment};
use uwumail_store::MailboxRole;

use crate::common::{PASSWORD, Server, args, basic, server};

const MINI: &str = "mini@example.org";
const USING: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:uwumail:jmap:shipments", "urn:uwumail:jmap:settings"];
const SHIPMENTS: &str = "urn:uwumail:jmap:shipments";

const SHIPPED: &str = "From: DHL Paket <noreply@dhl.example>
To: mini@example.org
Subject: Ihr DHL Paket kommt bald
Date: Sat, 14 Nov 2026 18:03:00 +0100
Message-ID: <shipped-1@dhl.example>
Content-Type: text/plain; charset=utf-8

Hallo Mini,

Ihre Sendung 00340434161234567890 von Beispielshop wurde an DHL übergeben.
Voraussichtliche Zustellung: 16.11.2026
";

/// Delivers a mail and records its shipment the way the delivery does.
async fn deliver(server: &Server, raw: &str) -> String {
    let email = server.deliver(MINI, raw).await;
    let mail = Mail::parse(raw.replace('\n', "\r\n").as_bytes());
    let facts = shipment(&mail).expect("a shipment");
    let number = email[1..].parse().unwrap();
    let account = server.id(MINI).await;
    server.store.record_shipment_mail(account, number, 1_794_679_380, mail.subject.clone(), facts, true).await.unwrap();
    email
}

/// An event source that ends with the first state change; it listens once this returns.
async fn open_push(router: Router) -> tokio::task::JoinHandle<String> {
    let request = Request::get("/jmap/eventsource/?types=Shipment&closeafter=state&ping=0")
        .header(header::AUTHORIZATION, basic(MINI, PASSWORD))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::spawn(async move {
        String::from_utf8(to_bytes(response.into_body(), 1024 * 1024).await.unwrap().to_vec()).unwrap()
    })
}

async fn call(server: &Server, name: &str, arguments: Value) -> Value {
    let responses = server.api_using(MINI, &USING, json!([[name, arguments, "0"]])).await;
    args(&responses, 0, name).clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shipping_mail_becomes_a_shipment_apps_can_follow() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let session = server.session_of(MINI).await;
    assert_eq!(session["capabilities"][SHIPMENTS], json!({}));
    assert_eq!(session["primaryAccounts"][SHIPMENTS], json!(account));
    assert_eq!(session["accounts"][&account]["accountCapabilities"][SHIPMENTS]["maxEvents"], 200);
    let before = call(&server, "Shipment/get", json!({ "accountId": account, "ids": [] })).await["state"].clone();

    // A new shipping mail is pushed as a Shipment change.
    let push = open_push(server.router.clone()).await;
    let email = deliver(&server, SHIPPED).await;
    let pushed = tokio::time::timeout(std::time::Duration::from_secs(10), push).await.unwrap().unwrap();
    assert!(pushed.contains("\"Shipment\""), "{pushed}");

    let changes = call(&server, "Shipment/changes", json!({ "accountId": account, "sinceState": before })).await;
    let id = changes["created"][0].as_str().unwrap_or_else(|| panic!("{changes}")).to_owned();
    assert!(id.starts_with('s'));

    let got = call(&server, "Shipment/get", json!({ "accountId": account, "ids": [id] })).await;
    let shipment = &got["list"][0];
    assert_eq!(shipment["carrier"], "dhl");
    assert_eq!(shipment["number"], "00340434161234567890");
    assert_eq!(shipment["status"], "shipped");
    assert_eq!(
        shipment["expected"],
        json!({ "from": "2026-11-16T00:00:00+01:00", "to": "2026-11-17T00:00:00+01:00", "allDay": true })
    );
    assert_eq!(shipment["emailIds"], json!([email]));
    assert_eq!(shipment["events"][0]["source"], "mail");
    assert_eq!(shipment["events"][0]["emailId"], json!(email));
    assert_eq!(shipment["hidden"], false);

    // Home Assistant reports what the carrier says, under its app password's name.
    let app = server
        .store
        .create_app_password(
            server.id(MINI).await,
            uwumail_store::NewAppPassword {
                name: "Home Assistant".into(),
                scopes: vec![uwumail_store::AppScope::Mail],
                expires_at: None,
            },
        )
        .await
        .unwrap();
    let event = |at: &str, status: &str| json!({ "at": at, "status": status, "text": "Zustellung heute", "location": "Musterstadt" });
    let update = json!({ &id: { "addEvents": [
        event("2026-11-16T08:12:00Z", "outForDelivery"),
        event("2026-11-16T08:12:00Z", "outForDelivery"),
        event("2026-11-15T07:00:00Z", "inTransit"),
    ] } });
    let (status, response) = server
        .api_as(
            &basic(MINI, &app.secret),
            &USING,
            json!([
                ["Shipment/set", { "accountId": account, "update": update }, "0"],
                ["Shipment/get", { "accountId": account, "ids": [id], "properties": ["status", "events"] }, "1"],
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let responses = response["methodResponses"].as_array().unwrap();
    assert_eq!(args(responses, 0, "Shipment/set")["updated"], json!({ &id: null }));
    let got = &args(responses, 1, "Shipment/get")["list"][0];
    assert_eq!(got["status"], "outForDelivery");
    let events = got["events"].as_array().unwrap();
    assert_eq!(events.len(), 3, "the same event twice is kept once: {events:?}");
    assert_eq!(events[2]["source"], "client");
    assert_eq!(events[2]["clientName"], "Home Assistant");

    // Taken back: the status follows what is left.
    let undo = json!({ "accountId": account, "update": { &id: { "removeEvents": [{ "at": "2026-11-16T08:12:00Z", "status": "outForDelivery" }] } } });
    let set = call(&server, "Shipment/set", undo).await;
    assert_eq!(set["updated"], json!({ &id: null }), "{set}");
    let got = call(
        &server,
        "Shipment/get",
        json!({ "accountId": account, "ids": [&id], "properties": ["status", "events"] }),
    )
    .await;
    assert_ne!(got["list"][0]["status"], "outForDelivery", "{got}");
    assert_eq!(got["list"][0]["events"].as_array().unwrap().len(), 2);
    let bad = json!({ "accountId": account, "update": { &id: { "removeEvents": [{ "at": "2026-11-16T08:12:00Z", "status": "inTransit", "text": "x" }] } } });
    assert_eq!(call(&server, "Shipment/set", bad).await["notUpdated"][&id]["type"], "invalidProperties");
    let again = json!({ "accountId": account, "update": { &id: { "addEvents": [event("2026-11-16T08:12:00Z", "outForDelivery")] } } });
    assert_eq!(call(&server, "Shipment/set", again).await["updated"], json!({ &id: null }));

    // Bad events are refused whole.
    let bad = json!({
        "accountId": account,
        "update": {
            &id: { "addEvents": [{ "at": "2026-11-16T09:00:00Z", "status": "teleported" }] },
            "s999999": { "hidden": true }
        }
    });
    let set = call(&server, "Shipment/set", bad).await;
    assert_eq!(set["notUpdated"][&id]["type"], "invalidProperties");
    assert_eq!(set["notUpdated"]["s999999"]["type"], "notFound");
    let ahead = json!({ "accountId": account, "update": { &id: { "addEvents": [{ "at": "2099-01-01T00:00:00Z", "status": "delivered" }] } } });
    let set = call(&server, "Shipment/set", ahead).await;
    assert_eq!(set["notUpdated"][&id]["type"], "invalidProperties", "{set}");
    let long = "x".repeat(501);
    let set = call(
        &server,
        "Shipment/set",
        json!({ "accountId": account, "update": { &id: { "addEvents": [{ "at": "2026-11-16T09:00:00Z", "status": "delivered", "text": long }] } } }),
    )
    .await;
    assert_eq!(set["notUpdated"][&id]["type"], "invalidProperties", "{set}");
    let many: Vec<Value> = (0..201).map(|_| event("2026-11-16T09:00:00Z", "delivered")).collect();
    let set =
        call(&server, "Shipment/set", json!({ "accountId": account, "update": { &id: { "addEvents": many } } })).await;
    assert_eq!(set["notUpdated"][&id]["type"], "invalidProperties");

    // Made by hand, hidden, found by query.
    let set = call(
        &server,
        "Shipment/set",
        json!({ "accountId": account, "create": {
            "mine": { "carrier": "ups", "number": "1z 999aa1 0123456784", "title": "Kopfhörer" },
            "again": { "carrier": "dhl", "number": "00340434161234567890" },
            "what": { "carrier": "zeppelin", "number": "1" },
            "extra": { "carrier": "dhl", "number": "1", "status": "delivered" }
        }, "update": { &id: { "hidden": true, "title": "Pullover" } } }),
    )
    .await;
    let mine = set["created"]["mine"]["id"].as_str().unwrap_or_else(|| panic!("{set}")).to_owned();
    assert_eq!(set["created"]["mine"]["status"], "shipped");
    assert_eq!(set["notCreated"]["again"]["type"], "alreadyExists");
    assert_eq!(set["notCreated"]["what"]["type"], "invalidProperties");
    assert_eq!(set["notCreated"]["extra"]["type"], "invalidProperties");
    let got = call(
        &server,
        "Shipment/get",
        json!({ "accountId": account, "ids": [mine], "properties": ["number", "title"] }),
    )
    .await;
    assert_eq!(got["list"][0]["number"], "1Z999AA10123456784");
    assert_eq!(got["list"][0]["title"], "Kopfhörer");

    let query = |filter: Value| {
        call(
            &server,
            "Shipment/query",
            json!({ "accountId": account, "filter": filter, "sort": [{ "property": "expected" }] }),
        )
    };
    assert_eq!(query(json!({ "hidden": false })).await["ids"], json!([mine]));
    assert_eq!(query(json!({ "status": ["outForDelivery"] })).await["ids"], json!([id]));
    assert_eq!(query(json!({ "active": true })).await["ids"], json!([id, mine]), "a day first, unknown last");
    assert_eq!(query(json!({ "carrier": "ups" })).await["ids"], json!([mine]));
    assert_eq!(query(json!({ "expectedBefore": "2026-11-16T12:00:00Z" })).await["ids"], json!([id]));
    assert_eq!(query(json!({ "deliveredAfter": "2026-11-01T00:00:00Z" })).await["ids"], json!([]));
    let wrong = server
        .api_using(
            MINI,
            &USING,
            json!([["Shipment/query", { "accountId": account, "filter": { "colour": "red" } }, "0"]]),
        )
        .await;
    assert_eq!(wrong[0][1]["type"], "unsupportedFilter");
    let changes = server
        .api_using(
            MINI,
            &USING,
            json!([["Shipment/queryChanges", { "accountId": account, "sinceQueryState": "1" }, "0"]]),
        )
        .await;
    assert_eq!(changes[0][1]["type"], "cannotCalculateChanges");

    // A look back answers at once; days are limited.
    let rescan = call(&server, "Shipment/rescan", json!({ "accountId": account, "days": 30 })).await;
    assert_eq!(rescan["rescan"]["running"], true, "{rescan}");
    let too_far =
        server.api_using(MINI, &USING, json!([["Shipment/rescan", { "accountId": account, "days": 91 }, "0"]])).await;
    assert_eq!(too_far[0][1]["type"], "invalidArguments");

    // Destroyed, then everything gone when switched off.
    let set = call(&server, "Shipment/set", json!({ "accountId": account, "destroy": [mine, "s999999"] })).await;
    assert_eq!(set["destroyed"], json!([mine]));
    assert_eq!(set["notDestroyed"]["s999999"]["type"], "notFound");
    let off = json!({ "accountId": account, "update": { "singleton": { "values/shipments": false } } });
    let responses = server.api_using(MINI, &USING, json!([["UserSettings/set", off, "0"]])).await;
    assert!(responses[0][1]["updated"].is_object(), "{responses:?}");
    // The account no longer carries the capability, so every Shipment call is turned away.
    for (name, arguments) in [
        ("Shipment/get", json!({ "accountId": account, "ids": null })),
        (
            "Shipment/set",
            json!({ "accountId": account, "create": { "a": { "carrier": "dhl", "number": "00340434161234567891" } } }),
        ),
        ("Shipment/rescan", json!({ "accountId": account })),
    ] {
        let responses = server.api_using(MINI, &USING, json!([[name, arguments, "0"]])).await;
        assert_eq!(responses[0][1]["type"], "accountNotSupportedByMethod", "{name}: {responses:?}");
    }
    let session = server.session_of(MINI).await;
    assert_eq!(session["capabilities"][SHIPMENTS], json!({}), "the server still offers it");
    assert!(session["accounts"][&account]["accountCapabilities"].get(SHIPMENTS).is_none());
    assert!(session["primaryAccounts"].get(SHIPMENTS).is_none());
    assert!(server.store.shipments(server.id(MINI).await, None).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_admin_switch_hides_shipments_without_deleting_them() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    deliver(&server, SHIPPED).await;
    let on = server.session_of(MINI).await;

    server.store.set_shipments_allowed(false);
    let off = server.session_of(MINI).await;
    assert!(!off.to_string().contains(SHIPMENTS), "{off}");
    assert_ne!(off["state"], on["state"]);
    let responses =
        server.api_using(MINI, &USING, json!([["Shipment/get", { "accountId": account, "ids": null }, "0"]])).await;
    assert_eq!(responses[0][1]["type"], "accountNotSupportedByMethod", "{responses:?}");
    // The account setting can still be written while the server is off.
    let write = json!({ "accountId": account, "update": { "singleton": { "values/shipments": true } } });
    let responses = server.api_using(MINI, &USING, json!([["UserSettings/set", write, "0"]])).await;
    assert!(responses[0][1]["updated"].is_object(), "{responses:?}");
    // Nothing new is recorded while off.
    let mail = Mail::parse(SHIPPED.replace('\n', "\r\n").as_bytes());
    let id = server.id(MINI).await;
    let recorded = server
        .store
        .record_shipment_mail(id, 1, 1_794_679_380, mail.subject.clone(), shipment(&mail).unwrap(), true)
        .await
        .unwrap();
    assert!(!recorded);

    server.store.set_shipments_allowed(true);
    let back = server.session_of(MINI).await;
    assert_eq!(back["state"], on["state"]);
    assert_eq!(back["primaryAccounts"][SHIPMENTS], json!(account));
    let got = call(&server, "Shipment/get", json!({ "accountId": account, "ids": null })).await;
    assert_eq!(got["list"].as_array().unwrap().len(), 1, "kept while off: {got}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shipments_need_their_capability_and_stay_in_their_account() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let responses = server
        .api_using(MINI, &["urn:ietf:params:jmap:core"], json!([["Shipment/get", { "accountId": account }, "0"]]))
        .await;
    assert_eq!(responses[0][1]["type"], "unknownMethod");
    deliver(&server, SHIPPED).await;
    let other = server.account_id("nyu@example.org").await;
    let responses =
        server.api_using(MINI, &USING, json!([["Shipment/get", { "accountId": other, "ids": null }, "0"]])).await;
    assert_ne!(responses[0][0], "Shipment/get", "{responses:?}");
    let theirs = server
        .api_using("nyu@example.org", &USING, json!([["Shipment/get", { "accountId": other, "ids": null }, "0"]]))
        .await;
    assert_eq!(args(&theirs, 0, "Shipment/get")["list"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn whoever_may_read_the_mailbox_sees_its_shipments() {
    const NYU: &str = "nyu@example.org";
    const MAIL: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", SHIPMENTS];
    let server = server().await;
    let mini = server.id(MINI).await;
    let account = server.account_id(MINI).await;
    let nyu = format!("p{}", server.id(NYU).await);
    let mailboxes = server.store.mailboxes(mini).await.unwrap();
    let inbox = mailboxes.iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap().id;
    let archive = mailboxes.iter().find(|m| m.role == Some(MailboxRole::Archive)).unwrap().id;

    // One parcel from a mail in the inbox, one from a mail moved to the archive.
    deliver(&server, SHIPPED).await;
    let private = SHIPPED.replace("00340434161234567890", "JJD000390007777777777").replace("shipped-1", "shipped-2");
    let moved = deliver(&server, &private).await;
    // A second mail about the inbox's parcel, moved to the archive too.
    let again = SHIPPED.replace("shipped-1", "shipped-3").replace("kommt bald", "ist unterwegs");
    let again = deliver(&server, &again).await;
    let update = json!({
        moved.clone(): { "mailboxIds": { format!("m{archive}"): true } },
        again.clone(): { "mailboxIds": { format!("m{archive}"): true } },
    });
    server.api_using(MINI, &MAIL, json!([["Email/set", { "accountId": account, "update": update }, "0"]])).await;
    let own =
        call(&server, "Shipment/get", json!({ "accountId": account, "ids": null, "properties": ["number"] })).await;
    assert_eq!(own["list"].as_array().unwrap().len(), 2, "{own}");

    let share = |level: &str| {
        json!([["Mailbox/set", { "accountId": account, "update": {
            format!("m{inbox}"): { format!("shareWith/{nyu}"): level } } }, "0"]])
    };
    let nyu_call = |name: &'static str, arguments: Value| {
        let server = &server;
        async move { args(&server.api_using(NYU, &MAIL, json!([[name, arguments, "0"]])).await, 0, name).clone() }
    };

    // Reading the inbox shows its parcel, not the archived one, and changes nothing.
    server.api_using(MINI, &MAIL, share("read")).await;
    let seen = nyu_call("Shipment/get", json!({ "accountId": account, "ids": null })).await;
    let list = seen["list"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{seen}");
    assert_eq!(list[0]["number"], "00340434161234567890");
    assert_eq!(list[0]["emailIds"].as_array().unwrap().len(), 1, "not the archived mail: {seen}");
    assert!(!list[0]["emailIds"].as_array().unwrap().contains(&json!(again)));
    assert!(!seen.to_string().contains("ist unterwegs"), "nor its subject: {seen}");
    let parcel = list[0]["id"].as_str().unwrap().to_owned();
    let hidden = own["list"].as_array().unwrap().iter().find(|s| s["id"] != parcel).unwrap()["id"].clone();
    let found = nyu_call("Shipment/get", json!({ "accountId": account, "ids": [parcel, hidden] })).await;
    assert_eq!(found["notFound"], json!([hidden]), "{found}");
    let queried = nyu_call("Shipment/query", json!({ "accountId": account })).await;
    assert_eq!(queried["ids"], json!([parcel]), "{queried}");
    let refused = nyu_call(
        "Shipment/set",
        json!({ "accountId": account,
            "create": { "c": { "carrier": "dhl", "number": "00340434160000000017" } },
            "update": { parcel.clone(): { "hidden": true }, hidden.as_str().unwrap(): { "hidden": true } },
            "destroy": [parcel] }),
    )
    .await;
    assert_eq!(refused["notCreated"]["c"]["type"], "forbidden", "{refused}");
    assert_eq!(refused["notUpdated"][&parcel]["type"], "forbidden");
    assert_eq!(refused["notUpdated"][hidden.as_str().unwrap()]["type"], "notFound");
    assert_eq!(refused["notDestroyed"][&parcel]["type"], "forbidden");
    let rescan = server.api_using(NYU, &MAIL, json!([["Shipment/rescan", { "accountId": account }, "0"]])).await;
    assert_eq!(rescan[0][1]["type"], "accountNotSupportedByMethod", "{rescan:?}");

    // The right to change the inbox lets them change its parcel, still only in Mini's account.
    server.api_using(MINI, &MAIL, share("write")).await;
    let changed =
        nyu_call("Shipment/set", json!({ "accountId": account, "update": { parcel.clone(): { "hidden": true } } }))
            .await;
    assert!(changed["updated"].get(&parcel).is_some(), "{changed}");
    let own_account = server.account_id(NYU).await;
    let theirs = nyu_call("Shipment/get", json!({ "accountId": own_account, "ids": null })).await;
    assert_eq!(theirs["list"], json!([]), "Nyu's own account has no parcels");
}
