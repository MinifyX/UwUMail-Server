//! One-click unsubscribing through the server (RFC 8058, docs/jmap-unsubscribe.md): only for signed
//! headers, only to public addresses, once per email in a while, and only in accounts one may act in.
//!
//! The newsletter here is a transport that answers like one; what the server's own egress sends on the
//! wire is tested against a TLS server in `uwumail-smtp`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::{Value, json};
use uwumail_jmap::{Jmap, UnsubscribeTransport};
use uwumail_smtp::egress::EgressError;
use uwumail_store::{DkimKey, DkimKeyAlgorithm, IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

use crate::common::{PASSWORD, Server, smtp};

const USING: [&str; 4] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:principals",
    "urn:uwumail:jmap:unsubscribe",
];
const MINI: &str = "mini@example.org";
const NYU: &str = "nyu@example.org";

/// The newsletter's signing key, made once for all tests.
static KEY: LazyLock<DkimKey> = LazyLock::new(|| {
    let generated = uwumail_smtp::dkim::generate_keys("202609")
        .unwrap()
        .into_iter()
        .find(|key| key.algorithm == DkimKeyAlgorithm::Ed25519Sha256)
        .unwrap();
    DkimKey {
        id: 0,
        domain: "shop.example".into(),
        selector: generated.selector,
        algorithm: generated.algorithm,
        private_key: generated.private_key,
        public_key: generated.public_key,
        active: true,
        created_at: 0,
        retired_at: None,
    }
});

/// A newsletter that remembers where it was asked and answers 500 under `/broken`, 200 elsewhere.
#[derive(Default)]
struct Newsletter {
    asked: Mutex<Vec<String>>,
}

impl UnsubscribeTransport for Newsletter {
    fn post<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<u16, EgressError>> + Send + 'a>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(url.to_owned());
            Ok(if url.contains("/broken") { 500 } else { 200 })
        })
    }
}

impl Newsletter {
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

/// A server whose DNS knows the newsletter's key; with `newsletter`, unsubscriptions go there instead
/// of out through the egress.
async fn server(newsletter: Option<Arc<Newsletter>>) -> Server {
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
    let smtp = smtp(&store);
    let (name, record) = KEY.dns_record();
    smtp.dns_cache().pin_txt(&name, &record).unwrap();
    let mut jmap = Jmap::new(smtp);
    if let Some(newsletter) = newsletter {
        jmap = jmap.with_unsubscribe_transport(newsletter);
    }
    Server { router: jmap.router(), jmap, store, dir }
}

/// A newsletter mail with these list headers, signed over the headers it has at that point; `added`
/// comes after signing.
fn newsletter_mail(list_headers: &str, added: &str) -> Vec<u8> {
    let message = format!(
        "From: Shop <news@shop.example>\r\nTo: mini@example.org\r\nSubject: Angebote\r\n\
         Message-ID: <{}@shop.example>\r\n{list_headers}\r\nNur heute!\r\n",
        rand_id()
    );
    let signature = uwumail_smtp::dkim::sign(message.as_bytes(), std::slice::from_ref(&*KEY)).unwrap();
    format!("{signature}{added}{message}").into_bytes()
}

fn rand_id() -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}

fn one_click(link: &str) -> String {
    format!(
        "List-Unsubscribe: <mailto:leave@shop.example>, <{link}>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n"
    )
}

async fn deliver(server: &Server, login: &str, raw: Vec<u8>) -> String {
    let ingested = server
        .store
        .ingest(IngestRequest {
            account_id: server.id(login).await,
            raw,
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        })
        .await
        .unwrap();
    format!("e{}", ingested.id)
}

/// `Email/unsubscribe` as `login`, in `account` (the login's own when `None`).
async fn unsubscribe(server: &Server, login: &str, account: Option<&str>, email: &str) -> (String, Value) {
    let account = match account {
        Some(account) => account.to_owned(),
        None => server.account_id(login).await,
    };
    let call = json!([["Email/unsubscribe", { "accountId": account, "emailId": email }, "0"]]);
    let responses = server.api_using(login, &USING, call).await;
    (responses[0][0].as_str().unwrap().to_owned(), responses[0][1].clone())
}

