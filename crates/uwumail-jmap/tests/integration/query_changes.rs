//! Email/queryChanges and Mailbox/queryChanges, checked the way a client uses them: the old
//! results with the changes applied must be the new results. CalendarEvent/queryChanges too. And
//! Email/copy's errors.

use crate::common;

use common::{args, server};
use serde_json::{Value, json};

const MINI: &str = "mini@example.org";

fn ids(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()).collect()
}

/// RFC 8620, section 5.6: drop every removed id, then insert the added ones in index order.
fn apply(old: &[String], changes: &Value) -> Vec<String> {
    let removed = ids(&changes["removed"]);
    let mut list: Vec<String> = old.iter().filter(|id| !removed.contains(id)).cloned().collect();
    let mut added: Vec<(usize, String)> = changes["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["index"].as_u64().unwrap() as usize, a["id"].as_str().unwrap().to_owned()))
        .collect();
    added.sort();
    for (index, id) in added {
        list.insert(index.min(list.len()), id);
    }
    list
}

async fn query(server: &common::Server, method: &str, arguments: Value) -> (Vec<String>, String, Value) {
    let responses = server.api(MINI, json!([[method, arguments, "0"]])).await;
    let response = args(&responses, 0, method).clone();
    (ids(&response["ids"]), response["queryState"].as_str().unwrap().to_owned(), response)
}

async fn changes(server: &common::Server, method: &str, arguments: Value) -> Value {
    let responses = server.api(MINI, json!([[method, arguments, "0"]])).await;
    responses[0][1].clone()
}

fn message(subject: &str, id: &str, reply_to: Option<&str>) -> String {
    let reply = reply_to.map(|r| format!("In-Reply-To: <{r}>\nReferences: <{r}>\n")).unwrap_or_default();
    format!("From: nyu@example.org\nTo: mini@example.org\nSubject: {subject}\nMessage-ID: <{id}>\n{reply}\nPurr\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn email_query_changes_replay_to_the_new_results() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let inbox = server.mailbox(MINI, "inbox").await;
    let first = server.deliver(MINI, &message("One", "one@example.org", None)).await;
    let second = server.deliver(MINI, &message("Two", "two@example.org", None)).await;
    server.deliver(MINI, &message("Three", "three@example.org", None)).await;

    for collapse in [false, true] {
        let arguments = json!({
            "accountId": account,
            "filter": { "inMailbox": inbox },
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "collapseThreads": collapse,
        });
        let (old, state, response) = query(&server, "Email/query", arguments.clone()).await;
        assert_eq!(response["canCalculateChanges"], true);

        // A new mail, a reply in an old thread, a flag, one deleted, one moved out of the inbox.
        server.deliver(MINI, &message(&format!("New {collapse}"), &format!("new-{collapse}@example.org"), None)).await;
        server
            .deliver(MINI, &message("Re: One", &format!("re-one-{collapse}@example.org"), Some("one@example.org")))
            .await;
        let archive = server.mailbox(MINI, "archive").await;
        let current = query(&server, "Email/query", arguments.clone()).await.0;
        let victim = current.iter().find(|id| **id != first && **id != second).unwrap().clone();
        let responses = server
            .api(
                MINI,
                json!([["Email/set", { "accountId": account,
                    "update": { &second: { "keywords/$flagged": true }, &first: { "mailboxIds": { &archive: true } } },
                    "destroy": [victim] }, "0"]]),
            )
            .await;
        assert!(args(&responses, 0, "Email/set")["notUpdated"].is_null(), "{}", responses[0]);

        let mut since = arguments.clone();
        since["sinceQueryState"] = json!(state);
        since["calculateTotal"] = json!(true);
        let result = changes(&server, "Email/queryChanges", since).await;
        let (new, new_state, _) = query(&server, "Email/query", arguments.clone()).await;
        assert_eq!(apply(&old, &result), new, "collapse {collapse}: {result}");
        assert_eq!(result["oldQueryState"], state);
        assert_eq!(result["newQueryState"], new_state);
        assert_eq!(result["total"], new.len());

        // Nothing changed since: nothing to do.
        let mut since = arguments.clone();
        since["sinceQueryState"] = json!(new_state);
        let result = changes(&server, "Email/queryChanges", since).await;
        assert_eq!(result["removed"], json!([]));
        assert_eq!(result["added"], json!([]));
    }

    // Too many changes for the client, and states the server does not know.
    let (_, state, _) = query(&server, "Email/query", json!({ "accountId": account })).await;
    server.deliver(MINI, &message("Four", "four@example.org", None)).await;
    let result = changes(
        &server,
        "Email/queryChanges",
        json!({ "accountId": account, "sinceQueryState": state, "maxChanges": 1 }),
    )
    .await;
    assert_eq!(result["type"], "tooManyChanges", "{result}");
    let result =
        changes(&server, "Email/queryChanges", json!({ "accountId": account, "sinceQueryState": "nope" })).await;
    assert_eq!(result["type"], "cannotCalculateChanges");
    let result =
        changes(&server, "Email/queryChanges", json!({ "accountId": account, "sinceQueryState": "999999" })).await;
    assert_eq!(result["type"], "cannotCalculateChanges");
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_query_changes_replay_to_the_new_results() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    for as_tree in [false, true] {
        let arguments = json!({
            "accountId": account,
            "sort": [{ "property": "name" }],
            "sortAsTree": as_tree,
        });
        let (old, state, response) = query(&server, "Mailbox/query", arguments.clone()).await;
        assert_eq!(response["canCalculateChanges"], true);
        let responses = server
            .api(
                MINI,
                json!([["Mailbox/set", { "accountId": account, "create": {
                    "a": { "name": format!("Aardvark {as_tree}") },
                    "z": { "name": format!("Zebra {as_tree}") }
                } }, "0"]]),
            )
            .await;
        assert!(args(&responses, 0, "Mailbox/set")["notCreated"].is_null(), "{}", responses[0]);
        let mut since = arguments.clone();
        since["sinceQueryState"] = json!(state);
        let result = changes(&server, "Mailbox/queryChanges", since).await;
        let new = query(&server, "Mailbox/query", arguments).await.0;
        assert_eq!(apply(&old, &result), new, "{result}");
        assert_eq!(new.len(), old.len() + 2);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn email_copy_follows_the_rfc_for_accounts_it_cannot_read() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let nyu = server.account_id("nyu@example.org").await;
    let inbox = server.mailbox(MINI, "inbox").await;
    let email = server.deliver("nyu@example.org", &message("Hers", "hers@example.org", None)).await;
    let create = json!({ "c": { "id": email, "mailboxIds": { inbox: true } } });

    let result =
        changes(&server, "Email/copy", json!({ "accountId": account, "fromAccountId": nyu, "create": create })).await;
    assert_eq!(result["type"], "fromAccountNotFound", "another person's mail is not readable");
    let result =
        changes(&server, "Email/copy", json!({ "accountId": account, "fromAccountId": account, "create": create }))
            .await;
    assert_eq!(result["type"], "invalidArguments", "copying needs two accounts");
    let result = changes(&server, "Email/copy", json!({ "accountId": account, "create": create })).await;
    assert_eq!(result["type"], "invalidArguments");
    let result =
        changes(&server, "Email/copy", json!({ "accountId": nyu, "fromAccountId": account, "create": create })).await;
    assert_eq!(result["type"], "accountNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn calendar_event_query_changes_replay_to_the_new_results() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let using = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"];
    let call = |method: &'static str, arguments: Value| {
        let server = &server;
        async move { server.api_using(MINI, &using, json!([[method, arguments, "0"]])).await[0][1].clone() }
    };
    let calendars = call("Calendar/get", json!({ "accountId": account })).await;
    let calendar = calendars["list"][0]["id"].as_str().unwrap().to_owned();
    let event = |title: &str, start: &str| {
        json!({ "calendarIds": { &calendar: true }, "title": title, "start": start,
                "timeZone": "Europe/Berlin", "duration": "PT1H" })
    };
    let created = call(
        "CalendarEvent/set",
        json!({ "accountId": account, "create": {
            "a": event("Tierarzt", "2026-10-20T09:00:00"),
            "b": event("Yoga", "2026-10-21T09:00:00"),
            "c": event("Kino", "2026-10-22T20:00:00"),
            "d": event("Urlaub", "2026-12-24T09:00:00")
        } }),
    )
    .await;
    let id = |key: &str| created["created"][key]["id"].as_str().unwrap_or_else(|| panic!("{created}")).to_owned();
    let arguments = json!({
        "accountId": account,
        "filter": { "after": "2026-10-01T00:00:00", "before": "2026-11-01T00:00:00" },
        "sort": [{ "property": "start", "isAscending": true }],
    });
    let response = call("CalendarEvent/query", arguments.clone()).await;
    assert_eq!(response["canCalculateChanges"], true);
    let old = ids(&response["ids"]);
    assert_eq!(old, vec![id("a"), id("b"), id("c")]);
    let state = response["queryState"].as_str().unwrap().to_owned();

    // One moves to the end, one leaves the window, one comes into it, one is new.
    let changed = call(
        "CalendarEvent/set",
        json!({ "accountId": account,
            "update": {
                id("a"): { "start": "2026-10-30T09:00:00" },
                id("b"): { "start": "2026-11-15T09:00:00" },
                id("d"): { "start": "2026-10-10T09:00:00" }
            },
            "create": { "e": event("Konzert", "2026-10-25T19:00:00") },
            "destroy": [id("c")] }),
    )
    .await;
    assert!(changed["notUpdated"].is_null() && changed["notCreated"].is_null(), "{changed}");

    let mut since = arguments.clone();
    since["sinceQueryState"] = json!(state);
    since["calculateTotal"] = json!(true);
    let result = call("CalendarEvent/queryChanges", since).await;
    let now = call("CalendarEvent/query", arguments.clone()).await;
    let new = ids(&now["ids"]);
    assert_eq!(apply(&old, &result), new, "{result}");
    assert_eq!(new.len(), 3);
    assert_eq!(result["total"], 3);
    assert_eq!(result["newQueryState"], now["queryState"]);
}
