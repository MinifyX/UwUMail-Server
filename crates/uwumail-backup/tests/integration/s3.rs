//! Backups into an S3 bucket, against a small stand-in for S3 that runs inside the test: it keeps
//! objects in memory, pages its listings, checks every signature the way S3 does, and is busy once.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use uwumail_backup::s3::{Credentials, authorization};
use uwumail_backup::{Error, RepoKey, Repository, Retention, S3Target, Storage, Target};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

const ACCESS_KEY: &str = "AKIDEXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const BUCKET: &str = "uwumail-backups";
/// Small pages, so listings have to follow continuation tokens.
const PAGE: usize = 3;

#[derive(Default)]
struct Bucket {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    requests: AtomicUsize,
    /// Answers 503 to this many PUTs first, like S3 asking to slow down.
    busy: AtomicUsize,
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn answer(status: StatusCode, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder().status(status).body(Full::new(body.into())).unwrap()
}

fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    answer(status, format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>{code}</Message></Error>"))
}

async fn handle(bucket: Arc<Bucket>, request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    bucket.requests.fetch_add(1, Ordering::SeqCst);
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
    let (host, date, payload, given) =
        (header("host"), header("x-amz-date"), header("x-amz-content-sha256"), header("authorization"));
    let body = request.into_body().collect().await.unwrap().to_bytes();

    // What S3 checks: the payload is the one signed for, and the signature is right.
    use sha2::Digest;
    if hex::encode(sha2::Sha256::digest(&body)) != payload {
        return Ok(error(StatusCode::BAD_REQUEST, "XAmzContentSHA256Mismatch"));
    }
    let credentials = Credentials { access_key: ACCESS_KEY, secret_key: SECRET_KEY, region: "eu-central-1" };
    let signed = [("host", host.as_str()), ("x-amz-content-sha256", payload.as_str()), ("x-amz-date", date.as_str())];
    if date.len() != 16 || authorization(&credentials, &method, &path, &query, &signed, &payload, &date) != given {
        return Ok(error(StatusCode::FORBIDDEN, "SignatureDoesNotMatch"));
    }

    let Some(rest) = path.strip_prefix(&format!("/{BUCKET}")) else {
        return Ok(error(StatusCode::NOT_FOUND, "NoSuchBucket"));
    };
    let key = decode(rest.trim_start_matches('/'));
    let mut objects = bucket.objects.lock().unwrap();
    Ok(match (method.as_str(), key.is_empty()) {
        ("PUT", false) => {
            if bucket.busy.load(Ordering::SeqCst) > 0 {
                bucket.busy.fetch_sub(1, Ordering::SeqCst);
                return Ok(error(StatusCode::SERVICE_UNAVAILABLE, "SlowDown"));
            }
            objects.insert(key, body.to_vec());
            answer(StatusCode::OK, "")
        }
        ("GET", false) => match objects.get(&key) {
            Some(content) => answer(StatusCode::OK, content.clone()),
            None => error(StatusCode::NOT_FOUND, "NoSuchKey"),
        },
        ("DELETE", false) => {
            objects.remove(&key);
            answer(StatusCode::NO_CONTENT, "")
        }
        ("GET", true) => {
            let params: BTreeMap<String, String> = query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(name, value)| (decode(name), decode(value)))
                .collect();
            assert_eq!(params.get("list-type").map(String::as_str), Some("2"));
            assert_eq!(params.get("delimiter").map(String::as_str), Some("/"));
            let prefix = params.get("prefix").cloned().unwrap_or_default();
            let after = params.get("continuation-token").cloned().unwrap_or_default();
            // Every name directly under the prefix: objects, and the folders below it.
            let mut entries: Vec<(String, bool)> = Vec::new();
            for name in objects.keys().filter_map(|key| key.strip_prefix(&prefix)) {
                match name.split_once('/') {
                    Some((folder, _)) => {
                        let common = format!("{prefix}{folder}/");
                        if !entries.contains(&(common.clone(), true)) {
                            entries.push((common, true));
                        }
                    }
                    None => entries.push((format!("{prefix}{name}"), false)),
                }
            }
            entries.sort();
            entries.retain(|(name, _)| name.as_str() > after.as_str());
            let truncated = entries.len() > PAGE;
            entries.truncate(PAGE);
            let mut xml = format!("<ListBucketResult><Name>{BUCKET}</Name><IsTruncated>{truncated}</IsTruncated>");
            for (name, common) in &entries {
                if *common {
                    xml.push_str(&format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", escape(name)));
                } else {
                    xml.push_str(&format!("<Contents><Key>{}</Key><Size>1</Size></Contents>", escape(name)));
                }
            }
            if truncated {
                let last = &entries.last().unwrap().0;
                xml.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", escape(last)));
            }
            xml.push_str("</ListBucketResult>");
            answer(StatusCode::OK, xml)
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed"),
    })
}

async fn fake_s3() -> (SocketAddr, Arc<Bucket>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let bucket = Arc::new(Bucket::default());
    let serving = bucket.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let bucket = serving.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request| handle(bucket.clone(), request));
                let _ =
                    hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });
    (address, bucket)
}

