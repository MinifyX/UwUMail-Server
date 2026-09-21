//! The gateway: the tunnel endpoint with the pairing, and the public ports.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use quinn::{Connection, Endpoint, Incoming, RecvStream, SendStream, VarInt};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::timeout;
use uwumail_tunnel::proto::{
    self, GatewayMessage, Hello, HelloReply, Refusal, ServerMessage, Service, VERSION, Welcome,
};
use uwumail_tunnel::{Fingerprint, Identity, PairingCode, Token, TunnelStream};

use crate::config::GatewayConfig;
use crate::limits::Limits;
use crate::logs::LogQueue;
use crate::machine::Machine;
use crate::state::{Pairing, State};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the gateway looks whether the pairing changed on disk (`uwumail-gateway unpair`).
const PAIRING_CHECK: Duration = Duration::from_secs(2);
/// How often the machine's state goes to the server for the portal. The report behind it is
/// refreshed once a day; this pace also carries the count of banned addresses.
const REPORT_EVERY: Duration = Duration::from_secs(5 * 60);
/// While a task runs, someone is watching the portal and five minutes is a long time to stare at
/// nothing.
const REPORT_WHILE_BUSY: Duration = Duration::from_secs(3);
/// How long the gateway gathers log lines before it sends them to the server.
const LOG_BATCH_DELAY: Duration = Duration::from_secs(1);
/// Refused tunnel attempts from one address before it has to wait for the window to pass.
const MAX_REFUSALS: u32 = 10;
const REFUSAL_WINDOW: Duration = Duration::from_secs(600);
/// How long one address stays quiet in the log after it was turned away for being over its limit.
const TURNED_AWAY_QUIET: Duration = Duration::from_secs(300);
/// Used in answers to mail senders until a server told its name.
const NO_HOSTNAME: &str = "uwumail-gateway";

const CLOSE_REFUSED: VarInt = VarInt::from_u32(1);
const CLOSE_REPLACED: VarInt = VarInt::from_u32(2);
const CLOSE_UNPAIRED: VarInt = VarInt::from_u32(3);
const CLOSE_SHUTDOWN: VarInt = VarInt::from_u32(4);

/// The connected server.
#[derive(Clone)]
pub(crate) struct Active {
    pub connection: Connection,
    pub server: Fingerprint,
    pub hostname: String,
    /// The public services the server takes; connections to the others are turned away.
    pub services: Vec<Service>,
}

pub(crate) struct Shared {
    pub config: GatewayConfig,
    pub active: watch::Sender<Option<Active>>,
    pub limits: Arc<Limits>,
    state: State,
    machine: Machine,
    identity: Identity,
    public_addresses: Vec<IpAddr>,
    services: Vec<Service>,
    hostname: RwLock<String>,
    pairing_lock: tokio::sync::Mutex<()>,
    refusals: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    turned_away: Mutex<HashMap<IpAddr, Instant>>,
    /// The gateway's own log lines, for a server that asks for them.
    logs: Option<Arc<LogQueue>>,
}

/// A started gateway.
pub struct Running {
    /// Where the tunnel waits (UDP).
    pub tunnel: SocketAddr,
    /// The public ports as bound.
    pub listeners: Vec<(Service, SocketAddr)>,
    pub fingerprint: Fingerprint,
    tasks: JoinSet<()>,
}

impl Running {
    /// Waits until everything stopped after `shutdown` changed.
    pub async fn wait(mut self) {
        while self.tasks.join_next().await.is_some() {}
    }

    pub fn listener(&self, service: Service) -> Option<SocketAddr> {
        self.listeners.iter().find(|(s, _)| *s == service).map(|(_, address)| *address)
    }
}

/// The pairing code for `token`, with the tunnel port on each public address.
pub fn pairing_code(addresses: &[IpAddr], tunnel_port: u16, identity: &Identity, token: &Token) -> Option<String> {
    if addresses.is_empty() {
        return None;
    }
    let addresses = addresses.iter().map(|ip| SocketAddr::new(*ip, tunnel_port)).collect();
    Some(PairingCode { addresses, fingerprint: identity.fingerprint(), token: token.clone() }.encode())
}

