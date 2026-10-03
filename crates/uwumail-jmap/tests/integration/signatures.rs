//! Signatures of sending identities (RFC 8621 Identity), shared with the portal.

use crate::common;

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

const SIGNATURES: &str = "urn:uwumail:jmap:signatures";
const USING_SIGNATURES: [&str; 4] =
    ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission", SIGNATURES];

#[tokio::test(flavor = "multi_thread")]
async fn one_signature_per_domain_and_identities_show_the_effective_one() {
    let server = server().await;
    let login = "mini@example.org";
    server.store.create_domain("example.net").await.unwrap();
    server.store.add_alias("info@example.org", login).await.unwrap();
    server.store.add_alias("mini@example.net", login).await.unwrap();
    let account = server.account_id(login).await;
    let session = server.session_of(login).await;
    assert_eq!(session["capabilities"][SIGNATURES]["allDomains"], "*");

    let responses = server
        .api_using(login, &USING_SIGNATURES, json!([["SignatureSettings/get", { "accountId": account }, "0"]]))
        .await;
    let overview = args(&responses, 0, "SignatureSettings/get");
    let domains: Vec<(String, u64)> = overview["domains"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["domain"].as_str().unwrap().to_owned(), d["addressCount"].as_u64().unwrap()))
        .collect();
    assert_eq!(domains, vec![("example.net".to_owned(), 1), ("example.org".to_owned(), 2)]);
    let info = overview["identities"].as_array().unwrap().iter().find(|i| i["email"] == "info@example.org").unwrap();
    let info_id = info["id"].as_str().unwrap().to_owned();
    assert!(info_id.starts_with('i'));

    let responses = server
        .api_using(
            login,
            &USING_SIGNATURES,
            json!([
                ["SignatureSettings/set", { "accountId": account,
                    "domains": { "example.org": { "text": "{name} | {adresse}", "html": "<p>{name}</p>" }, "*": { "text": "Alle" } } }, "0"],
                ["Identity/get", { "accountId": account }, "1"],
            ]),
        )
        .await;
    let set = args(&responses, 0, "SignatureSettings/set");
    assert_ne!(set["oldState"], set["newState"]);
    let list = args(&responses, 1, "Identity/get")["list"].as_array().unwrap().clone();
    let of = |email: &str| list.iter().find(|i| i["email"] == email).unwrap().clone();
    assert_eq!(of("info@example.org")["textSignature"], "MINI | info@example.org");
    assert_eq!(of("info@example.org")["htmlSignature"], "<p>MINI</p>");
    assert_eq!(of("mini@example.net")["textSignature"], "Alle");

    // An identity's own signature wins; Identity/set writes it. Writing back the effective one
    // changes nothing.
    let responses = server
        .api_using(
            login,
            &USING_SIGNATURES,
            json!([
                ["Identity/set", { "accountId": account, "update": {
                    &info_id: { "textSignature": "MINI | info@example.org" } } }, "0"],
                ["SignatureSettings/get", { "accountId": account }, "1"],
            ]),
        )
        .await;
    let overview = args(&responses, 1, "SignatureSettings/get");
    let info = overview["identities"].as_array().unwrap().iter().find(|i| i["email"] == "info@example.org").unwrap();
    assert_eq!(info["source"], "domain");
    let responses = server
        .api_using(
            login,
            &USING_SIGNATURES,
            json!([
                ["Identity/set", { "accountId": account, "update": { &info_id: { "textSignature": "Eigene" } } }, "0"],
                ["SignatureSettings/get", { "accountId": account }, "1"],
            ]),
        )
        .await;
    let overview = args(&responses, 1, "SignatureSettings/get");
    let info = overview["identities"].as_array().unwrap().iter().find(|i| i["email"] == "info@example.org").unwrap();
    assert_eq!(info["source"], "identity");
    assert_eq!(info["signature"]["text"], "Eigene");
    assert_eq!(info["effective"]["text"], "Eigene");

    // Back to the domain's.
    let responses = server
        .api_using(
            login,
            &USING_SIGNATURES,
            json!([
                ["SignatureSettings/set", { "accountId": account, "identities": { &info_id: null } }, "0"],
                ["Identity/get", { "accountId": account, "ids": [&info_id] }, "1"],
            ]),
        )
        .await;
    assert_eq!(args(&responses, 1, "Identity/get")["list"][0]["textSignature"], "MINI | info@example.org");

    // Someone else's domain, someone else's identity, bad values: refused, nothing changed.
    let nyu_identity = {
        let nyu = server.account_id("nyu@example.org").await;
        let responses = server.api("nyu@example.org", json!([["Identity/get", { "accountId": nyu }, "0"]])).await;
        args(&responses, 0, "Identity/get")["list"][0]["id"].as_str().unwrap().to_owned()
    };
    for bad in [
        json!({ "domains": { "example.com": { "text": "x" } } }),
        json!({ "identities": { &nyu_identity: { "text": "x" } } }),
        json!({ "domains": { "example.org": { "text": 5 } } }),
        json!({ "domains": { "example.org": { "colour": "red" } } }),
        json!({ "domains": ["example.org"] }),
        json!({ "domains": { "example.org": { "text": "x".repeat(uwumail_store::IDENTITY_SIGNATURE_MAX_BYTES + 1) } } }),
    ] {
        let mut call = bad.clone();
        call["accountId"] = json!(account);
        let responses = server.api_using(login, &USING_SIGNATURES, json!([["SignatureSettings/set", call, "0"]])).await;
        assert_eq!(responses[0][0], "error", "{bad}");
        assert_eq!(responses[0][1]["type"], "invalidArguments", "{bad}");
    }
    let nyu_overview = server.store.signature_overview(server.id("nyu@example.org").await).await.unwrap();
    assert!(nyu_overview.identities[0].signature.is_none());

    // Without the capability in `using` the methods are unknown.
    let responses = server.api(login, json!([["SignatureSettings/get", { "accountId": account }, "0"]])).await;
    assert_eq!(responses[0][1]["type"], "unknownMethod");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_company_footer_is_added_on_sending() {
    use uwumail_store::{CompanySignature, CompanySignatureMode};
    let server = server().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    server
        .store
        .set_domain_signature(
            "example.org",
            CompanySignature {
                mode: CompanySignatureMode::Footer,
                text: "-- \nBeispiel GmbH, {name} <{adresse}>".into(),
                html: String::new(),
            },
        )
        .await
        .unwrap();
    // The footer is the server's to add, not the identity's.
    let responses = server.api(login, json!([["Identity/get", { "accountId": account }, "0"]])).await;
    assert_eq!(args(&responses, 0, "Identity/get")["list"][0]["textSignature"], "");

    let raw = "From: \"Mini <b>\" <mini@example.org>\nTo: Nyu <nyu@example.org>\nSubject: Mit Fusszeile\n\nMiau!\n";
    let email = server.deliver(login, raw).await;
    let responses = server
        .api(
            login,
            json!([
                ["UserSettings/set", { "accountId": account, "update": { "singleton": { "values/undoSendSeconds": 0 } } }, "s"],
                ["Identity/get", { "accountId": account }, "0"],
            ]),
        )
        .await;
    let identity = args(&responses, 1, "Identity/get")["list"][0]["id"].as_str().unwrap().to_owned();
    let responses = server
        .api(
            login,
            json!([["EmailSubmission/set", { "accountId": account,
                "create": { "s": { "identityId": identity, "emailId": email } } }, "0"]]),
        )
        .await;
    assert!(args(&responses, 0, "EmailSubmission/set")["created"]["s"].is_object(), "{}", responses[0]);

    let nyu = server.account_id("nyu@example.org").await;
    let responses = server
        .api(
            "nyu@example.org",
            json!([
                ["Email/query", { "accountId": nyu }, "0"],
                ["Email/get", { "accountId": nyu, "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                    "properties": ["textBody", "bodyValues"], "fetchTextBodyValues": true }, "1"],
            ]),
        )
        .await;
    let got = &args(&responses, 1, "Email/get")["list"][0];
    let part = got["textBody"][0]["partId"].as_str().unwrap();
    let text = got["bodyValues"][part]["value"].as_str().unwrap();
    assert!(text.starts_with("Miau!"), "{text}");
    assert!(text.contains("Beispiel GmbH, Mini <b> <mini@example.org>"), "{text}");
}
