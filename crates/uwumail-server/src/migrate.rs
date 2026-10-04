//! Moving from another provider: copying a person's old mailbox over IMAP, in the background.
//!
//! The person starts it in the portal with the old address and its password (an app password
//! where the provider wants one); the store keeps it as a job. This worker takes one queued job
//! at a time and copies with the same IMAP client as the migration import (`import::imap`): every
//! folder with its flags and dates, the special ones (sent, drafts, junk, trash, archive) into
//! ours. It works in time slices, so one big mailbox does not hold up everybody else's, and every
//! folder remembers the last message it took over, so a slice, a restart or "sync again" a week
//! later all go on from where things stood.
//!
//! A message the folder's mailbox here holds already -- the same Message-ID, or the same bytes --
//! is not brought twice. One the old provider shows in several folders (Gmail's labels) is one
//! email here too, in all of their mailboxes. What is left out (here already, too large, not
//! readable) is counted apart and listed, so the person sees which messages they are.
//!
//! Like fetched mailboxes, it only ever connects to public addresses, through the egress proxy
//! when the admin sends fetching that way. A full mailbox or a refused password pauses the job
//! with a reason the portal explains; nothing is retried behind the person's back.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::watch;
use uwumail_store::{DavKind, MigrationJob, MigrationProgress, MigrationRun, SkippedOf, Store, StoreError};

use crate::fetch::Detour;
use crate::import::imap::{Connection, CopyEnd, CopyEvent, CopyOptions, Source, copy_folders, quoted};

/// How often the queue is looked at when it was empty.
const TICK: Duration = Duration::from_secs(5);
/// How long one job is worked on before the next one gets its turn.
const RUN_LIMIT: Duration = Duration::from_secs(300);
/// On top of the time slice: the portion that was being fetched when it ran out may still finish.
const GRACE: Duration = Duration::from_secs(300);

/// Works through the queued moves until `shutdown` changes.
pub async fn run_migrations(
    store: Store,
    smtp: uwumail_smtp::Smtp,
    egress: uwumail_smtp::egress::Egress,
    mut shutdown: watch::Receiver<bool>,
) {
    // Moves that were running when the server stopped go on where their folders stood.
    match store.recover_interrupted_migrations().await {
        Ok((0, 0)) => {}
        Ok((count, paused)) => {
            tracing::info!(count, "moves from other providers continue after the restart");
            if paused > 0 {
                tracing::warn!(paused, "moves the server went down during again and again are paused");
            }
        }
        Err(err) => tracing::warn!(%err, "looking for interrupted moves failed"),
    }
    loop {
        loop {
            if *shutdown.borrow() {
                return;
            }
            let job = match store.take_migration_job().await {
                Ok(Some(job)) => job,
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(%err, "reading the queued moves failed");
                    break;
                }
            };
            let dialer = Some(egress.dialer(uwumail_smtp::egress::Purpose::Fetch));
            let run = tokio::select! {
                run = run_job(&store, &job, None, dialer, RUN_LIMIT, smtp.max_message_size()) => run,
                // Back in the queue; the next start goes on with it.
                _ = shutdown.changed() => MigrationRun::Continue,
            };
            if let Err(err) = store.finish_migration_run(job.id, run).await {
                tracing::warn!(address = %job.address, %err, "writing down how a move went failed");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = shutdown.changed() => return,
        }
    }
}

pub(crate) fn paused(code: &str, detail: impl std::fmt::Display) -> MigrationRun {
    MigrationRun::Paused { code: code.to_owned(), detail: detail.to_string() }
}

/// What `import_progress` remembers this old mailbox's folders under: its login at its host, so two
/// old mailboxes at the same provider keep their folders apart.
fn source_name(job: &MigrationJob) -> String {
    format!("move:{}@{}", job.login, job.host)
}

/// Works on one move for up to `limit` and says how it ended. Never fails: whatever went wrong
/// becomes a pause with a reason.
///
/// All of it, from looking up the host to logging out, has `limit` and the grace after it: before,
/// only the copying did, and a provider that took the connection and never spoke TLS, or answered
/// LOGIN with an untagged line now and then, held the worker, and so every other person's move,
/// for ever (security-audit-0.16.0 PLAT-4).
pub(crate) async fn run_job(
    store: &Store,
    job: &MigrationJob,
    detour: Option<Detour>,
    dialer: Option<uwumail_smtp::egress::Dialer>,
    limit: Duration,
    max_size: usize,
) -> MigrationRun {
    run_job_within(store, job, detour, dialer, limit, GRACE, max_size).await
}