/// Binds the tunnel and the public ports and starts serving until `shutdown` changes.
pub async fn start(config: GatewayConfig, shutdown: watch::Receiver<bool>) -> anyhow::Result<Running> {
    start_with_logs(config, shutdown, None).await
}

/// Like [`start`], handing the lines `logs` collects to a server that asks for them.
pub async fn start_with_logs(
    config: GatewayConfig,
    shutdown: watch::Receiver<bool>,
    logs: Option<Arc<LogQueue>>,
) -> anyhow::Result<Running> {
    config.validate()?;
    let state = State::open(&config.state_dir)?;
    let identity = state.load_or_create_identity()?;
    let endpoint = bind_tunnel(&config.tunnel, &identity)?;
    let tunnel = endpoint.local_addr()?;
    tracing::info!(address = %tunnel, "waiting for the UwUMail server's tunnel (UDP)");

    let mut listeners = Vec::new();
    for (service, address) in config.listen.addresses() {
        let listener = bind(address).await.with_context(|| {
            format!("could not listen on {address} for {} (is another mail or web server using it?)", service.as_str())
        })?;
        let local = listener.local_addr()?;
        tracing::info!(address = %local, service = service.as_str(), "listening");
        listeners.push((service, listener, local));
    }

    let public_addresses = config.public_addresses();
    if public_addresses.is_empty() {
        tracing::warn!("found no public address on this machine; set public_addresses in the configuration");
    } else {
        tracing::info!(addresses = ?public_addresses, "public addresses");
    }
    let hostname = state.pairing()?.map(|pairing| pairing.hostname).unwrap_or_else(|| NO_HOSTNAME.into());
    let (active, _) = watch::channel(None);
    let shared = Arc::new(Shared {
        limits: Limits::new(config.limits.max_connections, config.limits.max_connections_per_ip),
        machine: Machine::new(&config.state_dir),
        config,
        active,
        state,
        identity,
        public_addresses,
        services: listeners.iter().map(|(service, _, _)| *service).collect(),
        hostname: RwLock::new(hostname),
        pairing_lock: tokio::sync::Mutex::new(()),
        refusals: Mutex::new(HashMap::new()),
        turned_away: Mutex::new(HashMap::new()),
        logs,
    });

    let bound = listeners.iter().map(|(service, _, local)| (*service, *local)).collect();
    let fingerprint = shared.identity.fingerprint();
    let mut tasks = JoinSet::new();
    tasks.spawn(accept_tunnels(shared.clone(), endpoint, shutdown.clone()));
    tasks.spawn(watch_pairing(shared.clone(), tunnel.port(), shutdown.clone()));
    for (service, listener, _) in listeners {
        tasks.spawn(crate::public::serve(shared.clone(), listener, service, shutdown.clone()));
    }
    Ok(Running { tunnel, listeners: bound, fingerprint, tasks })
}

fn bind_tunnel(address: &str, identity: &Identity) -> anyhow::Result<Endpoint> {
    let parsed: SocketAddr = address.parse().with_context(|| format!("`tunnel` '{address}' is not an address"))?;
    match uwumail_tunnel::server_endpoint(parsed, identity) {
        Ok(endpoint) => Ok(endpoint),
        // Machines without IPv6 cannot bind [::], so fall back to all IPv4 addresses.
        Err(_) if parsed.is_ipv6() && parsed.ip().is_unspecified() => {
            uwumail_tunnel::server_endpoint(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), parsed.port()), identity)
                .with_context(|| format!("could not wait for the tunnel on {address} (UDP)"))
        }
        Err(err) => Err(err).with_context(|| format!("could not wait for the tunnel on {address} (UDP)")),
    }
}

