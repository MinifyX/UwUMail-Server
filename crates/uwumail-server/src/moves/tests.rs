//! The admin's moves against an old server made of this program: its IMAP server on localhost,
//! and its CalDAV/CardDAV served in the same process as "the internet".

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::body::Bytes;
use axum::http::Request;
use tower::ServiceExt;
use uwumail_dav::client::{Answer, BoxFuture, RemoteError, Transport};
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::ClientInfo;
use uwumail_store::{
    DavImportMode, DavKind, IngestRequest, MailboxRole, MailboxTarget, MoveKind, MoveMailboxState, MoveState,
    NewAccount, NewDavCollection, NewMove, NewMoveMailbox, Role, Store,
};

use super::*;

const PASSWORD: &str = "katzenpfote-123";

/// "The internet": requests go to a router in this process, whatever host they name.
struct Internet(Router);

impl Transport for Internet {
    fn send(&self, request: Request<Bytes>, max_bytes: usize) -> BoxFuture<'_, Result<Answer, RemoteError>> {
        Box::pin(async move {
            let (mut parts, body) = request.into_parts();
            parts.uri = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").parse().unwrap();
            let mut request = Request::from_parts(parts, Body::from(body));
            request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
            let response = self.0.clone().oneshot(request).await.unwrap();
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body =
                axum::body::to_bytes(response.into_body(), max_bytes).await.map_err(|_| RemoteError::TooLarge)?;
            Ok(Answer { status, headers, body })
        })
    }
}

async fn person(store: &Store, address: &str, password: Option<&str>, quota_bytes: i64) -> i64 {
    store.create_domain("example.org").await.ok();
    let new = NewAccount {
        address: address.into(),
        display_name: String::new(),
        password: password.map(str::to_owned),
        role: Role::User,
        quota_bytes,
        protocols: None,
    };
    store.create_account(new).await.unwrap().id
}

async fn deliver(store: &Store, account: i64, mailbox: MailboxTarget, subject: &str, keywords: &[&str]) {
    let raw = format!(
        "From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@example.net>\r\n\r\nHallo\r\n"
    );
    deliver_raw(store, account, mailbox, raw.into_bytes(), keywords).await;
}

async fn deliver_raw(store: &Store, account: i64, mailbox: MailboxTarget, raw: Vec<u8>, keywords: &[&str]) {
    let request = IngestRequest {
        account_id: account,
        raw,
        mailboxes: vec![mailbox],
        keywords: keywords.iter().map(|keyword| keyword.to_string()).collect(),
        received_at: Some(1_700_000_000),
    };
    store.ingest(request).await.unwrap();
}

