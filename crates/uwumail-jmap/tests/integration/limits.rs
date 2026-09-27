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
