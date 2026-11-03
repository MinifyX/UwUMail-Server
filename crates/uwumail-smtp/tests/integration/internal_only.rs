//! An account held to this server (docs/groups.md): it reaches people here and nobody else, over
//! every way mail can leave in its name.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use lettre::AsyncTransport;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{AsyncSmtpConnection, TlsParameters};
use lettre::transport::smtp::commands::{Data, Mail, Rcpt};
use lettre::transport::smtp::extension::ClientId;
use uwumail_smtp::{Submission, SubmissionRecipient, SubmitError};
use uwumail_store::{AccountUpdate, NewGroup, NewSharedMailbox, StoreError, VacationResponse, WhoMaySend};

use crate::flow::{PASSWORD, RawSession, TestServer, mail, start};

const REFUSED: &str = "This account may only send to addresses on this server";

/// Nothing listens on port 9, so what goes to c.test waits in the queue where a test can see it.
async fn server() -> TestServer {
    let a = start("a.test", &["mini", "leni", "webserver"], &[("c.test", SocketAddr::from(([127, 0, 0, 1], 9)))]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    a
}

async fn hold(server: &TestServer, login: &str, on: bool) {
    let update = AccountUpdate { internal_only: Some(on), ..Default::default() };
    server.smtp.store().update_account(login, update).await.unwrap();
}

async fn queued(server: &TestServer) -> Vec<String> {
    let entries = server.smtp.store().queue_entries().await.unwrap();
    entries.iter().flat_map(|e| e.recipients.iter().map(|r| r.address.clone())).collect()
}

async fn wait_for_queue(server: &TestServer, address: &str) {
    let started = Instant::now();
    while !queued(server).await.iter().any(|queued| queued == address) {
        assert!(started.elapsed() < Duration::from_secs(10), "{address} was not queued");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A confirmed forwarding target elsewhere, without a copy staying here.
async fn forwards_away(server: &TestServer, login: &str) {
    let store = server.smtp.store();
    let account = store.account(login).await.unwrap().unwrap();
    let (_, token) = store.add_forward_target(account.id, "oma@c.test", true).await.unwrap();
    store.confirm_forward_link(&token.unwrap()).await.unwrap();
    store.set_forward_keep_copy(account.id, false).await.unwrap();
}

/// A message from someone elsewhere, through the door for other servers.
async fn from_outside(server: &TestServer, to: &str, subject: &str) {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command(&format!("RCPT TO:<{to}>")).await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let message = format!("From: news@sender.test\r\nTo: {to}\r\nSubject: {subject}\r\n\r\nHallo\r\n.");
    let reply = session.command(&message).await;
    assert!(reply.starts_with("250"), "{reply}");
}

/// Logged in over TLS, command by command: unlike a mail library, this goes on after a refused RCPT.
async fn logged_in(server: &TestServer, login: &str) -> AsyncSmtpConnection {
    let tls = TlsParameters::builder("localhost".into()).dangerous_accept_invalid_certs(true).build_rustls().unwrap();
    let mut connection = AsyncSmtpConnection::connect_tokio1(
        (IpAddr::from([127, 0, 0, 1]), server.submission_tls.port()),
        Some(Duration::from_secs(10)),
        &ClientId::default(),
        Some(tls),
        None,
    )
    .await
    .unwrap();
    connection.auth(&[Mechanism::Plain], &Credentials::new(login.into(), PASSWORD.into())).await.unwrap();
    connection
}

fn address(address: &str) -> lettre::Address {
    address.parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn over_smtp_it_reaches_people_here_and_nobody_else() {
    let a = server().await;
    hold(&a, "webserver@a.test", true).await;
    let mailer = a.mailer("webserver@a.test", PASSWORD, false);

    mailer.send(mail("webserver@a.test", &["leni@a.test"], "Kontakt")).await.unwrap();
    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Kontakt");

    let refused = mailer.send(mail("webserver@a.test", &["oma@c.test"], "Raus")).await.unwrap_err().to_string();
    assert!(refused.contains("5.7.1") && refused.contains(REFUSED), "{refused}");
    assert!(queued(&a).await.is_empty());

    // Everyone else is as they were, and so is this account with the switch off again.
    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["oma@c.test"], "Hallo Oma")).await.unwrap();
    wait_for_queue(&a, "oma@c.test").await;
    a.smtp.store().queue_entries().await.unwrap().iter().for_each(|entry| {
        assert_eq!(entry.message.return_path, "mini@a.test");
    });
    hold(&a, "webserver@a.test", false).await;
    mailer.send(mail("webserver@a.test", &["opa@c.test"], "Wieder frei")).await.unwrap();
    wait_for_queue(&a, "opa@c.test").await;
}

/// SMTP answers per recipient: someone elsewhere is refused at RCPT, and the people here still get
/// the message from a client that goes on.
#[tokio::test(flavor = "multi_thread")]
async fn a_mixed_list_over_smtp_is_refused_per_recipient() {
    let a = server().await;
    hold(&a, "webserver@a.test", true).await;
    let mut connection = logged_in(&a, "webserver@a.test").await;
    connection.command(Mail::new(Some(address("webserver@a.test")), vec![])).await.unwrap();

    let refused = connection.command(Rcpt::new(address("oma@c.test"), vec![])).await.unwrap_err().to_string();
    assert!(refused.contains("5.7.1") && refused.contains(REFUSED), "{refused}");
    connection.command(Rcpt::new(address("leni@a.test"), vec![])).await.unwrap();

    connection.command(Data).await.unwrap();
    let raw = b"From: webserver@a.test\r\nTo: leni@a.test, oma@c.test\r\nSubject: Halb\r\n\r\nHallo\r\n";
    connection.message(raw).await.unwrap();

    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Halb");
    assert!(queued(&a).await.is_empty());
}

/// Every other door is all or nothing: nobody takes a half-sent message for a sent one.
#[tokio::test(flavor = "multi_thread")]
async fn a_mixed_list_elsewhere_is_refused_as_a_whole() {
    let a = server().await;
    hold(&a, "webserver@a.test", true).await;
    let account = a.smtp.store().account("webserver@a.test").await.unwrap().unwrap();
    let submission = Submission {
        account,
        mail_from: "webserver@a.test".into(),
        recipients: vec![SubmissionRecipient::new("leni@a.test"), SubmissionRecipient::new("oma@c.test")],
        raw: b"From: webserver@a.test\r\nSubject: Alles oder nichts\r\n\r\nHallo\r\n".to_vec(),
        env_id: None,
        trace: None,
    };
    match a.smtp.submit(submission).await {
        Err(SubmitError::InternalOnly(address)) => assert_eq!(address, "oma@c.test"),
        other => panic!("{other:?}"),
    }
    assert!(a.inbox("leni@a.test").await.is_empty());
    assert!(queued(&a).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forwarding_address_only_passes_it_on_to_people_here() {
    let a = server().await;
    hold(&a, "webserver@a.test", true).await;
    let store = a.smtp.store();
    store.set_forward_address("kasse@a.test", vec!["oma@c.test".into(), "leni@a.test".into()], "").await.unwrap();
    store.set_forward_address("raus@a.test", vec!["oma@c.test".into()], "").await.unwrap();
    let mailer = a.mailer("webserver@a.test", PASSWORD, false);

    // With nobody here among the targets it could only leave: refused like an address elsewhere.
    let refused = mailer.send(mail("webserver@a.test", &["raus@a.test"], "Raus")).await.unwrap_err().to_string();
    assert!(refused.contains("5.7.1") && refused.contains(REFUSED), "{refused}");

    mailer.send(mail("webserver@a.test", &["kasse@a.test"], "Beitrag")).await.unwrap();
    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Beitrag");
    assert!(queued(&a).await.is_empty(), "the target elsewhere is left out");

    // Everyone else reaches both targets.
    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["kasse@a.test"], "Quittung")).await.unwrap();
    a.wait_for_inbox("leni@a.test", 2).await;
    wait_for_queue(&a, "oma@c.test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forwarding_of_someone_here_stops_at_this_server() {
    let a = server().await;
    hold(&a, "webserver@a.test", true).await;
    forwards_away(&a, "leni@a.test").await;

    a.mailer("webserver@a.test", PASSWORD, false)
        .send(mail("webserver@a.test", &["leni@a.test"], "Kontakt"))
        .await
        .unwrap();
    let inbox = a.wait_for_inbox("leni@a.test", 1).await;
    assert_eq!(inbox[0].subject, "Kontakt", "it stays, although she keeps no copies otherwise");
    assert!(queued(&a).await.is_empty());

    // Her forwarding works for everyone else.
    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["leni@a.test"], "Hallo")).await.unwrap();
    wait_for_queue(&a, "oma@c.test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forwarding_of_a_group_member_stops_at_this_server() {
    let a = server().await;
    hold(&a, "webserver@a.test", true).await;
    forwards_away(&a, "leni@a.test").await;
    let group = NewGroup {
        address: "vorstand@a.test".into(),
        name: "Vorstand".into(),
        who_may_send: WhoMaySend::Anyone,
        members_may_send_as: false,
        members: vec!["leni@a.test".into(), "mini@a.test".into()],
    };
    a.smtp.store().create_group(group).await.unwrap();

    a.mailer("webserver@a.test", PASSWORD, false)
        .send(mail("webserver@a.test", &["vorstand@a.test"], "Sitzung"))
        .await
        .unwrap();
    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Sitzung");
    assert_eq!(a.wait_for_inbox("mini@a.test", 1).await[0].subject, "Sitzung");
    assert!(queued(&a).await.is_empty());
}

/// Someone with the credentials sets a forward or a rule and writes to the account from anywhere.
#[tokio::test(flavor = "multi_thread")]
async fn its_own_forwarding_and_rules_stay_here() {
    let a = server().await;
    let store = a.smtp.store();
    hold(&a, "webserver@a.test", true).await;
    forwards_away(&a, "webserver@a.test").await;

    // Its own mail to itself, and mail from elsewhere: both stay.
    a.mailer("webserver@a.test", PASSWORD, false)
        .send(mail("webserver@a.test", &["webserver@a.test"], "An mich"))
        .await
        .unwrap();
    from_outside(&a, "webserver@a.test", "Von draussen").await;
    a.wait_for_inbox("webserver@a.test", 2).await;
    assert!(queued(&a).await.is_empty(), "forwarding");

    // A redirect to the same, confirmed target.
    let account = store.account("webserver@a.test").await.unwrap().unwrap();
    let (script, _) = store.put_sieve_script(account.id, "raus", b"redirect \"oma@c.test\";").await.unwrap();
    store.activate_sieve_script(account.id, Some(script.id)).await.unwrap();
    from_outside(&a, "webserver@a.test", "Umleitung").await;
    a.wait_for_inbox("webserver@a.test", 3).await;
    assert!(queued(&a).await.is_empty(), "redirect");

    // The same two go out once the switch is off.
    hold(&a, "webserver@a.test", false).await;
    from_outside(&a, "webserver@a.test", "Wieder frei").await;
    wait_for_queue(&a, "oma@c.test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn its_vacation_reply_only_goes_to_people_here() {
    let a = server().await;
    // A reply only goes to a sender the checks verified.
    a.smtp.dns_cache().pin_txt("sender.test", "v=spf1 ip4:127.0.0.1 -all").unwrap();
    let store = a.smtp.store();
    let account = store.account("webserver@a.test").await.unwrap().unwrap();
    let vacation = VacationResponse {
        is_enabled: true,
        subject: Some("Geheim".into()),
        text_body: Some("Das geht niemanden etwas an.".into()),
        ..Default::default()
    };
    store.set_vacation_response(account.id, vacation).await.unwrap();
    hold(&a, "webserver@a.test", true).await;

    from_outside(&a, "webserver@a.test", "Bist du da").await;
    a.wait_for_inbox("webserver@a.test", 1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(queued(&a).await.is_empty(), "no reply leaves");

    a.mailer("mini@a.test", PASSWORD, false)
        .send(mail("mini@a.test", &["webserver@a.test"], "Bist du da"))
        .await
        .unwrap();
    assert_eq!(a.wait_for_inbox("mini@a.test", 1).await[0].subject, "Geheim", "people here get one");

    // Without the switch the reply goes out; the sender did not have one yet today.
    hold(&a, "webserver@a.test", false).await;
    from_outside(&a, "webserver@a.test", "Noch da").await;
    wait_for_queue(&a, "news@sender.test").await;
}

/// The switch of a shared mailbox holds for mail in its name, whoever's login sends it, and the
/// switch of a person holds for everything they send.
#[tokio::test(flavor = "multi_thread")]
async fn sending_as_a_shared_mailbox_held_here_stays_here() {
    let a = server().await;
    let store = a.smtp.store();
    let shared = NewSharedMailbox {
        address: "info@a.test".into(),
        name: "Info".into(),
        quota_bytes: 0,
        members: vec![("mini@a.test".into(), true)],
    };
    store.create_shared_mailbox(shared).await.unwrap();
    hold(&a, "info@a.test", true).await;
    let mailer = a.mailer("mini@a.test", PASSWORD, false);

    // By the envelope at RCPT.
    let refused = mailer.send(mail("info@a.test", &["oma@c.test"], "Raus")).await.unwrap_err().to_string();
    assert!(refused.contains("5.7.1") && refused.contains(REFUSED), "{refused}");
    // By the From, which only shows after DATA.
    let mut connection = logged_in(&a, "mini@a.test").await;
    connection.command(Mail::new(Some(address("mini@a.test")), vec![])).await.unwrap();
    connection.command(Rcpt::new(address("oma@c.test"), vec![])).await.unwrap();
    connection.command(Data).await.unwrap();
    let refused =
        connection.message(b"From: info@a.test\r\nSubject: Raus\r\n\r\nHallo\r\n").await.unwrap_err().to_string();
    assert!(refused.contains("5.7.1") && refused.contains(REFUSED), "{refused}");
    assert!(queued(&a).await.is_empty());

    // In its name to people here, and in Mini's own name to anyone.
    mailer.send(mail("info@a.test", &["leni@a.test"], "Antwort")).await.unwrap();
    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Antwort");
    mailer.send(mail("mini@a.test", &["oma@c.test"], "Privat")).await.unwrap();
    wait_for_queue(&a, "oma@c.test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_queue_itself_takes_nothing_in_its_name() {
    let a = server().await;
    let store = a.smtp.store();
    hold(&a, "webserver@a.test", true).await;
    let account = store.account("webserver@a.test").await.unwrap().unwrap();
    let recipient = || uwumail_store::NewQueueRecipient { address: "oma@c.test".into(), notify_flags: 0, orcpt: None };

    let refused = store.enqueue("", vec![recipient()], b"x", Some(account.id), None, 60).await;
    assert!(matches!(refused, Err(StoreError::Rule { code: "internalOnly", .. })), "{refused:?}");
    assert!(queued(&a).await.is_empty());

    // Notices of the server itself belong to no account.
    store.enqueue("", vec![recipient()], b"x", None, None, 60).await.unwrap();
    assert_eq!(queued(&a).await, ["oma@c.test"]);
}
