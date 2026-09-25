//! A JMAP server on a fresh store with two people, for the tests of tokens, WebSocket push,
//! delayed sending, copying, query changes, signatures and address suggestions.

#![allow(dead_code)]

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

pub const PASSWORD: &str = "katzenpfote-123";
pub const USING: [&str; 4] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "urn:uwumail:jmap:settings",
];

pub struct Server {
    pub jmap: Jmap,
    pub router: Router,
    pub store: Store,
    pub dir: tempfile::TempDir,
}

pub fn smtp(store: &Store) -> Smtp {
    Smtp::new(
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
    .unwrap()
}

pub async fn server() -> Server {
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
    let jmap = Jmap::new(smtp(&store));
    Server { router: jmap.router(), jmap, store, dir }
}

pub fn basic(login: &str, password: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{login}:{password}")))
}

impl Server {
    pub async fn request(&self, request: Request<Body>) -> (StatusCode, Vec<u8>) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        (status, to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap().to_vec())
    }

    /// Method calls with any `Authorization` header value.
    pub async fn api_as(&self, authorization: &str, using: &[&str], calls: Value) -> (StatusCode, Value) {
        let body = json!({ "using": using, "methodCalls": calls });
        let request = Request::post("/jmap/api")
            .header(header::AUTHORIZATION, authorization)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, bytes) = self.request(request).await;
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    pub async fn api_using(&self, login: &str, using: &[&str], calls: Value) -> Vec<Value> {
        let (status, response) = self.api_as(&basic(login, PASSWORD), using, calls).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        response["methodResponses"].as_array().unwrap().clone()
    }

    pub async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        self.api_using(login, &USING, calls).await
    }

    pub async fn account_id(&self, login: &str) -> String {
        format!("a{}", self.id(login).await)
    }

    pub async fn id(&self, login: &str) -> i64 {
        self.store.account(login).await.unwrap().unwrap().id
    }

    /// Delivers a message into a person's inbox and returns its JMAP id.
    pub async fn deliver(&self, login: &str, raw: &str) -> String {
        let ingested = self
            .store
            .ingest(IngestRequest {
                account_id: self.id(login).await,
                raw: raw.replace('\n', "\r\n").into_bytes(),
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec![],
                received_at: None,
            })
            .await
            .unwrap();
        format!("e{}", ingested.id)
    }

    /// The JMAP id of a person's mailbox with this role.
    pub async fn mailbox(&self, login: &str, role: &str) -> String {
        let responses =
            self.api(login, json!([["Mailbox/get", { "accountId": self.account_id(login).await }, "0"]])).await;
        let list = responses[0][1]["list"].as_array().unwrap();
        list.iter().find(|m| m["role"] == role).unwrap()["id"].as_str().unwrap().to_owned()
    }
}

pub fn args<'a>(responses: &'a [Value], index: usize, name: &str) -> &'a Value {
    assert_eq!(responses[index][0], name, "response {index}: {}", responses[index]);
    &responses[index][1]
}
