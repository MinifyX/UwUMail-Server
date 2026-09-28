//! DANE (RFC 7672) and TLS reports (RFC 8460) between two in-process servers: a.test delivers to
//! b.test through its MX, with DNS answers pinned, and reports to it how TLS went.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio::sync::watch;
use uwumail_smtp::dane::dane_ee_record;
use uwumail_smtp::egress::Egress;
use uwumail_smtp::{DeliveryConfig, ListenerKind, Smtp, SmtpConfig, SmtpSettings, SpamConfig, ToneConfig};
use uwumail_store::{
    MailboxRole, NewAccount, NewQueueRecipient, QueueRecipientStatus, Role, Store, TLS_REPORT_ADDRESS, tls_rpt_day,
};

struct Server {
    smtp: Smtp,
    mx: SocketAddr,
    _shutdown: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

/// A server for `domain` with the user `user`, whose MX is `mx.<domain>` with a new self-signed
/// certificate (offered for STARTTLS when `starttls`), delivering to other servers' MX at `mx_port`.
async fn server(domain: &str, user: &str, mx_port: u16, starttls: bool) -> (Server, CertificateDer<'static>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain(domain).await.unwrap();
    store
        .create_account(NewAccount {
            address: format!("{user}@{domain}"),
            display_name: user.to_owned(),
            password: Some("katzenpfote-123".into()),
            role: Role::Admin,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();

    let hostname = format!("mx.{domain}");
    let generated = rcgen::generate_simple_self_signed(vec![hostname.clone()]).unwrap();
    let certificate = generated.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()));
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key)
        .unwrap();
    let smtp = Smtp::new(
        store,
        SmtpSettings {
            hostname: hostname.clone(),
            smtp: SmtpConfig::default(),
            spam: SpamConfig { enabled: false, ..SpamConfig::default() },
            delivery: DeliveryConfig { mx_port, ..DeliveryConfig::default() },
            tone: ToneConfig::default(),
            server_tls: starttls.then(|| Arc::new(tls)),
        },
    )
    .unwrap();
    for name in [domain.to_owned(), hostname, format!("_dmarc.{domain}"), format!("_mta-sts.{domain}")] {
        smtp.dns_cache().pin_no_txt(&name);
    }

