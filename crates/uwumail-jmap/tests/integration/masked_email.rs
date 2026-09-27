//! MaskedEmail/get and MaskedEmail/set (Fastmail's `https://www.fastmail.com/dev/maskedemail`).

use serde_json::{Value, json};
use uwumail_store::{AccountMaskedPolicy, DomainKind, DomainMaskedPolicy, MaskedMode};

use crate::common::{Server, args, server};

const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "https://www.fastmail.com/dev/maskedemail"];
const MASKED: &str = "https://www.fastmail.com/dev/maskedemail";

async fn session(server: &Server, login: &str) -> Value {
    let request = axum::http::Request::get("/jmap/session")
        .header(axum::http::header::AUTHORIZATION, crate::common::basic(login, crate::common::PASSWORD))
        .body(axum::body::Body::empty())
        .unwrap();
    let (_, body) = server.request(request).await;
    serde_json::from_slice(&body).unwrap()
}

fn own_domain() -> DomainMaskedPolicy {
    DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() }
}

#[tokio::test(flavor = "multi_thread")]
async fn masked_email_get_and_set() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;

    // Without an open domain there is nothing to make.
    let responses = server
        .api_using(
            "mini@example.org",
            &USING,
            json!([["MaskedEmail/set", { "accountId": account, "create": { "a": { "forDomain": "https://shop.example.com" } } }, "0"]]),
        )
        .await;
    assert_eq!(args(&responses, 0, "MaskedEmail/set")["notCreated"]["a"]["type"], "forbidden");
    server.store.set_domain_masked_policy("example.org", own_domain()).await.unwrap();

    let session = session(&server, "mini@example.org").await;
    assert_eq!(session["capabilities"][MASKED], json!({}));
    assert_eq!(session["primaryAccounts"][MASKED], json!(account));
    assert_eq!(
        session["accounts"][&account]["accountCapabilities"][MASKED],
        json!({ "domains": ["example.org"], "defaultDomain": "example.org" })
    );

    let responses = server
        .api_using(
            "mini@example.org",
            &USING,
            json!([
                ["MaskedEmail/get", { "accountId": account, "ids": null }, "0"],
                ["MaskedEmail/set", { "accountId": account, "create": {
                    "shop": { "forDomain": "https://shop.example.com", "description": "Shop", "emailPrefix": "Shop_1" },
                    "news": { "state": "enabled", "url": "https://news.example.net/signup" },
                    "bad": { "state": "disabled" },
                    "worse": { "email": "mine@example.org" }
                } }, "1"],
                ["MaskedEmail/get", { "accountId": account, "ids": ["#shop"] }, "2"]
            ]),
        )
        .await;
    let first = args(&responses, 0, "MaskedEmail/get");
    assert_eq!(first["list"], json!([]));
    let set = args(&responses, 1, "MaskedEmail/set");
    assert_eq!(set["oldState"], first["state"]);
    assert_ne!(set["newState"], set["oldState"]);
    let shop = &set["created"]["shop"];
    let email = shop["email"].as_str().unwrap();
    assert!(email.starts_with("shop_1.") && email.ends_with("@example.org"), "{email}");
    assert_eq!(shop["state"], "pending");
    assert_eq!(set["created"]["news"]["state"], "enabled");
    assert_eq!(set["notCreated"]["bad"]["type"], "invalidProperties");
    assert_eq!(set["notCreated"]["worse"]["type"], "invalidProperties");
    let got = &args(&responses, 2, "MaskedEmail/get")["list"][0];
    assert_eq!(got["email"], email);
    assert_eq!(got["forDomain"], "https://shop.example.com");
    assert_eq!(got["description"], "Shop");
    assert_eq!(got["createdBy"], "JMAP");
    assert_eq!(got["lastMessageAt"], Value::Null);
    assert!(got["createdAt"].as_str().unwrap().ends_with('Z'));
    let id = got["id"].as_str().unwrap().to_owned();

    // Mini may send as it; Nyu sees nothing of it.
    let mini = server.id("mini@example.org").await;
    assert!(server.store.account_owns_address(mini, email).await.unwrap());
    let nyu_account = server.account_id("nyu@example.org").await;
    let theirs = server
        .api_using(
            "nyu@example.org",
            &USING,
            json!([["MaskedEmail/get", { "accountId": nyu_account, "ids": [id] }, "0"]]),
        )
        .await;
    assert_eq!(args(&theirs, 0, "MaskedEmail/get")["notFound"], json!([id]));

    // Updating: state and the notes, not the address; never back to pending.
    let responses = server
        .api_using(
            "mini@example.org",
            &USING,
            json!([
                ["MaskedEmail/set", { "accountId": account, "update": {
                    (id.clone()): { "state": "disabled", "description": "Laden" }
                } }, "0"],
                ["MaskedEmail/set", { "accountId": account, "update": {
                    (id.clone()): { "email": "other@example.org" },
                    "x999": { "state": "enabled" }
                } }, "1"],
                ["MaskedEmail/set", { "accountId": account, "update": { (id.clone()): { "state": "pending" } } }, "2"],
                ["MaskedEmail/changes", { "accountId": account, "sinceState": set["newState"] }, "3"],
                ["MaskedEmail/get", { "accountId": account, "ids": [id], "properties": ["state", "description"] }, "4"]
            ]),
        )
        .await;
    assert!(args(&responses, 0, "MaskedEmail/set")["updated"].get(&id).is_some());
    let refused = args(&responses, 1, "MaskedEmail/set");
    assert_eq!(refused["notUpdated"][&id]["type"], "invalidProperties");
    assert_eq!(refused["notUpdated"]["x999"]["type"], "notFound");
    assert_eq!(args(&responses, 2, "MaskedEmail/set")["notUpdated"][&id]["type"], "invalidProperties");
    assert_eq!(args(&responses, 3, "MaskedEmail/changes")["updated"], json!([id]));
    let now = &args(&responses, 4, "MaskedEmail/get")["list"][0];
    assert_eq!(now, &json!({ "id": id, "state": "disabled", "description": "Laden" }));

    // Destroying deletes it for good: it stays, refusing mail, and its address is never handed out again.
    let responses = server
        .api_using(
            "mini@example.org",
            &USING,
            json!([
                ["MaskedEmail/set", { "accountId": account, "destroy": [id] }, "0"],
                ["MaskedEmail/get", { "accountId": account, "ids": [id], "properties": ["state"] }, "1"]
            ]),
        )
        .await;
    assert_eq!(args(&responses, 0, "MaskedEmail/set")["destroyed"], json!([id]));
    assert_eq!(args(&responses, 1, "MaskedEmail/get")["list"][0]["state"], "deleted");
    assert_eq!(server.store.resolve_recipient(email).await.unwrap(), None);

    // Without the capability in `using` the methods are unknown.
    let responses = server.api("mini@example.org", json!([["MaskedEmail/get", { "accountId": account }, "0"]])).await;
    assert_eq!(args(&responses, 0, "error")["type"], "unknownMethod");
}

