//! Delayed sending: the undo window, send later (sendAt, FUTURERELEASE), cancelling, and held
//! mail that outlives a restart.

use crate::common;

use std::time::Duration;

use common::{args, server};
use serde_json::{Value, json};
use uwumail_jmap::Jmap;
use uwumail_store::Store;

const MINI: &str = "mini@example.org";
const NYU: &str = "nyu@example.org";

/// A message from Mini to Nyu in Mini's account, and Mini's identity.
async fn draft(server: &common::Server, subject: &str) -> (String, String) {
    let raw = format!("From: Mini <mini@example.org>\nTo: Nyu <nyu@example.org>\nSubject: {subject}\n\nMiau!\n");
    let email = server.deliver(MINI, &raw).await;
    let account = server.account_id(MINI).await;
    let responses = server.api(MINI, json!([["Identity/get", { "accountId": account }, "0"]])).await;
    let identity = args(&responses, 0, "Identity/get")["list"][0]["id"].as_str().unwrap().to_owned();
    (email, identity)
}

async fn submit(server: &common::Server, email: &str, identity: &str, extra: Value) -> Value {
    let account = server.account_id(MINI).await;
    let mut object = json!({ "identityId": identity, "emailId": email });
    if let (Value::Object(object), Value::Object(extra)) = (&mut object, extra) {
        object.extend(extra);
    }
    let responses = server
        .api(MINI, json!([["EmailSubmission/set", { "accountId": account, "create": { "s": object } }, "0"]]))
        .await;
    args(&responses, 0, "EmailSubmission/set").clone()
}

