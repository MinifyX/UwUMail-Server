//! Putting one person's mail back from a snapshot while the server runs: the way the portal does it
//! (open a snapshot, pick a person and folders, restore) and the way the command line does it.

use std::time::Duration;

use uwumail_backup::{BackupSettings, Backups, FolderTarget, MailboxRestore, RepoKey, Target};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

async fn person(store: &Store, address: &str) -> i64 {
    let new = NewAccount {
        address: address.into(),
        display_name: String::new(),
        password: None,
        role: Role::User,
        quota_bytes: 0,
        protocols: None,
    };
    store.create_account(new).await.unwrap().id
}

async fn deliver(store: &Store, account: i64, mailbox: MailboxTarget, subject: &str, keywords: &[&str]) -> i64 {
    let raw = format!(
        "From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: {subject}\r\nMessage-ID: <{subject}@example.net>\r\n\r\nHallo\r\n"
    );
    let request = IngestRequest {
        account_id: account,
        raw: raw.into_bytes(),
        mailboxes: vec![mailbox],
        keywords: keywords.iter().map(|keyword| keyword.to_string()).collect(),
        received_at: Some(1_700_000_000),
    };
    store.ingest(request).await.unwrap().id
}

/// The folders of an account as paths, with what is in them: `(path, subject, keywords)`.
async fn contents(store: &Store, account: i64) -> Vec<(String, String, Vec<String>)> {
    let mailboxes = store.mailboxes(account).await.unwrap();
    let path_of = |id: i64| {
        let mut path = Vec::new();
        let mut current = Some(id);
        while let Some(mailbox) = current.and_then(|id| mailboxes.iter().find(|mailbox| mailbox.id == id)) {
            path.insert(0, mailbox.name.clone());
            current = mailbox.parent_id;
        }
        path.join("/")
    };
    let mut found = Vec::new();
    for mailbox in &mailboxes {
        for email in store.emails_in_mailbox(mailbox.id, 100).await.unwrap() {
            let mut keywords = email.keywords.clone();
            keywords.sort();
            found.push((path_of(mailbox.id), email.subject, keywords));
        }
    }
    found.sort();
    found
}

async fn settled(backups: &Backups) -> MailboxRestore {
    for _ in 0..200 {
        let job = backups.mailbox_restore();
        if !matches!(job.state.as_str(), "opening" | "restoring") {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the mailbox restore never finished: {:?}", backups.mailbox_restore());
}

#[tokio::test(flavor = "multi_thread")]
async fn one_mailbox_comes_back_next_to_what_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    let mini = person(&store, "mini@example.org").await;
    let nyu = person(&store, "nyu@example.org").await;
    let eins = deliver(&store, mini, MailboxTarget::Role(MailboxRole::Inbox), "Eins", &["$seen", "$flagged"]).await;
    deliver(&store, mini, MailboxTarget::Role(MailboxRole::Inbox), "Zwei", &["$seen"]).await;
    let projects = store.create_mailbox(mini, "Projekte", None, None, 0, true).await.unwrap();
    let old = store.create_mailbox(mini, "Alt", Some(projects), None, 0, true).await.unwrap();
    let drei = deliver(&store, mini, MailboxTarget::Id(old), "Drei", &["$answered"]).await;
    deliver(&store, nyu, MailboxTarget::Role(MailboxRole::Inbox), "Für Nyu", &[]).await;

    std::fs::create_dir(dir.path().join("nas")).unwrap();
    let backups = Backups::new(store.clone(), "mail.example.org", "0.1.0");
    let settings = BackupSettings {
        enabled: true,
        target: Some(Target::Folder(FolderTarget { path: dir.path().join("nas").display().to_string() })),
        key: Some(RepoKey::generate().recovery_text()),
        ..Default::default()
    };
    backups.save_settings(&settings).await.unwrap();
    backups.run_now().await.unwrap();

    // Two messages go, and the folder they were in with them.
    store.destroy_emails(mini, vec![eins, drei]).await.unwrap();
    store.destroy_mailbox(mini, old, true).await.unwrap();
    let before = contents(&store, mini).await;
    assert_eq!(before.len(), 1, "{before:?}");

    // The portal's way: open the latest snapshot, see who is in it, restore one person.
    backups.open_snapshot("latest").await.unwrap();
    let open = settled(&backups).await;
    assert_eq!(open.state, "open", "{}", open.error);
    let logins: Vec<&str> = open.people.iter().map(|person| person.login.as_str()).collect();
    assert_eq!(logins, ["mini@example.org", "nyu@example.org"]);
    let mini_then = &open.people[0];
    assert_eq!(mini_then.emails, 3);
    assert_eq!(mini_then.folders[0].role.as_deref(), Some("inbox"), "the inbox comes first");
    assert!(mini_then.folders.iter().any(|folder| folder.path == ["Projekte", "Alt"]));

    assert!(backups.start_mailbox_restore("somebody@example.org", None, None, "test").await.is_err());
    backups.start_mailbox_restore("mini@example.org", None, None, "test").await.unwrap();
    let done = settled(&backups).await;
    let last = done.last.unwrap();
    assert_eq!(last.error, "");
    assert_eq!((last.restored, last.skipped), (2, 1), "Zwei is still there");
    assert!(last.folder.starts_with("Restored 20"), "{}", last.folder);

    let after = contents(&store, mini).await;
    let restored = |path: &str| format!("{}/{path}", last.folder);
    assert!(after.contains(&(restored("Inbox"), "Eins".into(), vec!["$flagged".into(), "$seen".into()])), "{after:?}");
    assert!(after.contains(&(restored("Projekte/Alt"), "Drei".into(), vec!["$answered".into()])), "{after:?}");
    assert_eq!(after.len(), 3, "nothing twice: {after:?}");
    assert_eq!(contents(&store, nyu).await.len(), 1, "nobody else is touched");

    // Again: everything is there now, so nothing comes twice.
    backups.start_mailbox_restore("mini@example.org", None, None, "test").await.unwrap();
    let again = settled(&backups).await.last.unwrap();
    assert_eq!((again.restored, again.skipped), (0, 3));
    backups.close_snapshot().await.unwrap();
    assert_eq!(backups.mailbox_restore().state, "", "closed");
    assert!(!dir.path().join("data/backup-tmp/restore-mailbox").exists(), "its database is gone");

    // The command line's way, one folder only, into another person's mailbox.
    let mut lines = Vec::new();
    let report = backups
        .restore_mailbox_now(
            "latest",
            "mini@example.org",
            Some("nyu@example.org"),
            &["Projekte".into()],
            &mut |line, _| {
                if !line.is_empty() {
                    lines.push(line.to_owned());
                }
            },
        )
        .await
        .unwrap();
    assert_eq!((report.restored, report.skipped), (1, 0));
    let nyu_now = contents(&store, nyu).await;
    assert!(nyu_now.contains(&(format!("{}/Projekte/Alt", report.folder), "Drei".into(), vec!["$answered".into()])));
    assert_eq!(nyu_now.len(), 2, "{nyu_now:?}");
    assert!(
        backups
            .restore_mailbox_now("latest", "mini@example.org", None, &["Gibt es nicht".into()], &mut |_, _| {})
            .await
            .is_err()
    );
}