#[tokio::test]
async fn a_signed_one_click_link_is_posted_once() {
    let newsletter = Arc::new(Newsletter::default());
    let server = server(Some(newsletter.clone())).await;
    let session = server.session_of(MINI).await;
    assert_eq!(session["capabilities"]["urn:uwumail:jmap:unsubscribe"], json!({}));
    let account = server.account_id(MINI).await;
    assert_eq!(session["accounts"][&account]["accountCapabilities"]["urn:uwumail:jmap:unsubscribe"], json!({}));

    let email = deliver(&server, MINI, newsletter_mail(&one_click("https://shop.example/u/tok3n"), "")).await;
    let (name, answer) = unsubscribe(&server, MINI, None, &email).await;
    assert_eq!(name, "Email/unsubscribe", "{answer}");
    assert_eq!(answer, json!({ "accountId": account, "emailId": email }));
    assert_eq!(newsletter.asked(), ["https://shop.example/u/tok3n"]);

    // Clicked again: done already, and the newsletter is not asked a second time.
    let (name, _) = unsubscribe(&server, MINI, None, &email).await;
    assert_eq!(name, "Email/unsubscribe");
    assert_eq!(newsletter.asked().len(), 1);

    // Without the capability in `using`, the method is not there.
    let call = json!([["Email/unsubscribe", { "accountId": account, "emailId": email }, "0"]]);
    let responses = server.api_using(MINI, &USING[..2], call).await;
    assert_eq!(responses[0][1]["type"], "unknownMethod");
}

#[tokio::test]
async fn without_signed_one_click_headers_nothing_is_sent() {
    let newsletter = Arc::new(Newsletter::default());
    let server = server(Some(newsletter.clone())).await;
    let link = "https://shop.example/u/tok3n";
    let cases = [
        // Only the classic headers: the webmail sends the mail or opens the page.
        newsletter_mail(&format!("List-Unsubscribe: <{link}>\r\n"), ""),
        // Only mailto.
        newsletter_mail(
            "List-Unsubscribe: <mailto:leave@shop.example>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
            "",
        ),
        // Signed, but List-Unsubscribe-Post was added afterwards, so h= does not cover it.
        newsletter_mail(
            &format!("List-Unsubscribe: <{link}>\r\n"),
            "List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
        ),
        // Both signed, then another List-Unsubscribe written above them.
        newsletter_mail(&one_click(link), "List-Unsubscribe: <https://elsewhere.example/u>\r\n"),
        // Not signed at all.
        format!("From: news@shop.example\r\nSubject: Angebote\r\n{}\r\nHallo\r\n", one_click(link)).into_bytes(),
    ];
    for raw in cases {
        let email = deliver(&server, MINI, raw).await;
        let (name, answer) = unsubscribe(&server, MINI, None, &email).await;
        assert_eq!((name.as_str(), &answer["type"]), ("error", &json!("cannotUnsubscribe")), "{answer}");
    }

    // Signed, and the link changed after signing: the signature no longer holds.
    let raw = String::from_utf8(newsletter_mail(&one_click(link), "")).unwrap().replace("tok3n", "other");
    let email = deliver(&server, MINI, raw.into_bytes()).await;
    let (_, answer) = unsubscribe(&server, MINI, None, &email).await;
    assert_eq!(answer["type"], "cannotUnsubscribe", "{answer}");
    assert!(newsletter.asked().is_empty(), "{:?}", newsletter.asked());
}

#[tokio::test]
async fn only_public_addresses_are_posted_to() {
    // The server's own egress this time, which refuses before anything leaves the machine.
    let server = server(None).await;
    for link in ["https://192.0.2.10/u/tok3n", "https://127.0.0.1/u/tok3n", "https://localhost/u"] {
        let email = deliver(&server, MINI, newsletter_mail(&one_click(link), "")).await;
        let (_, answer) = unsubscribe(&server, MINI, None, &email).await;
        assert_eq!(answer["type"], "unsubscribeFailed", "{link}: {answer}");
        let description = answer["description"].as_str().unwrap();
        assert!(description.contains("private network"), "{description}");
    }
}

