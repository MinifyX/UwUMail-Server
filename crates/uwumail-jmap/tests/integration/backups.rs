//! A person's own mail in the backups over JMAP (`urn:uwumail:jmap:backups`, docs/jmap-backups.md).

use std::sync::Arc;

use serde_json::json;
use uwumail_backup::{BackupSettings, Backups, FolderTarget, Target};
use uwumail_jmap::Jmap;

use crate::common::{NoNet, Server, args, server, smtp};

const BACKUPS: &str = "urn:uwumail:jmap:backups";
const USING: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", BACKUPS];

#[tokio::test]
async fn people_find_their_deleted_mail_in_the_backups_and_take_it_back() {
    let plain = server().await;
    assert!(plain.session_of("mini@example.org").await["capabilities"].get(BACKUPS).is_none());
    let nas = tempfile::tempdir().unwrap();
    let backups = Backups::new(plain.store.clone(), "mail.example.org", "0.1.0");
    let settings = BackupSettings {
        enabled: true,
        target: Some(Target::Folder(FolderTarget { path: nas.path().display().to_string() })),
        ..Default::default()
    };
    backups.save_settings(&settings).await.unwrap();
    let jmap = Jmap::new(smtp(&plain.store)).with_avatar_net(Arc::new(NoNet)).with_backups(backups.clone());
    let server = Server { router: jmap.router(), jmap, store: plain.store, dir: plain.dir };

    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let email = server
        .deliver(login, "From: Telekom <esim@example.com>\nTo: mini@example.org\nSubject: Deine eSIM\n\nDer QR-Code.\n")
        .await;
    server.deliver("nyu@example.org", "From: x@example.com\nTo: nyu@example.org\nSubject: Deine eSIM\n\nNyus.\n").await;
    backups.run_now().await.unwrap();
    server.api(login, json!([["Email/set", { "accountId": account, "destroy": [email] }, "0"]])).await;

    let session = server.session_of(login).await;
    assert_eq!(session["primaryAccounts"][BACKUPS], account);
    assert!(session["state"].as_str().unwrap().ends_with("-k"), "{}", session["state"]);

    let query = json!([["BackupEmail/query", { "accountId": account, "filter": { "text": "esim" } }, "0"]]);
    let responses = server.api_using(login, &USING, query.clone()).await;
    let found = &args(&responses, 0, "BackupEmail/query")["list"];
    assert_eq!(found.as_array().unwrap().len(), 1, "only Mini's own: {found}");
    assert_eq!(found[0]["from"][0]["email"], "esim@example.com");
    assert_eq!(found[0]["inMailbox"], false);
    let id = found[0]["id"].as_str().unwrap();

    let responses = server
        .api_using(
            login,
            &USING,
            json!([
                ["BackupEmail/get", { "accountId": account, "ids": [id, "k999"] }, "0"],
                ["BackupEmail/restore", { "accountId": account, "ids": [id] }, "1"],
                ["BackupEmail/restore", { "accountId": account, "ids": [id] }, "2"],
            ]),
        )
        .await;
    let got = args(&responses, 0, "BackupEmail/get");
    assert!(got["list"][0]["textBody"].as_str().unwrap().contains("Der QR-Code."), "{got}");
    assert_eq!(got["notFound"], json!(["k999"]));
    let mailbox = args(&responses, 1, "BackupEmail/restore")["restored"][id].as_str().unwrap().to_owned();
    assert_eq!(args(&responses, 2, "BackupEmail/restore")["alreadyThere"], json!([id]));
    let responses = server
        .api(login, json!([["Email/query", { "accountId": account, "filter": { "inMailbox": mailbox } }, "0"]]))
        .await;
    assert_eq!(args(&responses, 0, "Email/query")["ids"].as_array().unwrap().len(), 1);

    // Nyu cannot take Mini's mail.
    let nyu = server.account_id("nyu@example.org").await;
    let responses = server
        .api_using("nyu@example.org", &USING, json!([["BackupEmail/restore", { "accountId": nyu, "ids": [id] }, "0"]]))
        .await;
    assert_eq!(args(&responses, 0, "BackupEmail/restore")["notRestored"][id]["type"], "notFound");

    // Switched off by the admin: gone from the session, and the methods say no.
    backups.save_settings(&BackupSettings { user_search: false, ..settings }).await.unwrap();
    assert!(server.session_of(login).await["capabilities"].get(BACKUPS).is_none());
    let responses = server.api_using(login, &USING, query).await;
    assert_eq!(responses[0][0], "error");
    assert_eq!(responses[0][1]["type"], "forbidden");
}