/// The old server's IMAP: our own on localhost, with a certificate it made itself.
async fn old_imap(old: &Store) -> (Detour, watch::Sender<bool>) {
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

fn env(old: &Store, detour: Detour) -> Env {
    let dav =
        Dav::new(old.clone(), DavSettings { calendar_name: "Privat".into(), addressbook_name: "Adressen".into() });
    Env {
        transport: Arc::new(Internet(dav.router())),
        dns: None,
        dialer: None,
        detour: Some(detour),
        names: ("Kalender".into(), "Kontakte".into()),
        limit: Duration::from_secs(60),
        grace: Duration::from_secs(60),
    }
}

fn new_move(kind: MoveKind) -> NewMove {
    NewMove {
        kind,
        domain: "example.org".into(),
        imap_host: "imap.example.net".into(),
        imap_port: 993,
        dav_mode: DavMode::Auto,
        dav_host: String::new(),
        dav_url: String::new(),
        contacts: true,
        calendars: true,
        parallel: 2,
        sync_minutes: 60,
        created_by: None,
    }
}

fn mailbox(account_id: i64, address: &str, password: &str) -> NewMoveMailbox {
    NewMoveMailbox {
        account_id,
        old_address: address.into(),
        login: address.into(),
        password: password.into(),
        imap_host: None,
        imap_port: None,
        dav_url: String::new(),
        created_account: true,
    }
}

/// One turn, as the worker takes it.
async fn take_turn(store: &Store, env: &Env) -> MoveMailbox {
    let mailbox = store.take_move_mailbox().await.unwrap().expect("a mailbox is queued");
    turn(store, env, mailbox.clone()).await;
    store.move_mailbox(None, mailbox.id).await.unwrap().unwrap()
}

fn total(store_mailboxes: &[uwumail_store::Mailbox]) -> i64 {
    store_mailboxes.iter().map(|mailbox| mailbox.total_emails).sum()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_domain_moves_with_mail_contacts_and_calendars_and_keeps_up_until_finished() {
    let dir = tempfile::tempdir().unwrap();
    // The old server: Mini with mail in several folders, a calendar, an address book, a quota.
    let old = Store::open(&dir.path().join("old")).await.unwrap();
    let old_mini = person(&old, "mini@example.org", Some(PASSWORD), 50 * 1024 * 1024).await;
    deliver(&old, old_mini, MailboxTarget::Role(MailboxRole::Inbox), "Eins", &["$seen", "$flagged"]).await;
    deliver(&old, old_mini, MailboxTarget::Role(MailboxRole::Sent), "Gesendet", &["$seen"]).await;
    let projects = old.create_mailbox(old_mini, "Projekte", None, None, 0, true).await.unwrap();
    deliver(&old, old_mini, MailboxTarget::Id(projects), "Plan", &[]).await;
    let calendar =
        old.dav_collections(old_mini, DavKind::Calendar, NewDavCollection::default_calendar("Privat")).await.unwrap();
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:fest@example.org\r\n\
DTSTAMP:20260101T000000Z\r\nDTSTART:20261003T100000Z\r\nSUMMARY:Fest\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let split = uwumail_store::split_ics(ics, false);
    old.dav_import(old_mini, calendar[0].id, split.objects, DavImportMode::Merge).await.unwrap();
    let book = old
        .dav_collections(old_mini, DavKind::Addressbook, NewDavCollection::default_address_book("Adressen"))
        .await
        .unwrap();
    let cards = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:nyu@example.org\r\nFN:Nyu\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nUID:leni@example.org\r\nFN:Leni\r\nEND:VCARD\r\n";
    old.dav_import(old_mini, book[0].id, uwumail_store::split_vcf(cards).objects, DavImportMode::Merge).await.unwrap();
    // A Kolab-style folder of contacts kept as messages.
    let kolab = old.create_mailbox(old_mini, "Contacts", None, None, 0, true).await.unwrap();
    let card_mail = "From: mini@example.org\r\nTo: mini@example.org\r\nSubject: kolab\r\nMessage-ID: <k1@example.org>\r\n\
MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
This is a contact.\r\n--b\r\nContent-Type: text/vcard\r\n\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nUID:kolab-1\r\n\
FN:Kolab Kim\r\nEND:VCARD\r\n--b--\r\n";
    deliver_raw(&old, old_mini, MailboxTarget::Id(kolab), card_mail.as_bytes().to_vec(), &[]).await;
    // Nyu's password at the old server is not the one the admin was given.
    let old_nyu = person(&old, "nyu@example.org", Some(PASSWORD), 0).await;
    deliver(&old, old_nyu, MailboxTarget::Role(MailboxRole::Inbox), "Hallo-Nyu", &[]).await;
    let (detour, _stop) = old_imap(&old).await;
    let env = env(&old, detour);

    // This server: the same domain, Mini's mailbox already holds one of the messages.
    let new = Store::open(&dir.path().join("new")).await.unwrap();
    let mini = person(&new, "mini@example.org", None, 0).await;
    let nyu = person(&new, "nyu@example.org", None, 0).await;
    deliver(&new, mini, MailboxTarget::Role(MailboxRole::Inbox), "Eins", &[]).await;
    let created = new
        .create_move(
            new_move(MoveKind::Domain),
            vec![mailbox(mini, "mini@example.org", PASSWORD), mailbox(nyu, "nyu@example.org", "falsch")],
        )
        .await
        .unwrap();

    // Both run at once (the move allows two); Nyu's refused password pauses only Nyu.
    let first = new.take_move_mailbox().await.unwrap().unwrap();
    let second = new.take_move_mailbox().await.unwrap().unwrap();
    tokio::join!(turn(&new, &env, first.clone()), turn(&new, &env, second.clone()));
    let boxes = new.move_mailboxes(created.id).await.unwrap();
    let (m, n) = (&boxes[0], &boxes[1]);
    assert_eq!(m.address, "mini@example.org");
    assert_eq!(m.state, MoveMailboxState::Synced, "{m:?}");
    assert_eq!((n.state, n.error.as_str()), (MoveMailboxState::Paused, "loginRefused"), "{n:?}");
    // Eins was here already; Gesendet, Plan come, the Kolab card becomes a contact.
    assert_eq!((m.progress.messages_skipped, m.rounds), (1, 1), "{m:?}");
    assert!(m.source_bytes.is_some(), "the old server's quota says how big it is");
    assert_eq!((m.contacts_done, m.events_done, m.dav_error.as_str()), (3, 1, ""), "{m:?}");
    let here = new.mailboxes(mini).await.unwrap();
    assert_eq!(total(&here), 3, "Eins once, Gesendet, Plan");
    let sent = here.iter().find(|mailbox| mailbox.role == Some(MailboxRole::Sent)).unwrap();
    assert_eq!(sent.total_emails, 1);
    assert!(here.iter().any(|mailbox| mailbox.name == "Projekte"));
    assert!(!here.iter().any(|mailbox| mailbox.name == "Contacts"), "the contacts folder is not mail");
    let books = new
        .dav_collections(mini, DavKind::Addressbook, NewDavCollection::default_address_book("Kontakte"))
        .await
        .unwrap();
    assert!(books.iter().any(|book| book.display_name == "Adressen"), "{books:?}");
    assert!(books.iter().any(|book| book.display_name == "Contacts"), "{books:?}");

    // Delta: new mail at the old server comes with the next round, nothing twice.
    deliver(&old, old_mini, MailboxTarget::Role(MailboxRole::Inbox), "Zwei", &[]).await;
    new.retry_move_mailbox(created.id, m.id, None, None).await.unwrap();
    let again = take_turn(&new, &env).await;
    assert_eq!((again.state, again.rounds), (MoveMailboxState::Synced, 2), "{again:?}");
    assert_eq!(total(&new.mailboxes(mini).await.unwrap()), 4);
    assert_eq!(again.contacts_done, 3, "contacts come in the first and the last round only");

    // The admin finishes after the MX switch: a last round, then the password goes. Nyu, still
    // paused, is queued for the last round too and gets the right password now.
    deliver(&old, old_mini, MailboxTarget::Role(MailboxRole::Inbox), "Drei", &[]).await;
    let finishing = new.finish_move(created.id, false).await.unwrap();
    assert_eq!(finishing.state, MoveState::Finishing);
    let one = take_turn(&new, &env).await;
    let other = take_turn(&new, &env).await;
    let (last, nyu_last) = if one.id == m.id { (one, other) } else { (other, one) };
    assert_eq!((last.state, last.has_password), (MoveMailboxState::Done, false), "{last:?}");
    assert_eq!(new.move_mailbox_password(m.id).await.unwrap(), None);
    assert_eq!(total(&new.mailboxes(mini).await.unwrap()), 5);
    assert_eq!(nyu_last.state, MoveMailboxState::Paused, "still the wrong password");
    new.retry_move_mailbox(created.id, n.id, None, Some(PASSWORD.into())).await.unwrap();
    let nyu_done = take_turn(&new, &env).await;
    assert_eq!(nyu_done.state, MoveMailboxState::Done, "{nyu_done:?}");
    assert_eq!(total(&new.mailboxes(nyu).await.unwrap()), 1);
    let done = new.move_by_id(created.id).await.unwrap().unwrap();
    assert_eq!((done.state, done.summary.done), (MoveState::Done, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_cut_short_goes_on_where_it_stood() {
    let dir = tempfile::tempdir().unwrap();
    let old = Store::open(&dir.path().join("old")).await.unwrap();
    let old_mini = person(&old, "mini@example.org", Some(PASSWORD), 0).await;
    for subject in ["Eins", "Zwei", "Drei"] {
        deliver(&old, old_mini, MailboxTarget::Role(MailboxRole::Inbox), subject, &[]).await;
    }
    let (detour, _stop) = old_imap(&old).await;
    let mut env = env(&old, detour);
    let new = Store::open(&dir.path().join("new")).await.unwrap();
    let mini = person(&new, "mini@example.org", None, 0).await;
    let single = NewMove { contacts: false, calendars: false, ..new_move(MoveKind::Mailbox) };
    let created = new.create_move(single, vec![mailbox(mini, "mini@example.org", PASSWORD)]).await.unwrap();

    // A time slice that is over looks at the folders and stops before the first portion.
    env.limit = Duration::ZERO;
    let sliced = take_turn(&new, &env).await;
    assert_eq!(sliced.state, MoveMailboxState::Queued);
    assert_eq!((sliced.progress.messages_total, sliced.progress.messages_done), (3, 0));

    // The server stops while it runs: the next start queues it again.
    env.limit = Duration::from_secs(60);
    new.take_move_mailbox().await.unwrap().unwrap();
    assert!(new.take_move_mailbox().await.unwrap().is_none());
    assert_eq!(new.requeue_running_moves().await.unwrap(), 1);
    let done = take_turn(&new, &env).await;
    assert_eq!(done.state, MoveMailboxState::Synced, "{done:?}");
    assert_eq!(done.progress.messages_done, 3);
    assert_eq!(total(&new.mailboxes(mini).await.unwrap()), 3);

    // A pause by the admin keeps it out of the queue until it goes on.
    new.pause_move(created.id).await.unwrap();
    new.retry_move_mailbox(created.id, done.id, None, None).await.unwrap();
    assert!(new.take_move_mailbox().await.unwrap().is_none());
    new.resume_move(created.id).await.unwrap();
    let again = take_turn(&new, &env).await;
    assert_eq!((again.state, again.progress.messages_done), (MoveMailboxState::Synced, 3));
    assert_eq!(total(&new.mailboxes(mini).await.unwrap()), 3, "nothing twice");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_mailbox_pauses_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let old = Store::open(&dir.path().join("old")).await.unwrap();
    let old_mini = person(&old, "mini@example.org", Some(PASSWORD), 0).await;
    deliver(&old, old_mini, MailboxTarget::Role(MailboxRole::Inbox), "Eins", &[]).await;
    let (detour, _stop) = old_imap(&old).await;
    let env = env(&old, detour);
    let new = Store::open(&dir.path().join("new")).await.unwrap();
    let mini = person(&new, "mini@example.org", None, 1).await;
    new.create_move(new_move(MoveKind::Mailbox), vec![mailbox(mini, "mini@example.org", PASSWORD)]).await.unwrap();
    let paused = take_turn(&new, &env).await;
    assert_eq!((paused.state, paused.error.as_str()), (MoveMailboxState::Paused, "quotaExceeded"));
}

#[test]
fn dav_servers_follow_the_preset() {
    let mut found = Move {
        id: 1,
        kind: MoveKind::Domain,
        domain: "example.org".into(),
        imap_host: "mail.example.org".into(),
        imap_port: 993,
        dav_mode: DavMode::Sogo,
        dav_host: String::new(),
        dav_url: String::new(),
        contacts: true,
        calendars: true,
        parallel: 2,
        sync_minutes: 60,
        state: MoveState::Active,
        created_at: 0,
        finish_requested_at: None,
        finished_at: None,
        summary: Default::default(),
    };
    let mailbox = |dav_url: &str| MoveMailbox {
        id: 1,
        move_id: 1,
        account_id: 1,
        address: "mini@example.org".into(),
        display_name: String::new(),
        quota_bytes: 0,
        used_bytes: 0,
        old_address: "mini@example.org".into(),
        login: "mini@example.org".into(),
        imap_host: String::new(),
        imap_port: 0,
        dav_url: dav_url.into(),
        created_account: true,
        has_password: true,
        state: MoveMailboxState::Queued,
        final_round: false,
        error: String::new(),
        error_detail: String::new(),
        progress: Default::default(),
        source_bytes: None,
        contacts_done: 0,
        events_done: 0,
        dav_error: String::new(),
        dav_found: String::new(),
        rounds: 0,
        created_at: 0,
        last_run_at: None,
        last_synced_at: None,
        next_sync_at: None,
        finished_at: None,
    };
    let host = "mail.example.org";
    assert_eq!(
        servers(&found, &mailbox(""), host, DavKind::Calendar),
        Some(Some("https://mail.example.org/SOGo/dav/".into()))
    );
    found.dav_mode = DavMode::Nextcloud;
    found.dav_host = "cloud.example.org".into();
    assert_eq!(
        servers(&found, &mailbox(""), host, DavKind::Addressbook),
        Some(Some("https://cloud.example.org/remote.php/dav/".into()))
    );
    found.dav_mode = DavMode::Icloud;
    assert_eq!(
        servers(&found, &mailbox(""), host, DavKind::Addressbook),
        Some(Some("https://contacts.icloud.com/".into()))
    );
    found.dav_mode = DavMode::None;
    assert_eq!(servers(&found, &mailbox(""), host, DavKind::Calendar), None);
    assert_eq!(
        servers(&found, &mailbox("https://dav.example.net/"), host, DavKind::Calendar),
        Some(Some("https://dav.example.net/".into())),
        "a mailbox's own address wins"
    );
    found.dav_mode = DavMode::Auto;
    assert_eq!(servers(&found, &mailbox(""), host, DavKind::Calendar), Some(None));
}

/// A message in a folder named like a calendar or address book, as a raw RFC 5322 text.
fn object_mail(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: mini@example.org\r\nTo: mini@example.org\r\nSubject: {subject}\r\nMessage-ID: <{subject}@example.org>\r\n\
MIME-Version: 1.0\r\n{body}"
    )
    .into_bytes()
}

const PURE_EVENT: &str = "Content-Type: text/calendar; charset=utf-8\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
PRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:kolab-ev@example.org\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART:20261003T100000Z\r\nSUMMARY:Kolab\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const PURE_CARD: &str = "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
This is a Kolab Groupware object.\r\n--b\r\nContent-Type: text/vcard\r\n\r\nBEGIN:VCARD\r\nVERSION:3.0\r\n\
UID:kolab-2\r\nFN:Kolab Kai\r\nEND:VCARD\r\n--b--\r\n";

/// An invitation someone filed into "Kalender": a letter with an event attached.
const INVITATION: &str = "Content-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\n\
Content-Type: multipart/alternative; boundary=a\r\n\r\n--a\r\nContent-Type: text/plain\r\n\r\nHallo Mini, \
anbei die Einladung zum Fest.\r\n--a\r\nContent-Type: text/html\r\n\r\n<p>Hallo Mini</p>\r\n--a--\r\n--m\r\n\
Content-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n\
METHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:fest-2@example.net\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART:20261003T100000Z\r\nSUMMARY:Fest\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--m--\r\n";

/// A letter with a business card attached, filed into "Contacts".
const LETTER_WITH_CARD: &str = "Content-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/html\r\n\r\n\
<p>Meine neue Adresse</p>\r\n--m\r\nContent-Type: text/vcard\r\nContent-Disposition: attachment; filename=nyu.vcf\r\n\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nUID:nyu-card\r\nFN:Nyu\r\nEND:VCARD\r\n--m--\r\n";

fn emails_in(here: &[uwumail_store::Mailbox], name: &str) -> i64 {
    here.iter().find(|mailbox| mailbox.name == name).map_or(0, |mailbox| mailbox.total_emails)
}

#[tokio::test(flavor = "multi_thread")]
async fn only_pure_objects_of_kinds_asked_for_leave_the_mail() {
    let dir = tempfile::tempdir().unwrap();
    let old = Store::open(&dir.path().join("old")).await.unwrap();
    let old_mini = person(&old, "mini@example.org", Some(PASSWORD), 0).await;
    let calendar = old.create_mailbox(old_mini, "Kalender", None, None, 0, true).await.unwrap();
    for (subject, body) in [("event", PURE_EVENT), ("invitation", INVITATION)] {
        deliver_raw(&old, old_mini, MailboxTarget::Id(calendar), object_mail(subject, body), &[]).await;
    }
    let contacts = old.create_mailbox(old_mini, "Contacts", None, None, 0, true).await.unwrap();
    for (subject, body) in [("card", PURE_CARD), ("letter", LETTER_WITH_CARD)] {
        deliver_raw(&old, old_mini, MailboxTarget::Id(contacts), object_mail(subject, body), &[]).await;
    }
    let (detour, _stop) = old_imap(&old).await;
    let env = env(&old, detour);

    let new = Store::open(&dir.path().join("new")).await.unwrap();
    let mini = person(&new, "mini@example.org", None, 0).await;
    // Contacts wanted, calendars not (security review 0.22 MOV-1).
    let only_contacts = NewMove { calendars: false, ..new_move(MoveKind::Mailbox) };
    new.create_move(only_contacts, vec![mailbox(mini, "mini@example.org", PASSWORD)]).await.unwrap();
    let done = take_turn(&new, &env).await;
    assert_eq!(done.state, MoveMailboxState::Synced, "{done:?}");
    let here = new.mailboxes(mini).await.unwrap();
    // Calendars were not asked for: both messages stay mail, the event too.
    assert_eq!(emails_in(&here, "Kalender"), 2, "{here:?}");
    // The letter with a card attached is mail; only the bare card became a contact.
    assert_eq!(emails_in(&here, "Contacts"), 1, "{here:?}");
    assert_eq!(done.contacts_done, 1, "{done:?}");
    let events = new.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("Kalender")).await;
    let event_count: i64 = events.unwrap().iter().map(|c| c.resources).sum();
    assert_eq!(event_count, 0, "no event without asking, and no invitation became one");
}

#[tokio::test(flavor = "multi_thread")]
async fn objects_that_cannot_be_stored_are_copied_as_mail() {
    let dir = tempfile::tempdir().unwrap();
    let old = Store::open(&dir.path().join("old")).await.unwrap();
    let old_mini = person(&old, "mini@example.org", Some(PASSWORD), 0).await;
    let contacts = old.create_mailbox(old_mini, "Contacts", None, None, 0, true).await.unwrap();
    deliver_raw(&old, old_mini, MailboxTarget::Id(contacts), object_mail("card", PURE_CARD), &[]).await;
    let (detour, _stop) = old_imap(&old).await;

    let new = Store::open(&dir.path().join("new")).await.unwrap();
    let mini = person(&new, "mini@example.org", None, 0).await;
    let old_box = OldMailbox { account_id: mini, host: "imap.example.net", port: 993, login: "mini@example.org" };
    let mut connection = connect(&new, &old_box, PASSWORD.into(), Some(detour), None).await.ok().unwrap();
    // The import fails (as a broken address book would): the message must not be lost.
    let failing: Box<Objects> = Box::new(|_, _, _| Box::pin(std::future::ready(false)));
    let note: Box<crate::migrate::Note> = Box::new(|_, _| Box::pin(std::future::ready(true)));
    let options = CopyOptions { skip_known: true, contacts: true, ..CopyOptions::default() };
    let run = copy_with(
        &new,
        &mut connection,
        mini,
        "imap.example.net",
        Default::default(),
        options,
        Duration::from_secs(60),
        note,
        Some(failing),
    )
    .await;
    assert_eq!(run, MigrationRun::Done);
    assert_eq!(emails_in(&new.mailboxes(mini).await.unwrap(), "Contacts"), 1, "kept as mail");
}