#[tokio::test]
async fn an_error_answer_fails_and_is_not_repeated_at_once() {
    let newsletter = Arc::new(Newsletter::default());
    let server = server(Some(newsletter.clone())).await;
    let email = deliver(&server, MINI, newsletter_mail(&one_click("https://shop.example/broken"), "")).await;
    let (_, answer) = unsubscribe(&server, MINI, None, &email).await;
    assert_eq!(answer["type"], "unsubscribeFailed");
    assert!(answer["description"].as_str().unwrap().contains("500"), "{answer}");
    let (_, again) = unsubscribe(&server, MINI, None, &email).await;
    assert_eq!(again["type"], "unsubscribeFailed");
    assert!(again["description"].as_str().unwrap().contains("tried a moment ago"), "{again}");
    assert_eq!(newsletter.asked().len(), 1);
}

#[tokio::test]
async fn an_account_has_an_hourly_budget() {
    let newsletter = Arc::new(Newsletter::default());
    let server = server(Some(newsletter.clone())).await;
    let account = server.account_id(MINI).await;
    let mut calls = Vec::new();
    for n in 0..31 {
        let email =
            deliver(&server, MINI, newsletter_mail(&one_click(&format!("https://shop.example/u/{n}")), "")).await;
        calls.push(json!(["Email/unsubscribe", { "accountId": account, "emailId": email }, n.to_string()]));
    }
    let responses = server.api_using(MINI, &USING, json!(calls)).await;
    assert!(responses[..30].iter().all(|response| response[0] == "Email/unsubscribe"));
    assert_eq!(responses[30][1]["type"], "unsubscribeFailed", "{}", responses[30]);
    assert_eq!(newsletter.asked().len(), 30);
}

#[tokio::test]
async fn only_mail_one_may_act_on() {
    let newsletter = Arc::new(Newsletter::default());
    let server = server(Some(newsletter.clone())).await;
    let email = deliver(&server, MINI, newsletter_mail(&one_click("https://shop.example/u/tok3n"), "")).await;
    let mini_account = server.account_id(MINI).await;

    // Someone else's email id in one's own account is not there; their account is not one's own.
    let (_, answer) = unsubscribe(&server, NYU, None, &email).await;
    assert_eq!(answer["type"], "notFound");
    let (_, answer) = unsubscribe(&server, NYU, Some(&mini_account), &email).await;
    assert_eq!(answer["type"], "accountNotFound");
    let (_, answer) = unsubscribe(&server, MINI, None, "e999999").await;
    assert_eq!(answer["type"], "notFound");

    // A read-only share: Nyu may read the mail, but not unsubscribe for Mini.
    let inbox = server.mailbox(MINI, "inbox").await;
    let nyu_principal = format!("p{}", server.id(NYU).await);
    let share = |level: &str| {
        json!([["Mailbox/set", { "accountId": mini_account, "update": {
            inbox.clone(): { "shareWith": { nyu_principal.clone(): level } } } }, "0"]])
    };
    server.api_using(MINI, &USING, share("read")).await;
    let session = server.session_of(NYU).await;
    assert!(session["accounts"][&mini_account]["accountCapabilities"]["urn:uwumail:jmap:unsubscribe"].is_null());
    let (_, answer) = unsubscribe(&server, NYU, Some(&mini_account), &email).await;
    assert_eq!(answer["type"], "forbidden", "{answer}");

    // With write rights it works, as it would for Mini.
    server.api_using(MINI, &USING, share("write")).await;
    let session = server.session_of(NYU).await;
    assert_eq!(session["accounts"][&mini_account]["accountCapabilities"]["urn:uwumail:jmap:unsubscribe"], json!({}));
    let (name, answer) = unsubscribe(&server, NYU, Some(&mini_account), &email).await;
    assert_eq!(name, "Email/unsubscribe", "{answer}");
    assert_eq!(newsletter.asked().len(), 1);
}
