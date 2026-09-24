//! End-to-end JMAP tests against the router, as a client would use it.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::Jmap;
use uwumail_smtp::{DeliveryConfig, Smtp, SmtpConfig, SmtpSettings, ToneConfig};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";
const USING: [&str; 6] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "urn:ietf:params:jmap:vacationresponse",
    "urn:uwumail:jmap:senders",
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

fn basic(login: &str, password: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{login}:{password}")))
}

impl Server {
    async fn request(&self, request: Request<Body>) -> (StatusCode, Vec<u8>) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        (status, to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap().to_vec())
    }

    async fn get(&self, uri: &str, login: &str) -> (StatusCode, Vec<u8>) {
        let request = Request::get(uri)
            .header(header::AUTHORIZATION, basic(login, PASSWORD))
            .header(header::HOST, "mail.example.org")
            .body(Body::empty())
            .unwrap();
        self.request(request).await
    }

    async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        let body = json!({ "using": USING, "methodCalls": calls });
        let request = Request::post("/jmap/api")
            .header(header::AUTHORIZATION, basic(login, PASSWORD))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, bytes) = self.request(request).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        response["methodResponses"].as_array().unwrap().clone()
    }

    async fn account_id(&self, login: &str) -> String {
        format!("a{}", self.store.account(login).await.unwrap().unwrap().id)
    }
}

fn args<'a>(responses: &'a [Value], index: usize, name: &str) -> &'a Value {
    assert_eq!(responses[index][0], name, "response {index}: {}", responses[index]);
    &responses[index][1]
}