async fn bind(address: &str) -> std::io::Result<TcpListener> {
    match TcpListener::bind(address).await {
        Ok(listener) => Ok(listener),
        Err(_) if address.starts_with("[::]:") => TcpListener::bind(address.replacen("[::]", "0.0.0.0", 1)).await,
        Err(err) => Err(err),
    }
}

impl Shared {
    pub fn hostname(&self) -> String {
        self.hostname.read().expect("hostname poisoned").clone()
    }

    /// The open services the server takes: a server cannot read the names of newer ones.
    fn shared_services(&self, hello: &Hello) -> Vec<Service> {
        self.services.iter().copied().filter(|service| hello.services().contains(service)).collect()
    }

    fn welcome(&self, services: Vec<Service>) -> Welcome {
        Welcome {
            version: VERSION,
            software: format!("uwumail-gateway {}", env!("CARGO_PKG_VERSION")),
            addresses: self.public_addresses.clone(),
            services,
            outbound_ports: self.config.outbound.ports.clone(),
            control: true,
            // Only with a helper beside us. Without one the portal keeps showing the commands,
            // and a button that quietly did nothing would be worse than no button.
            tasks: self.machine.has_helper(),
        }
    }

    /// Whether a ban the server asks for may be carried out. The tunnel's own address is the one
    /// that matters: a mail app at home with a wrong password would otherwise take the whole
    /// household's connection down, gateway and all.
    fn may_ban(&self, ip: IpAddr) -> Result<(), &'static str> {
        let ip = ip.to_canonical();
        if self.machine.is_trusted(ip) {
            return Err("it is where the server's own tunnel comes from");
        }
        if self.public_addresses.iter().any(|own| own.to_canonical() == ip) {
            return Err("it is the gateway itself");
        }
        if !uwumail_tunnel::net::is_global(ip) {
            return Err("it is not a public address");
        }
        Ok(())
    }

    /// Decides whether the server with `fingerprint` may use the gateway, pairing it when it
    /// brings the right token. Returns the server's host name.
    async fn authorize(&self, fingerprint: Fingerprint, hello: &Hello) -> Result<String, (Refusal, String)> {
        if hello.version != VERSION {
            return Err((Refusal::Version, format!("this gateway speaks tunnel version {VERSION}, please update")));
        }
        let hostname = clean_hostname(&hello.hostname);
        let failed = |err: anyhow::Error| {
            tracing::error!(error = %format!("{err:#}"), "reading or saving the pairing failed");
            (Refusal::NotPaired, "the gateway could not read its pairing".to_owned())
        };
        let _pairing = self.pairing_lock.lock().await;
        match self.state.pairing().map_err(failed)? {
            Some(pairing) if pairing.server == fingerprint => {
                if pairing.hostname != hostname {
                    let renamed = Pairing { hostname: hostname.clone(), ..pairing };
                    if let Err(err) = self.state.save_pairing(&renamed) {
                        tracing::warn!(error = %format!("{err:#}"), "saving the server's new name failed");
                    }
                }
                Ok(hostname)
            }
            Some(_) => Err((Refusal::OtherServer, "this gateway is paired with another server".into())),
            None => {
                let Some(given) = &hello.token else {
                    return Err((Refusal::NotPaired, "this server is not paired with the gateway yet".into()));
                };
                let expected = self.state.token().map_err(failed)?;
                match (expected, Token::from_text(given)) {
                    (Some(expected), Some(given)) if expected.matches(&given) => {
                        let pairing =
                            Pairing { server: fingerprint, hostname: hostname.clone(), paired_at: unix_now() };
                        self.state.save_pairing(&pairing).map_err(failed)?;
                        if let Err(err) = self.state.remove_token() {
                            tracing::warn!(%err, "removing the used pairing token failed");
                        }
                        tracing::info!(server = %hostname, %fingerprint, "paired with a UwUMail server (=^･ω･^=)");
                        Ok(hostname)
                    }
                    _ => Err((Refusal::WrongToken, "the pairing code is not valid (any more)".into())),
                }
            }
        }
    }

    fn refused_too_often(&self, ip: IpAddr) -> bool {
        let refusals = self.refusals.lock().expect("refusals poisoned");
        refusals.get(&ip).is_some_and(|(count, since)| since.elapsed() < REFUSAL_WINDOW && *count >= MAX_REFUSALS)
    }

    fn record_refusal(&self, ip: IpAddr) {
        let mut refusals = self.refusals.lock().expect("refusals poisoned");
        if refusals.len() > 10_000 {
            refusals.retain(|_, (_, since)| since.elapsed() < REFUSAL_WINDOW);
        }
        let entry = refusals.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= REFUSAL_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    /// A client turned away because its network already has as many connections as it may have.
    /// Worth knowing when mail goes missing, but at most one line per address and window: the
    /// case that fills this log is exactly the one that floods it.
    pub(crate) fn note_turned_away(&self, ip: IpAddr, service: Service) {
        let mut noted = self.turned_away.lock().expect("turned away poisoned");
        if noted.len() > 10_000 {
            noted.retain(|_, at: &mut Instant| at.elapsed() < TURNED_AWAY_QUIET);
        }
        let entry = noted.entry(ip).or_insert_with(|| Instant::now() - TURNED_AWAY_QUIET);
        if entry.elapsed() < TURNED_AWAY_QUIET {
            return;
        }
        *entry = Instant::now();
        tracing::warn!(
            client = %ip,
            service = service.as_str(),
            limit = self.config.limits.max_connections_per_ip,
            "turned a client away: its network holds as many connections as it may"
        );
    }
}

