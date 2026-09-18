//! Mail through a UwUMail Gateway, all on this machine: server A sits behind the gateway,
//! server B stands for the rest of the internet.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uwumail_gateway::GatewayConfig;
use uwumail_gateway::config::{ListenConfig, OutboundConfig};
use uwumail_gateway::state::State;
use uwumail_smtp::{
    BoxIo, Connector, DeliveryConfig, ListenerKind, Smtp, SmtpConfig, SmtpSettings, Submission, SubmissionRecipient,
    ToneConfig,
};
use uwumail_store::{EmailSummary, MailboxRole, NewAccount, Role, Store};
use uwumail_tunnel::{
    ClientSettings, Identity, Inbound, Open, PairingCode, Service, Status, TunnelClient, TunnelStream,
};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct Server {
    smtp: Smtp,
    mx: SocketAddr,
    _shutdown: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

async fn start_server(domain: &str, user: &str, routes: &[(&str, SocketAddr)]) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain(domain).await.unwrap();
    store
        .create_account(NewAccount {
            address: format!("{user}@{domain}"),
            display_name: user.into(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    let delivery = DeliveryConfig {
        routes: routes.iter().map(|(d, a)| (d.to_string(), a.to_string())).collect::<HashMap<_, _>>(),
        ..DeliveryConfig::default()
    };
    // The test domains have no DNS records.
    let smtp_config = SmtpConfig { verify_senders: false, ..SmtpConfig::default() };
    let settings = SmtpSettings {
        hostname: format!("mx.{domain}"),
        smtp: smtp_config,
        spam: Default::default(),
        delivery,
        tone: ToneConfig::default(),
        server_tls: None,
    };
    let smtp = Smtp::new(store, settings).unwrap();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let listener = TcpListener::bind((LOCALHOST, 0)).await.unwrap();
    let mx = listener.local_addr().unwrap();
    tokio::spawn(uwumail_smtp::serve(smtp.clone(), listener, ListenerKind::Mx, shutdown_rx.clone()));
    tokio::spawn(uwumail_smtp::run_queue(smtp.clone(), shutdown_rx));
    Server { smtp, mx, _shutdown: shutdown, _dir: dir }
}

async fn inbox(smtp: &Smtp, login: &str, count: usize) -> Vec<EmailSummary> {
    let store = smtp.store();
    let account = store.account(login).await.unwrap().unwrap();
    let mailboxes = store.mailboxes(account.id).await.unwrap();
    let inbox = mailboxes.into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
    let started = Instant::now();
    loop {
        let emails = store.emails_in_mailbox(inbox.id, 50).await.unwrap();
        if emails.len() >= count {
            return emails;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "{login} has {} of {count} emails", emails.len());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Hands the connections from the gateway to server A, like the real server does.
struct BehindGateway(Smtp);

impl Inbound for BehindGateway {
    fn open(&self, open: Open, stream: TunnelStream) {
        assert_eq!(open.service, Service::Smtp);
        tokio::spawn(uwumail_smtp::serve_stream(self.0.clone(), Box::new(stream), open.client, ListenerKind::Mx));
    }
}

/// Sends everything through the tunnel, even to this machine.
struct ThroughTunnel(TunnelClient);

impl Connector for ThroughTunnel {
    fn connect(
        &self,
        address: SocketAddr,
        limit: Duration,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<BoxIo>> + Send + '_>> {
        Box::pin(async move { Ok(Box::new(self.0.connect(address, limit).await?) as BoxIo) })
    }
}

async fn reply(reader: &mut BufReader<TcpStream>) -> String {
    let mut all = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        all.push_str(&line);
        if line.len() < 4 || line.as_bytes()[3] != b'-' {
            return all;
        }
    }
}

async fn say(reader: &mut BufReader<TcpStream>, line: &str, expected: &str) {
    reader.get_mut().write_all(line.as_bytes()).await.unwrap();
    let answer = reply(reader).await;
    assert!(answer.starts_with(expected), "{} got {answer}", line.trim());
}

#[tokio::test]
async fn mail_comes_in_and_goes_out_through_the_gateway() {
    let b = start_server("b.test", "bo", &[]).await;

    let dir = tempfile::tempdir().unwrap();
    let (_gateway_shutdown, gateway_shutdown_rx) = watch::channel(false);
    let config = GatewayConfig {
        tunnel: "127.0.0.1:0".into(),
        state_dir: dir.path().to_owned(),
        public_addresses: vec![LOCALHOST],
        listen: ListenConfig {
            smtp: "127.0.0.1:0".into(),
            submission: String::new(),
            submissions: String::new(),
            http: String::new(),
            https: String::new(),
            imaps: String::new(),
        },
        outbound: OutboundConfig { ports: vec![b.mx.port()], allow_private: true },
        ..GatewayConfig::default()
    };
    let gateway = uwumail_gateway::start(config, gateway_shutdown_rx).await.unwrap();
    let state = State::open(dir.path()).unwrap();
    let token = loop {
        if let Some(token) = state.token().unwrap() {
            break token;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let code = PairingCode {
        addresses: vec![gateway.tunnel],
        fingerprint: state.identity().unwrap().unwrap().fingerprint(),
        token,
    };

    let a = start_server("a.test", "al", &[("b.test", b.mx)]).await;
    let (_client_shutdown, client_shutdown_rx) = watch::channel(false);
    let settings = ClientSettings {
        addresses: code.addresses.clone(),
        gateway: code.fingerprint,
        identity: Identity::generate().unwrap(),
        hostname: "mx.a.test".into(),
        software: "test".into(),
        services: uwumail_tunnel::Service::FIRST.to_vec(),
        token: Some(code.token.clone()),
    };
    let client = TunnelClient::start(settings, Arc::new(BehindGateway(a.smtp.clone())), client_shutdown_rx);
    a.smtp.set_connector(Some(Arc::new(ThroughTunnel(client.clone()))));
    let mut status = client.subscribe();
    tokio::time::timeout(Duration::from_secs(20), status.wait_for(|s| matches!(s, Status::Connected { .. })))
        .await
        .expect("the tunnel comes up")
        .unwrap();

    // Mail from outside arrives at the gateway's port 25 and lands in al's inbox at home.
    let socket = TcpStream::connect(gateway.listener(Service::Smtp).unwrap()).await.unwrap();
    let mut sender = BufReader::new(socket);
    assert!(reply(&mut sender).await.starts_with("220 mx.a.test "), "the greeting comes from home");
    say(&mut sender, "EHLO sender.example\r\n", "250").await;
    say(&mut sender, "MAIL FROM:<bo@b.test>\r\n", "250").await;
    say(&mut sender, "RCPT TO:<al@a.test>\r\n", "250").await;
    say(&mut sender, "DATA\r\n", "354").await;
    say(
        &mut sender,
        "From: bo@b.test\r\nTo: al@a.test\r\nSubject: Hello through the gateway\r\n\r\nHi!\r\n.\r\n",
        "250",
    )
    .await;
    say(&mut sender, "QUIT\r\n", "221").await;
    let received = inbox(&a.smtp, "al@a.test", 1).await;
    assert_eq!(received[0].subject, "Hello through the gateway");

    // Mail to other servers leaves through the gateway.
    let account = a.smtp.store().account("al@a.test").await.unwrap().unwrap();
    let submit = |subject: &str| Submission {
        account: account.clone(),
        mail_from: "al@a.test".into(),
        recipients: vec![SubmissionRecipient::new("bo@b.test")],
        raw: format!("From: al@a.test\r\nTo: bo@b.test\r\nSubject: {subject}\r\n\r\nHi back!\r\n").into_bytes(),
        env_id: None,
        trace: None,
    };
    a.smtp.submit(submit("Reply through the gateway")).await.unwrap();
    let delivered = inbox(&b.smtp, "bo@b.test", 1).await;
    assert_eq!(delivered[0].subject, "Reply through the gateway");

    // Without the tunnel nothing leaves: the mail waits in the queue.
    client.stop();
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| matches!(s, Status::Stopped)))
        .await
        .expect("the tunnel stops")
        .unwrap();
    a.smtp.submit(submit("Waits for the gateway")).await.unwrap();
    let started = Instant::now();
    let error = loop {
        let entries = a.smtp.store().queue_entries().await.unwrap();
        if let Some(error) = entries.iter().flat_map(|e| &e.recipients).find_map(|r| r.last_error.clone()) {
            break error;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "the delivery attempt shows up in the queue");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(error.contains("not connected"), "{error}");
    assert_eq!(inbox(&b.smtp, "bo@b.test", 1).await.len(), 1, "nothing went out directly");
}