/// [`run_job`] with another grace than [`GRACE`].
async fn run_job_within(
    store: &Store,
    job: &MigrationJob,
    detour: Option<Detour>,
    dialer: Option<uwumail_smtp::egress::Dialer>,
    limit: Duration,
    grace: Duration,
    max_size: usize,
) -> MigrationRun {
    let deadline = tokio::time::Instant::now() + limit + grace;
    let password = match store.migration_password(job.account_id, job.id).await {
        Ok(Some(password)) => password,
        Ok(None) => return MigrationRun::Continue,
        Err(err) => return paused("failed", err),
    };
    let source = OldMailbox { account_id: job.account_id, host: &job.host, port: job.port, login: &job.login };
    let mut connection =
        match tokio::time::timeout_at(deadline, connect(store, &source, password, detour, dialer)).await {
            Ok(Ok(connection)) => connection,
            Ok(Err(run)) => return run,
            Err(_) => return paused("unreachable", format!("{} did not answer in time", job.host)),
        };
    let id = job.id;
    let note = move |store: Store, progress: MigrationProgress| -> Pin<Box<dyn Future<Output = bool> + Send>> {
        // `false` from the store: the person paused the move or ended it meanwhile.
        Box::pin(async move { store.note_migration_progress(id, progress).await.unwrap_or(true) })
    };
    let options = CopyOptions { skip_known: true, max_size, ..CopyOptions::default() };
    let source_name = source_name(job);
    let skipped = SkippedOf::MigrationJob(job.id);
    let copy = copy(store, &mut connection, job.account_id, &source_name, job.progress, options, limit, note, skipped);
    let run = match tokio::time::timeout_at(deadline, copy).await {
        Ok(run) => run,
        // A portion that never ended; the next turn starts it again.
        Err(_) => MigrationRun::Continue,
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), connection.command("LOGOUT")).await;
    run
}

/// An old mailbox to copy from, and the account here it goes into.
pub(crate) struct OldMailbox<'a> {
    pub account_id: i64,
    pub host: &'a str,
    pub port: u16,
    pub login: &'a str,
}

/// Checks the host again, connects and logs in. What went wrong comes back as the pause to make.
/// Shared by a person's own move and the moves the admin runs (`crate::moves`).
pub(crate) async fn connect(
    store: &Store,
    old: &OldMailbox<'_>,
    password: String,
    detour: Option<Detour>,
    dialer: Option<uwumail_smtp::egress::Dialer>,
) -> Result<Connection, MigrationRun> {
    // The host was checked when the move was set up; its name is checked again now, so it cannot
    // have come to point at this machine or the local network since (as for fetched mailboxes,
    // security-audit-0.5.2 S-10). The connection itself is only ever made to a public address the
    // dialer found (Source::remote).
    if detour.is_none() && !crate::import::imap::resolves_publicly(old.host, old.port).await {
        return Err(paused("notPublic", format!("{} does not resolve to a public address", old.host)));
    }
    match store.account_by_id(old.account_id).await {
        Ok(Some(account)) if account.has_mailbox() => {}
        Ok(_) => return Err(paused("noMailbox", "this account has no mailbox to move into")),
        Err(err) => return Err(paused("failed", err)),
    }
    let source = match detour {
        // Through the proxy when the admin wants fetching to take it; straight otherwise.
        None => Source::remote(old.host, old.port, password, dialer),
        Some(detour) => Source {
            address: detour.address,
            tls_name: Some(detour.tls_name),
            roots: Some(detour.roots),
            master_user: None,
            password,
            dialer: None,
        },
    };
    let mut connection = match Connection::open(&source).await {
        Ok(connection) => connection,
        Err(err) => return Err(paused("unreachable", format!("{err:#}"))),
    };
    if let Err(err) = connection.command(&format!("LOGIN {} {}", quoted(old.login), quoted(&source.password))).await {
        return Err(paused("loginRefused", format!("{err:#}")));
    }
    Ok(connection)
}

/// Writes down how far a turn got; `false` stops it.
pub(crate) type Note = dyn Fn(Store, MigrationProgress) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync;

/// One turn of copying: what is new since the last one, for up to `limit`, counted on top of
/// `base` (the turns of this round before). `Done` when everything there was is here. Contacts
/// and calendars found in IMAP folders (with [`CopyOptions::contacts`]/`calendars`) go to `objects`.
/// The messages left out go on the list of `skipped`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy(
    store: &Store,
    connection: &mut Connection,
    account_id: i64,
    source_name: &str,
    base: MigrationProgress,
    options: CopyOptions,
    limit: Duration,
    note: impl Fn(Store, MigrationProgress) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync + 'static,
    skipped: SkippedOf,
) -> MigrationRun {
    copy_with(store, connection, account_id, source_name, base, options, limit, Box::new(note), None, skipped).await
}

