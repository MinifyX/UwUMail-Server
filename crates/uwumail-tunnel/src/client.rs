//! The server's end of the tunnel: dials the gateway, stays connected, hands the connections that
//! arrive at the gateway to the server and opens connections to other servers from there.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use quinn::{Connection, Endpoint, RecvStream, SendStream, VarInt};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use crate::code::Token;
use crate::identity::{CERTIFICATE_NAME, Fingerprint, Identity};
use crate::proto::{
    self, Connect, ConnectReply, GatewayMessage, GatewayStatus, Hello, HelloReply, Open, Refusal, ServerMessage,
    Service, VERSION, Welcome,
};
use crate::quic;
use crate::stream::TunnelStream;

const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// After a refusal nothing changes until someone acts; asking often would only fill the gateway's log.
const REFUSED_RETRY: Duration = Duration::from_secs(300);
/// Bans waiting for a gateway that is slow to read them. Past this the oldest are dropped: the
/// server's own limiter already holds the line, and a full queue must never stall a login.
const BANS_WAITING: usize = 64;

pub struct ClientSettings {
    /// Where the gateway waits for the tunnel, tried in this order.
    pub addresses: Vec<SocketAddr>,
    pub gateway: Fingerprint,
    pub identity: Identity,
    pub hostname: String,
    pub software: String,
    /// The services this server takes from the gateway.
    pub services: Vec<Service>,
    /// The token from the pairing code, until the gateway knows this server.
    pub token: Option<Token>,
}

/// Takes the connections that arrive at the gateway.
pub trait Inbound: Send + Sync + 'static {
    /// Called once per connection, on a task of its own.
    fn open(&self, open: Open, stream: TunnelStream);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Trying to reach the gateway; `error` says why the last attempt failed.
    Connecting {
        error: Option<String>,
    },
    Connected {
        gateway: SocketAddr,
        welcome: Welcome,
        /// Unix time.
        since: i64,
    },
    /// The gateway does not accept this server. It asks again every few minutes.
    Refused {
        reason: Refusal,
        message: String,
    },
    Stopped,
}

#[derive(Clone)]
pub struct TunnelClient {
    shared: Arc<Shared>,
}

struct Shared {
    connection: Mutex<Option<Connection>>,
    status: watch::Sender<Status>,
    /// What the gateway last said about the machine it runs on. `None` until it says something,
    /// and for gateways from before the control stream.
    gateway_status: watch::Sender<Option<GatewayStatus>>,
    /// Where bans go while a gateway that listens is connected.
    control: Mutex<Option<mpsc::Sender<ServerMessage>>>,
    stop: watch::Sender<bool>,
}

enum Ended {
    Lost(String),
    Stopped,
}

enum Failure {
    Refused(Refusal, String),
    Failed(String),
}

impl TunnelClient {
    /// Connects in the background, again and again, until `shutdown` changes or [`TunnelClient::stop`].
    pub fn start(settings: ClientSettings, inbound: Arc<dyn Inbound>, shutdown: watch::Receiver<bool>) -> TunnelClient {
        let (status, _) = watch::channel(Status::Connecting { error: None });
        let (gateway_status, _) = watch::channel(None);
        let (stop, stop_rx) = watch::channel(false);
        let shared =
            Arc::new(Shared { connection: Mutex::new(None), status, gateway_status, control: Mutex::new(None), stop });
        tokio::spawn(run(shared.clone(), settings, inbound, shutdown, stop_rx));
        TunnelClient { shared }
    }

    pub fn status(&self) -> Status {
        self.shared.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.shared.status.subscribe()
    }

    /// What the gateway last said about its machine: updates, firewall and fail2ban. `None` while
    /// nothing was said yet, and for gateways from before the control stream.
    pub fn gateway_status(&self) -> Option<GatewayStatus> {
        self.shared.gateway_status.borrow().clone()
    }

    /// Asks the gateway to keep `ip` away from its public ports. Does nothing when the gateway is
    /// away or too old to listen: the server's own limiter holds the line either way.
    ///
    /// The gateway refuses addresses its own tunnel comes from, so a mail app at home with the
    /// wrong password cannot cut the household off from its own gateway.
    pub fn ban(&self, ip: IpAddr, how_long: Duration, reason: &str) {
        self.send_control(ServerMessage::Ban {
            ip,
            seconds: how_long.as_secs().clamp(60, 7 * 24 * 3600) as u32,
            reason: reason.to_owned(),
        });
    }

    pub fn unban(&self, ip: IpAddr) {
        self.send_control(ServerMessage::Unban { ip });
    }