#[tokio::test(flavor = "multi_thread")]
async fn session_and_authentication() {
    let server = server().await;
    let (status, body) = server.get("/.well-known/jmap", "mini@example.org").await;
    assert_eq!(status, StatusCode::OK);
    let session: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["apiUrl"], "http://mail.example.org/jmap/api");
    assert_eq!(session["username"], "mini@example.org");
    let account = server.account_id("mini@example.org").await;
    assert_eq!(session["primaryAccounts"]["urn:ietf:params:jmap:mail"], account);

    let wrong = Request::get("/jmap/session")
        .header(header::AUTHORIZATION, basic("mini@example.org", "nope"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(server.request(wrong).await.0, StatusCode::UNAUTHORIZED);
    let missing = Request::get("/jmap/session").body(Body::empty()).unwrap();
    assert_eq!(server.request(missing).await.0, StatusCode::UNAUTHORIZED);

    let other = server.account_id("nyu@example.org").await;
    let responses = server.api("mini@example.org", json!([["Mailbox/get", { "accountId": other }, "0"]])).await;
    assert_eq!(responses[0][1]["type"], "accountNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_pictures_are_fetched_only_for_their_account_and_never_from_inside() {
    let server = server().await;
    let (_, body) = server.get("/jmap/session", "mini@example.org").await;
    let session: Value = serde_json::from_slice(&body).unwrap();
    let account = server.account_id("mini@example.org").await;
    assert_eq!(
        session["capabilities"]["urn:uwumail:jmap:remote"]["imageUrl"],
        "http://mail.example.org/jmap/image/{accountId}?url={url}"
    );

    let inside = format!("/jmap/image/{account}?url=http%3A%2F%2F127.0.0.1%2Fadmin");
    let (status, body) = server.get(&inside, "mini@example.org").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", String::from_utf8_lossy(&body));
    let (status, _) =
        server.get(&format!("/jmap/image/{account}?url=file%3A%2F%2F%2Fetc%2Fpasswd"), "mini@example.org").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let long = format!("/jmap/image/{account}?url=https%3A%2F%2Fpictures.example%2F{}", "a".repeat(5000));
    assert_eq!(server.get(&long, "mini@example.org").await.0, StatusCode::BAD_REQUEST);

    let (status, _) = server.get(&inside, "nyu@example.org").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not someone else's account");
    let anonymous = Request::get(inside.as_str()).body(Body::empty()).unwrap();
    assert_eq!(server.request(anonymous).await.0, StatusCode::UNAUTHORIZED);

    // Sender pictures: announced, and never asked of a mail provider on a person's behalf.
    assert_eq!(
        session["capabilities"]["urn:uwumail:jmap:remote"]["pictureUrl"],
        "http://mail.example.org/jmap/picture/{accountId}?email={email}"
    );
    let person = format!("/jmap/picture/{account}?email=friend%40gmail.com");
    assert_eq!(server.get(&person, "mini@example.org").await.0, StatusCode::NOT_FOUND);
    assert_eq!(server.get(&person, "nyu@example.org").await.0, StatusCode::NOT_FOUND, "not someone else's account");
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_jmap_off_shuts_the_door_at_once() {
    let server = server().await;
    assert_eq!(server.get("/.well-known/jmap", "mini@example.org").await.0, StatusCode::OK);

    // The right password is remembered for a while, because checking it is slow on purpose. The
    // switch has to hold anyway, or it would only hold once that memory runs out.
    server
        .store
        .update_account(
            "mini@example.org",
            uwumail_store::AccountUpdate {
                protocols: Some(uwumail_store::Protocols { jmap: false, ..Default::default() }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(server.get("/.well-known/jmap", "mini@example.org").await.0, StatusCode::UNAUTHORIZED);

    // And back on again, without anybody having to type a new password.
    server
        .store
        .update_account(
            "mini@example.org",
            uwumail_store::AccountUpdate { protocols: Some(uwumail_store::Protocols::default()), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(server.get("/.well-known/jmap", "mini@example.org").await.0, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_searching_and_changing_mail() {
    let server = server().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let account_number: i64 = account[1..].parse().unwrap();

    let first = server.api(login, json!([["Email/get", { "accountId": account, "ids": [] }, "0"]])).await;
    let start_state = args(&first, 0, "Email/get")["state"].as_str().unwrap().to_owned();

    let raw = "From: Nyu <nyu@example.net>\r\nTo: Mini <mini@example.org>\r\nSubject: Katzenfutter\r\nMessage-ID: <k1@example.net>\r\n\
MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
Bitte Thunfisch kaufen\r\n--x\r\nContent-Type: text/csv; name=liste.csv\r\nContent-Disposition: attachment; filename=liste.csv\r\n\r\n\
Thunfisch;2\r\n--x--\r\n";
    server
        .store
        .ingest(IngestRequest {
            account_id: account_number,
            raw: raw.as_bytes().to_vec(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        })
        .await
        .unwrap();

    let responses = server
        .api(
            login,
            json!([
                ["Mailbox/get", { "accountId": account, "ids": null }, "m"],
                ["Mailbox/query", { "accountId": account, "filter": { "role": "inbox" } }, "q"],
                ["Email/query", { "accountId": account, "filter": { "inMailbox": "#inbox", "text": "thunfisch" },
                    "sort": [{ "property": "receivedAt", "isAscending": false }], "calculateTotal": true }, "e"],
            ]),
        )
        .await;
    let mailboxes = args(&responses, 0, "Mailbox/get")["list"].as_array().unwrap().clone();
    assert_eq!(mailboxes.len(), 6);
    let inbox = mailboxes.iter().find(|m| m["role"] == "inbox").unwrap();
    let archive_id = mailboxes.iter().find(|m| m["role"] == "archive").unwrap()["id"].as_str().unwrap().to_owned();
    let drafts_id = mailboxes.iter().find(|m| m["role"] == "drafts").unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(inbox["unreadEmails"], 1);
    let inbox_id = inbox["id"].as_str().unwrap().to_owned();
    assert_eq!(args(&responses, 1, "Mailbox/query")["ids"], json!([inbox_id]));
    // "#inbox" is not a creation id here, so the filter matches nothing.
    assert_eq!(args(&responses, 2, "Email/query")["ids"], json!([]));

    let responses = server
        .api(
            login,
            json!([
                ["Email/query", { "accountId": account, "filter": { "inMailbox": inbox_id, "text": "thunfisch" }, "calculateTotal": true }, "0"],
                ["Email/get", { "accountId": account, "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                    "properties": ["id", "threadId", "subject", "from", "mailboxIds", "keywords", "textBody", "attachments", "bodyValues", "header:Message-ID:asMessageIds"],
                    "fetchTextBodyValues": true }, "1"],
                ["Thread/get", { "accountId": account, "#ids": { "resultOf": "1", "name": "Email/get", "path": "/list/*/threadId" } }, "2"],
                ["SearchSnippet/get", { "accountId": account, "filter": { "text": "thunfisch" },
                    "#emailIds": { "resultOf": "0", "name": "Email/query", "path": "/ids" } }, "3"],
                ["Email/changes", { "accountId": account, "sinceState": start_state }, "4"],
            ]),
        )
        .await;
    assert_eq!(args(&responses, 0, "Email/query")["total"], 1);
    let email = &args(&responses, 1, "Email/get")["list"][0];
    let email_id = email["id"].as_str().unwrap().to_owned();
    assert_eq!(email["subject"], "Katzenfutter");
    assert_eq!(email["from"][0]["email"], "nyu@example.net");
    assert_eq!(email["header:Message-ID:asMessageIds"], json!(["k1@example.net"]));
    let text_part = email["textBody"][0]["partId"].as_str().unwrap();
    assert!(email["bodyValues"][text_part]["value"].as_str().unwrap().contains("Thunfisch kaufen"));
    assert_eq!(args(&responses, 2, "Thread/get")["list"][0]["emailIds"], json!([email_id]));
    assert_eq!(args(&responses, 3, "SearchSnippet/get")["list"][0]["preview"], "Bitte <mark>Thunfisch</mark> kaufen");
    assert_eq!(args(&responses, 4, "Email/changes")["created"], json!([email_id]));

    // Attachment download.
    let attachment_blob = email["attachments"][0]["blobId"].as_str().unwrap();
    let (status, bytes) =
        server.get(&format!("/jmap/download/{account}/{attachment_blob}/liste.csv?accept=text/csv"), login).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&bytes).starts_with("Thunfisch;2"));
    let (status, _) = server.get(&format!("/jmap/download/{account}/{attachment_blob}/x"), "nyu@example.org").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "other accounts cannot download it");

    // Mark as read and archive.
    let responses = server
        .api(
            login,
            json!([
                ["Email/set", { "accountId": account, "update": { email_id.clone(): {
                    "keywords/$seen": true, format!("mailboxIds/{inbox_id}"): null, format!("mailboxIds/{archive_id}"): true
                } } }, "0"],
                ["Mailbox/get", { "accountId": account, "ids": [inbox_id, archive_id], "properties": ["unreadEmails", "totalEmails"] }, "1"],
                ["Email/set", { "accountId": account, "update": { "e999999": { "keywords/$seen": true } } }, "2"],
            ]),
        )
        .await;
    assert!(args(&responses, 0, "Email/set")["updated"].get(&email_id).is_some());
    let counts = args(&responses, 1, "Mailbox/get")["list"].as_array().unwrap().clone();
    assert_eq!((counts[0]["totalEmails"].clone(), counts[1]["totalEmails"].clone()), (json!(0), json!(1)));
    assert_eq!(counts[1]["unreadEmails"], 0);
    assert_eq!(args(&responses, 2, "Email/set")["notUpdated"]["e999999"]["type"], "notFound");

    // Folders.
    let responses = server
        .api(
            login,
            json!([
                ["Mailbox/set", { "accountId": account, "create": { "k": { "name": "Katzen", "parentId": null } } }, "0"],
                ["Mailbox/set", { "accountId": account, "create": { "n": { "name": "Nyu", "parentId": "#k" } } }, "1"],
                ["Mailbox/set", { "accountId": account, "destroy": ["#k", inbox_id] }, "2"],
                ["Mailbox/set", { "accountId": account, "create": {
                    "child": { "name": "Bugs", "parentId": "#parent" }, "parent": { "name": "Projekte" } } }, "3"],
            ]),
        )
        .await;
    assert!(args(&responses, 0, "Mailbox/set")["created"]["k"]["id"].is_string());
    assert!(args(&responses, 1, "Mailbox/set")["created"]["n"]["id"].is_string());
    let not_destroyed = &args(&responses, 2, "Mailbox/set")["notDestroyed"];
    assert_eq!(not_destroyed.as_object().unwrap().len(), 2, "{not_destroyed}");
    let nested = &args(&responses, 3, "Mailbox/set")["created"];
    assert!(
        nested["child"]["id"].is_string() && nested["parent"]["id"].is_string(),
        "parents are created first: {}",
        responses[3]
    );
    let _ = drafts_id;
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_import_and_send_like_the_uwumail_app() {
    let server = server().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let nyu = server.account_id("nyu@example.org").await;

    let responses = server
        .api(login, json!([["Mailbox/get", { "accountId": account, "ids": null, "properties": ["id", "role"] }, "0"]]))
        .await;
    let mailboxes = args(&responses, 0, "Mailbox/get")["list"].as_array().unwrap().clone();
    let sent_id = mailboxes.iter().find(|m| m["role"] == "sent").unwrap()["id"].as_str().unwrap().to_owned();
    let drafts_id = mailboxes.iter().find(|m| m["role"] == "drafts").unwrap()["id"].as_str().unwrap().to_owned();

    let message = "From: Mini <mini@example.org>\r\nTo: Nyu <nyu@example.org>\r\nBcc: geheim@example.org\r\nSubject: Hallo Nyu\r\n\r\nMiau!\r\n";
    let upload = Request::post(format!("/jmap/upload/{account}/"))
        .header(header::AUTHORIZATION, basic(login, PASSWORD))
        .header(header::CONTENT_TYPE, "message/rfc822")
        .body(Body::from(message))
        .unwrap();
    let (status, body) = server.request(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    let blob_id = serde_json::from_slice::<Value>(&body).unwrap()["blobId"].as_str().unwrap().to_owned();

    let responses = server
        .api(
            login,
            json!([
                ["Identity/get", { "accountId": account, "ids": null }, "0"],
                ["Email/import", { "accountId": account, "emails": { "outgoing": {
                    "blobId": blob_id, "mailboxIds": { drafts_id.clone(): true }, "keywords": { "$seen": true, "$draft": true } } } }, "1"],
            ]),
        )
        .await;
    let identity = args(&responses, 0, "Identity/get")["list"][0].clone();
    assert_eq!(identity["email"], "mini@example.org");
    let email_id = args(&responses, 1, "Email/import")["created"]["outgoing"]["id"].as_str().unwrap().to_owned();

    let responses = server
        .api(
            login,
            json!([
                ["EmailSubmission/set", {
                    "accountId": account,
                    "create": { "send": { "identityId": identity["id"], "emailId": email_id } },
                    "onSuccessUpdateEmail": { "#send": { format!("mailboxIds/{drafts_id}"): null, format!("mailboxIds/{sent_id}"): true, "keywords/$draft": null } }
                }, "0"],
                ["Email/get", { "accountId": account, "ids": [email_id], "properties": ["mailboxIds", "keywords"] }, "1"],
            ]),
        )
        .await;
    let created = &args(&responses, 0, "EmailSubmission/set")["created"]["send"];
    assert_eq!(created["undoStatus"], "final", "{}", responses[0]);
    assert!(args(&responses, 1, "Email/set")["updated"].get(&email_id).is_some());
    let sent = &args(&responses, 2, "Email/get")["list"][0];
    assert_eq!(sent["mailboxIds"], json!({ sent_id: true }));
    assert_eq!(sent["keywords"], json!({ "$seen": true }));

    // Nyu got it, without the Bcc header.
    let responses = server
        .api(
            "nyu@example.org",
            json!([
                ["Email/query", { "accountId": nyu }, "0"],
                ["Email/get", { "accountId": nyu, "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                    "properties": ["subject", "bcc", "header:Bcc", "header:DKIM-Signature"] }, "1"],
            ]),
        )
        .await;
    let received = &args(&responses, 1, "Email/get")["list"][0];
    assert_eq!(received["subject"], "Hallo Nyu");
    assert_eq!(received["header:Bcc"], Value::Null);
    assert!(received["header:DKIM-Signature"].is_string(), "outgoing mail is signed");

    // Sending as someone else is refused.
    let responses = server
        .api(
            login,
            json!([["EmailSubmission/set", { "accountId": account, "create": { "x": {
                "identityId": identity["id"], "emailId": email_id,
                "envelope": { "mailFrom": { "email": "boss@bank.example" }, "rcptTo": [{ "email": "nyu@example.org" }] } } } }, "0"]]),
        )
        .await;
    assert_eq!(args(&responses, 0, "EmailSubmission/set")["notCreated"]["x"]["type"], "forbiddenMailFrom");
}

#[tokio::test(flavor = "multi_thread")]
async fn drafts_vacation_and_push() {
    let server = server().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let responses = server
        .api(login, json!([["Mailbox/query", { "accountId": account, "filter": { "role": "drafts" } }, "0"]]))
        .await;
    let drafts = args(&responses, 0, "Mailbox/query")["ids"][0].as_str().unwrap().to_owned();

    // Push: wait for the next state change in the background.
    let router = server.router.clone();
    let push = open_push(router, "*").await;

    let responses = server
        .api(
            login,
            json!([
                ["Email/set", { "accountId": account, "create": { "draft": {
                    "mailboxIds": { drafts.clone(): true },
                    "keywords": { "$draft": true },
                    "from": [{ "name": "Mini", "email": "mini@example.org" }],
                    "to": [{ "email": "nyu@example.org" }],
                    "subject": "Entwurf",
                    "bodyValues": { "1": { "value": "Noch nicht fertig" } },
                    "textBody": [{ "partId": "1", "type": "text/plain" }]
                } } }, "0"],
                ["Email/get", { "accountId": account, "ids": ["#draft"], "properties": ["subject", "preview", "keywords"] }, "1"],
                ["VacationResponse/set", { "accountId": account, "update": { "singleton": {
                    "isEnabled": true, "subject": "Bin weg", "textBody": "Ab Montag wieder da", "toDate": "2030-01-01T00:00:00Z" } } }, "2"],
                ["VacationResponse/get", { "accountId": account }, "3"],
            ]),
        )
        .await;
    assert!(args(&responses, 0, "Email/set")["created"]["draft"]["id"].is_string(), "{}", responses[0]);
    let draft = &args(&responses, 1, "Email/get")["list"][0];
    assert_eq!(draft["subject"], "Entwurf");
    assert_eq!(draft["preview"], "Noch nicht fertig");
    assert_eq!(args(&responses, 2, "VacationResponse/set")["updated"], json!({ "singleton": null }));
    let vacation = &args(&responses, 3, "VacationResponse/get")["list"][0];
    assert_eq!(vacation["isEnabled"], true);
    assert_eq!(vacation["toDate"], "2030-01-01T00:00:00Z");

    let event = tokio::time::timeout(std::time::Duration::from_secs(10), push).await.unwrap().unwrap();
    assert!(event.contains("event: state"), "{event}");
    assert!(event.contains("\"Email\""), "{event}");
}

#[tokio::test(flavor = "multi_thread")]
async fn people_block_senders_on_the_server_like_the_uwumail_app() {
    let server = server().await;
    let (_, body) = server.get("/.well-known/jmap", "mini@example.org").await;
    let session: Value = serde_json::from_slice(&body).unwrap();
    let account = server.account_id("mini@example.org").await;
    assert!(session["capabilities"]["urn:uwumail:jmap:senders"].is_object(), "{session}");
    assert_eq!(session["accounts"][&account]["accountCapabilities"]["urn:uwumail:jmap:senders"]["maxEntries"], 1000);

    let responses = server
        .api(
            "mini@example.org",
            json!([
                ["SenderList/set", { "accountId": account, "create": {
                    "a": { "list": "block", "value": "Werbung@Shop.example" },
                    "b": { "list": "block", "kind": "domain", "value": "@newsletter.example" },
                    "c": { "list": "block", "value": "not an address@" },
                    "d": { "list": "maybe", "value": "x@example.net" },
                } }, "0"],
                ["SenderList/get", { "accountId": account }, "1"],
            ]),
        )
        .await;
    let set = args(&responses, 0, "SenderList/set");
    assert_eq!(set["created"]["a"]["value"], "werbung@shop.example", "the client learns the stored form");
    assert_eq!(set["created"]["a"]["kind"], "address");
    assert_eq!(set["created"]["b"]["value"], "newsletter.example");
    assert_eq!(set["notCreated"]["c"]["type"], "senderInvalid");
    assert_eq!(set["notCreated"]["d"]["type"], "invalidProperties");
    let get = args(&responses, 1, "SenderList/get");
    assert_eq!(get["list"].as_array().map(Vec::len), Some(2));
    assert_eq!(get["state"], set["newState"]);
    assert_ne!(set["oldState"], set["newState"]);

    // The same list as in the portal, and nobody else's.
    let entries = server
        .store
        .sender_list(uwumail_store::ListScope::Account(
            server.store.account("mini@example.org").await.unwrap().unwrap().id,
        ))
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    let other = server.account_id("nyu@example.org").await;
    let theirs = server.api("nyu@example.org", json!([["SenderList/get", { "accountId": other }, "0"]])).await;
    assert_eq!(args(&theirs, 0, "SenderList/get")["list"], json!([]));
    let id = set["created"]["a"]["id"].as_str().unwrap();
    let stolen =
        server.api("nyu@example.org", json!([["SenderList/set", { "accountId": other, "destroy": [id] }, "0"]])).await;
    assert_eq!(args(&stolen, 0, "SenderList/set")["notDestroyed"][id]["type"], "notFound");

    let responses = server
        .api(
            "mini@example.org",
            json!([
                ["SenderList/set", { "accountId": account, "ifInState": "stale", "destroy": [id] }, "0"],
                ["SenderList/set", { "accountId": account, "destroy": [id], "update": { "l1": { "note": "x" } } }, "1"],
            ]),
        )
        .await;
    assert_eq!(responses[0][1]["type"], "stateMismatch");
    let set = args(&responses, 1, "SenderList/set");
    assert_eq!(set["destroyed"], json!([id]));
    assert_eq!(set["notUpdated"]["l1"]["type"], "forbidden");

    // Without the capability in `using` the methods are unknown.
    let body = json!({ "using": ["urn:ietf:params:jmap:core"], "methodCalls": [["SenderList/get", { "accountId": account }, "0"]] });
    let request = Request::post("/jmap/api")
        .header(header::AUTHORIZATION, basic("mini@example.org", PASSWORD))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (_, bytes) = server.request(request).await;
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["methodResponses"][0][1]["type"], "unknownMethod");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_sync_between_the_apps_the_webmail_and_the_portal() {
    let server = server().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let (_, body) = server.get("/.well-known/jmap", login).await;
    let session: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["capabilities"]["urn:uwumail:jmap:settings"], json!({}));
    assert_eq!(
        session["accounts"][&account]["accountCapabilities"]["urn:uwumail:jmap:settings"],
        json!({ "maxKeys": 5000, "maxSize": 1048576, "maxValueSize": 262144 })
    );
    assert_eq!(session["primaryAccounts"]["urn:uwumail:jmap:settings"], account);

    // Nothing stored yet.
    let responses = server.api(login, json!([["UserSettings/get", { "accountId": account, "ids": null }, "0"]])).await;
    let get = args(&responses, 0, "UserSettings/get");
    assert_eq!(get["list"], json!([{ "id": "singleton", "values": {} }]));
    let empty_state = get["state"].clone();

    let signature = json!({ "email": "mini@example.org", "name": "Arbeit", "html": "<p>Mini</p>", "forNew": true, "forReplies": false });
    let responses = server
        .api(
            login,
            json!([
                ["UserSettings/set", { "accountId": account, "update": { "singleton": {
                    "values/theme": "dark",
                    "values/conversations": false,
                    "values/undoSendSeconds": 10,
                    "values/trustedSenders:@shop.example": true,
                    "values/linkDomains:example.com": true,
                    "values/signature:work": signature.clone(),
                } } }, "0"],
                ["UserSettings/get", { "accountId": account, "ids": ["singleton", "other"] }, "1"],
            ]),
        )
        .await;
    let set = args(&responses, 0, "UserSettings/set");
    assert_eq!(set["updated"], json!({ "singleton": null }), "{set}");
    assert_eq!(set["oldState"], empty_state);
    assert_ne!(set["newState"], empty_state);
    let get = args(&responses, 1, "UserSettings/get");
    assert_eq!(get["state"], set["newState"]);
    assert_eq!(get["notFound"], json!(["other"]));
    assert_eq!(
        get["list"][0]["values"],
        json!({
            "theme": "dark",
            "conversations": false,
            "undoSendSeconds": 10,
            "trustedSenders:@shop.example": true,
            "linkDomains:example.com": true,
            "signature:work": signature,
        })
    );
    let state = set["newState"].as_str().unwrap().to_owned();

    // The portal and the webmail read the same preferences.
    let id = server.store.account(login).await.unwrap().unwrap().id;
    let preferences = server.store.preferences(id).await.unwrap();
    assert_eq!((preferences["theme"].as_str(), preferences["mailConversations"].as_str()), (Some("dark"), Some("off")));

    // One bad key refuses the whole update, and nothing of it is kept.
    let responses = server
        .api(
            login,
            json!([
                ["UserSettings/set", { "accountId": account, "update": { "singleton": {
                    "values/tone": "neutral",
                    "values/colour": "pink",
                    "values/trustedSenders:Big@Shop.example": true,
                    "values/theme": null,
                } } }, "0"],
                ["UserSettings/get", { "accountId": account }, "1"],
            ]),
        )
        .await;
    let set = args(&responses, 0, "UserSettings/set");
    assert_eq!(set["notUpdated"]["singleton"]["type"], "invalidProperties");
    assert_eq!(set["notUpdated"]["singleton"]["properties"], json!(["colour", "trustedSenders:Big@Shop.example"]));
    assert_eq!(set["newState"], state);
    let values = &args(&responses, 1, "UserSettings/get")["list"][0]["values"];
    assert_eq!((values["theme"].as_str(), values.get("tone")), (Some("dark"), None));

    // null removes a key; ifInState guards against writes in between.
    let responses = server
        .api(
            login,
            json!([
                ["UserSettings/set", { "accountId": account, "ifInState": state, "update": { "singleton": {
                    "values/theme": null,
                    "values/trustedSenders:@shop.example": null,
                    "values/signature:work": null,
                } } }, "0"],
                ["UserSettings/set", { "accountId": account, "ifInState": state, "update": { "singleton": {
                    "values/tone": "neutral",
                } } }, "1"],
                ["UserSettings/get", { "accountId": account, "properties": ["values"] }, "2"],
            ]),
        )
        .await;
    let set = args(&responses, 0, "UserSettings/set");
    assert_eq!(set["updated"], json!({ "singleton": null }), "{set}");
    assert_eq!(responses[1][1]["type"], "stateMismatch", "the state moved with the first call");
    let values = &args(&responses, 2, "UserSettings/get")["list"][0]["values"];
    assert_eq!(values, &json!({ "conversations": false, "undoSendSeconds": 10, "linkDomains:example.com": true }));
    assert!(!server.store.preferences(id).await.unwrap().contains_key("theme"));

    // Limits: one value too large, too many keys.
    let huge = json!({ "email": "", "name": "", "html": "x".repeat(300_000), "forNew": true, "forReplies": true });
    let many: serde_json::Map<String, Value> =
        (0..5000).map(|n| (format!("values/linkDomains:host{n}.example"), json!(true))).collect();
    let responses = server
        .api(
            login,
            json!([
                ["UserSettings/set", { "accountId": account, "update": { "singleton": { "values/signature:big": huge } } }, "0"],
                ["UserSettings/set", { "accountId": account, "update": { "singleton": many } }, "1"],
            ]),
        )
        .await;
    assert_eq!(args(&responses, 0, "UserSettings/set")["notUpdated"]["singleton"]["type"], "tooLarge");
    assert_eq!(args(&responses, 1, "UserSettings/set")["notUpdated"]["singleton"]["type"], "overQuota");

    // A whole replacement, and the singleton cannot be created or destroyed.
    let responses = server
        .api(
            login,
            json!([
                ["UserSettings/set", { "accountId": account,
                    "create": { "new": { "values": {} } },
                    "update": { "singleton": { "values": { "language": "en", "senderAppearance:news@shop.example": "dark" } } },
                    "destroy": ["singleton"] }, "0"],
                ["UserSettings/get", { "accountId": account }, "1"],
            ]),
        )
        .await;
    let set = args(&responses, 0, "UserSettings/set");
    assert_eq!(set["notCreated"]["new"]["type"], "singleton");
    assert_eq!(set["notDestroyed"]["singleton"]["type"], "singleton");
    assert_eq!(set["updated"], json!({ "singleton": null }));
    let get = args(&responses, 1, "UserSettings/get");
    assert_eq!(get["list"][0]["values"], json!({ "language": "en", "senderAppearance:news@shop.example": "dark" }));
    let preferences = server.store.preferences(id).await.unwrap();
    assert!(!preferences.contains_key("mailConversations"), "replaced away: {preferences:?}");

    // A change in the portal shows up here with a new state.
    let before = get["state"].clone();
    let change = json!({ "mailSenderPictures": "off" }).as_object().unwrap().clone();
    server.store.update_preferences(id, change).await.unwrap();
    let responses = server.api(login, json!([["UserSettings/get", { "accountId": account }, "0"]])).await;
    let get = args(&responses, 0, "UserSettings/get");
    assert_ne!(get["state"], before);
    assert_eq!(get["list"][0]["values"]["senderPictures"], false);

    // Somebody else's settings are out of reach.
    let other = server.account_id("nyu@example.org").await;
    let responses = server
        .api(
            login,
            json!([
                ["UserSettings/get", { "accountId": other }, "0"],
                ["UserSettings/set", { "accountId": other, "update": { "singleton": { "values/theme": "light" } } }, "1"],
            ]),
        )
        .await;
    assert_eq!(responses[0][1]["type"], "accountNotFound");
    assert_eq!(responses[1][1]["type"], "accountNotFound");
    let theirs = server.api("nyu@example.org", json!([["UserSettings/get", { "accountId": other }, "0"]])).await;
    assert_eq!(args(&theirs, 0, "UserSettings/get")["list"][0]["values"], json!({}));
}

/// Opens an event source and returns what it sends until the first state change. It is listening once
/// this returns: the server subscribes before it answers, so no change can slip past, however busy
/// the machine running the tests is.
async fn open_push(router: Router, types: &str) -> tokio::task::JoinHandle<String> {
    let request = Request::get(format!("/jmap/eventsource/?types={types}&closeafter=state&ping=0"))
        .header(header::AUTHORIZATION, basic("mini@example.org", PASSWORD))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::spawn(async move {
        String::from_utf8(to_bytes(response.into_body(), 1024 * 1024).await.unwrap().to_vec()).unwrap()
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settings_change_is_pushed_with_its_state() {
    let server = server().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let router = server.router.clone();
    let push = open_push(router, "UserSettings").await;

    let update = json!({ "singleton": { "values/linkConfirm": true } });
    let responses =
        server.api(login, json!([["UserSettings/set", { "accountId": account, "update": update }, "0"]])).await;
    let state = args(&responses, 0, "UserSettings/set")["newState"].as_str().unwrap().to_owned();

    let event = tokio::time::timeout(std::time::Duration::from_secs(10), push).await.unwrap().unwrap();
    let data = event.lines().find_map(|line| line.strip_prefix("data:")).unwrap();
    let data: Value = serde_json::from_str(data.trim()).unwrap();
    assert_eq!(data["changed"][&account], json!({ "UserSettings": state }), "{event}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_webmail_sees_the_settings_capability_too() {
    let server = server().await;
    let id = server.store.account("mini@example.org").await.unwrap().unwrap().id;
    let web_session = server.store.create_web_session(id, 3600, "", "").await.unwrap();
    let request = Request::get("/jmap/session")
        .header(header::COOKIE, format!("{}={}", uwumail_jmap::auth::PLAIN_SESSION_COOKIE, web_session.token))
        .header(header::HOST, "mail.example.org")
        .body(Body::empty())
        .unwrap();
    let (status, body) = server.request(request).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let session: Value = serde_json::from_slice(&body).unwrap();
    assert!(session["capabilities"]["urn:uwumail:jmap:settings"].is_object(), "{session}");
    let account = server.account_id("mini@example.org").await;
    assert_eq!(session["accounts"][&account]["accountCapabilities"]["urn:uwumail:jmap:settings"]["maxKeys"], 5000);
}