/// UwUMail's addition: the capability lists where the account may make masked addresses, and
/// `domain` on create picks one of them (docs/jmap-masked-email.md).
#[tokio::test(flavor = "multi_thread")]
async fn masked_email_on_the_domains_the_policy_allows() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let before = session(&server, "mini@example.org").await;
    assert_eq!(
        before["accounts"][&account]["accountCapabilities"][MASKED],
        json!({ "domains": [], "defaultDomain": null })
    );

    server.store.create_domain_with_kind("a.test", DomainKind::Masked).await.unwrap();
    server.store.create_domain_with_kind("b.test", DomainKind::Masked).await.unwrap();
    let policy = DomainMaskedPolicy {
        mode: MaskedMode::Both,
        masked_domains: vec!["a.test".into(), "b.test".into()],
        default_domain: Some("b.test".into()),
    };
    server.store.set_domain_masked_policy("example.org", policy).await.unwrap();
    let after = session(&server, "mini@example.org").await;
    assert_eq!(
        after["accounts"][&account]["accountCapabilities"][MASKED],
        json!({ "domains": ["a.test", "b.test", "example.org"], "defaultDomain": "b.test" })
    );
    assert_ne!(after["state"], before["state"], "a changed policy changes the session");

    let responses = server
        .api_using(
            "mini@example.org",
            &USING,
            json!([["MaskedEmail/set", { "accountId": account, "create": {
                "default": { "forDomain": "https://shop.example.com" },
                "chosen": { "domain": "A.test", "state": "enabled" },
                "own": { "domain": "example.org" },
                "elsewhere": { "domain": "example.net" },
                "nonsense": { "domain": "not a domain" },
                "wrong": { "domain": 7 }
            } }, "0"]]),
        )
        .await;
    let set = args(&responses, 0, "MaskedEmail/set");
    let email = |key: &str| set["created"][key]["email"].as_str().unwrap().to_owned();
    assert!(email("default").ends_with("@b.test"), "{set}");
    assert!(email("chosen").ends_with("@a.test"));
    assert!(email("own").ends_with("@example.org"));
    assert_eq!(set["created"]["default"].get("domain"), None, "the address says it");
    assert_eq!(set["notCreated"]["elsewhere"]["type"], "forbidden");
    assert_eq!(set["notCreated"]["nonsense"]["type"], "forbidden");
    assert_eq!(set["notCreated"]["wrong"]["type"], "invalidProperties");
    let id = set["created"]["chosen"]["id"].as_str().unwrap().to_owned();

    // The domain is made once; an update cannot move it.
    let responses = server
        .api_using(
            "mini@example.org",
            &USING,
            json!([["MaskedEmail/set", { "accountId": account, "update": { (id.clone()): { "domain": "b.test" } } }, "0"]]),
        )
        .await;
    assert_eq!(args(&responses, 0, "MaskedEmail/set")["notUpdated"][&id]["type"], "invalidProperties");

    // An admin narrows Mini down to a.test: b.test is forbidden now, the addresses there keep working.
    let mini = server.id("mini@example.org").await;
    let custom = AccountMaskedPolicy {
        mode: Some(MaskedMode::Dedicated),
        masked_domains: Some(vec!["a.test".into()]),
        default_domain: None,
    };
    server.store.set_account_masked_policy(mini, custom).await.unwrap();
    let (status, response) = server
        .api_as(
            &crate::common::basic("mini@example.org", crate::common::PASSWORD),
            &USING,
            json!([["MaskedEmail/set", { "accountId": account, "create": {
                "b": { "domain": "b.test" },
                "default": {}
            } }, "0"]]),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_ne!(response["sessionState"], after["state"]);
    let set = &response["methodResponses"][0][1];
    assert_eq!(set["notCreated"]["b"]["type"], "forbidden");
    assert!(set["created"]["default"]["email"].as_str().unwrap().ends_with("@a.test"));
    assert_eq!(server.store.resolve_recipient(&email("default")).await.unwrap(), Some(mini));
}