async fn accept_tunnels(shared: Arc<Shared>, endpoint: Endpoint, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => {
                    tokio::spawn(handle_tunnel(shared.clone(), incoming));
                }
                None => break,
            },
            _ = shutdown.changed() => break,
        }
    }
    if let Some(active) = shared.active.send_replace(None) {
        active.connection.close(CLOSE_SHUTDOWN, b"the gateway is shutting down");
    }
    endpoint.close(CLOSE_SHUTDOWN, b"the gateway is shutting down");
    let _ = timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
}

async fn handle_tunnel(shared: Arc<Shared>, incoming: Incoming) {
    let remote = incoming.remote_address();
    if shared.refused_too_often(remote.ip()) {
        incoming.refuse();
        return;
    }
    let connection = match timeout(HANDSHAKE_TIMEOUT, incoming).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(err)) => {
            tracing::debug!(%remote, %err, "a tunnel handshake failed");
            shared.record_refusal(remote.ip());
            return;
        }
        Err(_) => return,
    };
    let Some(fingerprint) = uwumail_tunnel::peer_fingerprint(&connection) else {
        connection.close(CLOSE_REFUSED, b"no certificate");
        return;
    };
    let (send, recv) = match timeout(HELLO_TIMEOUT, connection.accept_bi()).await {
        Ok(Ok(streams)) => streams,
        _ => {
            connection.close(CLOSE_REFUSED, b"no hello");
            return;
        }
    };
    let mut control = TunnelStream::new(send, recv);
    let hello: Hello = match timeout(HELLO_TIMEOUT, proto::read_message(&mut control)).await {
        Ok(Ok(hello)) => hello,
        _ => {
            connection.close(CLOSE_REFUSED, b"no hello");
            return;
        }
    };

    match shared.authorize(fingerprint, &hello).await {
        Ok(hostname) => {
            let services = shared.shared_services(&hello);
            serve_server(shared, connection, control, fingerprint, hostname, services, (hello.control, hello.logs))
                .await
        }
        Err((reason, message)) => {
            shared.record_refusal(remote.ip());
            tracing::warn!(%remote, ?reason, %fingerprint, "refused a tunnel");
            let _ = proto::write_message(&mut control, &HelloReply::Refused { reason, message }).await;
            let _ = control.shutdown().await;
            // The server hangs up after reading the answer; closing first could swallow it.
            let _ = timeout(Duration::from_secs(3), connection.closed()).await;
            connection.close(CLOSE_REFUSED, b"refused");
        }
    }
}