/// What a turn does with contacts and calendar entries found in IMAP folders: `true` when they
/// were stored, else the messages they came in are copied as mail.
pub(crate) type Objects =
    dyn Fn(DavKind, String, Vec<String>) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy_with(
    store: &Store,
    connection: &mut Connection,
    account_id: i64,
    source_name: &str,
    base: MigrationProgress,
    options: CopyOptions,
    limit: Duration,
    note: Box<Note>,
    objects: Option<Box<Objects>>,
    skipped: SkippedOf,
) -> MigrationRun {
    let options = CopyOptions { deadline: Some(tokio::time::Instant::now() + limit), ..options };
    let note: std::sync::Arc<Note> = note.into();
    let objects: Option<std::sync::Arc<Objects>> = objects.map(Into::into);
    let mut report = {
        let store = store.clone();
        let current = std::sync::Arc::new(std::sync::Mutex::new(base));
        move |event: CopyEvent| -> Pin<Box<dyn Future<Output = bool> + Send>> {
            let mut now = current.lock().expect("move progress poisoned");
            match event {
                CopyEvent::Planned { folders, messages } => {
                    now.folders_done = 0;
                    now.folders_total = folders as i64;
                    now.messages_total = base.messages_done + messages as i64;
                }
                CopyEvent::Progress(copied) => {
                    now.folders_done = copied.folders as i64;
                    now.messages_done = base.messages_done + (copied.messages + copied.skipped()) as i64;
                    now.messages_skipped = base.messages_skipped + copied.skipped() as i64;
                    now.messages_known = base.messages_known + copied.known as i64;
                    now.messages_too_large = base.messages_too_large + copied.too_large as i64;
                    now.messages_unreadable = base.messages_unreadable + copied.unreadable as i64;
                    now.bytes_done = base.bytes_done + copied.bytes as i64;
                }
                CopyEvent::Skipped(messages) => {
                    let store = store.clone();
                    return Box::pin(async move {
                        if let Err(err) = store.note_skipped_messages(skipped, messages).await {
                            tracing::warn!(%err, "writing down the messages a move left out failed");
                        }
                        true
                    });
                }
                CopyEvent::Renumbered { folder } => {
                    tracing::info!(folder, "the old provider renumbered a folder; copying it again");
                    return Box::pin(std::future::ready(true));
                }
                CopyEvent::Folder { .. } => return Box::pin(std::future::ready(true)),
                CopyEvent::Objects { folder, kind, texts } => {
                    // Nobody to take them: they stay mail.
                    let Some(objects) = objects.clone() else { return Box::pin(std::future::ready(false)) };
                    return Box::pin(async move { objects(kind, folder, texts).await });
                }
            }
            let noted: MigrationProgress = *now;
            note(store.clone(), noted)
        }
    };
    let result = copy_folders(store, connection, account_id, source_name, options, &mut report).await;
    match result {
        Ok((copied, end)) => {
            tracing::info!(
                account_id,
                copied = copied.messages,
                known = copied.known,
                too_large = copied.too_large,
                unreadable = copied.unreadable,
                ?end,
                "moved mail from another provider"
            );
            match end {
                CopyEnd::Finished => MigrationRun::Done,
                CopyEnd::OutOfTime | CopyEnd::Stopped => MigrationRun::Continue,
            }
        }
        Err(err) if matches!(err.downcast_ref::<StoreError>(), Some(StoreError::QuotaExceeded)) => {
            paused("quotaExceeded", "the mailbox here is full")
        }
        Err(err) => {
            tracing::warn!(account_id, err = %format!("{err:#}"), "moving mail failed");
            paused("failed", format!("{err:#}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, MigrationState, NewAccount, NewMigrationJob, Role};

    use super::*;

    const PASSWORD: &str = "katzenpfote-123";

    async fn store_with_person(path: &std::path::Path, password: Option<&str>, quota_bytes: i64) -> (Store, i64) {
        let store = Store::open(path).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let new = NewAccount {
            address: "mini@example.org".into(),
            display_name: "Mini".into(),
            password: password.map(str::to_owned),
            role: Role::User,
            quota_bytes,
            protocols: None,
        };
        let id = store.create_account(new).await.unwrap().id;
        (store, id)
    }

    async fn deliver(store: &Store, account: i64, mailbox: MailboxTarget, subject: &str, keywords: &[&str]) {
        let raw = format!(
            "From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: {subject}\r\n\
             Message-ID: <{subject}@example.net>\r\n\r\nHallo\r\n"
        );
        let request = IngestRequest {
            account_id: account,
            raw: raw.into_bytes(),
            mailboxes: vec![mailbox],
            keywords: keywords.iter().map(|keyword| keyword.to_string()).collect(),
            received_at: Some(1_700_000_000),
        };
        store.ingest(request).await.unwrap();
    }

    /// The old provider: our own IMAP server on localhost, with a certificate it made itself.
    async fn old_provider(old: &Store) -> (Detour, watch::Sender<bool>) {
        let generated = rcgen::generate_simple_self_signed(vec!["imap.example.net".to_owned()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (shutdown, shutdown_rx) = watch::channel(false);
        tokio::spawn(uwumail_imap::Imap::new(old.clone(), 1 << 20).serve(listener, Arc::new(tls), shutdown_rx));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(generated.cert.der().clone()).unwrap();
        (Detour { address, tls_name: "imap.example.net".into(), roots }, shutdown)
    }

    async fn start(store: &Store, account_id: i64, password: &str) -> MigrationJob {
        let new = NewMigrationJob {
            account_id,
            address: "mini@example.net".into(),
            host: "imap.example.net".into(),
            port: 993,
            login: "mini@example.org".into(),
            password: password.into(),
        };
        store.create_migration_job(new).await.unwrap();
        store.take_migration_job().await.unwrap().expect("the move is queued")
    }

    /// One turn of the worker, as `run_migrations` takes it.
    async fn turn(store: &Store, detour: &Detour, limit: Duration) -> MigrationJob {
        let job = store.take_migration_job().await.unwrap().expect("the move is queued");
        let run = run_job(store, &job, Some(detour.clone()), None, limit, 0).await;
        store.finish_migration_run(job.id, run).await.unwrap();
        store.migration_job(job.account_id, job.id).await.unwrap().unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mailbox_moves_with_folders_and_flags_and_then_only_what_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let (old, old_id) = store_with_person(&dir.path().join("old"), Some(PASSWORD), 0).await;
        deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Inbox), "Eins", &["$seen", "$flagged"]).await;
        deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Sent), "Gesendet", &["$seen"]).await;
        let projects = old.create_mailbox(old_id, "Projekte", None, None, 0, true).await.unwrap();
        let archive = old.create_mailbox(old_id, "Alt", Some(projects), None, 0, true).await.unwrap();
        deliver(&old, old_id, MailboxTarget::Id(archive), "Übergabe", &["$answered"]).await;
        // The same message in a second folder, the way Gmail shows a message with two labels.
        deliver(&old, old_id, MailboxTarget::Id(projects), "Eins", &[]).await;
        let (detour, _stop) = old_provider(&old).await;
        let (new, new_id) = store_with_person(&dir.path().join("new"), None, 0).await;

        // A wrong password pauses the move and says why.
        let job = start(&new, new_id, "falsch-falsch").await;
        let run = run_job(&new, &job, Some(detour.clone()), None, RUN_LIMIT, 0).await;
        assert!(matches!(&run, MigrationRun::Paused { code, .. } if code == "loginRefused"), "{run:?}");
        new.finish_migration_run(job.id, run).await.unwrap();

        // With the right one it goes on.
        new.sync_migration_job(new_id, job.id, Some(PASSWORD.into())).await.unwrap();
        // A time slice that is already over looks at the folders and stops before the first portion.
        let sliced = turn(&new, &detour, Duration::ZERO).await;
        assert_eq!(sliced.state, MigrationState::Queued, "{sliced:?}");
        assert_eq!((sliced.progress.messages_total, sliced.progress.messages_done), (4, 0));
        let done = turn(&new, &detour, RUN_LIMIT).await;
        assert_eq!(done.state, MigrationState::Done, "{done:?}");
        // Eins in two folders is one email in two mailboxes: copied, nothing left out.
        assert_eq!((done.progress.messages_done, done.progress.messages_skipped), (4, 0), "{done:?}");
        assert_eq!(done.progress.folders_done, done.progress.folders_total);
        assert!(done.progress.bytes_done > 0);

        let mailboxes = new.mailboxes(new_id).await.unwrap();
        let inbox = mailboxes.iter().find(|mailbox| mailbox.role == Some(MailboxRole::Inbox)).unwrap();
        assert_eq!((inbox.total_emails, inbox.unread_emails), (1, 0), "flags come along");
        let emails = new.emails_in_mailbox(inbox.id, 10).await.unwrap();
        assert!(emails[0].keywords.contains(&"$flagged".to_owned()), "{:?}", emails[0].keywords);
        let sent = mailboxes.iter().find(|mailbox| mailbox.role == Some(MailboxRole::Sent)).unwrap();
        assert_eq!(sent.total_emails, 1);
        let parent = mailboxes.iter().find(|mailbox| mailbox.name == "Projekte").unwrap();
        let child = mailboxes.iter().find(|mailbox| mailbox.name == "Alt").unwrap();
        assert_eq!((child.parent_id, child.total_emails), (Some(parent.id), 1));
        assert_eq!(parent.total_emails, 1, "the second label of Eins comes along");
        let labelled = new.emails_in_mailbox(parent.id, 10).await.unwrap();
        assert_eq!(labelled[0].id, emails[0].id, "as the same email, not a second copy");
        assert!(new.skipped_messages(SkippedOf::MigrationJob(job.id)).await.unwrap().is_empty());

        // A week later: "sync again" brings only what arrived since.
        deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Inbox), "Zwei", &[]).await;
        new.sync_migration_job(new_id, job.id, None).await.unwrap();
        let again = turn(&new, &detour, RUN_LIMIT).await;
        assert_eq!(again.state, MigrationState::Done);
        assert_eq!((again.progress.messages_total, again.progress.messages_done), (1, 1), "{again:?}");
        let inbox = new.mailboxes(new_id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
        assert_eq!(inbox.unwrap().total_emails, 2);

        // Done for good: the password goes with the job, the mail stays.
        new.delete_migration_job(new_id, job.id).await.unwrap();
        assert_eq!(new.migration_password(new_id, job.id).await.unwrap(), None);
        assert_eq!(new.mailboxes(new_id).await.unwrap().iter().map(|m| m.total_emails).sum::<i64>(), 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_mailbox_pauses_the_move() {
        let dir = tempfile::tempdir().unwrap();
        let (old, old_id) = store_with_person(&dir.path().join("old"), Some(PASSWORD), 0).await;
        for subject in ["Eins", "Zwei", "Drei"] {
            deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Inbox), subject, &[]).await;
        }
        let (detour, _stop) = old_provider(&old).await;
        let (new, new_id) = store_with_person(&dir.path().join("new"), None, 1).await;
        let job = start(&new, new_id, PASSWORD).await;
        let run = run_job(&new, &job, Some(detour), None, RUN_LIMIT, 0).await;
        assert!(matches!(&run, MigrationRun::Paused { code, .. } if code == "quotaExceeded"), "{run:?}");
    }

    /// security-audit-0.16.0 PLAT-4: a provider that takes the connection and never speaks TLS,
    /// or answers LOGIN with an untagged line now and then, no longer holds the worker for ever.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_provider_that_stalls_does_not_hold_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let (new, new_id) = store_with_person(&dir.path().join("new"), None, 0).await;
        let job = start(&new, new_id, PASSWORD).await;
        let limit = Duration::from_millis(300);

        // Silent after the TCP handshake.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = silent.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = silent.accept().await {
                held.push(socket);
            }
        });
        let detour = Detour { address, tls_name: "imap.example.net".into(), roots: rustls::RootCertStore::empty() };
        let run = tokio::time::timeout(
            Duration::from_secs(10),
            run_job_within(&new, &job, Some(detour), None, limit, limit, 0),
        )
        .await
        .expect("the turn ended in time");
        assert!(matches!(&run, MigrationRun::Paused { code, .. } if code == "unreachable"), "{run:?}");

        // Greets over TLS, then answers LOGIN with "* OK" every few milliseconds, never with its tag.
        let generated = rcgen::generate_simple_self_signed(vec!["imap.example.net".to_owned()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .unwrap();
        let chatty = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = chatty.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
            while let Ok((socket, _)) = chatty.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(socket).await else { return };
                    let _ = stream.write_all(b"* OK hello\r\n").await;
                    loop {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        if stream.write_all(b"* OK still thinking\r\n").await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        let mut roots = rustls::RootCertStore::empty();
        roots.add(generated.cert.der().clone()).unwrap();
        let detour = Detour { address, tls_name: "imap.example.net".into(), roots };
        let run = tokio::time::timeout(
            Duration::from_secs(10),
            run_job_within(&new, &job, Some(detour), None, limit, limit, 0),
        )
        .await
        .expect("the turn ended in time");
        assert!(matches!(&run, MigrationRun::Paused { code, .. } if code == "unreachable"), "{run:?}");
    }
}
