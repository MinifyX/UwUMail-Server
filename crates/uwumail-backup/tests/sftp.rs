//! A backup to a real SFTP server. Needs one, so it only runs when asked:
//!
//! ```sh
//! UWUMAIL_TEST_SFTP=user@host:/path/to/empty/dir UWUMAIL_TEST_SFTP_KEY=~/.ssh/id_ed25519 \
//!   cargo test -p uwumail-backup --test sftp -- --ignored
//! ```

use uwumail_backup::sftp::Sftp;
use uwumail_backup::{Login, RepoKey, Repository, Retention, Storage, Target};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

fn target(host_key: Option<String>) -> Target {
    let spec = std::env::var("UWUMAIL_TEST_SFTP").expect("UWUMAIL_TEST_SFTP=user@host:/path");
    let (user, rest) = spec.split_once('@').expect("user@host:/path");
    let (host, path) = rest.split_once(':').expect("user@host:/path");
    let key_file = std::env::var("UWUMAIL_TEST_SFTP_KEY").expect("UWUMAIL_TEST_SFTP_KEY=path to an OpenSSH key");
    Target {
        host: host.into(),
        port: 22,
        user: user.into(),
        path: path.into(),
        login: Login::Key { private_key: std::fs::read_to_string(key_file).unwrap() },
        host_key,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an SFTP server"]
async fn backup_and_restore_over_sftp() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    let new = NewAccount {
        address: "mini@example.de".into(),
        display_name: String::new(),
        password: None,
        role: Role::User,
        quota_bytes: 0,
    };
    let mini = store.create_account(new).await.unwrap().id;
    let raw = b"From: nyu@example.org\r\nSubject: Hallo\r\n\r\nHallo\r\n".to_vec();
    let request = IngestRequest {
        account_id: mini,
        raw,
        mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
        keywords: vec![],
        received_at: None,
    };
    store.ingest(request).await.unwrap();

    let first = Sftp::connect(&target(None)).await.unwrap();
    let host_key = first.host_key.clone();
    assert!(host_key.starts_with("SHA256:"), "{host_key}");
    let key = RepoKey::generate();
    let repo = Repository::open(Storage::Sftp(first), Some(key.clone()), 1).await.unwrap();
    let report =
        uwumail_backup::backup(&store, &repo, "mail.example.de", "0.1.0", Retention::default(), 1_000).await.unwrap();
    assert!(uwumail_backup::check(&repo, &report.snapshot).await.unwrap().is_empty());
    repo.storage.close().await;

    let wrong = target(Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into()));
    assert!(matches!(Sftp::connect(&wrong).await, Err(uwumail_backup::Error::HostKeyChanged { .. })));

    let again = Sftp::connect(&target(Some(host_key))).await.unwrap();
    let repo = Repository::open(Storage::Sftp(again), Some(key), 1).await.unwrap();
    let restored = dir.path().join("restored");
    uwumail_backup::restore(&repo, &report.snapshot, &restored).await.unwrap();
    repo.storage.close().await;
    assert!(Store::open(&restored).await.unwrap().account("mini@example.de").await.unwrap().is_some());
}
