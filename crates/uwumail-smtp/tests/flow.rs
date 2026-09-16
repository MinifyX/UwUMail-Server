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
    start_with(domain, users, routes, SmtpConfig::default()).await
}

async fn start_with(domain: &str, users: &[&str], routes: &[(&str, SocketAddr)], config: SmtpConfig) -> TestServer {
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
        self.mailbox(login, MailboxRole::Inbox).await
    }

    async fn mailbox(&self, login: &str, role: MailboxRole) -> Vec<EmailSummary> {
        let store = self.smtp.store();
        let account = store.account(login).await.unwrap().unwrap();
        let mailbox = store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(role)).unwrap();
        store.emails_in_mailbox(mailbox.id, 50).await.unwrap()
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
        assert!(session.command(&format!("RCPT TO:<{to}>")).await.starts_with("250"));
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

#[tokio::test(flavor = "multi_thread")]
async fn forged_mail_from_a_domain_that_quarantines_it_goes_to_junk() {
    let a = start("a.test", &["mini"], &[]).await;
    let reply = forged_bank_mail(&a, "v=DMARC1; p=quarantine").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert!(a.inbox("mini@a.test").await.is_empty(), "a quarantined forgery stays out of the inbox");
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Junk).await.len(), 1);
}