    /// Asks the gateway's machine for one of `os-update`, `reboot` or `gateway-update`. The answer
    /// comes back with the next status, which the gateway sends far more often while one runs.
    ///
    /// Says whether the ask went out at all. It does not when the gateway is away or too old, and
    /// then nothing happened and the portal should say so rather than wait for an answer that is
    /// not coming.
    pub fn task(&self, id: &str, verb: &str, version: Option<&str>) -> bool {
        self.send_control(ServerMessage::Task {
            id: id.to_owned(),
            verb: verb.to_owned(),
            version: version.map(str::to_owned),
        })
    }

    fn send_control(&self, message: ServerMessage) -> bool {
        let sender = self.shared.control.lock().expect("tunnel control poisoned").clone();
        let Some(sender) = sender else { return false };
        // Never waits: a gateway that cannot keep up must not hold up a login.
        match sender.try_send(message) {
            Ok(()) => true,
            Err(_) => {
                tracing::debug!("the gateway is not taking anything right now");
                false
            }
        }
    }

    pub fn stop(&self) {
        let _ = self.shared.stop.send(true);
    }

    /// Opens a TCP connection from the gateway to `address`.
    pub async fn connect(&self, address: SocketAddr, limit: Duration) -> io::Result<TunnelStream> {
        let connection = self
            .shared
            .connection
            .lock()
            .expect("tunnel connection poisoned")
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "the UwUMail Gateway is not connected"))?;
        let attempt = async move {
            let (send, recv) =
                connection.open_bi().await.map_err(|err| io::Error::new(io::ErrorKind::NotConnected, err))?;
            let mut stream = TunnelStream::new(send, recv);
            let request = Connect { address, timeout_secs: limit.as_secs().clamp(1, 600) as u32 };
            proto::write_message(&mut stream, &request).await?;
            match proto::read_message::<_, ConnectReply>(&mut stream).await? {
                ConnectReply::Connected { .. } => Ok(stream),
                ConnectReply::Failed { reason, message } => {
                    Err(io::Error::new(reason.io_kind(), format!("the gateway could not connect: {message}")))
                }
            }
        };
        timeout(limit + Duration::from_secs(5), attempt).await.map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, format!("connecting to {address} through the gateway timed out"))
        })?
    }
}