    let (shutdown, rx) = watch::channel(false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mx = listener.local_addr().unwrap();
    tokio::spawn(uwumail_smtp::serve(smtp.clone(), listener, ListenerKind::Mx, rx.clone()));
    tokio::spawn(uwumail_smtp::run_queue(smtp.clone(), rx));
    (Server { smtp, mx, _shutdown: shutdown, _dir: dir }, certificate)
}

/// a.test and b.test, where a.test finds b.test's MX with DNSSEC and `tlsa` as its TLSA records.
async fn pair(tlsa: impl Fn(&CertificateDer<'static>) -> Vec<String>) -> (Server, Server) {
    pair_with(true, tlsa).await
}

async fn pair_with(starttls: bool, tlsa: impl Fn(&CertificateDer<'static>) -> Vec<String>) -> (Server, Server) {
    let (b, b_cert) = server("b.test", "nyu", 0, starttls).await;
    let (a, _) = server("a.test", "mini", b.mx.port(), true).await;
    let dns = a.smtp.dns_cache();
    dns.pin_signed_mx("b.test", &[(10, "mx.b.test")]);
    dns.pin_ipv4("mx.b.test", &[Ipv4Addr::LOCALHOST]);
    let records = tlsa(&b_cert);
    dns.pin_tlsa("mx.b.test", &records.iter().map(String::as_str).collect::<Vec<_>>()).unwrap();
    // b.test checks where mail from a.test comes from.
    for name in ["a.test", "mx.a.test", "_dmarc.a.test"] {
        b.smtp.dns_cache().pin_no_txt(name);
    }
    (a, b)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

async fn send(from: &Server, to: &str) {
    let raw = format!(
        "From: mini@a.test\r\nTo: {to}\r\nSubject: Hallo\r\nMessage-ID: <{}@a.test>\r\n\r\nHallo aus a.test\r\n",
        std::process::id()
    );
    let recipient = NewQueueRecipient { address: to.to_owned(), notify_flags: 0, orcpt: None };
    from.smtp.store().enqueue("mini@a.test", vec![recipient], raw.as_bytes(), None, None, 3600).await.unwrap();
}

async fn inbox(server: &Server, login: &str) -> usize {
    let store = server.smtp.store();
    let account = store.account(login).await.unwrap().unwrap();
    let mailbox =
        store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
    store.emails_in_mailbox(mailbox.id, 50).await.unwrap().len()
}

async fn wait_until<F: AsyncFn() -> bool>(what: &str, done: F) {
    let started = Instant::now();
    while !done().await {
        assert!(started.elapsed() < Duration::from_secs(20), "{what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_matching_tlsa_record_lets_mail_through_and_the_report_goes_to_the_domain() {
    let (a, b) = pair(|cert| vec![dane_ee_record(cert).unwrap().to_string()]).await;
    send(&a, "nyu@b.test").await;
    wait_until("the mail arrives over DANE", async || inbox(&b, "nyu@b.test").await == 1).await;

    let today = tls_rpt_day(now());
    let sessions = a.smtp.store().tls_rpt_sessions(today, "b.test").await.unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0].session.policy_type, "tlsa");
    assert_eq!(sessions[0].session.result_type, None);
    assert_eq!(sessions[0].session.mx_host, ["mx.b.test"]);

    // Once the day is over, b.test hears about it at the address its record names, and reads it
    // like any report.
    a.smtp
        .dns_cache()
        .pin_txt("_smtp._tls.b.test", &format!("v=TLSRPTv1; rua=mailto:{TLS_REPORT_ADDRESS}@b.test"))
        .unwrap();
    assert_eq!(a.smtp.send_tls_reports(&Egress::direct(), today + 1).await, 1);
    assert_eq!(a.smtp.send_tls_reports(&Egress::direct(), today + 1).await, 0, "only once");
    let store = b.smtp.store().clone();
    wait_until("the report is read at b.test", async || {
        store.report_summary("b.test", 0).await.unwrap().tls.reports == 1
    })
    .await;
    let summary = store.report_summary("b.test", 0).await.unwrap();
    assert_eq!((summary.tls.successful, summary.tls.failed), (1, 0));
    assert_eq!(summary.tls.reporters[0].organization, "mx.a.test");
    assert_eq!(inbox(&b, "nyu@b.test").await, 1, "the report landed in no mailbox");

    let sent = a.smtp.store().tls_rpt_sent(7, 10).await.unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].status.as_str(), sent[0].successful), ("sent", 1));
    assert_eq!(sent[0].destinations, ["mailto:tls-reports@b.test"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_certificate_no_tlsa_record_matches_holds_the_mail_back() {
    let (a, b) = pair(|_| vec![format!("3 1 1 {}", "00".repeat(32))]).await;
    send(&a, "nyu@b.test").await;
    let store = a.smtp.store().clone();
    let error = deferred(&a).await;
    assert!(error.contains("TLS"), "{error}");
    assert_eq!(inbox(&b, "nyu@b.test").await, 0);

    let today = tls_rpt_day(now());
    let sessions = store.tls_rpt_sessions(today, "b.test").await.unwrap();
    assert_eq!(sessions[0].session.result_type.as_deref(), Some("tlsa-invalid"));
    assert_eq!(sessions[0].session.receiving_mx_hostname, "mx.b.test");
    assert_eq!(sessions[0].session.receiving_ip, "127.0.0.1");
}

#[tokio::test(flavor = "multi_thread")]
async fn tlsa_records_that_do_not_validate_hold_the_mail_back_too() {
    let (a, b) = pair(|_| Vec::new()).await;
    a.smtp.dns_cache().pin_bogus_tlsa("mx.b.test");
    send(&a, "nyu@b.test").await;
    let store = a.smtp.store().clone();
    assert!(deferred(&a).await.contains("DNSSEC"));
    assert_eq!(inbox(&b, "nyu@b.test").await, 0);
    let sessions = store.tls_rpt_sessions(tls_rpt_day(now()), "b.test").await.unwrap();
    assert_eq!(sessions[0].session.result_type.as_deref(), Some("dnssec-invalid"));
}

/// Waits until the first delivery attempt was deferred, and gives its error.
async fn deferred(server: &Server) -> String {
    let store = server.smtp.store().clone();
    wait_until("the delivery is deferred", async || {
        let entries = store.queue_entries().await.unwrap();
        entries
            .iter()
            .flat_map(|entry| &entry.recipients)
            .any(|r| r.status == QueueRecipientStatus::Pending && r.attempts > 0)
    })
    .await;
    let entries = store.queue_entries().await.unwrap();
    entries[0].recipients[0].last_error.clone().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn dane_requires_starttls() {
    let (a, b) = pair_with(false, |cert| vec![dane_ee_record(cert).unwrap().to_string()]).await;
    send(&a, "nyu@b.test").await;
    let error = deferred(&a).await;
    assert!(error.contains("STARTTLS"), "{error}");
    assert_eq!(inbox(&b, "nyu@b.test").await, 0);
    let sessions = a.smtp.store().tls_rpt_sessions(tls_rpt_day(now()), "b.test").await.unwrap();
    assert_eq!(sessions[0].session.policy_type, "tlsa");
    assert_eq!(sessions[0].session.result_type.as_deref(), Some("starttls-not-supported"));
}

#[tokio::test(flavor = "multi_thread")]
async fn mx_records_that_do_not_validate_hold_the_mail_back() {
    let (a, b) = pair(|_| Vec::new()).await;
    a.smtp.dns_cache().pin_bogus_mx("b.test", &[(10, "mx.b.test")]);
    send(&a, "nyu@b.test").await;
    let error = deferred(&a).await;
    assert!(error.contains("DNSSEC"), "{error}");
    assert_eq!(inbox(&b, "nyu@b.test").await, 0);
    let sessions = a.smtp.store().tls_rpt_sessions(tls_rpt_day(now()), "b.test").await.unwrap();
    assert_eq!(
        (sessions[0].session.policy_type.as_str(), sessions[0].session.result_type.as_deref()),
        ("tlsa", Some("dnssec-invalid"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_tlsa_records_mail_goes_as_before() {
    let (a, b) = pair(|_| Vec::new()).await;
    send(&a, "nyu@b.test").await;
    wait_until("the mail arrives", async || inbox(&b, "nyu@b.test").await == 1).await;
    let sessions = a.smtp.store().tls_rpt_sessions(tls_rpt_day(now()), "b.test").await.unwrap();
    assert_eq!(sessions[0].session.policy_type, "no-policy-found");
    assert_eq!(sessions[0].session.result_type, None);
}

/// b.test's MX records are signed and name mx.b.test, whose TLSA record matches. An attacker on the
/// path answers the ordinary (non-validating) MX lookup with `forged`: another host, which has no
/// TLSA records, or none at all. The mail must still go to mx.b.test under DANE, never to the forged
/// host with opportunistic TLS (security-audit-0.16.0 SMTP-3).
async fn forged_mx_answer(forged: &[(u16, &str)]) {
    let (a, b) = pair(|cert| vec![dane_ee_record(cert).unwrap().to_string()]).await;
    let dns = a.smtp.dns_cache();
    dns.pin_unvalidated_mx("b.test", forged);
    for host in ["mx.attacker.test", "b.test"] {
        dns.pin_ipv4(host, &[Ipv4Addr::LOCALHOST]);
        dns.pin_tlsa(host, &[]).unwrap();
    }
    send(&a, "nyu@b.test").await;
    wait_until("the mail arrives", async || inbox(&b, "nyu@b.test").await == 1).await;
    let sessions = a.smtp.store().tls_rpt_sessions(tls_rpt_day(now()), "b.test").await.unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0].session.mx_host, ["mx.b.test"]);
    assert_eq!(sessions[0].session.policy_type, "tlsa");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forged_mx_host_does_not_take_dane_away() {
    forged_mx_answer(&[(10, "mx.attacker.test")]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forged_no_mx_answer_does_not_take_dane_away() {
    forged_mx_answer(&[]).await;
}
