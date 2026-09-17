//! The server's end of the tunnel: dials the gateway, stays connected, hands the connections that
//! arrive at the gateway to the server and opens connections to other servers from there.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use quinn::{Connection, Endpoint, VarInt};
use tokio::sync::watch;
use tokio::time::timeout;

use crate::code::Token;
use crate::identity::{CERTIFICATE_NAME, Fingerprint, Identity};
use crate::proto::{self, Connect, ConnectReply, Hello, HelloReply, Open, Refusal, Service, VERSION, Welcome};
use crate::quic;
use crate::stream::TunnelStream;

const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// After a refusal nothing changes until someone acts; asking often would only fill the gateway's log.
const REFUSED_RETRY: Duration = Duration::from_secs(300);

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
        let (stop, stop_rx) = watch::channel(false);
        let shared = Arc::new(Shared { connection: Mutex::new(None), status, stop });
        tokio::spawn(run(shared.clone(), settings, inbound, shutdown, stop_rx));
        TunnelClient { shared }
    }

    pub fn status(&self) -> Status {
        self.shared.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.shared.status.subscribe()
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
    shared.status.send_replace(Status::Connected { gateway, welcome, since: unix_now() });

    let accepting = tokio::spawn(accept_streams(connection.clone(), inbound.clone()));
    let ended = tokio::select! {
        error = connection.closed() => Ended::Lost(error.to_string()),
        _ = shutdown.changed() => Ended::Stopped,
        _ = stop.changed() => Ended::Stopped,
    };
    accepting.abort();
    drop(control);
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
