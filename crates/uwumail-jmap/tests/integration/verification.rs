//! Verification codes over JMAP (docs/jmap-verification.md): the Email property `verification`
//! and `VerificationCode/query`. The codes are kept the way SMTP keeps them when mail comes in.

use serde_json::{Value, json};
use uwumail_store::verification::{Detected, Kind, Purpose, Source};

use crate::common::{Server, server};

const CORE: &str = "urn:ietf:params:jmap:core";
const MAIL: &str = "urn:ietf:params:jmap:mail";
const VERIFICATION: &str = "urn:uwumail:jmap:verification";
const MINI: &str = "mini@example.org";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn code(code: &str, expires_at: i64) -> Detected {
    Detected {
        kind: Kind::Otp,
        origin: "login.shop.example".into(),
        code: Some(code.into()),
        url: None,
        purpose: Purpose::Login,
        expires_at,
        source: Source::Header,
        autofill: true,
    }
}

/// Delivers a mail to mini and keeps `found` for it, as SMTP does.
async fn deliver_with(server: &Server, subject: &str, found: Option<Detected>) -> String {
    let raw = format!("From: Shop <no-reply@shop.example>\nTo: {MINI}\nSubject: {subject}\n\nHallo\n");
    let id = server.deliver(MINI, &raw).await;
    if let Some(found) = found {
        let number = id[1..].parse().unwrap();
        let kept = server.store.record_verification(server.id(MINI).await, vec![number], &found, "shop.example");
        assert_eq!(kept.await.unwrap(), vec![number]);
    }
    id
}

#[tokio::test]
async fn emails_carry_their_code_for_clients_that_know_it() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let session = server.session_of(MINI).await;
    assert!(session["capabilities"].get(VERIFICATION).is_some(), "{session}");
    assert_eq!(session["primaryAccounts"][VERIFICATION], json!(account));

    let expires = now() + 600;
    let with = deliver_with(&server, "Dein Code", Some(code("482913", expires))).await;
    let without = deliver_with(&server, "Hallo", None).await;
    let get = json!([["Email/get", { "accountId": account, "ids": [with, without], "properties": ["subject", "verification"] }, "0"]]);

    let responses = server.api_using(MINI, &[CORE, MAIL, VERIFICATION], get.clone()).await;
    let list = responses[0][1]["list"].as_array().unwrap();
    let verification = &list[0]["verification"];
    assert_eq!(verification["code"], "482913");
    assert_eq!(verification["origin"], "login.shop.example");
    assert_eq!((verification["source"].as_str(), verification["autofill"].as_bool()), (Some("header"), Some(true)));
    assert_eq!(verification["purpose"], "login");
    assert!(verification["expiresAt"].as_str().unwrap().ends_with('Z'), "{verification}");
    assert_eq!(list[1]["verification"], Value::Null);
    assert!(list[1].as_object().unwrap().contains_key("verification"));

    // A client that does not use the capability does not get the property.
    let responses = server.api_using(MINI, &[CORE, MAIL], get).await;
    let list = responses[0][1]["list"].as_array().unwrap();
    assert!(list.iter().all(|email| email.get("verification").is_none()), "{list:?}");
}

#[tokio::test]
async fn the_query_lists_the_codes_newest_first_and_only_valid_ones_unless_asked() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let old = deliver_with(&server, "Alt", Some(code("111111", now() - 60))).await;
    let first = deliver_with(&server, "Erster", Some(code("222222", now() + 600))).await;
    let second = deliver_with(&server, "Zweiter", Some(code("333333", now() + 600))).await;
    deliver_with(&server, "Kein Code", None).await;

    let query = |filter: Value| json!([["VerificationCode/query", { "accountId": account, "filter": filter, "calculateTotal": true }, "0"]]);
    let using = [CORE, MAIL, VERIFICATION];
    let responses = server.api_using(MINI, &using, query(Value::Null)).await;
    assert_eq!(responses[0][1]["ids"], json!([second, first]), "{}", responses[0]);
    assert_eq!(responses[0][1]["total"], 2);
    assert_eq!(responses[0][1]["canCalculateChanges"], false);
    let responses = server.api_using(MINI, &using, query(json!({ "expired": true }))).await;
    assert_eq!(responses[0][1]["ids"], json!([old]));

    let responses = server.api_using(MINI, &using, query(json!({ "sender": "x" }))).await;
    assert_eq!(responses[0][1]["type"], "unsupportedFilter");
    // Without the capability there is no such method.
    let responses = server.api_using(MINI, &[CORE, MAIL], query(Value::Null)).await;
    assert_eq!(responses[0][1]["type"], "unknownMethod");
}
