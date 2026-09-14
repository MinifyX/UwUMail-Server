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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uwumail_smtp::{DeliveryConfig, ListenerKind, Smtp, SmtpConfig, SmtpSettings, ToneConfig};
use uwumail_store::{EmailSummary, MailboxRole, NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";

struct TestServer {
    smtp: Smtp,
    mx: SocketAddr,
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

async fn start(domain: &str, users: &[&str], routes: &[(&str, SocketAddr)]) -> TestServer {
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
            smtp: SmtpConfig::default(),
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
    tokio::spawn(uwumail_smtp::run_queue(smtp.clone(), rx));

    TestServer { smtp, mx: addrs[0], submission: addrs[1], submission_tls: addrs[2], _shutdown: shutdown, _dir: dir }
}

impl TestServer {
    async fn inbox(&self, login: &str) -> Vec<EmailSummary> {
        let store = self.smtp.store();
        let account = store.account(login).await.unwrap().unwrap();
        let inbox = store
            .mailboxes(account.id)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.role == Some(MailboxRole::Inbox))
            .unwrap();
        store.emails_in_mailbox(inbox.id, 50).await.unwrap()
    }

    async fn wait_for_inbox(&self, login: &str, count: usize) -> Vec<EmailSummary> {
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

    async fn raw(&self, email: &EmailSummary) -> String {
        let hash = uwumail_store::BlobHash::parse(&email.blob).unwrap();
        String::from_utf8(self.smtp.store().blob(&hash).await.unwrap()).unwrap()
    }

    fn mailer(&self, login: &str, password: &str, implicit_tls: bool) -> AsyncSmtpTransport<Tokio1Executor> {
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

fn mail(from: &str, to: &[&str], subject: &str) -> Message {
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
}

#[tokio::test(flavor = "multi_thread")]
async fn submission_rules() {
    let a = start("a.test", &["mini", "ami"], &[]).await;

    // Wrong password.
    assert!(a.mailer("mini@a.test", "falsch", false).send(mail("mini@a.test", &["ami@a.test"], "x")).await.is_err());
    // Sending as someone else.
    assert!(a.mailer("mini@a.test", PASSWORD, false).send(mail("ami@a.test", &["ami@a.test"], "x")).await.is_err());

    // Without TLS there is no AUTH, and MAIL needs a login.
    let mut session = RawSession::connect(a.submission).await;
    let ehlo = session.command("EHLO client.test").await;
    assert!(ehlo.contains("STARTTLS") && !ehlo.contains("AUTH"), "{ehlo}");
    assert!(session.command("MAIL FROM:<mini@a.test>").await.starts_with("530"));
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

struct RawSession {
    reader: BufReader<TcpStream>,
}

impl RawSession {
    async fn connect(addr: SocketAddr) -> RawSession {
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

    async fn command(&mut self, command: &str) -> String {
        self.reader.get_mut().write_all(format!("{command}\r\n").as_bytes()).await.unwrap();
        self.read_reply().await
    }
}
