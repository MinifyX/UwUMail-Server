//! AddressSuggestion/query: addresses from the address book and recent mail, best first.

mod common;

use common::{args, server};
use serde_json::{Value, json};

const MINI: &str = "mini@example.org";
const USING: [&str; 4] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:contacts",
    "urn:uwumail:jmap:suggest",
];

async fn suggest(server: &common::Server, text: &str, limit: Option<u64>) -> Vec<Value> {
    let account = server.account_id(MINI).await;
    let mut arguments = json!({ "accountId": account, "text": text });
    if let Some(limit) = limit {
        arguments["limit"] = json!(limit);
    }
    let responses = server.api_using(MINI, &USING, json!([["AddressSuggestion/query", arguments, "0"]])).await;
    args(&responses, 0, "AddressSuggestion/query")["list"].as_array().unwrap().clone()
}

fn emails(list: &[Value]) -> Vec<&str> {
    list.iter().map(|s| s["email"].as_str().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn suggestions_come_from_the_address_book_and_recent_mail() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let (_, body) = server
        .request(
            axum::http::Request::get("/jmap/session")
                .header("authorization", common::basic(MINI, common::PASSWORD))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    let session: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["accounts"][&account]["accountCapabilities"]["urn:uwumail:jmap:suggest"]["maxLimit"], 50);

    // In the address book.
    let responses = server
        .api_using(
            MINI,
            &USING,
            json!([["ContactCard/set", { "accountId": account, "create": { "c": {
                "name": { "full": "Nyu Katze" },
                "emails": { "e1": { "address": "nyu.katze@cats.example" } }
            } } }, "0"]]),
        )
        .await;
    assert!(args(&responses, 0, "ContactCard/set")["created"]["c"].is_object(), "{}", responses[0]);

    // Heard from, several times, and in the junk mailbox (never suggested).
    for n in 0..3 {
        server
            .deliver(MINI, &format!("From: Karla Kater <karla@cats.example>\nTo: mini@example.org\nSubject: {n}\n\nMiau\n"))
            .await;
    }
    // Written to: a message in Sent.
    let sent = server.mailbox(MINI, "sent").await;
    let raw = "From: mini@example.org\r\nTo: Kai Kralle <kai@paws.example>, Mini <mini@example.org>\r\nSubject: Hi\r\n\r\nPurr\r\n";
    let upload = axum::http::Request::post(format!("/jmap/upload/{account}/"))
        .header("authorization", common::basic(MINI, common::PASSWORD))
        .body(axum::body::Body::from(raw))
        .unwrap();
    let (_, body) = server.request(upload).await;
    let blob = serde_json::from_slice::<Value>(&body).unwrap()["blobId"].as_str().unwrap().to_owned();
    let responses = server
        .api(
            MINI,
            json!([["Email/import", { "accountId": account, "emails": { "s": { "blobId": blob, "mailboxIds": { sent: true } } } }, "0"]]),
        )
        .await;
    assert!(args(&responses, 0, "Email/import")["created"]["s"].is_object(), "{}", responses[0]);
    let junk_id = server
        .deliver(MINI, "From: Kevin Kaufmann <kevin@spam.example>\nTo: mini@example.org\nSubject: Buy\n\nNow\n")
        .await;
    let junk = server.mailbox(MINI, "junk").await;
    server
        .api(MINI, json!([["Email/set", { "accountId": account, "update": { junk_id: { "mailboxIds": { junk: true } } } }, "0"]]))
        .await;

    // "k" fits all three by a word of the name; the address book first, then whom Mini wrote to.
    let list = suggest(&server, "k", None).await;
    assert_eq!(emails(&list), ["nyu.katze@cats.example", "kai@paws.example", "karla@cats.example"], "{list:?}");
    assert_eq!(list[0]["name"], "Nyu Katze");
    assert_eq!(list[0]["source"], "contact");
    assert_eq!(list[1]["name"], "Kai Kralle");
    assert_eq!(list[1]["source"], "sent");
    assert_eq!(list[2]["sources"], json!(["received"]));
    assert!(list[2]["lastUsedAt"].is_string());

    // By address and by domain; the own address is never suggested; limit holds.
    assert_eq!(emails(&suggest(&server, "karla@", None).await), ["karla@cats.example"]);
    assert_eq!(emails(&suggest(&server, "cats.example", None).await), ["nyu.katze@cats.example", "karla@cats.example"]);
    assert!(emails(&suggest(&server, "mini", None).await).is_empty());
    assert_eq!(suggest(&server, "", Some(1)).await.len(), 1);
    assert!(emails(&suggest(&server, "kevin", None).await).is_empty(), "junk is not a source");

    // The capability has to be asked for, and the arguments are checked.
    let responses = server.api(MINI, json!([["AddressSuggestion/query", { "accountId": account, "text": "k" }, "0"]])).await;
    assert_eq!(responses[0][1]["type"], "unknownMethod");
    let responses = server
        .api_using(MINI, &USING, json!([["AddressSuggestion/query", { "accountId": account, "limit": 0 }, "0"]]))
        .await;
    assert_eq!(responses[0][1]["type"], "invalidArguments");
}
