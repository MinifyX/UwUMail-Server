//! Signatures of sending identities (RFC 8621 Identity), shared with the portal.

mod common;

use common::{args, server};
use serde_json::json;
use uwumail_store::IdentityUpdate;

#[tokio::test(flavor = "multi_thread")]
async fn signatures_are_kept_on_the_server_and_shared_with_the_portal() {
    let server = server().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let responses = server.api(login, json!([["Identity/get", { "accountId": account }, "0"]])).await;
    let identity = args(&responses, 0, "Identity/get")["list"][0].clone();
    let id = identity["id"].as_str().unwrap().to_owned();
    assert_eq!(identity["textSignature"], "");

    let responses = server
        .api(
            login,
            json!([
                ["Identity/set", { "accountId": account, "update": { &id: {
                    "textSignature": "Mini\nexample.org", "htmlSignature": "<p><b>Mini</b></p>" } } }, "0"],
                ["Identity/get", { "accountId": account, "ids": [&id], "properties": ["textSignature", "htmlSignature"] }, "1"],
            ]),
        )
        .await;
    assert!(args(&responses, 0, "Identity/set")["updated"].get(&id).is_some(), "{}", responses[0]);
    let got = &args(&responses, 1, "Identity/get")["list"][0];
    assert_eq!(got["textSignature"], "Mini\nexample.org");
    assert_eq!(got["htmlSignature"], "<p><b>Mini</b></p>");

    // What the portal writes is what JMAP reads.
    let number = id.trim_start_matches('i').parse().unwrap();
    let update = IdentityUpdate { text_signature: Some("From the portal".into()), ..IdentityUpdate::default() };
    server.store.update_identity(server.id(login).await, number, update).await.unwrap();
    let responses = server.api(login, json!([["Identity/get", { "accountId": account, "ids": [&id] }, "0"]])).await;
    assert_eq!(args(&responses, 0, "Identity/get")["list"][0]["textSignature"], "From the portal");

    // A signature has a size limit, on update and on create.
    let huge = "x".repeat(uwumail_store::IDENTITY_SIGNATURE_MAX_BYTES + 1);
    let responses = server
        .api(
            login,
            json!([["Identity/set", { "accountId": account,
                "update": { &id: { "htmlSignature": huge } },
                "create": { "n": { "email": login, "textSignature": huge } } }, "0"]]),
        )
        .await;
    let set = args(&responses, 0, "Identity/set");
    assert_eq!(set["notUpdated"][&id]["type"], "invalidProperties");
    assert_eq!(set["notCreated"]["n"]["type"], "invalidProperties");
    let responses = server.api(login, json!([["Identity/get", { "accountId": account }, "0"]])).await;
    assert_eq!(args(&responses, 0, "Identity/get")["list"].as_array().unwrap().len(), 1, "nothing half created");
}
