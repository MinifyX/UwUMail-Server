//! Limits on what one request may cost the server: bodies are read only after the login, uploads
//! count against a budget, and mail methods have per-request bounds.

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::server;

/// A body that never ends and notes whether anyone started reading it.
fn endless_body() -> (Body, Arc<AtomicBool>) {
    let polled = Arc::new(AtomicBool::new(false));
    let seen = polled.clone();
    let stream = futures_util::stream::poll_fn(move |_| -> Poll<Option<Result<Vec<u8>, std::io::Error>>> {
        seen.store(true, Ordering::SeqCst);
        Poll::Pending
    });
    (Body::from_stream(stream), polled)
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_read_before_the_login() {
    // security-audit-0.16.0 PROTOCOLS-6: uploads (50 MB) and API requests (10 MB) were read whole
    // before the login was checked, so anyone could make the server hold them, and trickle them.
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    for uri in [format!("/jmap/upload/{account}/"), "/jmap/api".to_owned()] {
        let (body, polled) = endless_body();
        let request = Request::post(&uri)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, "40000000")
            .body(body)
            .unwrap();
        let (status, _) = tokio::time::timeout(Duration::from_secs(10), server.request(request))
            .await
            .unwrap_or_else(|_| panic!("{uri}: the server waited for the body"));
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert!(!polled.load(Ordering::SeqCst), "{uri}: the body was read before the login");
    }
}