async fn serve_server(
    shared: Arc<Shared>,
    connection: Connection,
    mut control: TunnelStream,
    server: Fingerprint,
    hostname: String,
    services: Vec<Service>,
    (talks, wants_logs): (bool, bool),
) {
    let welcome = HelloReply::Welcome(shared.welcome(services.clone()));
    if proto::write_message(&mut control, &welcome).await.is_err() {
        return;
    }
    let remote = connection.remote_address();
    *shared.hostname.write().expect("hostname poisoned") = hostname.clone();
    let active = Active { connection: connection.clone(), server, hostname: hostname.clone(), services };
    if let Some(previous) = shared.active.send_replace(Some(active)) {
        previous.connection.close(CLOSE_REPLACED, b"a newer connection of the server took over");
    }
    // Before anything else: from here on no ban may touch the address this tunnel comes from.
    // A home connection brings a new one every night, so this is where the firewall learns it.
    shared.machine.trust(remote.ip().to_canonical());
    tracing::info!(%remote, server = %hostname, "the UwUMail server is connected");

    // A server from before the control stream never reads it again; for that one it is only held open.
    let mut held_open = Some(control);
    let talking = talks.then(|| {
        let (send, recv) = held_open.take().expect("the control stream is still here").into_parts();
        tokio::spawn(talk(shared.clone(), remote, send, recv, wants_logs))
    });

    while let Ok((send, recv)) = connection.accept_bi().await {
        tokio::spawn(crate::outbound::handle(shared.clone(), TunnelStream::new(send, recv)));
    }
    if let Some(talking) = talking {
        talking.abort();
    }
    drop(held_open);

    let id = connection.stable_id();
    shared.active.send_if_modified(|active| {
        let ours = active.as_ref().is_some_and(|active| active.connection.stable_id() == id);
        if ours {
            *active = None;
        }
        ours
    });
    let reason = connection.close_reason().map(|reason| reason.to_string()).unwrap_or_default();
    tracing::info!(%remote, server = %hostname, %reason, "the UwUMail server disconnected");
}

/// The control stream after the handshake: the gateway reports on the machine it runs on so the
/// portal can show it, and carries out the bans the server asks for.
async fn talk(shared: Arc<Shared>, remote: SocketAddr, send: SendStream, mut recv: RecvStream, wants_logs: bool) {
    // Status reports and log batches share the one stream, so they take turns at it.
    let send = tokio::sync::Mutex::new(send);
    let asked = async {
        // A message this version does not know is skipped, not fatal: a newer server keeps talking.
        while let Ok(message) = proto::read_known::<_, ServerMessage>(&mut recv).await {
            match message {
                Some(ServerMessage::Ban { ip, seconds, reason }) => match shared.may_ban(ip) {
                    Ok(()) => {
                        if shared.machine.ban(ip, seconds, &reason) {
                            tracing::info!(%ip, seconds, %reason, "the server asked to keep an address out");
                        }
                    }
                    // Loud on purpose: this is the case that would take the server off the internet.
                    Err(why) => tracing::warn!(%ip, why, "did not ban an address the server asked about"),
                },
                Some(ServerMessage::Unban { ip }) if shared.machine.unban(ip) => {
                    tracing::info!(%ip, "the server asked to let an address in again");
                }
                Some(ServerMessage::Task { id, verb, version }) => {
                    // The machine checks all three again before anything happens; this only hands
                    // them over. Whether it worked comes back with the next status.
                    shared.machine.ask(&id, &verb, version.as_deref());
                }
                Some(ServerMessage::Unban { .. }) | None => {}
            }
        }
    };
    let reporting = async {
        loop {
            // Says again that this is where the tunnel is, so the note does not time out under a
            // connection that is very much in use. The trust is deliberately short-lived: the
            // address the server gives up at the nightly reconnect belongs to someone else by
            // morning, and it must not stay unbannable after that.
            shared.machine.trust(remote.ip().to_canonical());
            let software = format!("uwumail-gateway {}", env!("CARGO_PKG_VERSION"));
            let status = shared.machine.status(software);
            // While something is being installed the portal is watching, so it is told every few
            // seconds instead of every few minutes. The rest of the time this is a quiet heartbeat.
            let busy = status.job.as_ref().is_some_and(|job| job.state == "running");
            if proto::write_message(&mut *send.lock().await, &GatewayMessage::Status(Box::new(status))).await.is_err() {
                break;
            }
            tokio::time::sleep(if busy { REPORT_WHILE_BUSY } else { REPORT_EVERY }).await;
        }
    };
    let logs = shared.logs.clone().filter(|_| wants_logs);
    let shipping = async {
        let Some(logs) = logs else { return std::future::pending().await };
        loop {
            logs.wait().await;
            // A moment for the lines that come together to travel together.
            tokio::time::sleep(LOG_BATCH_DELAY).await;
            loop {
                let batch = logs.take_batch();
                if batch.is_empty() {
                    break;
                }
                let message = GatewayMessage::Logs { lines: batch };
                if proto::write_message(&mut *send.lock().await, &message).await.is_err() {
                    let GatewayMessage::Logs { lines } = message else { unreachable!("built a line ago") };
                    logs.put_back(lines);
                    return;
                }
            }
        }
    };
    tokio::select! {
        _ = asked => {}
        _ = reporting => {}
        _ = shipping => {}
    }
}