async fn subjects_for_nyu(server: &common::Server) -> Vec<String> {
    let nyu = server.account_id(NYU).await;
    let responses = server
        .api(
            NYU,
            json!([
                ["Email/query", { "accountId": nyu }, "0"],
                ["Email/get", { "accountId": nyu, "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                    "properties": ["subject"] }, "1"],
            ]),
        )
        .await;
    let list = args(&responses, 1, "Email/get")["list"].as_array().unwrap().clone();
    list.iter().map(|e| e["subject"].as_str().unwrap().to_owned()).collect()
}

async fn submission(server: &common::Server, id: &str) -> Value {
    let account = server.account_id(MINI).await;
    let responses =
        server.api(MINI, json!([["EmailSubmission/get", { "accountId": account, "ids": [id] }, "0"]])).await;
    args(&responses, 0, "EmailSubmission/get")["list"][0].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_undo_window_holds_every_submission_and_cancelling_stops_it() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let (_, body) = server
        .request(
            axum::http::Request::get("/jmap/session")
                .header("authorization", common::basic(MINI, common::PASSWORD))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    let session: Value = serde_json::from_slice(&body).unwrap();
    let submission_capability =
        &session["accounts"][&account]["accountCapabilities"]["urn:ietf:params:jmap:submission"];
    assert_eq!(submission_capability["maxDelayedSend"], 2_592_000);
    assert_eq!(submission_capability["submissionExtensions"]["FUTURERELEASE"][0], "2592000");

    // Nobody chose: ten seconds.
    let (email, identity) = draft(&server, "Oops").await;
    let before = uwumail_store_now();
    let set = submit(&server, &email, &identity, json!({})).await;
    let created = &set["created"]["s"];
    assert_eq!(created["undoStatus"], "pending", "{set}");
    let send_at = uwumail_jmap::dates::parse(created["sendAt"].as_str().unwrap()).unwrap();
    assert!((before + 9..=before + 11).contains(&send_at), "{send_at} vs {before}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(submission(&server, &id).await["undoStatus"], "pending");

    // Cancelled in time: it never goes.
    let responses = server
        .api(
            MINI,
            json!([["EmailSubmission/set", { "accountId": account, "update": { &id: { "undoStatus": "canceled" } } }, "0"]]),
        )
        .await;
    assert!(args(&responses, 0, "EmailSubmission/set")["updated"].get(&id).is_some(), "{}", responses[0]);
    assert_eq!(submission(&server, &id).await["undoStatus"], "canceled");
    assert_eq!(server.store.next_held_submission().await.unwrap(), None);
    assert_eq!(server.jmap.release_due_submissions().await, 0);
    assert!(subjects_for_nyu(&server).await.is_empty());

    // Only undoStatus: canceled is a change a submission takes.
    let responses = server
        .api(
            MINI,
            json!([["EmailSubmission/set", { "accountId": account, "update": { &id: { "emailId": email } } }, "0"]]),
        )
        .await;
    assert_eq!(args(&responses, 0, "EmailSubmission/set")["notUpdated"][&id]["type"], "invalidProperties");

    // Chosen in the portal (or the settings): no window, it goes at once.
    let change = json!({ "mailUndoSend": "0" }).as_object().unwrap().clone();
    server.store.update_preferences(server.id(MINI).await, change).await.unwrap();
    let responses = server.api(MINI, json!([["UserSettings/get", { "accountId": account }, "0"]])).await;
    assert_eq!(args(&responses, 0, "UserSettings/get")["list"][0]["values"]["undoSendSeconds"], 0);
    let (email, identity) = draft(&server, "Straight away").await;
    let set = submit(&server, &email, &identity, json!({})).await;
    assert_eq!(set["created"]["s"]["undoStatus"], "final", "{set}");
    assert_eq!(subjects_for_nyu(&server).await, ["Straight away"]);
    let id = set["created"]["s"]["id"].as_str().unwrap().to_owned();
    let responses = server
        .api(
            MINI,
            json!([["EmailSubmission/set", { "accountId": account, "update": { &id: { "undoStatus": "canceled" } } }, "0"]]),
        )
        .await;
    assert_eq!(args(&responses, 0, "EmailSubmission/set")["notUpdated"][&id]["type"], "cannotUnsend");

    // Twenty seconds, set over JMAP.
    let responses = server
        .api(
            MINI,
            json!([["UserSettings/set", { "accountId": account, "update": { "singleton": { "values/undoSendSeconds": 20 } } }, "0"]]),
        )
        .await;
    assert!(args(&responses, 0, "UserSettings/set")["updated"].is_object());
    assert_eq!(server.store.undo_send_seconds(server.id(MINI).await).await.unwrap(), 20);
}

#[tokio::test(flavor = "multi_thread")]
async fn send_later_goes_when_its_time_comes() {
    let server = server().await;
    let (email, identity) = draft(&server, "Later").await;

    // Too far ahead, and a message that could never go, are refused at once.
    let far = uwumail_jmap::dates::format(uwumail_store_now() + 40 * 24 * 3600);
    let set = submit(&server, &email, &identity, json!({ "sendAt": far })).await;
    assert_eq!(set["notCreated"]["s"]["type"], "invalidProperties", "{set}");
    let set = submit(
        &server,
        &email,
        &identity,
        json!({ "envelope": { "mailFrom": { "email": "boss@bank.example", "parameters": { "HOLDFOR": "60" } },
            "rcptTo": [{ "email": "nyu@example.org" }] } }),
    )
    .await;
    assert_eq!(set["notCreated"]["s"]["type"], "forbiddenMailFrom", "{set}");

    // FUTURERELEASE with HOLDFOR, released by the sender loop.
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let sender = tokio::spawn(server.jmap.clone().run_scheduled_sending(shutdown_rx));
    let set = submit(
        &server,
        &email,
        &identity,
        json!({ "envelope": { "mailFrom": { "email": "mini@example.org", "parameters": { "HOLDFOR": "1" } },
            "rcptTo": [{ "email": "nyu@example.org" }] } }),
    )
    .await;
    assert_eq!(set["created"]["s"]["undoStatus"], "pending", "{set}");
    let id = set["created"]["s"]["id"].as_str().unwrap().to_owned();
    let mut delivered = false;
    for _ in 0..50 {
        if subjects_for_nyu(&server).await == ["Later"] {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(delivered, "the held message went out");
    shutdown.send(true).unwrap();
    sender.await.unwrap();
    let record = submission(&server, &id).await;
    assert_eq!(record["undoStatus"], "final");
    assert_eq!(record["deliveryStatus"], Value::Null);

    // A sendAt in the past is now.
    let (email, identity) = draft(&server, "Past").await;
    let set = submit(&server, &email, &identity, json!({ "sendAt": "2020-01-01T00:00:00Z" })).await;
    assert_eq!(set["created"]["s"]["undoStatus"], "final", "{set}");
}

#[tokio::test(flavor = "multi_thread")]
async fn held_mail_survives_a_restart() {
    let server = server().await;
    let (email, identity) = draft(&server, "After the restart").await;
    // A few seconds ahead: dates are whole seconds, and a slow machine must not reach the time
    // before the submission is even made.
    let soon = uwumail_jmap::dates::format(uwumail_store_now() + 4);
    let set = submit(&server, &email, &identity, json!({ "sendAt": soon })).await;
    assert_eq!(set["created"]["s"]["undoStatus"], "pending", "{set}");

    // The draft is deleted meanwhile: what was submitted is kept anyway.
    let account = server.account_id(MINI).await;
    let responses = server.api(MINI, json!([["Email/set", { "accountId": account, "destroy": [email] }, "0"]])).await;
    assert!(args(&responses, 0, "Email/set")["destroyed"].is_array());

    // Another server process on the same data, after the time has come.
    tokio::time::sleep(Duration::from_millis(5100)).await;
    let store = Store::open(server.dir.path()).await.unwrap();
    let restarted = Jmap::new(common::smtp(&store));
    assert_eq!(restarted.release_due_submissions().await, 1);
    assert_eq!(restarted.release_due_submissions().await, 0, "each is sent once");
    assert_eq!(subjects_for_nyu(&server).await, ["After the restart"]);
}

fn uwumail_store_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Held mail belongs to the login that held it: revoking that app password stops it
/// (security audit 0.16.0 PROTOCOLS-10).
#[tokio::test(flavor = "multi_thread")]
async fn held_mail_ends_with_the_app_password_that_held_it() {
    let server = server().await;
    let (email, identity) = draft(&server, "Spam from a stolen password").await;
    let mini = server.id(MINI).await;
    let app = server
        .store
        .create_app_password(
            mini,
            uwumail_store::NewAppPassword {
                name: "phone".into(),
                scopes: vec![uwumail_store::AppScope::Mail],
                expires_at: None,
            },
        )
        .await
        .unwrap();
    let account = server.account_id(MINI).await;
    let later = uwumail_jmap::dates::format(uwumail_store_now() + 3600);
    let using = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"];
    let create = json!({ "identityId": identity, "emailId": email, "sendAt": later });
    let (_, response) = server
        .api_as(
            &format!("Bearer {}", app.secret),
            &using,
            json!([["EmailSubmission/set", { "accountId": account, "create": { "s": create } }, "0"]]),
        )
        .await;
    let created = &response["methodResponses"][0][1]["created"]["s"];
    assert_eq!(created["undoStatus"], "pending", "{response}");
    let id = created["id"].as_str().unwrap().to_owned();

    server.store.revoke_app_password(mini, app.app_password.id).await.unwrap();
    assert_eq!(submission(&server, &id).await["undoStatus"], "canceled");
    assert_eq!(server.jmap.release_due_submissions().await, 0);
}

/// How many emails a person has, in all folders.
async fn email_count(server: &common::Server, login: &str) -> u64 {
    let account = server.account_id(login).await;
    let responses =
        server.api(login, json!([["Email/query", { "accountId": account, "calculateTotal": true }, "0"]])).await;
    args(&responses, 0, "Email/query")["total"].as_u64().unwrap()
}

async fn internal_only(server: &common::Server, on: bool) {
    let update = uwumail_store::AccountUpdate { internal_only: Some(on), ..Default::default() };
    server.store.update_account(MINI, update).await.unwrap();
}

/// An internal-only account (docs/jmap-sending.md): the session says so, people here get the
/// message, everyone else comes back in a bounce, and with nobody here it is refused as a whole.
#[tokio::test(flavor = "multi_thread")]
async fn an_internal_only_account_sends_inside_and_bounces_the_rest() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let session = server.session_of(MINI).await;
    let capability = &session["accounts"][&account]["accountCapabilities"]["urn:ietf:params:jmap:submission"];
    assert_eq!(capability["uwuInternalOnly"], false, "always there");
    assert!(capability.get("uwuInternalDomains").is_none(), "only while it is on");
    let state_before = session["state"].clone();

    server.store.create_domain_with_kind("masked.test", uwumail_store::DomainKind::Masked).await.unwrap();
    internal_only(&server, true).await;
    let session = server.session_of(MINI).await;
    let capability = &session["accounts"][&account]["accountCapabilities"]["urn:ietf:params:jmap:submission"];
    assert_eq!(capability["uwuInternalOnly"], true);
    assert_eq!(capability["uwuInternalDomains"], json!(["example.org", "masked.test"]));
    assert_ne!(session["state"], state_before, "the session state moves with the switch");
    // A new domain is somewhere to write to as well: the state moves, so apps read it again,
    // and API responses tell the same state.
    let state_before = session["state"].clone();
    server.store.create_domain("later.test").await.unwrap();
    let session = server.session_of(MINI).await;
    let capability = &session["accounts"][&account]["accountCapabilities"]["urn:ietf:params:jmap:submission"];
    assert_eq!(capability["uwuInternalDomains"], json!(["example.org", "later.test", "masked.test"]));
    assert_ne!(session["state"], state_before, "the session state moves with the domains");
    let (_, response) = server
        .api_as(
            &crate::common::basic(MINI, crate::common::PASSWORD),
            &["urn:ietf:params:jmap:core"],
            json!([["Core/echo", {}, "0"]]),
        )
        .await;
    let api_state = response["sessionState"].as_str().unwrap();
    assert!(session["state"].as_str().unwrap().starts_with(api_state), "{api_state} {}", session["state"]);

    // Mixed: Nyu gets it, the address elsewhere comes back to Mini.
    let (email, identity) = draft(&server, "Inside").await;
    let mini_before = email_count(&server, MINI).await;
    let envelope =
        json!({ "mailFrom": { "email": MINI }, "rcptTo": [{ "email": NYU }, { "email": "leni@elsewhere.test" }] });
    let set =
        submit(&server, &email, &identity, json!({ "envelope": envelope, "sendAt": "2020-01-01T00:00:00Z" })).await;
    assert_eq!(set["created"]["s"]["undoStatus"], "final", "{set}");
    assert_eq!(subjects_for_nyu(&server).await, ["Inside"]);
    assert_eq!(email_count(&server, MINI).await, mini_before + 1, "a bounce for the address elsewhere");
    assert!(server.store.queue_entries().await.unwrap().is_empty(), "nothing goes outside");

    // Nobody here: refused at once, also when held for later.
    let envelope = json!({ "mailFrom": { "email": MINI }, "rcptTo": [{ "email": "leni@elsewhere.test" }] });
    let set = submit(&server, &email, &identity, json!({ "envelope": envelope.clone() })).await;
    assert_eq!(set["notCreated"]["s"]["type"], "forbiddenToSend", "{set}");
    let later = uwumail_jmap::dates::format(uwumail_store_now() + 3600);
    let set = submit(&server, &email, &identity, json!({ "envelope": envelope, "sendAt": later })).await;
    assert_eq!(set["notCreated"]["s"]["type"], "forbiddenToSend", "{set}");
}

/// Mail held back before the switch went on is checked again when its time comes.
#[tokio::test(flavor = "multi_thread")]
async fn held_mail_is_checked_against_internal_only_when_it_goes() {
    let server = server().await;
    let mini = server.id(MINI).await;
    let identity = server.store.identities(mini).await.unwrap()[0].id;
    let hold = |subject: &'static str, rcpt: Value| {
        let server = &server;
        async move {
            let raw = format!("From: mini@example.org\r\nTo: nyu@example.org\r\nSubject: {subject}\r\n\r\nMiau\r\n");
            let email = server
                .store
                .ingest(uwumail_store::IngestRequest {
                    account_id: mini,
                    raw: raw.into_bytes(),
                    mailboxes: vec![uwumail_store::MailboxTarget::Role(uwumail_store::MailboxRole::Drafts)],
                    keywords: vec![],
                    received_at: None,
                })
                .await
                .unwrap();
            let envelope = json!({ "mailFrom": { "email": MINI }, "rcptTo": rcpt }).to_string();
            server
                .store
                .hold_submission(uwumail_store::NewHeldSubmission {
                    account_id: mini,
                    identity_id: identity,
                    email_id: email.id,
                    thread_id: email.thread_id,
                    envelope,
                    send_at: uwumail_store_now() - 1,
                    blob: email.blob,
                    credential: None,
                })
                .await
                .unwrap()
        }
    };
    let mixed = hold("Mixed", json!([{ "email": NYU }, { "email": "leni@elsewhere.test" }])).await;
    let outside = hold("Outside", json!([{ "email": "leni@elsewhere.test" }])).await;
    internal_only(&server, true).await;
    let mini_before = email_count(&server, MINI).await;

    assert_eq!(server.jmap.release_due_submissions().await, 2);
    assert_eq!(subjects_for_nyu(&server).await, ["Mixed"]);
    assert_eq!(email_count(&server, MINI).await, mini_before + 1, "a bounce for the address elsewhere");
    assert!(server.store.queue_entries().await.unwrap().is_empty(), "nothing goes outside");
    let records = server.store.submissions(mini, Some(vec![mixed, outside])).await.unwrap();
    let outside = records.iter().find(|record| record.id == outside).unwrap();
    assert!(
        outside.release_error.as_deref().is_some_and(|error| error.contains("only send to addresses on this server")),
        "{:?}",
        outside.release_error
    );
}