fn target(address: SocketAddr, secret_key: &str) -> S3Target {
    S3Target {
        endpoint: format!("http://{address}"),
        region: "eu-central-1".into(),
        bucket: BUCKET.into(),
        prefix: "server one/uwumail".into(),
        access_key: ACCESS_KEY.into(),
        secret_key: secret_key.into(),
        path_style: true,
    }
}

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
    for subject in ["Eins", "Zwei", "Drei", "Vier"] {
        let raw = format!("From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: {subject}\r\n\r\nHallo Mini\r\n");
        let request = IngestRequest {
            account_id: id,
            raw: raw.into_bytes(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        };
        store.ingest(request).await.unwrap();
    }
    (store, id)
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_and_restore_through_s3() {
    let dir = tempfile::tempdir().unwrap();
    let (address, bucket) = fake_s3().await;
    let (store, _mini) = server(&dir.path().join("data")).await;
    bucket.busy.store(1, Ordering::SeqCst);

    let s3 = Target::S3(target(address, SECRET_KEY));
    let key = RepoKey::generate();
    let repo = Repository::open(Storage::open(&s3).await.unwrap(), Some(key.clone()), 1).await.unwrap();
    repo.storage.check_writable().await.unwrap();
    let first = uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 1_000_000)
        .await
        .unwrap();
    assert!(first.uploaded > 0);
    assert_eq!(bucket.busy.load(Ordering::SeqCst), 0, "the busy answer was waited out");
    let keys: Vec<String> = bucket.objects.lock().unwrap().keys().cloned().collect();
    assert!(keys.iter().all(|key| key.starts_with("server one/uwumail/")), "{keys:?}");
    assert!(keys.iter().any(|key| key.starts_with("server one/uwumail/data/")), "{keys:?}");
    assert!(!keys.iter().any(|key| key.contains("uwumail-write-test")), "the test file is cleaned up");

    // A second backup finds what is there through paged listings and uploads only the new snapshot.
    let second = uwumail_backup::backup(&store, &repo, "mail.example.org", "0.1.0", Retention::default(), 1_086_400)
        .await
        .unwrap();
    assert!(second.uploaded < first.uploaded / 2, "{} after {}", second.uploaded, first.uploaded);
    assert_eq!(repo.snapshots().await.unwrap(), vec![first.snapshot.clone(), second.snapshot.clone()]);
    assert!(uwumail_backup::check(&repo, &second.snapshot).await.unwrap().is_empty());

    // Pruning deletes objects no snapshot needs any more.
    let none = Retention { daily: 0, weekly: 0, monthly: 0 };
    let (snapshots, _) = uwumail_backup::prune(&repo, none).await.unwrap();
    assert_eq!(snapshots, 1);

    let restored = dir.path().join("restored");
    uwumail_backup::restore(&repo, &second.snapshot, &restored).await.unwrap();
    let store = Store::open(&restored).await.unwrap();
    let account = store.authenticate("mini@example.org", "katzenpfote-123").await.unwrap().unwrap();
    let inbox = store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
    assert_eq!(inbox.unwrap().total_emails, 4);

    // The wrong secret is a refused login, not a damaged backup.
    let wrong = Storage::open(&Target::S3(target(address, "falsch"))).await.unwrap();
    assert!(matches!(Repository::open_existing(wrong, Some(key)).await, Err(Error::LoginRefused(_))));
    // A bucket that is not there says so.
    let missing = S3Target { bucket: "somebody-else".into(), ..target(address, SECRET_KEY) };
    let storage = Storage::open(&Target::S3(missing)).await.unwrap();
    let Err(Error::Storage(message)) = storage.list("snapshots").await else { panic!("a missing bucket lists") };
    assert!(message.contains("no bucket somebody-else"), "{message}");
}

/// The service with a folder as its target: set up, back up, list, restore -- the way the portal does.
#[tokio::test(flavor = "multi_thread")]
async fn backups_into_a_mounted_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _mini) = server(&dir.path().join("data")).await;
    let backups = uwumail_backup::Backups::new(store.clone(), "mail.example.org", "0.1.0");

    let missing = Target::Folder(uwumail_backup::FolderTarget { path: dir.path().join("nas").display().to_string() });
    let Err(Error::Config(message)) = Storage::open(&missing).await else { panic!("a folder that is not there") };
    assert!(message.contains("mounted"), "{message}");
    let relative = Target::Folder(uwumail_backup::FolderTarget { path: "backup".into() });
    assert!(matches!(Storage::open(&relative).await, Err(Error::Config(_))));

    std::fs::create_dir(dir.path().join("nas")).unwrap();
    let settings = uwumail_backup::BackupSettings {
        enabled: true,
        target: Some(missing.clone()),
        key: Some(RepoKey::generate().recovery_text()),
        ..Default::default()
    };
    backups.save_settings(&settings).await.unwrap();
    let report = backups.run_now().await.unwrap();
    assert_eq!(backups.snapshots().await.unwrap()[0].0, report.snapshot);
    assert!(dir.path().join("nas/uwumail-backup.json").exists());

    let repo = backups.repository().await.unwrap();
    uwumail_backup::restore(&repo, &report.snapshot, &dir.path().join("restored")).await.unwrap();
    let restored = Store::open(dir.path().join("restored")).await.unwrap();
    assert!(restored.account("mini@example.org").await.unwrap().is_some());
}