/// Follows changes of the pairing on disk: a removed pairing disconnects the server and brings a
/// new pairing code, which goes to the log.
async fn watch_pairing(shared: Arc<Shared>, tunnel_port: u16, mut shutdown: watch::Receiver<bool>) {
    let mut announced: Option<String> = None;
    loop {
        match shared.state.pairing() {
            Ok(Some(pairing)) => {
                announced = None;
                *shared.hostname.write().expect("hostname poisoned") = pairing.hostname.clone();
                shared.active.send_if_modified(|active| {
                    let other = active.as_ref().is_some_and(|active| active.server != pairing.server);
                    if other && let Some(active) = active.take() {
                        active.connection.close(CLOSE_UNPAIRED, b"the gateway was paired with another server");
                    }
                    other
                });
            }
            Ok(None) => {
                if let Some(active) = shared.active.send_replace(None) {
                    tracing::warn!(server = %active.hostname, "the pairing was removed, disconnecting the server");
                    active.connection.close(CLOSE_UNPAIRED, b"the gateway forgot this server");
                }
                let token = match shared.state.token() {
                    Ok(Some(token)) => Some(token),
                    Ok(None) => shared.state.create_token().map_err(|err| tracing::error!("{err:#}")).ok(),
                    Err(err) => {
                        tracing::error!("{err:#}");
                        None
                    }
                };
                if let Some(token) = token
                    && announced.as_deref() != Some(token.to_text().as_str())
                {
                    match pairing_code(&shared.public_addresses, tunnel_port, &shared.identity, &token) {
                        Some(code) => tracing::warn!(
                            "not paired yet: enter this pairing code on your UwUMail server under Server → Setup: {code}"
                        ),
                        None => tracing::warn!(
                            "not paired yet, and no public address is known for the pairing code: set public_addresses"
                        ),
                    }
                    announced = Some(token.to_text());
                }
            }
            Err(err) => tracing::error!(error = %format!("{err:#}"), "reading the pairing failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(PAIRING_CHECK) => {}
            _ = shutdown.changed() => break,
        }
    }
}

/// Host names go into SMTP answers, so only what a host name may contain gets through.
fn clean_hostname(hostname: &str) -> String {
    let hostname = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    let valid = !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if valid { hostname } else { NO_HOSTNAME.into() }
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_cannot_smuggle_commands() {
        assert_eq!(clean_hostname("Mail.Example.COM."), "mail.example.com");
        assert_eq!(clean_hostname("mail.example.com\r\n250 OK"), NO_HOSTNAME);
        assert_eq!(clean_hostname(""), NO_HOSTNAME);
    }
}
