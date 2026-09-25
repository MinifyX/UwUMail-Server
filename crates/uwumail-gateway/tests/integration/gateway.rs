//! A gateway and tunnel clients on this machine: pairing, carried connections, outgoing
//! connections and everything that has to be refused.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uwumail_gateway::GatewayConfig;
use uwumail_gateway::config::{ListenConfig, OutboundConfig};
use uwumail_gateway::logs::LogQueue;
use uwumail_gateway::state::State;
use uwumail_tunnel::{
    ClientSettings, GatewayLogLine, Identity, Inbound, Open, PairingCode, Refusal, Service, Status, Token,
    TunnelClient, TunnelStream,
};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn config(dir: &Path, outbound_port: u16) -> GatewayConfig {
    GatewayConfig {
        tunnel: "127.0.0.1:0".into(),
        state_dir: dir.to_owned(),
        public_addresses: vec![LOCALHOST],
        listen: ListenConfig {
            smtp: "127.0.0.1:0".into(),
            submission: String::new(),
            submissions: String::new(),
            http: "127.0.0.1:0".into(),
            https: "127.0.0.1:0".into(),
            imaps: String::new(),
        },
        outbound: OutboundConfig { ports: vec![25, outbound_port], allow_private: true },
        ..GatewayConfig::default()
    }
}

struct TestGateway {
    running: uwumail_gateway::Running,
    dir: tempfile::TempDir,
    code: PairingCode,
    _shutdown: watch::Sender<bool>,
}

impl TestGateway {
    async fn start(outbound_port: u16) -> TestGateway {
        TestGateway::start_with_logs(outbound_port, None).await
    }

    async fn start_with_logs(outbound_port: u16, logs: Option<Arc<LogQueue>>) -> TestGateway {
        let dir = tempfile::tempdir().unwrap();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let running =
            uwumail_gateway::start_with_logs(config(dir.path(), outbound_port), shutdown_rx, logs).await.unwrap();
        let state = State::open(dir.path()).unwrap();
        let token = wait_for_token(&state).await;
        let identity = state.identity().unwrap().unwrap();
        let text = uwumail_gateway::pairing_code(&[LOCALHOST], running.tunnel.port(), &identity, &token).unwrap();
        let code = PairingCode::parse(&text).unwrap();
        assert_eq!(code.fingerprint, running.fingerprint);
        TestGateway { running, dir, code, _shutdown: shutdown }
    }

    fn state(&self) -> State {
        State::open(self.dir.path()).unwrap()
    }
}

