//! MaskedEmail/get and MaskedEmail/set (Fastmail's `https://www.fastmail.com/dev/maskedemail`).

use serde_json::{Value, json};

use crate::common::{args, server};

const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "https://www.fastmail.com/dev/maskedemail"];

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
    server.store.set_domain_masked_addresses("example.org", true).await.unwrap();

    let session = server.request(
        axum::http::Request::get("/jmap/session")
            .header(
                axum::http::header::AUTHORIZATION,
                crate::common::basic("mini@example.org", crate::common::PASSWORD),
            )
            .body(axum::body::Body::empty())
            .unwrap(),
    );
    let (_, body) = session.await;
    let session: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["capabilities"]["https://www.fastmail.com/dev/maskedemail"], json!({}));
    assert_eq!(session["primaryAccounts"]["https://www.fastmail.com/dev/maskedemail"], json!(account));

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