async fn upload(server: &common::Server, account: &str, bytes: Vec<u8>) -> StatusCode {
    let request = Request::post(format!("/jmap/upload/{account}/"))
        .header(header::AUTHORIZATION, common::basic("mini@example.org", common::PASSWORD))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(bytes))
        .unwrap();
    server.request(request).await.0
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_count_against_the_storage() {
    // security-audit-0.16.0 PROTOCOLS-7: uploads were counted nowhere.
    let server = server().await;
    let update = uwumail_store::AccountUpdate { quota_bytes: Some(4096), ..Default::default() };
    server.store.update_account("mini@example.org", update).await.unwrap();
    let account = server.account_id("mini@example.org").await;
    assert_eq!(upload(&server, &account, vec![b'a'; 3000]).await, StatusCode::OK);
    assert_eq!(upload(&server, &account, vec![b'b'; 3000]).await, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(upload(&server, &account, vec![b'a'; 3000]).await, StatusCode::OK, "the same file again");
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_methods_have_per_request_bounds() {
    // security-audit-0.16.0 PROTOCOLS-9: Email/parse took any number of blob ids, and Email/query
    // and Mailbox/query any filter and sort, which calendars and contacts had been bounded for.
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let email = server.deliver("mini@example.org", "From: nyu@example.org\nSubject: Hallo\n\nHallo Mini\n").await;
    let blob = server
        .api(
            "mini@example.org",
            serde_json::json!([["Email/get", {
        "accountId": account, "ids": [email], "properties": ["blobId"]
    }, "0"]]),
        )
        .await[0][1]["list"][0]["blobId"]
        .as_str()
        .unwrap()
        .to_owned();

    let many: Vec<String> =
        (0..=uwumail_jmap::MAX_OBJECTS_IN_GET).map(|n| format!("{blob}{}", "x".repeat(n % 2))).collect();
    let few: Vec<String> = vec![blob.clone(); 3];
    let condition = serde_json::json!({ "text": "hallo" });
    let big_filter = serde_json::json!({ "operator": "AND", "conditions": vec![condition; 100] });
    let sort: Vec<_> = (0..20).map(|_| serde_json::json!({ "property": "subject" })).collect();
    let responses = server
        .api(
            "mini@example.org",
            serde_json::json!([
                ["Email/parse", { "accountId": account, "blobIds": many }, "0"],
                ["Email/parse", { "accountId": account, "blobIds": few }, "1"],
                ["Email/query", { "accountId": account, "filter": big_filter }, "2"],
                ["Email/query", { "accountId": account, "sort": sort }, "3"],
                ["Mailbox/query", { "accountId": account, "filter": { "operator": "OR", "conditions": vec![serde_json::json!({ "name": "x" }); 100] } }, "4"],
                ["Email/query", { "accountId": account, "filter": { "text": "hallo" }, "sort": [{ "property": "receivedAt" }] }, "5"],
                ["SearchSnippet/get", { "accountId": account, "emailIds": many, "filter": { "text": "hallo" } }, "6"],
            ]),
        )
        .await;
    assert_eq!(responses[0][0], "error", "{}", responses[0]);
    assert_eq!(responses[0][1]["type"], "requestTooLarge");
    assert_eq!(responses[1][0], "Email/parse", "{}", responses[1]);
    assert_eq!(responses[1][1]["parsed"][&blob]["subject"], "Hallo");
    assert_eq!(responses[2][1]["type"], "unsupportedFilter", "{}", responses[2]);
    assert_eq!(responses[3][1]["type"], "unsupportedSort", "{}", responses[3]);
    assert_eq!(responses[4][1]["type"], "unsupportedFilter", "{}", responses[4]);
    assert_eq!(responses[5][1]["ids"], serde_json::json!([email]), "{}", responses[5]);
    assert_eq!(responses[6][1]["type"], "requestTooLarge", "{}", responses[6]);
}

/// Email/get and Email/parse do each thing once: an id or blob named 500 times is read once, a
/// property named 100 times is worked out once (the cleaned HTML above all), and more than 100
/// properties are refused.
#[tokio::test(flavor = "multi_thread")]
async fn email_get_and_parse_do_each_thing_once() {
    use serde_json::json;

    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let raw = "From: nyu@example.org\nSubject: Hallo\nContent-Type: text/html\n\n<p>Hallo <b>Mini</b></p>\n";
    let email = server.deliver("mini@example.org", raw).await;
    let responses = server
        .api(
            "mini@example.org",
            json!([["Email/get", { "accountId": account, "ids": [email], "properties": ["blobId"] }, "0"]]),
        )
        .await;
    let blob = responses[0][1]["list"][0]["blobId"].as_str().unwrap().to_owned();

    let ids = vec![email.clone(); uwumail_jmap::MAX_OBJECTS_IN_GET];
    let blobs = vec![blob.clone(); uwumail_jmap::MAX_OBJECTS_IN_GET];
    let too_many = vec!["uwuSafeHtml"; 1000];
    let many = vec!["uwuSafeHtml"; 100];
    let body_many = vec!["partId"; 101];
    let responses = server
        .api(
            "mini@example.org",
            json!([
                ["Email/get", { "accountId": account, "ids": ids, "properties": too_many }, "0"],
                ["Email/parse", { "accountId": account, "blobIds": blobs, "properties": too_many }, "1"],
                ["Email/get", { "accountId": account, "ids": ids, "properties": ["id"], "bodyProperties": body_many }, "2"],
                ["Email/get", { "accountId": account, "ids": ids, "properties": many, "fetchAllBodyValues": true }, "3"],
                ["Email/parse", { "accountId": account, "blobIds": blobs, "properties": many }, "4"],
            ]),
        )
        .await;
    for response in &responses[..3] {
        assert_eq!(response[1]["type"], "invalidArguments", "{response}");
    }
    let list = responses[3][1]["list"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{}", responses[3]);
    let got = list[0].as_object().unwrap();
    assert_eq!(got.keys().collect::<Vec<_>>(), ["id", "uwuSafeHtml"], "{}", responses[3]);
    assert!(got["uwuSafeHtml"].as_str().unwrap().contains("Mini"));
    let parsed = responses[4][1]["parsed"].as_object().unwrap();
    assert_eq!(parsed.len(), 1, "{}", responses[4]);
    assert!(parsed[&blob]["uwuSafeHtml"].as_str().unwrap().contains("Mini"));
}

/// Email/import takes no more than a /set may create, and stores nothing of a request that
/// asks for more.
#[tokio::test(flavor = "multi_thread")]
async fn email_import_takes_at_most_max_objects_in_set() {
    use serde_json::json;

    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let inbox = server.mailbox("mini@example.org", "inbox").await;
    let email = server.deliver("mini@example.org", "From: nyu@example.org\nSubject: Hallo\n\nHallo\n").await;
    let responses = server
        .api(
            "mini@example.org",
            json!([["Email/get", { "accountId": account, "ids": [email], "properties": ["blobId"] }, "0"]]),
        )
        .await;
    let blob = responses[0][1]["list"][0]["blobId"].as_str().unwrap().to_owned();
    let emails: serde_json::Map<String, serde_json::Value> = (0..=uwumail_jmap::MAX_OBJECTS_IN_SET)
        .map(|n| (format!("i{n}"), json!({ "blobId": blob, "mailboxIds": { (inbox.clone()): true } })))
        .collect();
    let responses = server
        .api(
            "mini@example.org",
            json!([
                ["Email/import", { "accountId": account, "emails": emails }, "0"],
                ["Email/query", { "accountId": account }, "1"],
            ]),
        )
        .await;
    assert_eq!(responses[0][1]["type"], "requestTooLarge", "{}", responses[0]);
    assert_eq!(responses[1][1]["ids"].as_array().unwrap().len(), 1, "{}", responses[1]);
}

/// An email has at most 100 keywords: one create, import or update naming more is refused, and
/// so is an update that would take it past them. Removing some always works.
#[tokio::test(flavor = "multi_thread")]
async fn keywords_per_email_are_bounded() {
    use serde_json::{Map, Value, json};

    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let inbox = server.mailbox("mini@example.org", "inbox").await;
    let email = server.deliver("mini@example.org", "From: nyu@example.org\nSubject: Hallo\n\nHallo\n").await;
    let responses = server
        .api(
            "mini@example.org",
            json!([["Email/get", { "accountId": account, "ids": [email], "properties": ["blobId"] }, "0"]]),
        )
        .await;
    let blob = responses[0][1]["list"][0]["blobId"].as_str().unwrap().to_owned();
    let limit = uwumail_store::MAX_KEYWORDS_PER_EMAIL;
    let keywords = |from: usize, count: usize| -> Map<String, Value> {
        (from..from + count).map(|n| (format!("k{n}"), Value::Bool(true))).collect()
    };
    let patch = |from: usize, count: usize| -> Map<String, Value> {
        (from..from + count).map(|n| (format!("keywords/k{n}"), Value::Bool(true))).collect()
    };
    let invalid = |response: &Value, key: &str| {
        assert_eq!(response["type"], "invalidProperties", "{key}: {response}");
    };

    let responses = server
        .api(
            "mini@example.org",
            json!([
                ["Email/set", { "accountId": account, "update": { (email.clone()): { "keywords": keywords(0, limit + 1) } } }, "0"],
                ["Email/set", { "accountId": account, "update": { (email.clone()): patch(0, limit + 1) } }, "1"],
                ["Email/import", { "accountId": account, "emails": { "i": { "blobId": blob, "mailboxIds": { (inbox.clone()): true }, "keywords": keywords(0, limit + 1) } } }, "2"],
                ["Email/set", { "accountId": account, "update": { (email.clone()): patch(0, 60) } }, "3"],
                ["Email/set", { "accountId": account, "update": { (email.clone()): patch(60, 60) } }, "4"],
                ["Email/set", { "accountId": account, "update": { (email.clone()): patch(60, limit - 60) } }, "5"],
                ["Email/set", { "accountId": account, "update": { (email.clone()): { "keywords/k0": null } } }, "6"],
            ]),
        )
        .await;
    invalid(&responses[0][1]["notUpdated"][&email], "keywords");
    invalid(&responses[1][1]["notUpdated"][&email], "keywords/");
    invalid(&responses[2][1]["notCreated"]["i"], "import");
    assert!(responses[3][1]["updated"].get(&email).is_some(), "{}", responses[3]);
    invalid(&responses[4][1]["notUpdated"][&email], "past the limit");
    assert!(responses[5][1]["updated"].get(&email).is_some(), "{}", responses[5]);
    assert!(responses[6][1]["updated"].get(&email).is_some(), "{}", responses[6]);
}

/// An account has at most MAX_PUSH_CONNECTIONS event streams and WebSockets open together; one
/// more is answered 429, and a place is free again once one closes.
#[tokio::test(flavor = "multi_thread")]
async fn push_connections_per_account_are_bounded() {
    use tower::ServiceExt;

    let server = server().await;
    let stream = |login: &str| {
        Request::get("/jmap/eventsource/?types=*&ping=0")
            .header(header::AUTHORIZATION, common::basic(login, common::PASSWORD))
            .body(Body::empty())
            .unwrap()
    };
    let mut open = Vec::new();
    for _ in 0..uwumail_jmap::MAX_PUSH_CONNECTIONS {
        let response = server.router.clone().oneshot(stream("mini@example.org")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        open.push(response);
    }
    let (status, body) = server.request(stream("mini@example.org")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["limit"], "maxPushConnections", "{body}");
    let url = crate::websocket::listen(server.router.clone()).await;
    let basic = common::basic("mini@example.org", common::PASSWORD);
    assert!(crate::websocket::connect(&url, Some(&basic), Some("jmap")).await.is_err());

    // Someone else is not affected, and a closed stream makes room.
    let response = server.router.clone().oneshot(stream("nyu@example.org")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drop(open.pop());
    let socket = crate::websocket::connect(&url, Some(&basic), Some("jmap")).await.unwrap();
    let (status, _) = server.request(stream("mini@example.org")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    drop(socket);
}
