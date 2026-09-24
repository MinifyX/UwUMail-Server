//! Backing up a server into a local repository, again, pruning and restoring it.

use uwumail_backup::{RepoKey, Repository, Retention, Storage};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

async fn server(dir: &std::path::Path) -> (Store, i64) {
    let store = Store::open(dir).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    let new = NewAccount {
        address: "mini@example.org".into(),
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
    let raw = format!("From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: {subject}\r\n\r\n{body}\r\n");
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
    let first = uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 1_000_000)
        .await
        .unwrap();
    assert!(first.uploaded > 0);

    deliver(&store, mini, "Elf").await;
    let second = uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 1_086_400)
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
    assert_eq!(manifest.hostname, "mail.example.org");
    assert_eq!(std::fs::read(restored_dir.join("tls/key.pem")).unwrap(), b"not really a key");
    let restored = Store::open(&restored_dir).await.unwrap();
    let account = restored.authenticate("mini@example.org", "katzenpfote-123").await.unwrap().unwrap();
    let inbox =
        restored.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
    let emails = restored.emails_in_mailbox(inbox.id, 10).await.unwrap();
    assert_eq!(emails.len(), 10, "the first page of eleven");
    let blob = uwumail_store::BlobHash::parse(&emails[0].blob).unwrap();
    assert!(restored.blob(&blob).await.unwrap().starts_with(b"From: nyu@example.net"));
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
        uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 5).await.unwrap();
    let restored = dir.path().join("restored");
    uwumail_backup::restore(&repo, &report.snapshot, &restored).await.unwrap();
    assert!(Store::open(&restored).await.unwrap().account("mini@example.org").await.unwrap().is_some());
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

/// What a backup server lists and serves is its own to choose. Names that are not objects or
/// snapshots are left alone, and a file larger than it may be is refused, not read whole
/// (security-audit-0.8.0 INF-4).
#[tokio::test(flavor = "multi_thread")]
async fn a_backup_server_cannot_crash_a_backup_with_what_it_lists() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mini) = server(&dir.path().join("data")).await;
    deliver(&store, mini, "Eins").await;
    let repo_dir = dir.path().join("repo");
    let repo = Repository::open(Storage::Local(repo_dir.clone()), Some(RepoKey::generate()), 0).await.unwrap();
    uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 5).await.unwrap();

    // A one-byte name, a name that is no id, a prefix that is no prefix and a snapshot that is none.
    for stray in ["data/ab/x", "data/ab/é", "data/z/zz", "snapshots/x"] {
        let path = repo_dir.join(stray);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not ours").unwrap();
    }
    let none = Retention { daily: 0, weekly: 0, monthly: 0 };
    let report = uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", none, 1_000_000).await.unwrap();
    assert_eq!(repo.snapshots().await.unwrap(), vec![report.snapshot.clone()], "the stray snapshot is not one");
    assert!(repo_dir.join("data/ab/x").exists(), "what is not ours is left alone");
    assert!(matches!(repo.manifest("../uwumail-backup.json").await, Err(uwumail_backup::Error::Config(_))));

    // Files are only read up to their limit.
    std::fs::write(repo_dir.join("big"), vec![0u8; 100]).unwrap();
    let storage = Storage::Local(repo_dir.clone());
    assert_eq!(storage.read("big", 100).await.unwrap().unwrap().len(), 100);
    assert!(matches!(storage.read("big", 99).await, Err(uwumail_backup::Error::Damaged(_))));
}

/// The backup server holds the config that says whether a backup is encrypted, and every object.
/// With a key, it can neither turn encryption off nor have a restore take anything this server did
/// not write under that name (security-audit-0.8.0 INF-1).
#[tokio::test(flavor = "multi_thread")]
async fn a_backup_server_cannot_turn_encryption_off_or_swap_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mini) = server(&dir.path().join("data")).await;
    deliver(&store, mini, "Eins").await;
    std::fs::create_dir_all(dir.path().join("data/tls")).unwrap();
    std::fs::write(dir.path().join("data/tls/a.pem"), b"aaaa").unwrap();
    std::fs::write(dir.path().join("data/tls/b.pem"), b"bbbb").unwrap();
    let key = RepoKey::generate();
    let repo_dir = dir.path().join("repo");
    let storage = || Storage::Local(repo_dir.clone());
    let repo = Repository::open(storage(), Some(key.clone()), 0).await.unwrap();
    let first = uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 1_000_000)
        .await
        .unwrap();

    // "Not encrypted", says the config, to a server that has a key.
    let config = repo_dir.join("uwumail-backup.json");
    let genuine_config = std::fs::read(&config).unwrap();
    std::fs::write(&config, br#"{"format":1,"encrypted":false,"keyCheck":null,"createdAt":0}"#).unwrap();
    assert!(Repository::open(storage(), Some(key.clone()), 0).await.is_err(), "no plain text for the next backup");
    assert!(Repository::open_existing(storage(), Some(key.clone())).await.is_err(), "nor for a restore");
    std::fs::write(&config, &genuine_config).unwrap();

    let manifest = repo.manifest(&first.snapshot).await.unwrap();
    let object = |id: &str| repo_dir.join("data").join(&id[..2]).join(id);
    let id_of = |name: &str| manifest.files.iter().find(|file| file.path == name).unwrap().id.clone();
    let (a, b) = (object(&id_of("tls/a.pem")), object(&id_of("tls/b.pem")));
    let genuine_b = std::fs::read(&b).unwrap();
    let restoring = |name: &'static str| {
        let target = dir.path().join(name);
        let repo = &repo;
        let snapshot = first.snapshot.clone();
        async move { uwumail_backup::restore(repo, &snapshot, &target).await }
    };

    // A plain object where an encrypted one belongs.
    std::fs::write(&b, [1u8, 0, b'b', b'b', b'b', b'b']).unwrap();
    assert!(restoring("plain").await.is_err());
    // An authentic object, moved to another one's name.
    std::fs::copy(&a, &b).unwrap();
    assert!(restoring("moved").await.is_err());
    std::fs::write(&b, &genuine_b).unwrap();
    assert!(restoring("genuine").await.is_ok());

    // An authentic manifest, under another snapshot's name.
    let second = uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 1_086_400)
        .await
        .unwrap();
    let snapshots = repo_dir.join("snapshots");
    std::fs::copy(snapshots.join(&first.snapshot), snapshots.join(&second.snapshot)).unwrap();
    assert!(repo.manifest(&second.snapshot).await.is_err());
    assert_eq!(repo.manifest(&first.snapshot).await.unwrap().name.as_deref(), Some(first.snapshot.as_str()));
}
