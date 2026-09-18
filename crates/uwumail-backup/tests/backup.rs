//! Backing up a server into a local repository, again, pruning and restoring it.

use uwumail_backup::{RepoKey, Repository, Retention, Storage};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

async fn server(dir: &std::path::Path) -> (Store, i64) {
    let store = Store::open(dir).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    let new = NewAccount {
        address: "mini@example.de".into(),
        display_name: "Mini".into(),
        password: Some("katzenpfote-123".into()),
        role: Role::User,
        quota_bytes: 0,
        protocols: None,
    };
    let id = store.create_account(new).await.unwrap().id;
    (store, id)
}

async fn deliver(store: &Store, account: i64, subject: &str) {
    // Like an attachment: hardly compressible, so the blobs outweigh the database.
    use sha2::Digest;
    let mut body = format!("Hallo Mini, {subject}.\r\n");
    let mut block = sha2::Sha256::digest(subject.as_bytes());
    for _ in 0..3000 {
        block = sha2::Sha256::digest(block);
        body.push_str(&hex::encode(block));
        body.push_str("\r\n");
    }
    let raw = format!("From: nyu@example.org\r\nTo: mini@example.de\r\nSubject: {subject}\r\n\r\n{body}\r\n");
    let request = IngestRequest {
        account_id: account,
        raw: raw.into_bytes(),
        mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
        keywords: vec![],
        received_at: None,
    };
    store.ingest(request).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn backups_are_incremental_encrypted_and_restorable() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mini) = server(&dir.path().join("data")).await;
    for subject in ["Eins", "Zwei", "Drei", "Vier", "Fünf", "Sechs", "Sieben", "Acht", "Neun", "Zehn"] {
        deliver(&store, mini, subject).await;
    }
    std::fs::create_dir_all(dir.path().join("data/tls")).unwrap();
    std::fs::write(dir.path().join("data/tls/key.pem"), b"not really a key").unwrap();

    let key = RepoKey::generate();
    let repo_dir = dir.path().join("repo");
    let open = |key: Option<RepoKey>, now: i64| Repository::open(Storage::Local(repo_dir.clone()), key, now);
    let repo = open(Some(key.clone()), 1_000).await.unwrap();
    let first = uwumail_backup::backup(&store, &repo, "mail.example.de", "0.1.0", Retention::default(), 1_000_000)
        .await
        .unwrap();
    assert!(first.uploaded > 0);

    deliver(&store, mini, "Elf").await;
    let second = uwumail_backup::backup(&store, &repo, "mail.example.de", "0.1.0", Retention::default(), 1_086_400)
        .await
        .unwrap();
    assert!(second.uploaded * 3 < first.uploaded, "only what changed: {} after {}", second.uploaded, first.uploaded);
    assert_eq!(repo.snapshots().await.unwrap().len(), 2);
    assert!(uwumail_backup::check(&repo, &second.snapshot).await.unwrap().is_empty());

    // Nothing readable on the backup server.
    for entry in walk(&repo_dir) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(!bytes.windows(9).any(|window| window == b"Hallo Min"), "{} is readable", entry.display());
    }
    assert!(matches!(open(Some(RepoKey::generate()), 0).await, Err(uwumail_backup::Error::WrongKey)));
    assert!(matches!(open(None, 0).await, Err(uwumail_backup::Error::WrongKey)));

    // Keeping only the newest removes the first snapshot, but nothing the second one needs.
    let none = Retention { daily: 0, weekly: 0, monthly: 0 };
    let (snapshots, _objects) = uwumail_backup::prune(&repo, none).await.unwrap();
    assert_eq!(snapshots, 1);
    assert!(uwumail_backup::check(&repo, &second.snapshot).await.unwrap().is_empty());

    let restored_dir = dir.path().join("restored");
    let typed = RepoKey::from_recovery_text(&key.recovery_text()).unwrap();
    let repo = open(Some(typed), 0).await.unwrap();
    let manifest = uwumail_backup::restore(&repo, &second.snapshot, &restored_dir).await.unwrap();
    assert_eq!(manifest.hostname, "mail.example.de");
    assert_eq!(std::fs::read(restored_dir.join("tls/key.pem")).unwrap(), b"not really a key");
    let restored = Store::open(&restored_dir).await.unwrap();
    let account = restored.authenticate("mini@example.de", "katzenpfote-123").await.unwrap().unwrap();
    let inbox =
        restored.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
    let emails = restored.emails_in_mailbox(inbox.id, 10).await.unwrap();
    assert_eq!(emails.len(), 10, "the first page of eleven");
    let blob = uwumail_store::BlobHash::parse(&emails[0].blob).unwrap();
    assert!(restored.blob(&blob).await.unwrap().starts_with(b"From: nyu@example.org"));
    assert!(uwumail_backup::restore(&repo, &second.snapshot, &restored_dir).await.is_err(), "never over a server");
}

#[tokio::test(flavor = "multi_thread")]
async fn unencrypted_backups_work_too() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mini) = server(&dir.path().join("data")).await;
    deliver(&store, mini, "Eins").await;
    let repo = Repository::open(Storage::Local(dir.path().join("repo")), None, 0).await.unwrap();
    assert!(!repo.config.encrypted);
    let report =
        uwumail_backup::backup(&store, &repo, "mail.example.de", "0.1.0", Retention::default(), 5).await.unwrap();
    let restored = dir.path().join("restored");
    uwumail_backup::restore(&repo, &report.snapshot, &restored).await.unwrap();
    assert!(Store::open(&restored).await.unwrap().account("mini@example.de").await.unwrap().is_some());
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            files.extend(walk(&entry.path()));
        } else {
            files.push(entry.path());
        }
    }
    files
}
