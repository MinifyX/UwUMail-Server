//! Folders shared between people, over JMAP: shared accounts, `shareWith`, rights and Principals.

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
const USING: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:principals"];
const MINI: &str = "mini@example.org";
const NYU: &str = "nyu@example.org";

struct Server {
    router: Router,
    store: Store,
    mini: i64,
    nyu: i64,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    let mut ids = Vec::new();
    for login in [MINI, NYU] {
        let account = store
            .create_account(NewAccount {
                address: login.into(),
                display_name: login.split('@').next().unwrap().to_uppercase(),
                password: Some(PASSWORD.into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        ids.push(account.id);
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
    Server { router: Jmap::new(smtp).router(), store, mini: ids[0], nyu: ids[1], _dir: dir }
}

fn basic(login: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{login}:{PASSWORD}")))
}

impl Server {
    async fn get(&self, uri: &str, login: &str) -> (StatusCode, Vec<u8>) {
        let request = Request::get(uri)
            .header(header::AUTHORIZATION, basic(login))
            .header(header::HOST, "mail.example.org")
            .body(Body::empty())
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        (status, to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap().to_vec())
    }

    async fn session(&self, login: &str) -> Value {
        let (status, body) = self.get("/jmap/session", login).await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_slice(&body).unwrap()
    }

    async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        let body = json!({ "using": USING, "methodCalls": calls });
        let request = Request::post("/jmap/api")
            .header(header::AUTHORIZATION, basic(login))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        response["methodResponses"].as_array().unwrap().clone()
    }

    /// One call, its arguments.
    async fn call(&self, login: &str, name: &str, arguments: Value) -> Value {
        let responses = self.api(login, json!([[name, arguments, "0"]])).await;
        assert_eq!(responses.len(), 1);
        responses[0][1].clone()
    }

    async fn deliver(&self, account: i64, role: MailboxRole, subject: &str) -> i64 {
        let raw = format!("From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: {subject}\r\n\r\nHallo\r\n");
        let request = IngestRequest {
            account_id: account,
            raw: raw.into_bytes(),
            mailboxes: vec![MailboxTarget::Role(role)],
            keywords: vec![],
            received_at: None,
        };
        self.store.ingest(request).await.unwrap().id
    }

    async fn mailbox(&self, account: i64, role: MailboxRole) -> i64 {
        self.store.mailboxes(account).await.unwrap().into_iter().find(|m| m.role == Some(role)).unwrap().id
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_folders_are_accounts_of_their_own() {
    let server = server().await;
    let (mini_account, nyu_account) = (format!("a{}", server.mini), format!("a{}", server.nyu));
    let (mini_principal, nyu_principal) = (format!("p{}", server.mini), format!("p{}", server.nyu));
    let inbox = server.mailbox(server.mini, MailboxRole::Inbox).await;
    let archive = server.mailbox(server.mini, MailboxRole::Archive).await;
    let first = server.deliver(server.mini, MailboxRole::Inbox, "eins").await;
    let private = server.deliver(server.mini, MailboxRole::Archive, "privat").await;

    // The session names the principal and has no shared account yet.
    let session = server.session(NYU).await;
    assert_eq!(session["capabilities"]["urn:ietf:params:jmap:principals"], json!({}));
    assert_eq!(
        session["accounts"][&nyu_account]["accountCapabilities"]["urn:ietf:params:jmap:principals"]["currentUserPrincipalId"],
        json!(nyu_principal)
    );
    assert!(session["accounts"][&mini_account].is_null());
    let before = session["state"].clone();
    let refused = server.call(NYU, "Mailbox/get", json!({ "accountId": mini_account })).await;
    assert_eq!(refused["type"], "accountNotFound");

    // People on the server are principals.
    let people = server.call(NYU, "Principal/get", json!({ "accountId": nyu_account, "ids": null })).await;
    let list = people["list"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let mini = list.iter().find(|p| p["id"] == json!(mini_principal)).unwrap();
    assert_eq!(
        (mini["type"].as_str(), mini["email"].as_str(), mini["name"].as_str()),
        (Some("individual"), Some(MINI), Some("MINI"))
    );
    let found =
        server.call(NYU, "Principal/query", json!({ "accountId": nyu_account, "filter": { "text": "min" } })).await;
    assert_eq!(found["ids"], json!([mini_principal]));

    // Mini shares the inbox with Nyu for reading and keeping it read.
    let shared = server
        .call(
            MINI,
            "Mailbox/set",
            json!({ "accountId": mini_account, "update": {
                format!("m{inbox}"): { format!("shareWith/{nyu_principal}"): { "mayReadItems": true, "maySetSeen": true } }
            } }),
        )
        .await;
    assert!(shared["updated"].get(format!("m{inbox}")).is_some(), "{shared}");
    let own =
        server.call(MINI, "Mailbox/get", json!({ "accountId": mini_account, "ids": [format!("m{inbox}")] })).await;
    let share_with = &own["list"][0]["shareWith"];
    assert_eq!(share_with[&nyu_principal]["mayReadItems"], true);
    assert_eq!(share_with[&nyu_principal]["maySetSeen"], true);
    assert_eq!(share_with[&nyu_principal]["mayAddItems"], false);
    assert_eq!(own["list"][0]["myRights"]["mayAdmin"], true);
    let store_rights = server.store.mailbox_acl(server.mini, inbox).await.unwrap();
    assert_eq!(store_rights[0].rights, "lrs");

    // Nyu now has Mini's account, with the inbox and nothing else.
    let session = server.session(NYU).await;
    assert_ne!(session["state"], before, "the session changed");
    let account = &session["accounts"][&mini_account];
    assert_eq!(account["name"], MINI);
    assert_eq!(account["isPersonal"], false);
    assert_eq!(account["accountCapabilities"]["urn:ietf:params:jmap:mail"]["mayCreateTopLevelMailbox"], false);
    assert_eq!(
        account["accountCapabilities"]["urn:ietf:params:jmap:principals:owner"]["principalId"],
        json!(mini_principal)
    );
    let mailboxes = server.call(NYU, "Mailbox/get", json!({ "accountId": mini_account })).await;
    let list = mailboxes["list"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], json!(format!("m{inbox}")));
    assert_eq!(list[0]["myRights"]["mayReadItems"], true);
    assert_eq!(list[0]["myRights"]["mayAddItems"], false);
    assert_eq!(list[0]["shareWith"], Value::Null, "only who may administer sees the sharing");
    let hidden =
        server.call(NYU, "Mailbox/get", json!({ "accountId": mini_account, "ids": [format!("m{archive}")] })).await;
    assert_eq!(hidden["notFound"], json!([format!("m{archive}")]));

    // Mail: only what is in the shared inbox, and only there.
    server
        .store
        .update_emails(
            server.mini,
            vec![uwumail_store::EmailUpdate {
                id: first,
                mailboxes: uwumail_store::MailboxesChange::Patch(vec![(archive, true)]),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
    let query = server.call(NYU, "Email/query", json!({ "accountId": mini_account })).await;
    assert_eq!(query["ids"], json!([format!("e{first}")]));
    let emails = server
        .call(
            NYU,
            "Email/get",
            json!({ "accountId": mini_account, "ids": [format!("e{first}"), format!("e{private}")],
                    "properties": ["mailboxIds", "subject", "blobId"] }),
        )
        .await;
    assert_eq!(emails["notFound"], json!([format!("e{private}")]));
    assert_eq!(emails["list"][0]["mailboxIds"], json!({ format!("m{inbox}"): true }));
    let blob = emails["list"][0]["blobId"].as_str().unwrap().to_owned();
    let (status, body) = server.get(&format!("/jmap/download/{mini_account}/{blob}/mail.eml"), NYU).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("Subject: eins"));
    let other = server
        .call(
            MINI,
            "Email/get",
            json!({ "accountId": mini_account, "ids": [format!("e{private}")], "properties": ["blobId"] }),
        )
        .await;
    let private_blob = other["list"][0]["blobId"].as_str().unwrap().to_owned();
    let (status, _) = server.get(&format!("/jmap/download/{mini_account}/{private_blob}/x.eml"), NYU).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Rights: seen yes, other keywords, removing and adding no.
    let set = server
        .call(
            NYU,
            "Email/set",
            json!({ "accountId": mini_account,
                    "update": { format!("e{first}"): { "keywords/$seen": true } },
                    "destroy": [format!("e{first}")] }),
        )
        .await;
    assert!(set["updated"].get(format!("e{first}")).is_some(), "{set}");
    assert_eq!(set["notDestroyed"][format!("e{first}")]["type"], "forbidden");
    let flagged = server
        .call(
            NYU,
            "Email/set",
            json!({ "accountId": mini_account, "update": { format!("e{first}"): { "keywords/$flagged": true } } }),
        )
        .await;
    assert_eq!(flagged["notUpdated"][format!("e{first}")]["type"], "forbidden");
    let moved = server
        .call(
            NYU,
            "Email/set",
            json!({ "accountId": mini_account, "update": { format!("e{first}"): { "mailboxIds": { format!("m{inbox}"): true } } } }),
        )
        .await;
    assert!(moved["updated"].get(format!("e{first}")).is_some(), "keeping it where it is changes nothing: {moved}");
    let still = server.store.emails_by_ids(server.mini, vec![first]).await.unwrap();
    assert!(still[0].mailbox_ids.contains(&archive), "the unshared mailbox keeps it");
    let created = server
        .call(
            NYU,
            "Email/set",
            json!({ "accountId": mini_account, "create": { "x": {
                "mailboxIds": { format!("m{inbox}"): true }, "subject": "Hallo",
                "from": [{ "email": NYU }], "textBody": [{ "partId": "1", "type": "text/plain" }],
                "bodyValues": { "1": { "value": "Hi" } } } } }),
        )
        .await;
    assert_eq!(created["notCreated"]["x"]["type"], "forbidden");
    let wrong =
        server.call(NYU, "Mailbox/set", json!({ "accountId": mini_account, "destroy": [format!("m{inbox}")] })).await;
    assert_eq!(wrong["notDestroyed"][format!("m{inbox}")]["type"], "forbidden");

    // Methods that are not about mail stay with one's own account.
    let responses = server.api(NYU, json!([["Principal/get", { "accountId": mini_account, "ids": null }, "0"]])).await;
    assert_eq!(responses[0][0], "error");
    assert_eq!(responses[0][1]["type"], "accountNotSupportedByMethod");

    // Changes in the owner's unshared folders stay out of sight.
    let state = server.call(NYU, "Email/get", json!({ "accountId": mini_account, "ids": [] })).await["state"].clone();
    server.deliver(server.mini, MailboxRole::Archive, "noch privater").await;
    let fresh = server.deliver(server.mini, MailboxRole::Inbox, "neu").await;
    let changes = server.call(NYU, "Email/changes", json!({ "accountId": mini_account, "sinceState": state })).await;
    assert_eq!(changes["created"], json!([format!("e{fresh}")]));

    // Write rights by level: now Nyu may file in and remove.
    server
        .call(
            MINI,
            "Mailbox/set",
            json!({ "accountId": mini_account, "update": {
            format!("m{inbox}"): { "shareWith": { nyu_principal.clone(): "write" } } } }),
        )
        .await;
    let destroyed =
        server.call(NYU, "Email/set", json!({ "accountId": mini_account, "destroy": [format!("e{fresh}")] })).await;
    assert_eq!(destroyed["destroyed"], json!([format!("e{fresh}")]), "{destroyed}");

    // Taken back: the account is gone.
    server
        .call(
            MINI,
            "Mailbox/set",
            json!({ "accountId": mini_account, "update": {
            format!("m{inbox}"): { "shareWith": null } } }),
        )
        .await;
    assert!(server.session(NYU).await["accounts"][&mini_account].is_null());
    let refused = server.call(NYU, "Email/query", json!({ "accountId": mini_account })).await;
    assert_eq!(refused["type"], "accountNotFound");
}