async fn run(
    shared: Arc<Shared>,
    mut settings: ClientSettings,
    inbound: Arc<dyn Inbound>,
    mut shutdown: watch::Receiver<bool>,
    mut stop: watch::Receiver<bool>,
) {
    let mut backoff = Duration::from_secs(1);
    while !*shutdown.borrow() && !*stop.borrow() {
        let outcome = session(&shared, &mut settings, &inbound, &mut shutdown, &mut stop).await;
        *shared.connection.lock().expect("tunnel connection poisoned") = None;
        let wait = match outcome {
            Ok(Ended::Stopped) => break,
            Ok(Ended::Lost(error)) => {
                tracing::warn!(%error, "lost the connection to the UwUMail Gateway, reconnecting");
                shared.status.send_replace(Status::Connecting { error: Some(error) });
                backoff = Duration::from_secs(1);
                backoff
            }
            Err(Failure::Refused(reason, message)) => {
                tracing::error!(?reason, %message, "the UwUMail Gateway refused this server");
                shared.status.send_replace(Status::Refused { reason, message });
                REFUSED_RETRY
            }
            Err(Failure::Failed(error)) => {
                tracing::warn!(%error, "could not reach the UwUMail Gateway");
                shared.status.send_replace(Status::Connecting { error: Some(error) });
                let wait = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                wait
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => break,
            _ = stop.changed() => break,
        }
    }
    shared.status.send_replace(Status::Stopped);
}

async fn session(
    shared: &Shared,
    settings: &mut ClientSettings,
    inbound: &Arc<dyn Inbound>,
    shutdown: &mut watch::Receiver<bool>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Ended, Failure> {
    let (endpoint, connection) = dial(settings).await.map_err(Failure::Failed)?;
    let (send, recv) = connection.open_bi().await.map_err(|err| Failure::Failed(err.to_string()))?;
    let mut control = TunnelStream::new(send, recv);
    let hello = Hello {
        version: VERSION,
        hostname: settings.hostname.clone(),
        software: settings.software.clone(),
        token: settings.token.as_ref().map(Token::to_text),
        services: Some(settings.services.clone()),
        control: true,
    };
    let answer = timeout(ANSWER_TIMEOUT, async {
        proto::write_message(&mut control, &hello).await?;
        proto::read_message::<_, HelloReply>(&mut control).await
    })
    .await
    .map_err(|_| Failure::Failed("the gateway did not answer".into()))?
    .map_err(|err| Failure::Failed(format!("talking to the gateway failed: {err}")))?;
    let welcome = match answer {
        HelloReply::Welcome(welcome) => welcome,
        HelloReply::Refused { reason, message } => {
            connection.close(VarInt::from_u32(0), b"refused");
            return Err(Failure::Refused(reason, message));
        }
    };
    if settings.token.take().is_some() {
        tracing::info!("paired with the UwUMail Gateway (=^･ω･^=)");
    }
    let gateway = connection.remote_address();
    tracing::info!(%gateway, addresses = ?welcome.addresses, "connected to the UwUMail Gateway");
    *shared.connection.lock().expect("tunnel connection poisoned") = Some(connection.clone());
    let controlling = welcome.control;
    shared.status.send_replace(Status::Connected { gateway, welcome, since: unix_now() });

    // A gateway from before the control stream never reads it again, so for that one the stream is
    // only held open; talking into it would fill a buffer nobody empties.
    let mut held_open = Some(control);
    let talking = controlling.then(|| {
        let (bans, waiting) = mpsc::channel(BANS_WAITING);
        *shared.control.lock().expect("tunnel control poisoned") = Some(bans);
        let (send, recv) = held_open.take().expect("the control stream is still here").into_parts();
        tokio::spawn(talk(send, recv, waiting, shared.gateway_status.clone()))
    });

    let accepting = tokio::spawn(accept_streams(connection.clone(), inbound.clone()));
    let ended = tokio::select! {
        error = connection.closed() => Ended::Lost(error.to_string()),
        _ = shutdown.changed() => Ended::Stopped,
        _ = stop.changed() => Ended::Stopped,
    };
    accepting.abort();
    *shared.control.lock().expect("tunnel control poisoned") = None;
    // What the gateway said belongs to the connection that said it; keeping it would show a
    // reassuring old report while the tunnel is down.
    shared.gateway_status.send_replace(None);
    if let Some(talking) = talking {
        talking.abort();
    }
    drop(held_open);
    if matches!(ended, Ended::Stopped) {
        connection.close(VarInt::from_u32(0), b"stopped");
        let _ = timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
    }
    Ok(ended)
}

async fn dial(settings: &ClientSettings) -> Result<(Endpoint, Connection), String> {
    let config = quic::client_config(&settings.identity, settings.gateway).map_err(|err| err.to_string())?;
    let mut errors = Vec::new();
    for address in &settings.addresses {
        // Wildcard sockets of one family do not reliably reach the other on every system.
        let bind = match address {
            SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        };
        let endpoint = match Endpoint::client(bind) {
            Ok(endpoint) => endpoint,
            Err(err) => {
                errors.push(format!("{address}: {err}"));
                continue;
            }
        };
        let connecting = match endpoint.connect_with(config.clone(), *address, CERTIFICATE_NAME) {
            Ok(connecting) => connecting,
            Err(err) => {
                errors.push(format!("{address}: {err}"));
                continue;
            }
        };
        match timeout(DIAL_TIMEOUT, connecting).await {
            Ok(Ok(connection)) => return Ok((endpoint, connection)),
            Ok(Err(err)) => errors.push(format!("{address}: {err}")),
            Err(_) => errors.push(format!("{address}: no answer")),
        }
    }
    if errors.is_empty() {
        errors.push("no address for the gateway".into());
    }
    Err(errors.join("; "))
}

/// The control stream after the handshake: the gateway reports on its machine, the server asks for
/// bans. Both directions run until the connection ends and the task is dropped with it.
async fn talk(
    mut send: SendStream,
    mut recv: RecvStream,
    mut bans: mpsc::Receiver<ServerMessage>,
    status: watch::Sender<Option<GatewayStatus>>,
) {
    let listening = async {
        // A message this version does not know is skipped, not fatal: a newer gateway keeps talking.
        while let Ok(message) = proto::read_known::<_, GatewayMessage>(&mut recv).await {
            if let Some(GatewayMessage::Status(report)) = message {
                if let Some(system) = &report.system {
                    // Worth a line in the log of a server nobody is looking at right now.
                    if system.security_updates > 0 || system.reboot_required {
                        tracing::info!(
                            security_updates = system.security_updates,
                            reboot_required = system.reboot_required,
                            "the UwUMail Gateway's machine is waiting for updates"
                        );
                    }
                }
                status.send_replace(Some(*report));
            }
        }
    };
    let asking = async {
        while let Some(message) = bans.recv().await {
            if proto::write_message(&mut send, &message).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = listening => {}
        _ = asking => {}
    }
}

async fn accept_streams(connection: Connection, inbound: Arc<dyn Inbound>) {
    while let Ok((send, recv)) = connection.accept_bi().await {
        let inbound = inbound.clone();
        tokio::spawn(async move {
            let mut stream = TunnelStream::new(send, recv);
            match timeout(HEADER_TIMEOUT, proto::read_message::<_, Open>(&mut stream)).await {
                Ok(Ok(open)) => inbound.open(open, stream),
                _ => stream.abort(),
            }
        });
    }
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}
