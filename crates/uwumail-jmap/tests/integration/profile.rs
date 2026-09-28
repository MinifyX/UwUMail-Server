//! ProfilePicture/get and ProfilePicture/set (`urn:uwumail:jmap:profile`, docs/profile-pictures.md).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use uwumail_smtp::profile_pictures::sample;

use crate::common::{PASSWORD, Server, args, basic, server};

const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:uwumail:jmap:profile"];
const PROFILE: &str = "urn:uwumail:jmap:profile";

async fn session(server: &Server, login: &str) -> Value {
    let request = Request::get("/jmap/session")
        .header(header::AUTHORIZATION, basic(login, PASSWORD))
        .body(Body::empty())
        .unwrap();
    let (_, body) = server.request(request).await;
    serde_json::from_slice(&body).unwrap()
}

/// Uploads `bytes` as `login` and returns the blob id.
pub async fn upload(server: &Server, login: &str, bytes: Vec<u8>) -> String {
    let account = server.account_id(login).await;
    let request = Request::post(format!("/jmap/upload/{account}/"))
        .header(header::AUTHORIZATION, basic(login, PASSWORD))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(bytes))
        .unwrap();
    let (status, body) = server.request(request).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_slice::<Value>(&body).unwrap()["blobId"].as_str().unwrap().to_owned()
}

async fn set(server: &Server, login: &str, patch: Value) -> Value {
    let account = server.account_id(login).await;
    let responses = server
        .api_using(
            login,
            &USING,
            json!([["ProfilePicture/set", { "accountId": account, "update": { "singleton": patch } }, "0"]]),
        )
        .await;
    args(&responses, 0, "ProfilePicture/set").clone()
}

async fn get(server: &Server, login: &str) -> Value {
    let account = server.account_id(login).await;
    let responses = server
        .api_using(login, &USING, json!([["ProfilePicture/get", { "accountId": account, "ids": null }, "0"]]))
        .await;
    args(&responses, 0, "ProfilePicture/get").clone()
}

