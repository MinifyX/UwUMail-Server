//! End-to-end SMTP tests with two in-process servers talking to each other.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lettre::message::Mailbox as LettreMailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uwumail_smtp::{
    AntivirusConfig, DeliveryConfig, ListenerKind, Smtp, SmtpConfig, SmtpSettings, SpamConfig, ToneConfig,
};
use uwumail_store::{
    BayesTotals, EmailSummary, EmailUpdate, IngestRequest, KeywordsChange, ListScope, MailboxRole, MailboxTarget,
    MailboxesChange, NewAccount, NewSenderListEntry, Role, SenderList, SpamLimits, Stat, Store,
};

pub(crate) const PASSWORD: &str = "katzenpfote-123";

pub(crate) struct TestServer {
    pub(crate) smtp: Smtp,
    pub(crate) mx: SocketAddr,
    submission: SocketAddr,
    submission_tls: SocketAddr,
    _shutdown: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

fn server_tls(names: &[&str]) -> Arc<rustls::ServerConfig> {
    let generated =
        rcgen::generate_simple_self_signed(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![generated.cert.der().clone()], key)
        .unwrap();
    Arc::new(config)
}

pub(crate) async fn start(domain: &str, users: &[&str], routes: &[(&str, SocketAddr)]) -> TestServer {
    start_with(domain, users, routes, SmtpConfig::default()).await
}

async fn start_with(domain: &str, users: &[&str], routes: &[(&str, SocketAddr)], config: SmtpConfig) -> TestServer {
    let spam = SpamConfig { enabled: false, ..SpamConfig::default() };
    start_with_spam(domain, users, routes, config, spam).await
}

async fn start_with_spam(
    domain: &str,
    users: &[&str],
    routes: &[(&str, SocketAddr)],
    config: SmtpConfig,
    spam: SpamConfig,
) -> TestServer {
    let _ = tracing_subscriber::fmt().with_env_filter("debug").with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain(domain).await.unwrap();
    for (index, user) in users.iter().enumerate() {
        store
            .create_account(NewAccount {
                address: format!("{user}@{domain}"),
                display_name: user.to_string(),
                password: Some(PASSWORD.into()),
                role: if index == 0 { Role::Admin } else { Role::User },
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }

    let hostname = format!("mx.{domain}");
    let delivery = DeliveryConfig {
        routes: routes.iter().map(|(d, addr)| (d.to_string(), addr.to_string())).collect::<HashMap<_, _>>(),
        ..DeliveryConfig::default()
    };
    let smtp = Smtp::new(
        store,
        SmtpSettings {
            hostname: hostname.clone(),
            smtp: config,
            spam,
            delivery,
            tone: ToneConfig::default(),
            server_tls: Some(server_tls(&["localhost", &hostname])),
        },
    )
    .unwrap();

    // Keep sender checks local: no DNS records exist for the test domains.
    for name in
        [domain.to_string(), hostname.clone(), format!("_dmarc.{domain}"), "_dmarc.test".into(), "localhost".into()]
    {
        smtp.dns_cache().pin_no_txt(&name);
    }

    let (shutdown, rx) = watch::channel(false);
    let mut addrs = Vec::new();
    for kind in [ListenerKind::Mx, ListenerKind::Submission, ListenerKind::SubmissionTls] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addrs.push(listener.local_addr().unwrap());
        tokio::spawn(uwumail_smtp::serve(smtp.clone(), listener, kind, rx.clone()));
    }
    tokio::spawn(uwumail_smtp::run_learning(smtp.clone(), rx.clone()));
    tokio::spawn(uwumail_smtp::run_queue(smtp.clone(), rx));

    TestServer { smtp, mx: addrs[0], submission: addrs[1], submission_tls: addrs[2], _shutdown: shutdown, _dir: dir }
}

impl TestServer {
    pub(crate) async fn inbox(&self, login: &str) -> Vec<EmailSummary> {
        self.mailbox(login, MailboxRole::Inbox).await
    }

    pub(crate) async fn mailbox(&self, login: &str, role: MailboxRole) -> Vec<EmailSummary> {
        let store = self.smtp.store();
        let account = store.account(login).await.unwrap().unwrap();
        let mailbox = store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(role)).unwrap();
        store.emails_in_mailbox(mailbox.id, 50).await.unwrap()
    }

    pub(crate) async fn wait_for_inbox(&self, login: &str, count: usize) -> Vec<EmailSummary> {
        let started = Instant::now();
        loop {
            let emails = self.inbox(login).await;
            if emails.len() >= count {
                return emails;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "{login} has {} of {count} emails", emails.len());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// How often `stat` was counted for the statistics since the server started.
    fn counted(&self, stat: Stat) -> u64 {
        self.smtp.store().stats().since_start().into_iter().find(|(counted, _)| *counted == stat).unwrap().1
    }

    pub(crate) async fn raw(&self, email: &EmailSummary) -> String {
        let hash = uwumail_store::BlobHash::parse(&email.blob).unwrap();
        String::from_utf8(self.smtp.store().blob(&hash).await.unwrap()).unwrap()
    }

    pub(crate) fn mailer(&self, login: &str, password: &str, implicit_tls: bool) -> AsyncSmtpTransport<Tokio1Executor> {
        let tls =
            TlsParameters::builder("localhost".into()).dangerous_accept_invalid_certs(true).build_rustls().unwrap();
        let (port, tls) = if implicit_tls {
            (self.submission_tls.port(), Tls::Wrapper(tls))
        } else {
            (self.submission.port(), Tls::Required(tls))
        };
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
            .port(port)
            .tls(tls)
            .credentials(Credentials::new(login.into(), password.into()))
            .timeout(Some(Duration::from_secs(10)))
            .build()
    }
}

pub(crate) fn mail(from: &str, to: &[&str], subject: &str) -> Message {
    let mut builder = Message::builder().from(from.parse::<LettreMailbox>().unwrap()).subject(subject);
    for recipient in to {
        builder = builder.to(recipient.parse::<LettreMailbox>().unwrap());
    }
    builder.body(String::from("Hallo!\r\n.Diese Zeile beginnt mit einem Punkt.\r\nLiebe Grüße")).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn submitted_mail_reaches_local_and_remote_people_with_dkim() {
    let b = start("b.test", &["nyu"], &[]).await;
    let a = start("a.test", &["mini", "ami"], &[("b.test", b.mx)]).await;

    // Publish a.test's DKIM keys where b.test will look for them.
    let keys = uwumail_smtp::dkim::ensure_domain_keys(a.smtp.store(), "a.test").await.unwrap();
    assert_eq!(keys.len(), 2);
    for key in &keys {
        let (name, value) = key.dns_record();
        b.smtp.dns_cache().pin_txt(&name, &value).unwrap();
    }
    for name in ["a.test", "mx.a.test", "_dmarc.a.test"] {
        b.smtp.dns_cache().pin_no_txt(name);
    }

    a.mailer("mini@a.test", PASSWORD, false)
        .send(mail("Mini <mini@a.test>", &["nyu@b.test", "ami@a.test"], "Katzenfutter"))
        .await
        .unwrap();

    let local = a.wait_for_inbox("ami@a.test", 1).await;
    assert_eq!(local[0].subject, "Katzenfutter");

    let remote = b.wait_for_inbox("nyu@b.test", 1).await;
    assert_eq!(remote[0].subject, "Katzenfutter");
    let raw = b.raw(&remote[0]).await;
    assert!(raw.contains("Authentication-Results: mx.b.test"), "{raw}");
    assert_eq!(raw.matches("dkim=pass").count(), 2, "both signatures verify: {raw}");
    assert!(raw.contains("by mx.a.test (UwUMail) with ESMTPSA"));
    assert!(raw.contains("\r\n.Diese Zeile beginnt mit einem Punkt."), "dot-stuffing is undone");
    assert_eq!(
        raw.matches("127.0.0.1").count(),
        1,
        "only b.test notes the connecting server, the sender stays private: {raw}"
    );

    // The queue on a.test empties once delivery is done.
    let started = Instant::now();
    while !a.smtp.store().queue_entries().await.unwrap().is_empty() {
        assert!(started.elapsed() < Duration::from_secs(10), "queue did not drain");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The statistics saw it leave one server and arrive at the other.
    assert_eq!((a.counted(Stat::Submitted), a.counted(Stat::Delivered), a.counted(Stat::Received)), (1, 1, 0));
    assert_eq!((b.counted(Stat::Received), b.counted(Stat::Submitted)), (1, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_company_footer_is_added_before_signing_for_smtp_clients_too() {
    use lettre::message::MultiPart;
    use uwumail_store::{CompanySignature, CompanySignatureMode};
    let b = start("b.test", &["nyu"], &[]).await;
    let a = start("a.test", &["mini"], &[("b.test", b.mx)]).await;
    let keys = uwumail_smtp::dkim::ensure_domain_keys(a.smtp.store(), "a.test").await.unwrap();
    for key in &keys {
        let (name, value) = key.dns_record();
        b.smtp.dns_cache().pin_txt(&name, &value).unwrap();
    }
    for name in ["a.test", "mx.a.test", "_dmarc.a.test"] {
        b.smtp.dns_cache().pin_no_txt(name);
    }
    let footer = CompanySignature {
        mode: CompanySignatureMode::Footer,
        text: "A-Test GmbH · {name}".into(),
        html: "<p>A-Test GmbH &middot; {name}</p>".into(),
    };
    a.smtp.store().set_domain_signature("a.test", footer).await.unwrap();

    let message = Message::builder()
        .from("Mini & Co <mini@a.test>".parse::<LettreMailbox>().unwrap())
        .to("nyu@b.test".parse::<LettreMailbox>().unwrap())
        .subject("Mit Fusszeile")
        .multipart(MultiPart::alternative_plain_html("Hallo Nyu".to_owned(), "<p>Hallo Nyu</p>".to_owned()))
        .unwrap();
    a.mailer("mini@a.test", PASSWORD, false).send(message).await.unwrap();

    let remote = b.wait_for_inbox("nyu@b.test", 1).await;
    let raw = b.raw(&remote[0]).await;
    assert_eq!(raw.matches("dkim=pass").count(), 2, "the footer went in before signing: {raw}");
    let parsed = mail_parser::MessageParser::new().parse(raw.as_bytes()).unwrap();
    let text = parsed.body_text(0).unwrap();
    assert!(text.contains("Hallo Nyu") && text.contains("A-Test GmbH · Mini & Co"), "{text}");
    let html = parsed.body_html(0).unwrap();
    assert!(html.contains("<p>A-Test GmbH &middot; Mini &amp; Co</p>"), "the name is escaped in HTML: {html}");
}

#[tokio::test(flavor = "multi_thread")]
async fn implicit_tls_submission_works() {
    let a = start("a.test", &["mini", "ami"], &[]).await;
    a.mailer("mini@a.test", PASSWORD, true).send(mail("mini@a.test", &["ami@a.test"], "Über 465")).await.unwrap();
    assert_eq!(a.wait_for_inbox("ami@a.test", 1).await[0].subject, "Über 465");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_remote_recipients_bounce_to_the_sender() {
    let b = start("b.test", &["nyu"], &[]).await;
    let a = start("a.test", &["mini"], &[("b.test", b.mx)]).await;
    for name in ["a.test", "mx.a.test", "_dmarc.a.test"] {
        b.smtp.dns_cache().pin_no_txt(name);
    }

    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["ghost@b.test"], "Hallo Geist")).await.unwrap();

    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    assert!(inbox[0].subject.contains("nicht angekommen"), "playful German bounce: {}", inbox[0].subject);
    let raw = a.raw(&inbox[0]).await;
    assert!(raw.contains("ghost@b.test"));
    assert!(raw.contains("5.1.1"));
    assert!(raw.contains("multipart/report"));
    assert_eq!(b.counted(Stat::RefusedUnknownRecipient), 1);
    assert_eq!((a.counted(Stat::Bounced), a.counted(Stat::Delivered)), (1, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn submission_rules() {
    let a = start("a.test", &["mini", "ami"], &[]).await;

    // Wrong password.
    assert!(a.mailer("mini@a.test", "falsch", false).send(mail("mini@a.test", &["ami@a.test"], "x")).await.is_err());
    assert_eq!((a.counted(Stat::LoginFailedSmtp), a.counted(Stat::LoginFailedImap)), (1, 0));
    // Sending as someone else.
    assert!(a.mailer("mini@a.test", PASSWORD, false).send(mail("ami@a.test", &["ami@a.test"], "x")).await.is_err());

    // Without TLS there is no AUTH, and MAIL needs a login.
    let mut session = RawSession::connect(a.submission).await;
    let ehlo = session.command("EHLO client.test").await;
    assert!(ehlo.contains("STARTTLS") && !ehlo.contains("AUTH"), "{ehlo}");
    assert!(session.command("MAIL FROM:<mini@a.test>").await.starts_with("530"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_guessed_at_elsewhere_waits_here_too() {
    // security-audit-0.16.0 PROTOCOLS-8: ten wrong passwords for one login from ten networks,
    // over any protocol, and SMTP makes the next try wait as well, without checking the password.
    let a = start("a.test", &["mini", "ami"], &[]).await;
    for network in 0..10 {
        a.smtp.store().auth_limiter().record_failure(format!("198.51.100.{network}").parse().unwrap(), "mini@a.test");
    }
    let refused = a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["ami@a.test"], "x")).await;
    let refused = refused.expect_err("the login waits");
    assert!(refused.to_string().contains("Too many failed logins"), "{refused}");
    assert_eq!(a.counted(Stat::LoginFailedSmtp), 0, "no password was checked");
    a.mailer("ami@a.test", PASSWORD, false).send(mail("ami@a.test", &["mini@a.test"], "x")).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn mx_refuses_relaying_and_strips_forged_results() {
    let a = start("a.test", &["mini"], &[]).await;
    for name in ["elsewhere.test", "client.elsewhere.test", "_dmarc.elsewhere.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }

    let mut session = RawSession::connect(a.mx).await;
    let ehlo = session.command("EHLO client.elsewhere.test").await;
    assert!(!ehlo.contains("AUTH"), "{ehlo}");
    assert!(session.command("MAIL FROM:<someone@elsewhere.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<ghost@a.test>").await.starts_with("550 5.1.1"));
    assert!(session.command("RCPT TO:<friend@gmail.com>").await.starts_with("550 5.7.1"));
    assert_eq!((a.counted(Stat::RefusedUnknownRecipient), a.counted(Stat::RefusedPolicy)), (1, 1));
    assert!(session.command("RCPT TO:<MINI+katzen@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session
        .command(
            "Authentication-Results: mx.a.test; dkim=pass header.d=bank.example\r\n\
             From: someone@elsewhere.test\r\nSubject: Echt jetzt\r\n\r\nHallo\r\n.",
        )
        .await;
    assert!(reply.starts_with("250"), "{reply}");
    assert!(session.command("QUIT").await.starts_with("221"));

    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    let raw = a.raw(&inbox[0]).await;
    assert!(!raw.contains("header.d=bank.example"), "{raw}");
    assert!(raw.contains("Authentication-Results: mx.a.test"), "{raw}");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_client_cannot_take_every_connection() {
    // security-audit-0.16.0 SMTP-5: nothing counted connections per client.
    let config = SmtpConfig { max_connections_per_client: 2, ..SmtpConfig::default() };
    let a = start_with("a.test", &["mini"], &[], config).await;
    let _first = RawSession::connect(a.mx).await;
    let _second = RawSession::connect(a.submission).await;
    let mut third = BufReader::new(TcpStream::connect(a.mx).await.unwrap());
    let mut refused = String::new();
    third.read_line(&mut refused).await.unwrap();
    assert!(refused.starts_with("421 4.7.0"), "{refused}");

    // Someone else is served.
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let peer = "198.51.100.7:40000".parse().unwrap();
    tokio::spawn(uwumail_smtp::serve_stream(a.smtp.clone(), Box::new(server), peer, ListenerKind::Mx));
    let mut greeting = [0u8; 3];
    client.read_exact(&mut greeting).await.unwrap();
    assert_eq!(&greeting, b"220");
}

#[tokio::test]
async fn a_session_that_gets_nowhere_is_closed() {
    // security-audit-0.16.0 SMTP-5: a NOOP every few minutes kept a connection slot for ever; the
    // idle timeout is reset by every byte. The clock is the test's own.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    let settings = SmtpSettings {
        hostname: "mx.a.test".into(),
        smtp: SmtpConfig::default(),
        spam: SpamConfig { enabled: false, ..SpamConfig::default() },
        delivery: DeliveryConfig::default(),
        tone: ToneConfig::default(),
        server_tls: None,
    };
    let smtp = Smtp::new(store, settings).unwrap();
    tokio::time::pause();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let peer = "198.51.100.7:40000".parse().unwrap();
    tokio::spawn(uwumail_smtp::serve_stream(smtp, Box::new(server), peer, ListenerKind::Mx));
    let mut client = BufReader::new(client);
    let mut line = String::new();
    client.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("220"), "{line}");
    // A NOOP a little less than a minute apart, for three minutes: long before the five minutes of
    // idling are up, the session is over.
    for wait in [59, 59, 59, 30] {
        client.get_mut().write_all(b"NOOP\r\n").await.unwrap();
        line.clear();
        client.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("250"), "{line}");
        tokio::time::advance(Duration::from_secs(wait)).await;
    }
    line.clear();
    tokio::time::timeout(Duration::from_secs(10), client.read_line(&mut line))
        .await
        .expect("the session was closed in time")
        .unwrap();
    assert!(line.starts_with("421 4.4.2"), "{line}");
}

pub(crate) struct RawSession {
    reader: BufReader<TcpStream>,
}

impl RawSession {
    pub(crate) async fn connect(addr: SocketAddr) -> RawSession {
        let mut session = RawSession { reader: BufReader::new(TcpStream::connect(addr).await.unwrap()) };
        assert!(session.read_reply().await.starts_with("220"));
        session
    }

    async fn read_reply(&mut self) -> String {
        let mut reply = String::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).await.unwrap();
            reply.push_str(&line);
            if line.len() < 4 || line.as_bytes()[3] != b'-' {
                return reply;
            }
        }
    }

    pub(crate) async fn command(&mut self, command: &str) -> String {
        self.reader.get_mut().write_all(format!("{command}\r\n").as_bytes()).await.unwrap();
        self.read_reply().await
    }

    /// One BDAT chunk (RFC 3030) with exactly `data`.
    async fn bdat(&mut self, data: &[u8], last: bool) -> String {
        let last = if last { " LAST" } else { "" };
        let stream = self.reader.get_mut();
        stream.write_all(format!("BDAT {}{last}\r\n", data.len()).as_bytes()).await.unwrap();
        stream.write_all(data).await.unwrap();
        self.read_reply().await
    }
}

/// Many clients end a chunked message with an empty last chunk; it is answered at once, not after
/// another packet that never comes.
#[tokio::test(flavor = "multi_thread")]
async fn a_chunked_message_may_end_with_an_empty_chunk() {
    let a = start("a.test", &["mini"], &[]).await;
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO mail.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    let message = b"From: news@sender.test\r\nSubject: In Teilen\r\n\r\nHallo\r\n";
    assert!(session.bdat(message, false).await.starts_with("250"));
    let reply = tokio::time::timeout(Duration::from_secs(10), session.bdat(b"", true)).await.expect("an answer");
    assert!(reply.starts_with("250"), "{reply}");
    a.wait_for_inbox("mini@a.test", 1).await;
}

/// security-audit-0.8.0 T-1: a BDAT chunk is held to the size limit by the size it announces, before
/// any of it is kept, as DATA is while it streams. Mail within the limit still arrives in chunks.
#[tokio::test(flavor = "multi_thread")]
async fn bdat_chunks_are_held_to_the_size_limit() {
    let config = SmtpConfig { max_message_size: 4096, ..SmtpConfig::default() };
    let a = start_with("a.test", &["mini"], &[], config).await;
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO mail.sender.test").await.contains("CHUNKING"));

    async fn envelope(session: &mut RawSession) {
        assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
        assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    }
    // One chunk far past the limit.
    envelope(&mut session).await;
    let reply = session.bdat(&vec![b'x'; 64 * 1024], true).await;
    assert!(reply.starts_with("552 5.3.4"), "{reply}");
    // Chunks that fit one by one but not together.
    envelope(&mut session).await;
    assert!(session.bdat(&vec![b'x'; 3000], false).await.starts_with("250"));
    assert!(session.bdat(&vec![b'x'; 3000], false).await.starts_with("250"), "read and thrown away");
    let reply = session.bdat(b"", true).await;
    assert!(reply.starts_with("552 5.3.4"), "{reply}");

    // The session goes on, and a message within the limit arrives in two chunks.
    envelope(&mut session).await;
    assert!(session.bdat(b"From: news@sender.test\r\nSubject: In Teilen\r\n\r\n", false).await.starts_with("250"));
    let reply = session.bdat(b"Hallo\r\n", true).await;
    assert!(reply.starts_with("250"), "{reply}");
    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    assert!(a.raw(&inbox[0]).await.ends_with("Hallo\r\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sender_checks_behind_a_trusted_relay_use_the_original_client() {
    let config = SmtpConfig { trusted_relays: vec!["127.0.0.1".into()], ..SmtpConfig::default() };
    let a = start_with("a.test", &["mini"], &[], config).await;
    a.smtp.dns_cache().pin_txt("sender.test", "v=spf1 ip4:203.0.113.7 -all").unwrap();
    for name in ["mail.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }

    // The relay (us, from 127.0.0.1) received the message from 203.0.113.7 and forwards it.
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO relay.local").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session
        .command(
            "Received: from mail.sender.test (mail.sender.test [203.0.113.7])\r\n\
             \tby relay.local (Postfix) with ESMTPS id 4F1;\r\n\
             \tMon, 14 Sep 2026 10:00:00 +0200\r\n\
             From: news@sender.test\r\nSubject: Newsletter\r\n\r\nHallo\r\n.",
        )
        .await;
    assert!(reply.starts_with("250"), "{reply}");

    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    let raw = a.raw(&inbox[0]).await;
    assert!(raw.contains("spf=pass"), "SPF is checked against 203.0.113.7, not the relay: {raw}");
}

#[tokio::test(flavor = "multi_thread")]
async fn vacation_replies_once_per_sender() {
    let b = start("b.test", &["nyu", "mini"], &[]).await;
    let nyu = b.smtp.store().account("nyu@b.test").await.unwrap().unwrap();
    b.smtp
        .store()
        .set_vacation_response(
            nyu.id,
            uwumail_store::VacationResponse {
                is_enabled: true,
                subject: Some("Bin im Urlaub".into()),
                text_body: Some("Ab Montag wieder da.".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for subject in ["Erste Mail", "Zweite Mail"] {
        b.mailer("mini@b.test", PASSWORD, false).send(mail("mini@b.test", &["nyu@b.test"], subject)).await.unwrap();
    }
    b.wait_for_inbox("nyu@b.test", 2).await;
    let replies = b.wait_for_inbox("mini@b.test", 1).await;
    assert_eq!(replies[0].subject, "Bin im Urlaub");
    let raw = b.raw(&replies[0]).await;
    assert!(raw.contains("Auto-Submitted: auto-replied"), "{raw}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(b.inbox("mini@b.test").await.len(), 1, "only one reply per sender");
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_mail_uses_srs_and_bounces_find_the_original_sender() {
    let sender = start("sender.test", &["news"], &[]).await;
    // Nothing listens on port 9, so the forward to c.test waits in the queue where the test can see it.
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 9));
    let a = start("a.test", &["mini", "leni"], &[("c.test", unreachable), ("sender.test", sender.mx)]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    for name in ["a.test", "mx.a.test", "_dmarc.a.test", "c.test", "_dmarc.c.test"] {
        sender.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    let leni = store.account("leni@a.test").await.unwrap().unwrap();
    let (_, token) = store.add_forward_target(leni.id, "oma@c.test", true).await.unwrap();
    store.confirm_forward_link(&token.unwrap()).await.unwrap();

    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<leni@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session.command("From: news@sender.test\r\nSubject: Rabatt\r\n\r\nNur heute\r\n.").await;
    assert!(reply.starts_with("250"), "{reply}");

    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Rabatt", "a copy stays by default");
    let started = Instant::now();
    let return_path = loop {
        let entries = store.queue_entries().await.unwrap();
        if let Some(entry) = entries.iter().find(|e| e.recipients.iter().any(|r| r.address == "oma@c.test")) {
            break entry.message.return_path.clone();
        }
        assert!(started.elapsed() < Duration::from_secs(10), "the forward was not queued");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(return_path.starts_with("SRS0=") && return_path.ends_with("=sender.test=news@a.test"), "{return_path}");

    // The rewritten address takes delivery notices only, and only genuine ones.
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO mx.c.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<spam@c.test>").await.starts_with("250"));
    assert!(session.command(&format!("RCPT TO:<{return_path}>")).await.starts_with("550 5.7.1"));
    assert!(session.command("RSET").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<>").await.starts_with("250"));
    let forged = return_path.replacen("SRS0=", "SRS0=0", 1);
    assert!(session.command(&format!("RCPT TO:<{forged}>")).await.starts_with("550 5.1.1"));
    assert!(session.command(&format!("RCPT TO:<{return_path}>")).await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session
        .command("From: MAILER-DAEMON@c.test\r\nSubject: Undelivered Mail Returned to Sender\r\n\r\nNo such user\r\n.")
        .await;
    assert!(reply.starts_with("250"), "{reply}");

    let bounces = sender.wait_for_inbox("news@sender.test", 1).await;
    assert_eq!(bounces[0].subject, "Undelivered Mail Returned to Sender");
}

/// Sent on from here, a message passes SPF for our domain: a forged From of our own domain that
/// DMARC did not stop (no record, or `p=none`) must not leave with our name on it
/// (security-audit-0.16.0 SMTP-8).
#[tokio::test(flavor = "multi_thread")]
async fn forged_mail_from_our_own_domain_is_not_forwarded_elsewhere() {
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 9));
    let a = start("a.test", &["mini", "leni"], &[("c.test", unreachable)]).await;
    for name in ["a.test", "_dmarc.a.test", "client.sender.test", "sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    let leni = store.account("leni@a.test").await.unwrap().unwrap();
    let (_, token) = store.add_forward_target(leni.id, "oma@c.test", true).await.unwrap();
    store.confirm_forward_link(&token.unwrap()).await.unwrap();
    store.set_forward_keep_copy(leni.id, false).await.unwrap();

    let send = async |mail_from: &str, from: &str, subject: &str| {
        let mut session = RawSession::connect(a.mx).await;
        assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
        assert!(session.command(&format!("MAIL FROM:<{mail_from}>")).await.starts_with("250"));
        assert!(session.command("RCPT TO:<leni@a.test>").await.starts_with("250"));
        assert!(session.command("DATA").await.starts_with("354"));
        let reply = session.command(&format!("From: {from}\r\nSubject: {subject}\r\n\r\nBitte zahlen\r\n.")).await;
        assert!(reply.starts_with("250"), "{reply}");
    };

    // Nothing proves the From: it stays with Leni, although she keeps no copies otherwise.
    send("chef@a.test", "Chef <chef@a.test>", "Rechnung").await;
    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Rechnung");
    let queued = store.queue_entries().await.unwrap();
    assert!(!queued.iter().any(|e| e.recipients.iter().any(|r| r.address == "oma@c.test")), "{queued:?}");

    // An unproven envelope sender of ours is rewritten like anybody else's.
    send("chef@a.test", "news@sender.test", "Rabatt").await;
    let started = Instant::now();
    let return_path = loop {
        let entries = store.queue_entries().await.unwrap();
        if let Some(entry) = entries.iter().find(|e| e.recipients.iter().any(|r| r.address == "oma@c.test")) {
            break entry.message.return_path.clone();
        }
        assert!(started.elapsed() < Duration::from_secs(10), "the forward was not queued");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(return_path.starts_with("SRS0=") && return_path.ends_with("=a.test=chef@a.test"), "{return_path}");
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarding_addresses_pass_mail_on_without_a_mailbox() {
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 9));
    let a = start("a.test", &["mini", "leni"], &[("c.test", unreachable)]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    store.set_forward_address("kasse@a.test", vec!["oma@c.test".into(), "leni@a.test".into()], "").await.unwrap();

    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<kasse+2026@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session.command("From: news@sender.test\r\nSubject: Beitrag\r\n\r\nBitte zahlen\r\n.").await;
    assert!(reply.starts_with("250"), "{reply}");

    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Beitrag");
    let started = Instant::now();
    let return_path = loop {
        let entries = store.queue_entries().await.unwrap();
        if let Some(entry) = entries.iter().find(|e| e.recipients.iter().any(|r| r.address == "oma@c.test")) {
            break entry.message.return_path.clone();
        }
        assert!(started.elapsed() < Duration::from_secs(10), "the forward was not queued");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(return_path.starts_with("SRS0=") && return_path.ends_with("=sender.test=news@a.test"), "{return_path}");

    // People here reach it too, and their own address keeps its sender.
    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["kasse@a.test"], "Quittung")).await.unwrap();
    let inbox = a.wait_for_inbox("leni@a.test", 2).await;
    assert!(inbox.iter().any(|email| email.subject == "Quittung"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_without_a_mailbox_takes_no_mail_from_anywhere() {
    let a = start("a.test", &["mini", "leni"], &[]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store().clone();
    let service = store
        .create_account(uwumail_store::NewAccount {
            address: "reports@a.test".into(),
            display_name: "Reports".into(),
            password: None,
            role: uwumail_store::Role::Service,
            quota_bytes: 0,
            protocols: Some(uwumail_store::Protocols {
                smtp: true,
                imap: false,
                jmap: false,
                caldav: false,
                carddav: false,
            }),
        })
        .await
        .unwrap();
    assert!(!service.has_mailbox());

    // From outside, the door says so at RCPT.
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    let reply = session.command("RCPT TO:<reports@a.test>").await;
    assert!(reply.starts_with("550 5.1.1"), "{reply}");

    // And from a mail app on this server, where the recipient is looked up again.
    let sent = a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["reports@a.test"], "Bericht")).await;
    assert!(sent.is_err(), "submission accepted mail for a service without a mailbox");
    assert!(store.mailboxes(service.id).await.unwrap().is_empty(), "a service without a mailbox has none");

    // With an address named for it, both ways land there instead.
    store
        .update_account(
            "reports@a.test",
            uwumail_store::AccountUpdate { redirect_to: Some("leni@a.test".into()), ..Default::default() },
        )
        .await
        .unwrap();
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<reports@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session.command("From: news@sender.test\r\nSubject: Von aussen\r\n\r\nHallo\r\n.").await;
    assert!(reply.starts_with("250"), "{reply}");
    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["reports@a.test"], "Von innen")).await.unwrap();

    let inbox = a.wait_for_inbox("leni@a.test", 2).await;
    let subjects: Vec<_> = inbox.iter().map(|email| email.subject.as_str()).collect();
    assert!(subjects.contains(&"Von aussen") && subjects.contains(&"Von innen"), "{subjects:?}");
    assert!(store.mailboxes(service.id).await.unwrap().is_empty(), "nothing was stored under the service");
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarding_addresses_pass_no_spam_on() {
    let a = spam_test_server_for(&["mini", "leni"], SpamConfig::default(), Some("v=DMARC1; p=none")).await;
    a.smtp.store().set_forward_address("kasse@a.test", vec!["leni@a.test".into()], "").await.unwrap();

    let message = "From: news@sender.test\r\nSubject: Gewinn\r\n\r\nAngebot\r\n";
    let reply = relay_message_to(&a, &["kasse@a.test"], message).await;
    assert!(reply.starts_with("550 5.7.1"), "the sender learns it did not arrive: {reply}");
    let reply = relay_message_to(&a, &["kasse@a.test", "mini@a.test"], message).await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Junk).await.len(), 1);
    assert!(a.inbox("leni@a.test").await.is_empty() && a.mailbox("leni@a.test", MailboxRole::Junk).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_are_read_by_the_server_instead_of_landing_in_a_mailbox() {
    let a = start("a.test", &["mini"], &[]).await;
    let store = a.smtp.store().clone();
    // Report addresses win over the catch-all.
    store.set_catch_all("a.test", Some("mini@a.test")).await.unwrap();
    for name in ["reporter.test", "mx.reporter.test", "_dmarc.reporter.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }

    let dmarc = "<?xml version=\"1.0\"?><feedback><report_metadata><org_name>reporter.test</org_name>\
        <email>dmarc@reporter.test</email><report_id>r-1</report_id>\
        <date_range><begin>1757894400</begin><end>1757980799</end></date_range></report_metadata>\
        <policy_published><domain>a.test</domain><p>quarantine</p></policy_published>\
        <record><row><source_ip>192.0.2.10</source_ip><count>7</count><policy_evaluated>\
        <disposition>none</disposition><dkim>pass</dkim><spf>pass</spf></policy_evaluated></row>\
        <identifiers><header_from>a.test</header_from></identifiers><auth_results></auth_results></record></feedback>";
    let tls = r#"{"organization-name":"reporter.test","date-range":{"start-datetime":"2026-09-14T00:00:00Z","end-datetime":"2026-09-14T23:59:59Z"},"report-id":"t-1","policies":[{"policy":{"policy-type":"sts","policy-domain":"a.test","mx-host":["mx.a.test"]},"summary":{"total-successful-session-count":9,"total-failure-session-count":1},"failure-details":[{"result-type":"certificate-not-trusted","failed-session-count":1}]}]}"#;
    let messages = [
        ("dmarc-reports@a.test", "text/xml; name=report.xml".to_owned(), dmarc.to_owned()),
        ("tls-reports@a.test", "application/tlsrpt+json; name=report.json".to_owned(), tls.to_owned()),
    ];
    for (to, content_type, body) in messages {
        let mut session = RawSession::connect(a.mx).await;
        assert!(session.command("EHLO mx.reporter.test").await.starts_with("250"));
        assert!(session.command("MAIL FROM:<reports@reporter.test>").await.starts_with("250"));
        // Every reply line ends with CRLF, report addresses included (audit finding S-3).
        let accepted = session.command(&format!("RCPT TO:<{to}>")).await;
        assert!(accepted.starts_with("250") && accepted.ends_with("\r\n"), "{accepted:?}");
        assert!(session.command("DATA").await.starts_with("354"));
        let message = format!(
            "From: reports@reporter.test\r\nTo: {to}\r\nSubject: Report Domain: a.test\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nA report.\r\n\
             --b\r\nContent-Type: {content_type}\r\nContent-Disposition: attachment\r\n\r\n{body}\r\n--b--\r\n."
        );
        let reply = session.command(&message).await;
        assert!(reply.starts_with("250"), "{reply}");
    }

    let started = Instant::now();
    let summary = loop {
        let summary = store.report_summary("a.test", 0).await.unwrap();
        if summary.dmarc.reports == 1 && summary.tls.reports == 1 {
            break summary;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "reports were not stored: {summary:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!((summary.dmarc.messages, summary.dmarc.passed), (7, 7));
    assert_eq!((summary.tls.successful, summary.tls.failed), (9, 1));
    assert_eq!(summary.tls.failures[0].result_type, "certificate-not-trusted");
    assert!(a.inbox("mini@a.test").await.is_empty(), "nothing went to the catch-all");
}

/// Hands in mail claiming to be from bank.test, straight from 127.0.0.1, which bank.test's SPF
/// record does not allow, without a DKIM signature: what a plain forgery looks like.
async fn forged_bank_mail(server: &TestServer, dmarc: &str) -> String {
    server.smtp.dns_cache().pin_txt("bank.test", "v=spf1 ip4:198.51.100.1 -all").unwrap();
    server.smtp.dns_cache().pin_txt("_dmarc.bank.test", dmarc).unwrap();
    server.smtp.dns_cache().pin_no_txt("mail.bank.test");

    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO mail.bank.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<security@bank.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    session.command("From: security@bank.test\r\nSubject: Konto gesperrt\r\n\r\nBitte hier anmelden\r\n.").await
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_mail_from_a_domain_that_rejects_it_is_refused() {
    let a = start("a.test", &["mini"], &[]).await;
    let reply = forged_bank_mail(&a, "v=DMARC1; p=reject").await;
    assert!(reply.starts_with("550 5.7.1"), "neither SPF nor DKIM aligns, so DMARC fails: {reply}");
    assert!(a.inbox("mini@a.test").await.is_empty());
}

/// security-audit-0.8.0 T-2: DMARC does not judge a From whose addresses lie in several domains, so a
/// sender whose own SPF passes could name a domain that rejects forgeries next to itself. Such a
/// message is refused; several authors of one domain are fine.
#[tokio::test(flavor = "multi_thread")]
async fn a_from_with_addresses_in_several_domains_is_refused() {
    let a = start("a.test", &["mini"], &[]).await;
    a.smtp.dns_cache().pin_txt("sender.test", "v=spf1 ip4:127.0.0.1 -all").unwrap();
    a.smtp.dns_cache().pin_txt("bank.test", "v=spf1 ip4:198.51.100.1 -all").unwrap();
    a.smtp.dns_cache().pin_txt("_dmarc.bank.test", "v=DMARC1; p=reject").unwrap();
    for name in ["_dmarc.sender.test", "mail.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let send = async |from: &str| {
        let mut session = RawSession::connect(a.mx).await;
        assert!(session.command("EHLO mail.sender.test").await.starts_with("250"));
        assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
        assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
        assert!(session.command("DATA").await.starts_with("354"));
        session.command(&format!("From: {from}\r\nSender: news@sender.test\r\nSubject: Konto\r\n\r\nHallo\r\n.")).await
    };

    let reply = send("news@sender.test, Bank <security@bank.test>").await;
    assert!(reply.starts_with("550 5.7.1"), "{reply}");
    assert!(a.inbox("mini@a.test").await.is_empty());

    let reply = send("news@sender.test, Leni <leni@SENDER.test>").await;
    assert!(reply.starts_with("250"), "{reply}");
    a.wait_for_inbox("mini@a.test", 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_mail_from_a_domain_that_quarantines_it_goes_to_junk() {
    let a = start("a.test", &["mini"], &[]).await;
    let reply = forged_bank_mail(&a, "v=DMARC1; p=quarantine").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert!(a.inbox("mini@a.test").await.is_empty(), "a quarantined forgery stays out of the inbox");
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Junk).await.len(), 1);
}

/// A server behind a trusted relay on 127.0.0.1. The sender's SPF record does not allow the
/// address the relay got the message from, so the filter has something to count without asking
/// blocklists. The reverse name of that address may or may not resolve here; the thresholds in the
/// tests hold either way.
async fn spam_test_server(spam: SpamConfig, dmarc: Option<&str>) -> TestServer {
    spam_test_server_for(&["mini"], spam, dmarc).await
}

async fn spam_test_server_for(users: &[&str], spam: SpamConfig, dmarc: Option<&str>) -> TestServer {
    let config = SmtpConfig { trusted_relays: vec!["127.0.0.1".into()], ..SmtpConfig::default() };
    let spam = SpamConfig { blocklists: false, greylist_delay_secs: 0, ..spam };
    let a = start_with_spam("a.test", users, &[], config, spam).await;
    a.smtp.dns_cache().pin_txt("sender.test", "v=spf1 ip4:198.51.100.1 -all").unwrap();
    a.smtp.dns_cache().pin_no_txt("mail.sender.test");
    match dmarc {
        Some(record) => a.smtp.dns_cache().pin_txt("_dmarc.sender.test", record).unwrap(),
        None => a.smtp.dns_cache().pin_no_txt("_dmarc.sender.test"),
    }
    a
}

/// Hands in a message that the relay received from 203.0.113.7; returns the reply to the data.
async fn relay_from_outside(server: &TestServer) -> String {
    relay_message_from_outside(
        server,
        "X-Spam-Status: No, score=-99.0\r\nFrom: news@sender.test\r\nSubject: Nur heute\r\n\r\nAngebot\r\n",
    )
    .await
}

/// Hands in `message` (headers and body) the way the relay received it from 203.0.113.7, with the Date
/// and Message-ID every mail program writes, so only what the message itself adds is judged.
async fn relay_message_from_outside(server: &TestServer, message: &str) -> String {
    relay_message_to(server, &["mini@a.test"], message).await
}

async fn relay_message_to(server: &TestServer, recipients: &[&str], message: &str) -> String {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO relay.local").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    for recipient in recipients {
        assert!(session.command(&format!("RCPT TO:<{recipient}>")).await.starts_with("250"));
    }
    assert!(session.command("DATA").await.starts_with("354"));
    let id = uwumail_store::BlobHash::of(message.as_bytes());
    session
        .command(&format!(
            "Received: from mail.sender.test (mail.sender.test [203.0.113.7])\r\n\
             \tby relay.local (Postfix) with ESMTPS id 4F2;\r\n\
             \t{date}\r\n\
             Date: {date}\r\nMessage-ID: <{id}@sender.test>\r\n{message}.",
            date = mail_builder::headers::date::Date::now().to_rfc822(),
            id = &id.as_str()[..16],
        ))
        .await
}

/// A message wrapped in message/rfc822 thousands of times over overflowed the stack while it was
/// parsed and dropped, which took the whole server down, every time the sender retried
/// (security-audit-0.16.0 SMTP-1). It is refused at the door now, and the server stays up.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_nested_too_deep_is_refused_and_the_server_stays_up() {
    let a = spam_test_server(SpamConfig::default(), None).await;
    let mut message = String::from("From: news@sender.test\r\nSubject: Matroschka\r\n");
    for _ in 0..2_000 {
        message.push_str("Content-Type: message/rfc822\r\n\r\nSubject: layer\r\n");
    }
    message.push_str("\r\nbottom\r\n");
    let reply = relay_message_from_outside(&a, &message).await;
    assert!(reply.starts_with("554 5.6.0"), "{reply}");

    // Too many header fields are refused the same way (SMTP-4).
    let fields = "X-A: b\r\n".repeat(uwumail_store::mime_limits::MAX_HEADER_FIELDS + 1);
    let reply = relay_message_from_outside(&a, &format!("From: news@sender.test\r\n{fields}\r\nHallo\r\n")).await;
    assert!(reply.starts_with("554 5.6.0"), "{reply}");
    assert!(a.inbox("mini@a.test").await.is_empty());

    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO relay.local").await.starts_with("250"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phishing_mail_is_judged_by_what_it_contains() {
    let a = spam_test_server(SpamConfig::default(), None).await;
    let message = "From: \"service@bank.example\" <news@sender.test>\r\nSubject: Ihr Konto\r\n\
        MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n\
        --m\r\nContent-Type: text/html\r\n\r\n<a href=\"https://login.evil.example/\">www.bank.example</a>\r\n\
        --m\r\nContent-Type: application/octet-stream\r\n\
        Content-Disposition: attachment; filename=\"rechnung.pdf.exe\"\r\n\
        Content-Transfer-Encoding: base64\r\n\r\nTVo=\r\n--m--\r\n";

    let reply = relay_message_from_outside(&a, message).await;
    assert!(reply.starts_with("250"), "{reply}");
    let junk = a.mailbox("mini@a.test", MailboxRole::Junk).await;
    assert_eq!(junk.len(), 1, "the tricks alone are enough for Junk");
    let raw = a.raw(&junk[0]).await;
    for rule in ["FROM_NAME_SPOOFS_ADDRESS", "PHISHING_LINK_TEXT", "EXECUTABLE_ATTACHMENT", "DISGUISED_ATTACHMENT"] {
        assert!(raw.contains(rule), "{rule} in {raw}");
    }
    assert!(!raw.contains("MISSING_DATE") && !raw.contains("MISSING_MESSAGE_ID"), "{raw}");
}

#[tokio::test(flavor = "multi_thread")]
async fn suspicious_mail_is_greylisted_once_and_then_delivered_with_its_score() {
    let a = spam_test_server(SpamConfig::default(), None).await;

    let first = relay_from_outside(&a).await;
    assert!(first.starts_with("451 4.7.1"), "a suspicious first attempt has to wait: {first}");
    assert!(a.inbox("mini@a.test").await.is_empty());

    let retry = relay_from_outside(&a).await;
    assert!(retry.starts_with("250"), "the retry is let through: {retry}");
    assert_eq!((a.counted(Stat::RefusedGreylisted), a.counted(Stat::Received), a.counted(Stat::Junk)), (1, 1, 0));
    let inbox = a.inbox("mini@a.test").await;
    assert_eq!(inbox.len(), 1);
    let raw = a.raw(&inbox[0]).await;
    assert!(raw.contains("X-Spam-Status: No, score="), "{raw}");
    assert!(raw.contains("tests=SPF_FAIL,NO_AUTH"), "{raw}");
    assert_eq!(raw.matches("X-Spam-Status:").count(), 1, "the sender's own verdict is gone: {raw}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_over_the_junk_score_is_filed_as_junk_and_counts_against_the_sender() {
    // A published DMARC policy that both alignments fail adds enough for Junk; p=none alone
    // would let it into the inbox.
    let a = spam_test_server(SpamConfig::default(), Some("v=DMARC1; p=none")).await;

    let reply = relay_from_outside(&a).await;
    assert!(reply.starts_with("250"), "junk is accepted, not refused: {reply}");
    assert!(a.inbox("mini@a.test").await.is_empty());
    let junk = a.mailbox("mini@a.test", MailboxRole::Junk).await;
    assert_eq!(junk.len(), 1);
    let raw = a.raw(&junk[0]).await;
    assert!(raw.contains("X-Spam-Status: Yes, score="), "{raw}");
    assert!(raw.contains("DMARC_FAIL"), "{raw}");
    assert_eq!((a.counted(Stat::Received), a.counted(Stat::Junk)), (1, 1));

    // The sender is not vouched for by DMARC, so its network carries the count.
    let store = a.smtp.store();
    let network = || "network:203.0.113.0/24".to_owned();
    let reputation = store.reputation(network()).await.unwrap();
    assert_eq!((reputation.good, reputation.junk), (0, 1));

    // Someone says "Not spam" by moving it to the inbox: the same count moves to the good side.
    let account = store.account("mini@a.test").await.unwrap().unwrap();
    let inbox = store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
    let update = EmailUpdate {
        id: junk[0].id,
        keywords: KeywordsChange::Keep,
        mailboxes: MailboxesChange::Replace(vec![inbox.unwrap().id]),
    };
    assert!(store.update_emails(account.id, vec![update]).await.unwrap().iter().all(Result::is_ok));
    let reputation = store.reputation(network()).await.unwrap();
    assert_eq!((reputation.good, reputation.junk), (1, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_is_only_refused_once_a_reject_score_is_set() {
    let spam = SpamConfig { reject_score: Some(5.0), ..SpamConfig::default() };
    let a = spam_test_server(spam, Some("v=DMARC1; p=none")).await;

    let reply = relay_from_outside(&a).await;
    assert!(reply.starts_with("550 5.7.1"), "{reply}");
    assert!(a.mailbox("mini@a.test", MailboxRole::Junk).await.is_empty());
    assert_eq!((a.counted(Stat::RefusedSpam), a.counted(Stat::Received)), (1, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn people_set_their_own_limits_and_mail_is_refused_once_all_of_them_refuse() {
    let a = spam_test_server_for(&["mini", "leni"], SpamConfig::default(), Some("v=DMARC1; p=none")).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let leni = store.account("leni@a.test").await.unwrap().unwrap().id;
    let both = ["mini@a.test", "leni@a.test"];
    let message = |subject: &str| format!("From: news@sender.test\r\nSubject: {subject}\r\n\r\nAngebot\r\n");

    let lenient = SpamLimits { junk: Some(30.0), reject: None };
    store.set_spam_limits(leni, lenient).await.unwrap();
    let strict = SpamLimits { junk: None, reject: Some(5.0) };
    store.set_spam_limits(mini, strict).await.unwrap();
    let reply = relay_message_to(&a, &both, &message("Eins")).await;
    assert!(reply.starts_with("250"), "Leni still wants it: {reply}");
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Junk).await.len(), 1, "one recipient cannot be refused alone");
    assert_eq!(a.inbox("leni@a.test").await.len(), 1, "over the server's junk score, under Leni's own");

    store.set_spam_limits(leni, strict).await.unwrap();
    let reply = relay_message_to(&a, &both, &message("Zwei")).await;
    assert!(reply.starts_with("550 5.7.1"), "{reply}");
    assert_eq!(a.inbox("leni@a.test").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_from_our_own_network_is_not_judged() {
    // Every judged message would be junk with these numbers.
    let spam = SpamConfig { greylist_score: 0.0, junk_score: 0.0, ..SpamConfig::default() };
    let a = start_with_spam("a.test", &["mini"], &[], SmtpConfig::default(), spam).await;
    a.smtp.dns_cache().pin_txt("sender.test", "v=spf1 ip4:198.51.100.1 -all").unwrap();
    for name in ["mail.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }

    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO mail.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session.command("From: news@sender.test\r\nSubject: Scan\r\n\r\nPDF\r\n.").await;
    assert!(reply.starts_with("250"), "{reply}");

    let inbox = a.inbox("mini@a.test").await;
    assert_eq!(inbox.len(), 1, "127.0.0.1 is in our own network");
    assert!(!a.raw(&inbox[0]).await.contains("X-Spam-"));
}

/// Stores a message in `login`'s inbox and queues it to be learned, as if a person had marked it:
/// for the whole server, or with `personal` for that person.
async fn teach(server: &TestServer, login: &str, personal: bool, spam: bool, raw: String) {
    let store = server.smtp.store();
    let account = store.account(login).await.unwrap().unwrap().id;
    let request = IngestRequest {
        account_id: account,
        raw: raw.into_bytes(),
        mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
        keywords: vec![],
        received_at: None,
    };
    let stored = store.ingest(request).await.unwrap();
    store.queue_bayes_learning(stored.blob, personal.then_some(account), spam).await.unwrap();
}

async fn wait_until_learned(server: &TestServer, account: Option<i64>, expected: BayesTotals) {
    let started = Instant::now();
    loop {
        let totals = server.smtp.store().bayes_totals(account).await.unwrap();
        if totals == expected {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "learned {totals:?}, expected {expected:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bayes_filter_learns_and_a_person_can_see_it_differently() {
    let a = spam_test_server_for(&["mini", "leni"], SpamConfig::default(), None).await;
    let offer = |n: u32| {
        format!(
            "From: deals@shop.test\r\nSubject: Gratis Gewinnspiel {n}\r\n\r\nJetzt gratis teilnehmen und Luxusuhren gewinnen, nur heute {n}\r\n"
        )
    };
    let school = |n: u32| {
        format!(
            "From: verein@schule.test\r\nSubject: Elternabend {n}\r\n\r\nLiebe Eltern, der Elternabend der Klasse findet am Dienstag statt {n}\r\n"
        )
    };
    // The server learns offers as spam and school mail as wanted. Leni sees it the other way round.
    for n in 0..55 {
        teach(&a, "mini@a.test", false, true, offer(n)).await;
        teach(&a, "mini@a.test", false, false, school(n)).await;
        teach(&a, "leni@a.test", true, false, offer(n + 100)).await;
        teach(&a, "leni@a.test", true, true, school(n + 100)).await;
    }
    let leni = a.smtp.store().account("leni@a.test").await.unwrap().unwrap().id;
    wait_until_learned(&a, None, BayesTotals { spam: 55, ham: 55 }).await;
    wait_until_learned(&a, Some(leni), BayesTotals { spam: 55, ham: 55 }).await;

    let reply = relay_message_to(&a, &["mini@a.test", "leni@a.test"], &offer(999)).await;
    assert!(reply.starts_with("250"), "{reply}");
    let for_mini = a.mailbox("mini@a.test", MailboxRole::Junk).await;
    assert_eq!(for_mini.len(), 1, "the server's knowledge puts the offer into Junk");
    let raw = a.raw(&for_mini[0]).await;
    assert!(raw.contains("BAYES_SPAM"), "{raw}");
    assert!(a.mailbox("leni@a.test", MailboxRole::Junk).await.is_empty(), "Leni's own knowledge keeps it out of Junk");
    assert_eq!(a.inbox("leni@a.test").await.iter().filter(|email| email.subject.contains("999")).count(), 1);

    // Wanted mail by the server's knowledge gets points taken off.
    let reply = relay_message_to(&a, &["mini@a.test"], &school(998)).await;
    assert!(reply.starts_with("250"), "{reply}");
    let inbox = a.inbox("mini@a.test").await;
    let school_mail = inbox.iter().find(|email| email.subject.contains("998")).expect("in the inbox");
    assert!(a.raw(school_mail).await.contains("BAYES_HAM"));
}

fn list_entry(scope: ListScope, list: SenderList, value: &str) -> NewSenderListEntry {
    NewSenderListEntry {
        scope,
        list,
        kind: None,
        value: value.into(),
        note: String::new(),
        created_by: String::new(),
        expires_at: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn listed_senders_skip_the_filter_or_are_kept_out() {
    let a = spam_test_server_for(&["mini", "leni"], SpamConfig::default(), None).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let leni = store.account("leni@a.test").await.unwrap().unwrap().id;
    let both = ["mini@a.test", "leni@a.test"];

    // Leni allows the sending server's network: the suspicious message waits for nobody.
    store
        .add_sender_list_entry(list_entry(ListScope::Account(leni), SenderList::Allow, "203.0.113.0/24"))
        .await
        .unwrap();
    let reply = relay_message_to(&a, &both, "From: news@sender.test\r\nSubject: Nur heute\r\n\r\nAngebot\r\n").await;
    assert!(reply.starts_with("250"), "an allowed sender is not greylisted: {reply}");
    assert_eq!(a.inbox("leni@a.test").await.len(), 1);
    assert_eq!(a.inbox("mini@a.test").await.len(), 1, "the score alone is not enough for Junk");

    // Mini blocks the From address: only her copy goes to Junk.
    store
        .add_sender_list_entry(list_entry(ListScope::Account(mini), SenderList::Block, "News@Sender.test"))
        .await
        .unwrap();
    let reply = relay_message_to(&a, &both, "From: news@sender.test\r\nSubject: Noch einmal\r\n\r\nAngebot\r\n").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.inbox("leni@a.test").await.len(), 2);
    assert_eq!(a.inbox("mini@a.test").await.len(), 1);
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Junk).await.len(), 1);

    // The server blocks every host under the sender's domain whose name is confirmed both ways, which
    // outranks Leni's allowance: the message is refused.
    let client = "203.0.113.7".parse().unwrap();
    a.smtp.dns_cache().pin_ptr(client, &["mail.sender.test"]);
    a.smtp.dns_cache().pin_ipv4("mail.sender.test", &["203.0.113.7".parse().unwrap()]);
    store.add_sender_list_entry(list_entry(ListScope::Server, SenderList::Block, "*.sender.test")).await.unwrap();
    let reply =
        relay_message_to(&a, &both, "From: news@sender.test\r\nSubject: Letzte Chance\r\n\r\nAngebot\r\n").await;
    assert!(reply.starts_with("550 5.7.1"), "{reply}");
    assert_eq!(a.inbox("leni@a.test").await.len(), 2);

    // Every entry remembers how often it decided: counted beside the message, so give it a moment.
    let hits = |value: &'static str| async move {
        let query = uwumail_store::RuleQuery { search: value.into(), limit: 10, ..Default::default() };
        store.rules(query).await.unwrap().rules.iter().map(|rule| rule.hits).sum::<i64>()
    };
    for _ in 0..50 {
        if hits("203.0.113.0/24").await >= 2 && hits("*.sender.test").await >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(hits("203.0.113.0/24").await, 2, "Leni's allowance decided her first two copies");
    assert_eq!(hits("news@sender.test").await, 1);
    assert_eq!(hits("*.sender.test").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn word_lists_count_for_everyone_or_only_for_their_owner() {
    // No greylisting, so the sender's 3.0 points (SPF_FAIL, NO_AUTH) alone deliver into the inbox.
    let spam = SpamConfig { greylist_score: 5.0, ..SpamConfig::default() };
    let a = spam_test_server_for(&["mini", "leni"], spam, None).await;
    let store = a.smtp.store();
    let leni = store.account("leni@a.test").await.unwrap().unwrap().id;
    let both = ["mini@a.test", "leni@a.test"];
    let add = |scope, text: &str, points| store.add_words(scope, text.into(), points, String::new(), String::new());

    add(uwumail_store::ListScope::Server, "casino\n/\\sjackpot\\s/i", None).await.unwrap();
    add(uwumail_store::ListScope::Account(leni), "sonderangebot", Some(3.0)).await.unwrap();

    let reply =
        relay_message_to(&a, &both, "From: news@sender.test\r\nSubject: Einladung\r\n\r\nHeute Casino-Abend\r\n").await;
    assert!(reply.starts_with("250"), "{reply}");
    let junk = a.mailbox("mini@a.test", MailboxRole::Junk).await;
    assert_eq!(junk.len(), 1, "a word on the server's list counts for everyone");
    let raw = a.raw(&junk[0]).await;
    assert!(raw.contains("BAD_WORDS"), "{raw}");
    assert_eq!(a.mailbox("leni@a.test", MailboxRole::Junk).await.len(), 1);

    let reply =
        relay_message_to(&a, &both, "From: news@sender.test\r\nSubject: Nur heute\r\n\r\nUnser Sonderangebot\r\n")
            .await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.inbox("mini@a.test").await.len(), 1, "Leni's own word is none of Mini's business");
    assert_eq!(a.mailbox("leni@a.test", MailboxRole::Junk).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn built_in_lists_know_malware_links_throwaway_senders_and_shorteners() {
    let feeds = uwumail_smtp::FeedsConfig { abuse_ch_key: Some("testkey123".into()), ..Default::default() };
    let spam = SpamConfig { greylist_score: 5.0, feeds, ..SpamConfig::default() };
    let a = spam_test_server(spam, None).await;
    let store = a.smtp.store();
    store.replace_feed("urlhaus", vec!["https://files.evil.example/rechnung.exe".into()], None).await.unwrap();
    store.replace_feed("disposable", vec!["sender.test".into()], None).await.unwrap();
    store.replace_feed("redirectors", vec!["bit.example".into()], None).await.unwrap();
    store.replace_feed("freemail", vec!["freemail.example".into()], None).await.unwrap();
    store.replace_feed("bad_subjects", vec!["/Rekord.+Jackpot/i".into()], None).await.unwrap();

    let message = "From: news@sender.test\r\nReply-To: kasse@freemail.example\r\nSubject: Rekord Jackpot\r\n\
        MIME-Version: 1.0\r\nContent-Type: text/html\r\n\r\n\
        <a href=\"https://Files.Evil.example/rechnung.exe#jetzt\">Rechnung</a> <a href=\"https://mail.bit.example/x\">mehr</a>\r\n";
    let reply = relay_message_from_outside(&a, message).await;
    assert!(reply.starts_with("250"), "{reply}");
    let junk = a.mailbox("mini@a.test", MailboxRole::Junk).await;
    assert_eq!(junk.len(), 1, "a known malware link is enough for Junk");
    let raw = a.raw(&junk[0]).await;
    for rule in ["MALWARE_LINK", "DISPOSABLE_FROM", "LINK_SHORTENER", "FREEMAIL_REPLYTO", "BAD_WORDS"] {
        assert!(raw.contains(rule), "{rule} in {raw}");
    }

    // Switched off, a list counts no more, even with its values still stored.
    let mut settings = a.smtp.spam_settings();
    settings.feeds.urlhaus = false;
    settings.feeds.disposable = false;
    let smtp = SmtpConfig { trusted_relays: vec!["127.0.0.1".into()], ..SmtpConfig::default() };
    a.smtp.update_settings(smtp, settings, DeliveryConfig::default(), ToneConfig::default()).unwrap();
    let reply = relay_message_from_outside(
        &a,
        "From: news@sender.test\r\nSubject: Hallo\r\n\r\nhttps://files.evil.example/rechnung.exe\r\n",
    )
    .await;
    assert!(reply.starts_with("250"), "{reply}");
    let inbox = a.inbox("mini@a.test").await;
    assert_eq!(inbox.len(), 1);
    let raw = a.raw(&inbox[0]).await;
    assert!(!raw.contains("MALWARE_LINK") && !raw.contains("DISPOSABLE_FROM"), "{raw}");
}

/// A stand-in clamd that reads a whole INSTREAM and then says `reply` to it.
async fn fake_clamd(reply: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut command = Vec::new();
            let mut byte = [0u8; 1];
            while let Ok(1) = stream.read(&mut byte).await {
                if byte[0] == 0 {
                    break;
                }
                command.push(byte[0]);
            }
            if command == b"zINSTREAM" {
                loop {
                    let mut length = [0u8; 4];
                    if stream.read_exact(&mut length).await.is_err() {
                        break;
                    }
                    let length = u32::from_be_bytes(length) as usize;
                    if length == 0 {
                        break;
                    }
                    let mut chunk = vec![0u8; length];
                    if stream.read_exact(&mut chunk).await.is_err() {
                        break;
                    }
                }
            }
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.write_all(b"\0").await;
        }
    });
    address
}

fn with_scanner(address: String) -> SpamConfig {
    SpamConfig {
        antivirus: AntivirusConfig { enabled: true, address, timeout_secs: 5, ..AntivirusConfig::default() },
        ..SpamConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_with_a_virus_is_turned_away_and_a_scanner_that_is_away_never_stops_the_post() {
    let message = "From: news@sender.test\r\nSubject: Rechnung\r\n\r\nanbei\r\n";
    let found = spam_test_server(with_scanner(fake_clamd("stream: Win.Test.EICAR_HDB-1 FOUND").await), None).await;
    let reply = relay_message_from_outside(&found, message).await;
    assert!(reply.starts_with("554"), "{reply}");
    assert!(reply.contains("Win.Test.EICAR_HDB-1"), "{reply}");
    assert!(found.inbox("mini@a.test").await.is_empty(), "nothing of it reaches a mailbox");
    let entries =
        found.smtp.store().spam_log(uwumail_store::SpamLogFilter { limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(entries.first().map(|entry| entry.action.as_str()), Some("virus"));
    assert_eq!((found.counted(Stat::RefusedVirus), found.counted(Stat::Received)), (1, 0));

    // A scanner nobody can reach must not stop the post; the message says that nobody looked, and
    // whatever the sender claimed about a scan of their own is gone.
    let away = spam_test_server(with_scanner("127.0.0.1:1".into()), None).await;
    let message = "X-Virus-Scanned: yes (trust me)\r\nFrom: news@sender.test\r\nSubject: Nur heute\r\n\r\nAngebot\r\n";
    // Mail from a stranger waits once for the greylist, as everywhere else in these tests.
    assert!(relay_message_from_outside(&away, message).await.starts_with("451"));
    let reply = relay_message_from_outside(&away, message).await;
    assert!(reply.starts_with("250"), "{reply}");
    let inbox = away.wait_for_inbox("mini@a.test", 1).await;
    let raw = away.raw(&inbox[0]).await;
    assert!(raw.contains("X-Virus-Scanned: no (the virus scanner did not answer)"), "{raw}");
    assert!(!raw.contains("trust me"), "{raw}");
}

/// Hands in one message and returns the reply to RCPT TO, without sending any data.
async fn offer_to(server: &TestServer, recipient: &str) -> String {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO relay.local").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    session.command(&format!("RCPT TO:<{recipient}>")).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_that_only_sends_takes_no_mail_unless_it_names_a_place_for_it() {
    let a = start("a.test", &["mini", "ami"], &[]).await;
    let store = a.smtp.store();
    store
        .create_account(NewAccount {
            address: "monitoring@a.test".into(),
            display_name: "Monitoring".into(),
            password: None,
            role: Role::Service,
            quota_bytes: 0,
            protocols: Some(uwumail_store::Protocols {
                smtp: true,
                imap: false,
                jmap: false,
                caldav: false,
                carddav: false,
            }),
        })
        .await
        .unwrap();

    // Nothing to deliver into: said at the door, not swallowed.
    let refused = offer_to(&a, "monitoring@a.test").await;
    assert!(refused.starts_with("550"), "{refused}");
    assert!(refused.contains("does not take mail"), "{refused}");

    // With a place named for it, the mail lands there instead.
    store
        .update_account(
            "monitoring@a.test",
            uwumail_store::AccountUpdate { redirect_to: Some("ami@a.test".into()), ..Default::default() },
        )
        .await
        .unwrap();
    let message = "From: news@sender.test\r\nSubject: Platte fast voll\r\n\r\nbitte nachsehen\r\n";
    let reply = relay_message_to(&a, &["monitoring@a.test"], message).await;
    assert!(reply.starts_with("250"), "{reply}");
    let inbox = a.wait_for_inbox("ami@a.test", 1).await;
    assert_eq!(inbox[0].subject, "Platte fast voll");
    assert!(a.inbox("mini@a.test").await.is_empty(), "only the address that was named gets it");

    // A service may still send, and what it sends is signed and delivered like anyone's mail.
    let app = store
        .create_app_password(
            store.account("monitoring@a.test").await.unwrap().unwrap().id,
            uwumail_store::NewAppPassword {
                name: "Sender".into(),
                scopes: vec![uwumail_store::AppScope::Smtp],
                expires_at: None,
            },
        )
        .await
        .unwrap();
    a.mailer("monitoring@a.test", &app.secret.replace(' ', ""), false)
        .send(mail("Monitoring <monitoring@a.test>", &["mini@a.test"], "Alarm"))
        .await
        .unwrap();
    let delivered = a.wait_for_inbox("mini@a.test", 1).await;
    assert_eq!(delivered[0].subject, "Alarm");
}

/// A message as a provider would hand it over: with the Return-Path it recorded and, when it
/// checked, its own Authentication-Results above everything else.
fn fetched_message(auth: Option<&str>, extra: &str) -> Vec<u8> {
    let auth = auth.map(|line| format!("Authentication-Results: {line}\r\n")).unwrap_or_default();
    format!(
        "{auth}Return-Path: <news@sender.test>\r\n\
         {extra}From: news@sender.test\r\nTo: mini@freemail.example\r\n\
         Subject: Nur heute\r\nMessage-ID: <one@sender.test>\r\n\
         Date: Fri, 18 Sep 2026 10:00:00 +0200\r\n\r\nAngebot\r\n"
    )
    .into_bytes()
}

fn fetched_mailbox(account_id: i64) -> uwumail_smtp::FetchedMailbox {
    uwumail_smtp::FetchedMailbox {
        id: 1,
        account_id,
        address: "mini@freemail.example".into(),
        host: "imap.mail.freemail.example".into(),
        auth_serv_id: String::new(),
    }
}

/// Mail fetched from somewhere else goes through the same pipeline, and what the provider says
/// about it is worth points -- not a verdict.
#[tokio::test(flavor = "multi_thread")]
async fn fetched_mail_is_judged_here_and_the_providers_word_only_adds_points() {
    let a = spam_test_server(SpamConfig::default(), None).await;
    let account = a.smtp.store().account("mini@a.test").await.unwrap().unwrap();
    let mailbox = fetched_mailbox(account.id);
    let take = async |from_junk, raw: Vec<u8>| {
        uwumail_smtp::deliver_fetched(&a.smtp, mailbox.clone(), from_junk, "mini@a.test".into(), raw).await
    };

    // Out of the provider's inbox, with its own header saying everything passed: nothing is held
    // against it, and it lands where it would have landed had it come in at the door.
    let vouched = "mx.freemail.example; spf=pass smtp.mailfrom=news@sender.test; dkim=pass; dmarc=pass";
    assert_eq!(take(false, fetched_message(Some(vouched), "")).await, uwumail_smtp::Taken::Kept);
    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    assert_eq!(inbox[0].subject, "Nur heute");
    let raw = a.raw(&inbox[0]).await;
    assert!(raw.contains("(fetched for mini@freemail.example)"), "the trace says how it got here: {raw}");
    assert!(!raw.contains("PROVIDER_JUNK"), "{raw}");

    // The same message out of the provider's junk folder, with its spam flag on it: this server
    // says so in the score, and with nothing vouching for it that is enough for Junk.
    let flagged = fetched_message(None, "X-Spam-Flag: YES\r\n");
    assert_eq!(take(true, flagged).await, uwumail_smtp::Taken::Kept);
    let junk = a.mailbox("mini@a.test", MailboxRole::Junk).await;
    assert_eq!(junk.len(), 1, "the provider's verdict plus no authentication is enough");
    let raw = a.raw(&junk[0]).await;
    for rule in ["PROVIDER_JUNK", "PROVIDER_SPAM_FLAG", "FETCHED_NO_AUTH"] {
        assert!(raw.contains(rule), "{rule} should be in {raw}");
    }
}

/// Without a provider verdict to go on, the From the person sees still has to be a single one.
#[tokio::test(flavor = "multi_thread")]
async fn fetched_mail_without_a_verdict_still_gets_the_header_checks() {
    let a = spam_test_server(SpamConfig::default(), None).await;
    let account = a.smtp.store().account("mini@a.test").await.unwrap().unwrap();
    let mailbox = fetched_mailbox(account.id);
    let two_from = fetched_message(None, "From: chef@a.test\r\n");
    let two_domains = String::from_utf8(fetched_message(None, ""))
        .unwrap()
        .replace("From: news@sender.test", "From: news@sender.test, chef@a.test")
        .into_bytes();
    for raw in [two_from, two_domains] {
        let taken = uwumail_smtp::deliver_fetched(&a.smtp, mailbox.clone(), false, "mini@a.test".into(), raw).await;
        assert!(matches!(taken, uwumail_smtp::Taken::Refused(ref answer) if answer.starts_with("550")), "{taken:?}");
    }
    assert!(a.mailbox("mini@a.test", MailboxRole::Inbox).await.is_empty());
}

/// A header nobody signed for may count against a message, never for it.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsigned_verdict_in_a_fetched_message_cannot_vouch_for_it() {
    let a = spam_test_server(SpamConfig::default(), None).await;
    let account = a.smtp.store().account("mini@a.test").await.unwrap().unwrap();
    let mailbox = fetched_mailbox(account.id);

    // Somebody else's name on the header: it says nothing at all, so the message is unvouched for.
    let forged = "mx.somewhere-else.example; spf=pass; dkim=pass; dmarc=pass";
    let raw = fetched_message(Some(forged), "");
    assert_eq!(
        uwumail_smtp::deliver_fetched(&a.smtp, mailbox, false, "mini@a.test".into(), raw).await,
        uwumail_smtp::Taken::Kept
    );
    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    let raw = a.raw(&inbox[0]).await;
    assert!(raw.contains("FETCHED_NO_AUTH"), "a stranger's pass vouches for nothing: {raw}");
}

/// With auto-labels switched on (and labels to choose from), delivered mail waits in the assistant's
/// queue; Junk does not, and nothing waits for someone who did not switch them on.
#[tokio::test(flavor = "multi_thread")]
async fn delivered_mail_is_queued_for_labels_but_junk_is_not() {
    let a = spam_test_server(SpamConfig::default(), None).await;
    let store = a.smtp.store();
    let account = store.account("mini@a.test").await.unwrap().unwrap();
    let mailbox = fetched_mailbox(account.id);
    let take = async |from_junk, raw: Vec<u8>| {
        uwumail_smtp::deliver_fetched(&a.smtp, mailbox.clone(), from_junk, "mini@a.test".into(), raw).await
    };
    let vouched = "mx.freemail.example; spf=pass smtp.mailfrom=news@sender.test; dkim=pass; dmarc=pass";
    assert_eq!(take(false, fetched_message(Some(vouched), "")).await, uwumail_smtp::Taken::Kept);
    assert!(store.due_label_jobs(10).await.unwrap().is_empty(), "not switched on");

    store.set_assist_prefs(account.id, Default::default(), true).await.unwrap();
    store.create_assist_label(account.id, "Newsletter".into(), "Werbung und Newsletter".into(), None).await.unwrap();
    let flagged = String::from_utf8(fetched_message(None, "X-Spam-Flag: YES\r\n")).unwrap().replace("<one@", "<two@");
    assert_eq!(take(true, flagged.into_bytes()).await, uwumail_smtp::Taken::Kept);
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Junk).await.len(), 1);
    assert!(store.due_label_jobs(10).await.unwrap().is_empty(), "Junk gets no labels");

    let third = String::from_utf8(fetched_message(Some(vouched), "")).unwrap().replace("<one@", "<three@");
    assert_eq!(take(false, third.into_bytes()).await, uwumail_smtp::Taken::Kept);
    let inbox = a.wait_for_inbox("mini@a.test", 2).await;
    let jobs = store.due_label_jobs(10).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(inbox.iter().any(|email| email.id == jobs[0].email_id));
}

/// A trap address takes mail like a real one, teaches the filter and delivers nowhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_spam_trap_learns_from_what_it_catches_and_keeps_nothing() {
    let spam = SpamConfig { traps: vec!["alt@a.test".into()], ..SpamConfig::default() };
    let a = spam_test_server_for(&["mini"], spam, None).await;
    let before = a.smtp.store().bayes_totals(None).await.unwrap();

    // The trap is not a mailbox here, and it still answers like one.
    let reply =
        relay_message_to(&a, &["alt@a.test"], "From: news@sender.test\r\nSubject: Nur heute\r\n\r\nAngebot\r\n").await;
    assert!(reply.starts_with("250"), "a trap that answers differently stops being one: {reply}");

    // Nothing was delivered anywhere.
    assert!(a.inbox("mini@a.test").await.is_empty());
    assert!(a.mailbox("mini@a.test", MailboxRole::Junk).await.is_empty());

    // And the whole server learned it as spam, exactly once and never as wanted mail.
    wait_until_learned(&a, None, BayesTotals { spam: before.spam + 1, ham: before.ham }).await;
}

/// A trap keeps the transaction accepted so it goes on collecting, but a message the score rejects
/// must still not reach the real co-recipients (security-audit-0.5.2 S-17).
#[tokio::test(flavor = "multi_thread")]
async fn a_spam_trap_does_not_shield_its_co_recipients() {
    let spam = SpamConfig { traps: vec!["alt@a.test".into()], reject_score: Some(3.0), ..SpamConfig::default() };
    let a = spam_test_server_for(&["mini"], spam, None).await;

    let reply = relay_message_to(
        &a,
        &["mini@a.test", "alt@a.test"],
        "From: news@sender.test\r\nSubject: Nur heute\r\n\r\nAngebot\r\n",
    )
    .await;
    assert!(reply.starts_with("250"), "the trap keeps the transaction accepted: {reply}");

    // The real recipient gets nothing -- not even Junk -- although a trap shared the transaction.
    assert!(a.inbox("mini@a.test").await.is_empty(), "a rejected message does not reach the real recipient");
    assert!(a.mailbox("mini@a.test", MailboxRole::Junk).await.is_empty(), "not even Junk");
}

/// Hands in a message from news@sender.test for `to`; returns the reply to the data.
async fn deliver_to(server: &TestServer, to: &str, subject: &str) -> String {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command(&format!("RCPT TO:<{to}>")).await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    session.command(&format!("From: news@sender.test\r\nTo: {to}\r\nSubject: {subject}\r\n\r\nMiau\r\n.")).await
}

async fn folder(server: &TestServer, login: &str, path: &[&str]) -> Option<Vec<EmailSummary>> {
    let store = server.smtp.store();
    let account = store.account(login).await.unwrap().unwrap();
    let mailboxes = store.mailboxes(account.id).await.unwrap();
    let mut parent = None;
    let mut found = None;
    for name in path {
        found = mailboxes.iter().find(|m| m.parent_id == parent && m.name == *name);
        parent = Some(found?.id);
    }
    Some(store.emails_in_mailbox(found?.id, 50).await.unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sieve_script_files_flags_redirects_and_discards_but_leaves_junk_alone() {
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 9));
    let a = start("a.test", &["mini", "leni"], &[("c.test", unreachable)]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let script = br#"require ["fileinto", "imap4flags", "mailbox", "copy"];
if header :contains "subject" "Rechnung" {
    addflag "\\Seen";
    fileinto :create "Finanzen/Rechnungen";
    stop;
}
if header :contains "subject" "Weiter" { redirect :copy "leni@a.test"; }
if header :contains "subject" "Weg" { discard; }
if header :contains "subject" "Fremd" { redirect "fremd@c.test"; }
if header :contains "subject" "Nirgends" { fileinto "Gibt es nicht"; }
"#;
    uwumail_smtp::sieve::validate(script).unwrap();
    let created = store.create_sieve_script(mini, Some("UwUMail"), script).await.unwrap();
    store.activate_sieve_script(mini, Some(created.id)).await.unwrap();

    // Filed into a folder the script creates, marked as read.
    assert!(deliver_to(&a, "mini@a.test", "Rechnung 42").await.starts_with("250"));
    let filed = folder(&a, "mini@a.test", &["Finanzen", "Rechnungen"]).await.expect("the folder was created");
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].keywords, ["$seen"]);
    assert!(a.inbox("mini@a.test").await.is_empty());

    // A copy for someone on this server, one stays.
    assert!(deliver_to(&a, "mini@a.test", "Weiter bitte").await.starts_with("250"));
    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Weiter bitte");
    assert_eq!(a.inbox("mini@a.test").await.len(), 1);

    // Discarded: accepted, stored nowhere.
    assert!(deliver_to(&a, "mini@a.test", "Weg damit").await.starts_with("250"));
    assert_eq!(a.inbox("mini@a.test").await.len(), 1);

    // Elsewhere only after a confirmed forwarding: without one, the message stays here.
    assert!(deliver_to(&a, "mini@a.test", "Fremd").await.starts_with("250"));
    assert_eq!(a.inbox("mini@a.test").await.len(), 2);
    assert!(store.queue_entries().await.unwrap().is_empty(), "nothing went out");

    // A folder that does not exist, without :create: the inbox.
    assert!(deliver_to(&a, "mini@a.test", "Nirgends").await.starts_with("250"));
    assert_eq!(a.inbox("mini@a.test").await.len(), 3);

    // Once confirmed, the redirect goes out -- through the queue, with SRS, like forwarding.
    let (_, token) = store.add_forward_target(mini, "fremd@c.test", true).await.unwrap();
    store.confirm_forward_link(&token.unwrap()).await.unwrap();
    store.set_forward_keep_copy(mini, true).await.unwrap();
    assert!(deliver_to(&a, "mini@a.test", "Fremd again").await.starts_with("250"));
    let queued = store.queue_entries().await.unwrap();
    assert!(queued.iter().all(|entry| entry.message.return_path.starts_with("SRS0=")), "{queued:?}");
    assert!(!queued.is_empty());

    // Junk stays junk: the script does not see it.
    store
        .add_sender_list_entry(list_entry(ListScope::Account(mini), SenderList::Block, "news@sender.test"))
        .await
        .unwrap();
    assert!(deliver_to(&a, "mini@a.test", "Rechnung spam").await.starts_with("250"));
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Junk).await.len(), 1);
    assert_eq!(folder(&a, "mini@a.test", &["Finanzen", "Rechnungen"]).await.unwrap().len(), 1);

    // Leni has no script: her mail is untouched by Mini's.
    assert!(deliver_to(&a, "leni@a.test", "Rechnung für Leni").await.starts_with("250"));
    assert_eq!(a.inbox("leni@a.test").await.len(), 2);
}

/// Labels without a model go on at delivery, before the Sieve script, which sees them as headers;
/// such a header the sender wrote counts for nothing (docs/sieve.md, "Labels").
#[tokio::test(flavor = "multi_thread")]
async fn labels_without_a_model_come_before_the_sieve_script() {
    let a = start("a.test", &["mini"], &[]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let rules = serde_json::json!({ "conditions": [{ "field": "from", "value": "sender.test" }] });
    let news = uwumail_store::AssistLabelWrite {
        rules: Some(rules),
        ..uwumail_store::AssistLabelWrite::simple("Newsletter".into(), String::new(), None)
    };
    let news = store.create_assist_label_with(mini, news).await.unwrap();
    store.create_assist_label(mini, "Fake".into(), String::new(), None).await.unwrap();
    let script = br#"require ["fileinto", "imap4flags", "mailbox"];
if header :is "X-UwUMail-Label" "fake" { fileinto :create "Faked"; stop; }
if anyof (header :is "X-UwUMail-Label" "newsletter", hasflag "newsletter") {
    setflag "\\Flagged";
    fileinto :create "Newsletter";
}
"#;
    uwumail_smtp::sieve::validate(script).unwrap();
    let created = store.create_sieve_script(mini, Some("UwUMail"), script).await.unwrap();
    store.activate_sieve_script(mini, Some(created.id)).await.unwrap();

    let send = async |subject: &str| {
        let mut session = RawSession::connect(a.mx).await;
        assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
        assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
        assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
        assert!(session.command("DATA").await.starts_with("354"));
        let data = format!(
            "X-UwUMail-Label: fake\r\nFrom: news@sender.test\r\nTo: mini@a.test\r\nSubject: {subject}\r\n\r\nAngebot\r\n."
        );
        assert!(session.command(&data).await.starts_with("250"));
    };
    send("Neu im Herbst").await;
    let filed = folder(&a, "mini@a.test", &["Newsletter"]).await.expect("filed by its label");
    assert_eq!(filed.len(), 1);
    // The script's setflag does not take off the label set before it.
    let mut keywords = filed[0].keywords.clone();
    keywords.sort();
    assert_eq!(keywords, ["$flagged", "newsletter"]);
    assert!(folder(&a, "mini@a.test", &["Faked"]).await.is_none(), "the sender's own header is no label");
    let raw = a.raw(&filed[0]).await;
    // Neither the sender's header nor the ones the script saw are kept (security audit 0.21.0
    // LABELS-I1).
    assert!(!raw.contains("X-UwUMail-Label"), "{raw}");
    let log = store.label_log(mini, None, 10).await.unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!((log[0].label_id, log[0].source.as_str(), log[0].code.as_str()), (news.id, "rule", "rule"));
    assert_eq!(log[0].params["conditions"][0]["value"], "sender.test");
    assert!(log[0].provider.is_empty());

    // Switched off, nothing is put on without a model.
    store.set_non_ai_labels(mini, false).await.unwrap();
    send("Noch mehr").await;
    assert_eq!(a.wait_for_inbox("mini@a.test", 1).await[0].keywords, Vec::<String>::new());
    assert_eq!(store.label_log(mini, None, 10).await.unwrap().len(), 1);
}

/// With the sender checks switched off nothing vouches for a From address, so a sender whose mail
/// was labeled by hand twice does not get the label onto mail that merely claims their address
/// (docs/labels.md, "Learned senders"; security audit 0.21.0 LABELS-L3).
#[tokio::test(flavor = "multi_thread")]
async fn learned_senders_need_a_vouched_from_even_without_sender_checks() {
    let config = SmtpConfig { verify_senders: false, ..SmtpConfig::default() };
    let a = start_with("a.test", &["mini"], &[], config).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let label = store.create_assist_label(mini, "Privat".into(), String::new(), None).await.unwrap();
    for subject in ["Eins", "Zwei"] {
        assert!(deliver_to(&a, "mini@a.test", subject).await.starts_with("250"));
    }
    let delivered = a.wait_for_inbox("mini@a.test", 2).await;
    let updates = delivered
        .iter()
        .map(|email| EmailUpdate {
            id: email.id,
            keywords: KeywordsChange::Patch(vec![(label.keyword.clone(), true)]),
            ..EmailUpdate::default()
        })
        .collect();
    assert!(store.update_emails(mini, updates).await.unwrap().iter().all(Result::is_ok));

    assert!(deliver_to(&a, "mini@a.test", "Drei").await.starts_with("250"));
    let inbox = a.wait_for_inbox("mini@a.test", 3).await;
    let third = inbox.iter().find(|email| email.subject == "Drei").unwrap();
    assert!(third.keywords.is_empty(), "{:?}", third.keywords);
    assert!(store.label_log(mini, None, 10).await.unwrap().is_empty());
}

/// A label header hidden behind a bare CR, which one reader takes for the end of a line and another
/// not, never reaches the script as a label (security audit 0.21.0 LABELS-I1). And a subject of one
/// very long word full of detector stems is decided on in time (LABELS-H1).
#[tokio::test(flavor = "multi_thread")]
async fn label_headers_behind_a_bare_cr_and_long_subjects_change_nothing() {
    let a = start("a.test", &["mini"], &[]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    store.create_assist_label(mini, "Fake".into(), String::new(), None).await.unwrap();
    for (name, detector) in [("Termine", "appointment"), ("Rundbriefe", "newsletter")] {
        let label = uwumail_store::AssistLabelWrite {
            detector: Some(detector.into()),
            ..uwumail_store::AssistLabelWrite::simple(name.into(), String::new(), None)
        };
        store.create_assist_label_with(mini, label).await.unwrap();
    }
    let script = br#"require ["fileinto", "mailbox"];
if header :is "X-UwUMail-Label" "fake" { fileinto :create "Faked"; stop; }
"#;
    let created = store.create_sieve_script(mini, Some("UwUMail"), script).await.unwrap();
    store.activate_sieve_script(mini, Some(created.id)).await.unwrap();

    let send = async |data: String| {
        let mut session = RawSession::connect(a.mx).await;
        assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
        assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
        assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
        assert!(session.command("DATA").await.starts_with("354"));
        let started = std::time::Instant::now();
        let answer = session.command(&data).await;
        (answer, started.elapsed())
    };
    let (answer, _) = send(
        "From: news@sender.test\r\nTo: mini@a.test\r\nX-Note: a\rX-UwUMail-Label: fake\r\nSubject: Hallo\r\n\r\nText\r\n."
            .to_owned(),
    )
    .await;
    if answer.starts_with("250") {
        a.wait_for_inbox("mini@a.test", 1).await;
    }
    assert!(folder(&a, "mini@a.test", &["Faked"]).await.is_none(), "{answer}");

    // One word of about 200 000 characters, in encoded words that join without a space.
    let chunk = "=?utf-8?q?Liefertermin?=";
    let subject = vec![chunk; 16_000].join("\r\n ");
    let (answer, took) = send(format!(
        "From: news@sender.test\r\nTo: mini@a.test\r\nList-Unsubscribe: <mailto:off@sender.test>\r\nSubject: {subject}\r\n\r\nText\r\n."
    ))
    .await;
    assert!(answer.starts_with("250"), "{answer}");
    assert!(took < std::time::Duration::from_secs(10), "{took:?}");
}

/// security-audit-0.7.0 S-44: a redirect without `:copy` that reached nobody -- here because the
/// message was already passed on from this address once -- used to take the message with it.
#[tokio::test(flavor = "multi_thread")]
async fn a_sieve_redirect_that_goes_nowhere_keeps_the_message() {
    let a = start("a.test", &["mini", "leni"], &[]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let created = store.create_sieve_script(mini, Some("UwUMail"), b"redirect \"leni@a.test\";").await.unwrap();
    store.activate_sieve_script(mini, Some(created.id)).await.unwrap();

    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session
        .command("Delivered-To: mini@a.test\r\nFrom: news@sender.test\r\nTo: mini@a.test\r\nSubject: Schon hier\r\n\r\nMiau\r\n.")
        .await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.inbox("mini@a.test").await.len(), 1, "the message stays with Mini");
    assert!(a.inbox("leni@a.test").await.is_empty());

    // Without the loop, the redirect works and nothing stays.
    assert!(deliver_to(&a, "mini@a.test", "Weiter").await.starts_with("250"));
    assert_eq!(a.wait_for_inbox("leni@a.test", 1).await[0].subject, "Weiter");
    assert_eq!(a.inbox("mini@a.test").await.len(), 1);
}

/// security-audit-0.7.0 S-48: `fileinto :create` with a name taken from the message let every
/// message make up to 64 folders of up to 64 levels each in the recipient's account.
#[tokio::test(flavor = "multi_thread")]
async fn sieve_makes_only_a_few_folders_for_one_message() {
    let a = start("a.test", &["mini"], &[]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let script = br#"require ["fileinto", "mailbox", "variables", "copy"];
if header :matches "subject" "*" { set "path" "${1}"; }
fileinto :copy :create "${path}";
fileinto :create "Extra/One";
"#;
    uwumail_smtp::sieve::validate(script).unwrap();
    let created = store.create_sieve_script(mini, Some("UwUMail"), script).await.unwrap();
    store.activate_sieve_script(mini, Some(created.id)).await.unwrap();
    let before = store.mailboxes(mini).await.unwrap().len();

    let deep = (1..=20).map(|n| format!("L{n}")).collect::<Vec<_>>().join("/");
    assert!(deliver_to(&a, "mini@a.test", &deep).await.starts_with("250"));
    let made = store.mailboxes(mini).await.unwrap().len() - before;
    assert!(made <= 10, "{made} folders for one message");
    assert_eq!(a.inbox("mini@a.test").await.len(), 1, "what could not be filed stays in the inbox");

    // A message that names few folders still gets them.
    assert!(deliver_to(&a, "mini@a.test", "Kurz/Weg").await.starts_with("250"));
    assert_eq!(folder(&a, "mini@a.test", &["Kurz", "Weg"]).await.map(|m| m.len()), Some(1));
    assert_eq!(folder(&a, "mini@a.test", &["Extra", "One"]).await.map(|m| m.len()), Some(1));
}

/// An OAuth access token for `login`, as an app gets it through the portal (docs/oauth.md).
async fn oauth_token(server: &TestServer, login: &str, scopes: Vec<&'static str>) -> String {
    // RFC 7636 appendix B.
    let (verifier, challenge) =
        ("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    let store = server.smtp.store();
    let account = store.account(login).await.unwrap().unwrap();
    let client = store.register_oauth_client("Test app", vec!["http://127.0.0.1/cb".into()]).await.unwrap();
    let code = store
        .create_oauth_code(uwumail_store::NewOAuthCode {
            client_id: client.id,
            account_id: account.id,
            redirect_uri: "http://127.0.0.1/cb".into(),
            scopes,
            code_challenge: challenge.into(),
            nonce: None,
            auth_time: 0,
        })
        .await
        .unwrap();
    store.redeem_oauth_code(&code, client.id, "http://127.0.0.1/cb", verifier).await.unwrap().unwrap().access_token
}

/// Mail apps that signed in with OAuth send with their access token: XOAUTH2 and OAUTHBEARER.
#[tokio::test(flavor = "multi_thread")]
async fn apps_send_with_oauth_tokens() {
    use base64::Engine as _;
    let base64 = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    let config = SmtpConfig { require_tls_for_auth: false, ..SmtpConfig::default() };
    let a = start_with("a.test", &["mini", "nyu"], &[], config).await;
    let token = oauth_token(&a, "mini@a.test", vec!["smtp"]).await;

    // lettre speaks XOAUTH2, over implicit TLS.
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
        .port(a.submission_tls.port())
        .tls(Tls::Wrapper(
            TlsParameters::builder("localhost".into()).dangerous_accept_invalid_certs(true).build_rustls().unwrap(),
        ))
        .credentials(Credentials::new("mini@a.test".into(), token.clone()))
        .authentication(vec![lettre::transport::smtp::authentication::Mechanism::Xoauth2])
        .timeout(Some(Duration::from_secs(10)))
        .build();
    mailer.send(mail("mini@a.test", &["nyu@a.test"], "Mit Token")).await.unwrap();
    a.wait_for_inbox("nyu@a.test", 1).await;

    let mut session = RawSession::connect(a.submission).await;
    let ehlo = session.command("EHLO client.test").await;
    assert!(ehlo.contains("AUTH PLAIN LOGIN OAUTHBEARER XOAUTH2"), "{ehlo}");
    // A refused token: the JSON error challenge, the app's answer, then 535.
    let wrong = base64(&format!("n,,\x01auth=Bearer {token}x\x01\x01"));
    let challenge = session.command(&format!("AUTH OAUTHBEARER {wrong}")).await;
    let encoded = challenge.trim().strip_prefix("334 ").expect("an error challenge");
    let error: serde_json::Value =
        serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(encoded).unwrap()).unwrap();
    assert_eq!((error["status"].as_str(), error["scope"].as_str()), (Some("invalid_token"), Some("smtp")));
    assert_eq!(error["openid-configuration"], "https://mx.a.test/.well-known/openid-configuration");
    assert!(session.command("AQ==").await.starts_with("535"));
    // A token for reading mail only does not send.
    let reading = oauth_token(&a, "mini@a.test", vec!["mail"]).await;
    let reply = session
        .command(&format!("AUTH OAUTHBEARER {}", base64(&format!("n,,\x01auth=Bearer {reading}\x01\x01"))))
        .await;
    assert!(reply.starts_with("334 "), "{reply}");
    assert!(session.command("AQ==").await.starts_with("535"));
    // The right token after the empty challenge.
    assert!(session.command("AUTH OAUTHBEARER").await.starts_with("334"));
    let right = base64(&format!("n,a=mini@a.test,\x01auth=Bearer {token}\x01\x01"));
    let reply = session.command(&right).await;
    assert!(reply.starts_with("235"), "{reply}");
    assert!(session.command("MAIL FROM:<mini@a.test>").await.starts_with("250"));
}