async fn wait_for_token(state: &State) -> Token {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(token) = state.token().unwrap() {
                return token;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the gateway makes a pairing token")
}

/// Stands in for the server: greets like a mail server and echoes what it gets.
struct Echo;

impl Inbound for Echo {
    fn open(&self, open: Open, mut stream: TunnelStream) {
        tokio::spawn(async move {
            let greeting = format!("220 hello {} {}\r\n", open.client, open.service.as_str());
            stream.write_all(greeting.as_bytes()).await.unwrap();
            let mut buffer = [0u8; 1024];
            while let Ok(read) = stream.read(&mut buffer).await {
                if read == 0 || stream.write_all(&buffer[..read]).await.is_err() {
                    break;
                }
            }
            let _ = stream.shutdown().await;
        });
    }
}

fn start_client(code: &PairingCode, with_token: bool, stop: watch::Receiver<bool>) -> TunnelClient {
    let settings = ClientSettings {
        addresses: code.addresses.clone(),
        gateway: code.fingerprint,
        identity: Identity::generate().unwrap(),
        hostname: "mail.example.com".into(),
        software: "test".into(),
        services: uwumail_tunnel::Service::FIRST.to_vec(),
        token: with_token.then(|| code.token.clone()),
        logs: None,
    };
    TunnelClient::start(settings, Arc::new(Echo), stop)
}

async fn wait_for(client: &TunnelClient, wanted: impl Fn(&Status) -> bool) -> Status {
    let mut updates = client.subscribe();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = updates.borrow_and_update().clone();
            if wanted(&status) {
                return status;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the status stayed at {:?}", client.status()))
}

async fn read_line(stream: &mut TcpStream) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while stream.read(&mut byte).await.unwrap() == 1 {
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    String::from_utf8(line).unwrap()
}

async fn echo_server() -> u16 {
    let listener = TcpListener::bind((LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut reader, mut writer) = socket.split();
                let _ = tokio::io::copy(&mut reader, &mut writer).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_paired_server_takes_connections_and_sends_from_the_gateway() {
    let echo_port = echo_server().await;
    let gateway = TestGateway::start(echo_port).await;
    let (_stop, stop_rx) = watch::channel(false);
    let client = start_client(&gateway.code, true, stop_rx.clone());

    let Status::Connected { welcome, .. } = wait_for(&client, |s| matches!(s, Status::Connected { .. })).await else {
        unreachable!()
    };
    assert_eq!(welcome.addresses, [LOCALHOST]);
    assert!(welcome.services.contains(&Service::Smtp));
    let state = gateway.state();
    assert_eq!(state.pairing().unwrap().unwrap().hostname, "mail.example.com");
    assert!(state.token().unwrap().is_none(), "a used token is gone");

    // Mail from outside reaches the server with the sender's real address.
    let mut sender = TcpStream::connect(gateway.running.listener(Service::Smtp).unwrap()).await.unwrap();
    let sender_address = sender.local_addr().unwrap();
    assert_eq!(read_line(&mut sender).await, format!("220 hello {sender_address} smtp\r\n"));
    sender.write_all(b"QUIT\r\n").await.unwrap();
    assert_eq!(read_line(&mut sender).await, "QUIT\r\n");

    // Mail to other servers leaves from the gateway.
    let mut outgoing = client.connect(SocketAddr::new(LOCALHOST, echo_port), Duration::from_secs(5)).await.unwrap();
    outgoing.write_all(b"EHLO mail.example.com\r\n").await.unwrap();
    let mut answer = [0u8; 23];
    outgoing.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"EHLO mail.example.com\r\n");

    // Nothing but mail ports.
    let refused = client.connect(SocketAddr::new(LOCALHOST, 22), Duration::from_secs(5)).await.unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied, "{refused}");

    // Another server cannot take the gateway over, not even with the used code.
    let intruder = start_client(&gateway.code, true, stop_rx.clone());
    let status = wait_for(&intruder, |s| matches!(s, Status::Refused { .. })).await;
    assert!(matches!(status, Status::Refused { reason: Refusal::OtherServer, .. }), "{status:?}");
    assert!(matches!(client.status(), Status::Connected { .. }), "the paired server stays connected");
}

#[tokio::test]
async fn pairing_needs_the_right_code_from_the_right_gateway() {
    let gateway = TestGateway::start(2525).await;
    let (_stop, stop_rx) = watch::channel(false);

    let without_code = start_client(&gateway.code, false, stop_rx.clone());
    let status = wait_for(&without_code, |s| matches!(s, Status::Refused { .. })).await;
    assert!(matches!(status, Status::Refused { reason: Refusal::NotPaired, .. }), "{status:?}");

    let mut guessed = gateway.code.clone();
    guessed.token = Token::generate();
    let guesser = start_client(&guessed, true, stop_rx.clone());
    let status = wait_for(&guesser, |s| matches!(s, Status::Refused { .. })).await;
    assert!(matches!(status, Status::Refused { reason: Refusal::WrongToken, .. }), "{status:?}");

    // A code with another gateway's fingerprint never gets past the handshake.
    let mut impostor = gateway.code.clone();
    impostor.fingerprint = Identity::generate().unwrap().fingerprint();
    let fooled = start_client(&impostor, true, stop_rx.clone());
    wait_for(&fooled, |s| matches!(s, Status::Connecting { error: Some(_) })).await;

    assert!(gateway.state().pairing().unwrap().is_none(), "nobody got paired");
}

#[tokio::test]
async fn senders_hear_try_again_later_while_the_server_is_away() {
    let gateway = TestGateway::start(2525).await;

    let mut sender = TcpStream::connect(gateway.running.listener(Service::Smtp).unwrap()).await.unwrap();
    let line = read_line(&mut sender).await;
    assert!(line.starts_with("421 4.3.2 uwumail-gateway "), "{line}");

    let mut browser = TcpStream::connect(gateway.running.listener(Service::Http).unwrap()).await.unwrap();
    browser.write_all(b"GET / HTTP/1.1\r\nHost: mail.example.com\r\n\r\n").await.unwrap();
    assert!(read_line(&mut browser).await.starts_with("HTTP/1.1 503 "));

    // TLS ports are closed without a word: only the server has the certificate.
    let mut tls = TcpStream::connect(gateway.running.listener(Service::Https).unwrap()).await.unwrap();
    let mut buffer = [0u8; 16];
    assert_eq!(tls.read(&mut buffer).await.unwrap_or(0), 0);
}

#[tokio::test]
async fn unpairing_disconnects_the_server_and_brings_a_new_code() {
    let gateway = TestGateway::start(2525).await;
    let (_stop, stop_rx) = watch::channel(false);
    let client = start_client(&gateway.code, true, stop_rx);
    wait_for(&client, |s| matches!(s, Status::Connected { .. })).await;

    let state = gateway.state();
    assert!(state.remove_pairing().unwrap());
    let status = wait_for(&client, |s| matches!(s, Status::Refused { .. })).await;
    assert!(matches!(status, Status::Refused { reason: Refusal::NotPaired, .. }), "{status:?}");

    let token = wait_for_token(&state).await;
    assert!(!token.matches(&gateway.code.token), "the new code is a new one");
}

/// Waits until `path` exists and holds something, or gives up.
async fn wait_for_file(path: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && !text.trim().is_empty()
            {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{} never turned up", path.display()))
}

#[tokio::test]
async fn the_server_can_ask_for_bans_but_never_against_itself() {
    let gateway = TestGateway::start(2525).await;
    let (_stop, stop_rx) = watch::channel(false);
    let client = start_client(&gateway.code, true, stop_rx.clone());
    wait_for(&client, |s| matches!(s, Status::Connected { .. })).await;

    // Where the tunnel comes from is written down before anything else can happen: the helper and
    // fail2ban read this, and it is what keeps a ban from ever reaching the server.
    let trusted = wait_for_file(&gateway.dir.path().join("trusted")).await;
    assert!(trusted.starts_with("127.0.0.1 "), "the tunnel's address, as seen: {trusted:?}");
    assert!(trusted.trim_end().ends_with("127.0.0.1/32"), "with the range for fail2ban: {trusted:?}");

    // A stranger is passed on to the privileged helper.
    client.ban("9.9.9.9".parse().unwrap(), Duration::from_secs(3600), "tried logins that do not exist");
    let bans = wait_for_file(&gateway.dir.path().join("bans.jsonl")).await;
    let asked: serde_json::Value = serde_json::from_str(bans.lines().next().unwrap()).unwrap();
    assert_eq!(asked["ip"], "9.9.9.9");
    assert_eq!(asked["action"], "ban");
    assert_eq!(asked["seconds"], 3600);

    // The address the tunnel itself comes from is refused, however loudly the server asks. This is
    // the one that would take the server off the internet.
    client.ban(LOCALHOST, Duration::from_secs(3600), "a mail app at home got the password wrong");
    client.ban("8.8.4.4".parse().unwrap(), Duration::from_secs(3600), "another stranger");
    // The second stranger arriving means the one before it was handled, too.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if std::fs::read_to_string(gateway.dir.path().join("bans.jsonl")).is_ok_and(|text| text.contains("8.8.4.4"))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the second stranger never arrived");

    let bans = std::fs::read_to_string(gateway.dir.path().join("bans.jsonl")).unwrap();
    assert!(!bans.contains("127.0.0.1"), "the server asked to ban itself and the gateway did it: {bans}");
}

#[tokio::test]
async fn the_server_hears_how_the_gateway_is_doing() {
    let gateway = TestGateway::start(2525).await;
    // What the privileged helper would have written, as it writes it.
    std::fs::write(
        gateway.dir.path().join("machine.json"),
        serde_json::json!({
            "writtenAt": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            "system": { "name": "Ubuntu 26.04.1 LTS", "updates": 12, "securityUpdates": 3, "rebootRequired": true },
            "protection": { "firewall": "ufw", "firewallActive": true, "fail2ban": true, "banned": 2 },
        })
        .to_string(),
    )
    .unwrap();

    let (_stop, stop_rx) = watch::channel(false);
    let client = start_client(&gateway.code, true, stop_rx.clone());
    wait_for(&client, |s| matches!(s, Status::Connected { .. })).await;

    let status = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = client.gateway_status() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the gateway never said how it was doing");

    let system = status.system.expect("the machine report came through");
    assert_eq!(system.name, "Ubuntu 26.04.1 LTS");
    assert_eq!(system.security_updates, 3);
    assert!(system.reboot_required);
    assert_eq!(status.protection.expect("and the protection").firewall, "ufw");
    assert_eq!(status.trusted, [LOCALHOST], "and where it knows the server to be");
}

fn log_line(message: &str) -> GatewayLogLine {
    GatewayLogLine { at: 1, level: "info".into(), message: message.into(), fields: vec![] }
}

#[tokio::test]
async fn the_gateway_hands_its_log_to_a_server_that_asks() {
    let logs = LogQueue::new(100);
    // Said while no server was there: kept for the one that comes.
    logs.push(log_line("said before the server came"));
    let gateway = TestGateway::start_with_logs(echo_server().await, Some(logs.clone())).await;
    let identity = Identity::generate().unwrap();
    let settings = |token: Option<Token>, sink: Option<uwumail_tunnel::LogSink>| ClientSettings {
        addresses: gateway.code.addresses.clone(),
        gateway: gateway.code.fingerprint,
        identity: identity.clone(),
        hostname: "mail.example.com".into(),
        software: "test".into(),
        services: Service::FIRST.to_vec(),
        token,
        logs: sink,
    };

    // A server that does not ask (like one from before) leaves the lines where they are.
    let (stop_quiet, stop_quiet_rx) = watch::channel(false);
    let quiet = TunnelClient::start(settings(Some(gateway.code.token.clone()), None), Arc::new(Echo), stop_quiet_rx);
    wait_for(&quiet, |status| matches!(status, Status::Connected { .. })).await;
    logs.push(log_line("said while an older server listened"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let kept = logs.take_batch();
    assert_eq!(kept.len(), 2, "not asked, not sent");
    logs.put_back(kept);
    let _ = stop_quiet.send(true);
    wait_for(&quiet, |status| matches!(status, Status::Stopped)).await;

    // The same server, now asking: it gets what waited, then what comes.
    let (sink, mut received) = tokio::sync::mpsc::unbounded_channel();
    let sink: uwumail_tunnel::LogSink = Arc::new(move |lines: Vec<GatewayLogLine>| {
        for line in lines {
            let _ = sink.send(line.message);
        }
    });
    let (_stop, stop_rx) = watch::channel(false);
    let asking = TunnelClient::start(settings(None, Some(sink)), Arc::new(Echo), stop_rx);
    wait_for(&asking, |status| matches!(status, Status::Connected { .. })).await;
    assert_eq!(next_line(&mut received).await, "said before the server came");
    assert_eq!(next_line(&mut received).await, "said while an older server listened");
    logs.push(log_line("said while it listens"));
    assert_eq!(next_line(&mut received).await, "said while it listens");
}

async fn next_line(received: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(10), received.recv()).await.expect("a log line arrives").unwrap()
}