async fn download(server: &Server, login: &str, account: &str, blob: &str) -> (StatusCode, Vec<u8>, String) {
    let request = Request::get(format!("/jmap/download/{account}/{blob}/picture"))
        .header(header::AUTHORIZATION, basic(login, PASSWORD))
        .body(Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(server.router.clone(), request).await.unwrap();
    let status = response.status();
    let media_type =
        response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap().to_vec();
    (status, body, media_type)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_is_set_read_and_removed() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;

    let session = session(&server, "mini@example.org").await;
    let capability = json!({ "maxSize": 10485760, "mayBePublic": true });
    assert_eq!(session["capabilities"][PROFILE], capability);
    assert_eq!(session["accounts"][&account]["accountCapabilities"][PROFILE], capability);

    let fresh = get(&server, "mini@example.org").await;
    assert_eq!(
        fresh["list"][0],
        json!({ "id": "singleton", "blobId": null, "type": null, "visibility": "server", "sendFace": false, "updated": null })
    );

    let blob = upload(&server, "mini@example.org", sample(800, 600, "png")).await;
    let first =
        set(&server, "mini@example.org", json!({ "blobId": blob, "visibility": "public", "sendFace": true })).await;
    assert_eq!(first["oldState"], fresh["state"]);
    assert_ne!(first["newState"], first["oldState"]);
    let updated = &first["updated"]["singleton"];
    let stored = updated["blobId"].as_str().unwrap().to_owned();
    assert_ne!(stored, blob, "the picture was written anew");
    assert_eq!(updated["type"], "image/jpeg");
    assert!(updated["updated"].as_str().unwrap().ends_with('Z'));

    let got = get(&server, "mini@example.org").await;
    assert_eq!(got["state"], first["newState"]);
    assert_eq!(got["list"][0]["blobId"], json!(stored));
    assert_eq!(got["list"][0]["visibility"], "public");
    assert_eq!(got["list"][0]["sendFace"], true);

    let (status, bytes, media_type) = download(&server, "mini@example.org", &account, &stored).await;
    assert_eq!((status, media_type.as_str()), (StatusCode::OK, "image/jpeg"));
    assert!(bytes.starts_with(&[0xff, 0xd8, 0xff]));

    // Someone else gets neither the stored picture nor the upload, not even through their own account.
    let nyu = server.account_id("nyu@example.org").await;
    assert_eq!(download(&server, "nyu@example.org", &nyu, &stored).await.0, StatusCode::NOT_FOUND);
    assert_eq!(download(&server, "nyu@example.org", &account, &stored).await.0, StatusCode::NOT_FOUND);
    let foreign = set(&server, "nyu@example.org", json!({ "blobId": blob })).await;
    assert_eq!(foreign["notUpdated"]["singleton"]["type"], "blobNotFound");
    let responses = server
        .api_using(
            "nyu@example.org",
            &USING,
            json!([["ProfilePicture/get", { "accountId": account, "ids": null }, "0"]]),
        )
        .await;
    assert_eq!(responses[0][0], "error");
    assert_eq!(responses[0][1]["type"], "accountNotFound");

    // Sending the same blob again changes nothing; null removes it.
    let again = set(&server, "mini@example.org", json!({ "blobId": stored })).await;
    assert_eq!(again["updated"]["singleton"], Value::Null);
    let removed = set(&server, "mini@example.org", json!({ "blobId": null })).await;
    assert_eq!(removed["updated"]["singleton"]["blobId"], Value::Null);
    assert_eq!(get(&server, "mini@example.org").await["list"][0]["blobId"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_uploads_and_patches_are_refused() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;

    let text = upload(&server, "mini@example.org", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec()).await;
    let refused = set(&server, "mini@example.org", json!({ "blobId": text })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["type"], "invalidProperties");
    assert_eq!(refused["notUpdated"]["singleton"]["properties"], json!(["blobId"]));

    let broken = upload(&server, "mini@example.org", b"\x89PNG\r\n\x1a\nthat is all".to_vec()).await;
    let refused = set(&server, "mini@example.org", json!({ "blobId": broken })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["properties"], json!(["blobId"]));

    let mut huge = sample(16, 16, "png");
    huge.resize(10 * 1024 * 1024 + 1, 0);
    let huge = upload(&server, "mini@example.org", huge).await;
    let refused = set(&server, "mini@example.org", json!({ "blobId": huge })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["type"], "tooLarge");

    let refused = set(&server, "mini@example.org", json!({ "blobId": "bnothing" })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["type"], "blobNotFound");

    let refused = set(&server, "mini@example.org", json!({ "visibility": "everyone", "colour": "pink" })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["properties"], json!(["colour", "visibility"]));
    let refused = set(&server, "mini@example.org", json!({ "type": "image/png" })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["type"], "invalidProperties");

    let responses = server
        .api_using(
            "mini@example.org",
            &USING,
            json!([["ProfilePicture/set", {
                "accountId": account,
                "create": { "new": {} },
                "update": { "other": { "sendFace": true } },
                "destroy": ["singleton"]
            }, "0"]]),
        )
        .await;
    let set = args(&responses, 0, "ProfilePicture/set");
    assert_eq!(set["notCreated"]["new"]["type"], "singleton");
    assert_eq!(set["notUpdated"]["other"]["type"], "singleton");
    assert_eq!(set["notDestroyed"]["singleton"]["type"], "singleton");
    assert_eq!(set["oldState"], set["newState"], "nothing happened");

    // Without the capability in `using` the methods are unknown.
    let responses = server
        .api_using(
            "mini@example.org",
            &["urn:ietf:params:jmap:core"],
            json!([["ProfilePicture/get", { "accountId": account }, "0"]]),
        )
        .await;
    assert_eq!(responses[0][1]["type"], "unknownMethod");
}

#[tokio::test(flavor = "multi_thread")]
async fn public_pictures_follow_the_admin_switches() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let before = session(&server, "mini@example.org").await["state"].clone();
    let domain = server.store.domain("example.org").await.unwrap().unwrap().id;
    set(&server, "mini@example.org", json!({ "visibility": "public" })).await;
    let state = get(&server, "mini@example.org").await["state"].clone();

    server.store.set_domain_public_pictures(domain, false).await.unwrap();
    let session = session(&server, "mini@example.org").await;
    assert_eq!(session["accounts"][&account]["accountCapabilities"][PROFILE]["mayBePublic"], false);
    assert_ne!(session["state"], before, "clients learn that the capability changed");
    let got = get(&server, "mini@example.org").await;
    assert_ne!(got["state"], state);
    assert_eq!(got["list"][0]["visibility"], "server", "public counts as server while it is forbidden");
    let refused = set(&server, "mini@example.org", json!({ "visibility": "public" })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["type"], "invalidProperties");
    assert_eq!(refused["notUpdated"]["singleton"]["properties"], json!(["visibility"]));

    server.store.set_domain_public_pictures(domain, true).await.unwrap();
    server.store.set_public_pictures_allowed(false).await.unwrap();
    let refused = set(&server, "mini@example.org", json!({ "visibility": "public" })).await;
    assert_eq!(refused["notUpdated"]["singleton"]["type"], "invalidProperties");
    let fine = set(&server, "mini@example.org", json!({ "visibility": "off", "sendFace": true })).await;
    assert_eq!(fine["updated"]["singleton"], Value::Null);
}

/// Sent over JMAP, a person's mail carries their Face like mail sent over SMTP.
#[tokio::test(flavor = "multi_thread")]
async fn jmap_submission_carries_the_face() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let blob = upload(&server, "mini@example.org", sample(100, 100, "png")).await;
    set(&server, "mini@example.org", json!({ "blobId": blob, "visibility": "public", "sendFace": true })).await;
    let raw = "From: Mini <mini@example.org>\nTo: Nyu <nyu@example.org>\nSubject: Gesicht\n\nMiau!\n";
    let email = server.deliver("mini@example.org", raw).await;
    let using = [
        "urn:ietf:params:jmap:core",
        "urn:ietf:params:jmap:mail",
        "urn:ietf:params:jmap:submission",
        "urn:uwumail:jmap:settings",
    ];
    let responses = server
        .api_using(
            "mini@example.org",
            &using,
            json!([
                ["UserSettings/set", { "accountId": account, "update": { "singleton": { "values/undoSendSeconds": 0 } } }, "0"],
                ["Identity/get", { "accountId": account }, "1"]
            ]),
        )
        .await;
    let identity = args(&responses, 1, "Identity/get")["list"][0]["id"].as_str().unwrap().to_owned();
    let responses = server
        .api_using(
            "mini@example.org",
            &using,
            json!([["EmailSubmission/set", { "accountId": account, "create": { "s": { "identityId": identity, "emailId": email } } }, "0"]]),
        )
        .await;
    assert!(args(&responses, 0, "EmailSubmission/set")["created"]["s"].is_object(), "{}", responses[0]);

    let nyu = server.account_id("nyu@example.org").await;
    let responses = server
        .api_using(
            "nyu@example.org",
            &using,
            json!([
                ["Email/query", { "accountId": nyu }, "0"],
                ["Email/get", { "accountId": nyu, "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                    "properties": ["subject", "header:Face:asRaw"] }, "1"]
            ]),
        )
        .await;
    let got = &args(&responses, 1, "Email/get")["list"][0];
    assert_eq!(got["subject"], "Gesicht");
    assert!(got["header:Face:asRaw"].as_str().is_some_and(|face| face.trim().len() > 100), "{got}");
}
