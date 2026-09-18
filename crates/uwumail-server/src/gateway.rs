//! This server's side of a UwUMail Gateway: the pairing, the tunnel, connections that arrive
//! through the gateway and connections to other servers from there.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use tokio_rustls::TlsAcceptor;
use uwumail_smtp::{BoxIo, Connector, ListenerKind, Smtp};
use uwumail_store::Store;
use uwumail_tunnel::{
    ClientSettings, Fingerprint, Identity, Inbound, Open, PairingCode, Service, Status, Token, TunnelClient,
    TunnelStream,
};
use uwumail_web::gateway::{GatewayBackend, GatewayFuture, GatewayState, GatewayView};

use crate::config::GatewayConfig;
use crate::http;

/// Where the pairing lives in the settings table.
pub const PAIRING_KEY: &str = "gateway.pairing";

/// How long the gateway keeps a network away that this server turned away. The same hour its
/// fail2ban jail uses, so the two do not disagree about when someone may come back.
const BAN_AT_GATEWAY: Duration = Duration::from_secs(3600);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredPairing {
    /// Where the gateway waits for the tunnel.
    pub addresses: Vec<SocketAddr>,
    pub gateway: Fingerprint,
    /// This server's key in the tunnel.
    pub identity: Identity,
    /// The token of the code the pairing came from.
    pub token: String,
    /// Whether the gateway accepted the token already.
    pub confirmed: bool,
}

/// What connections that arrive through the gateway are served with.
#[derive(Clone)]
pub struct Services {
    pub smtp: Smtp,
    pub imap: uwumail_imap::Imap,
    /// TLS for mail apps (IMAP on 993).
    pub mail_tls: Arc<rustls::ServerConfig>,
    pub https_tls: Arc<rustls::ServerConfig>,
    pub https: Router,
    pub http: Router,
}

impl Inbound for Services {
    fn open(&self, open: Open, stream: TunnelStream) {
        let (client, stream): (SocketAddr, BoxIo) = (open.client, Box::new(stream));
        let kind = match open.service {
            Service::Smtp => ListenerKind::Mx,
            Service::Submission => ListenerKind::Submission,
            Service::Submissions => ListenerKind::SubmissionTls,
            Service::Http => {
                tokio::spawn(http::serve_connection(stream, client, None, self.http.clone()));
                return;
            }
            Service::Https => {
                let acceptor = TlsAcceptor::from(self.https_tls.clone());
                tokio::spawn(http::serve_connection(stream, client, Some(acceptor), self.https.clone()));
                return;
            }
            Service::Imaps => {
                self.imap.serve_stream(stream, client, self.mail_tls.clone());
                return;
            }
        };
        tokio::spawn(uwumail_smtp::serve_stream(self.smtp.clone(), stream, client, kind));
    }
}

/// Connections to other servers start at the gateway, so they never show this server's address.
struct ThroughGateway {
    client: TunnelClient,
}

impl Connector for ThroughGateway {
    fn connect(
        &self,
        address: SocketAddr,
        limit: Duration,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<BoxIo>> + Send + '_>> {
        Box::pin(async move {
            if !through_gateway(address) {
                return uwumail_smtp::connect_directly(address, limit).await;
            }
            let stream = self.client.connect(address, limit).await?;
            Ok(Box::new(stream) as BoxIo)
        })
    }
}

/// What the gateway said about its machine, in the portal's own words. The portal has no idea a
/// tunnel is involved, so the shapes are kept apart and translated here.
fn machine_view(status: uwumail_tunnel::proto::GatewayStatus) -> uwumail_web::gateway::GatewayMachine {
    use uwumail_web::gateway::{GatewayMachine, GatewayProtection, GatewaySystem};

    GatewayMachine {
        system: status.system.map(|system| GatewaySystem {
            name: system.name,
            updates: system.updates,
            security_updates: system.security_updates,
            reboot_required: system.reboot_required,
            automatic_security: system.automatic_security,
            new_release: system.new_release,
            command: system.command,
        }),
        protection: status.protection.map(|protection| GatewayProtection {
            firewall: protection.firewall,
            firewall_active: protection.firewall_active,
            fail2ban: protection.fail2ban,
            banned: protection.banned,
            jails: protection.jails,
            from_server: protection.from_server,
        }),
        trusted: status.trusted.iter().map(ToString::to_string).collect(),
        checked_at: status.checked_at,
        job: status.job.map(|job| uwumail_web::gateway::GatewayJob {
            id: job.id,
            state: job.state,
            error: job.error,
            at: job.at,
            log: job.log,
        }),
    }
}

/// An id that names a file on the other side, so letters and digits only. It does not have to be
/// hard to guess -- only one of a kind, and only ever made here.
fn job_id() -> String {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_nanos() as u64);
    format!("{nanos:016x}{:04x}", std::process::id() & 0xffff)
}

/// Servers in the own network, like fixed routes to a private address, are reached directly:
/// that reveals nothing, and the gateway would not connect there anyway.
fn through_gateway(address: SocketAddr) -> bool {
    uwumail_tunnel::net::is_global(address.ip())
}

/// The tunnel in use.
struct Current {
    client: TunnelClient,
    pairing: StoredPairing,
    /// Since when the tunnel is down, while it is.
    down_since: Arc<Mutex<Option<i64>>>,
}

/// Keeps the tunnel to the paired gateway up, and pairs or forgets while the server runs.
pub struct GatewayManager {
    store: Store,
    smtp: Smtp,
    hostname: String,
    from_config: bool,
    shutdown: watch::Receiver<bool>,
    services: OnceLock<Services>,
    current: Arc<Mutex<Option<Current>>>,
    /// Notified each time the tunnel comes up: from then on the certificate authority reaches us.
    tunnel_up: Arc<Notify>,
}

impl GatewayManager {
    pub fn new(
        store: Store,
        smtp: Smtp,
        hostname: String,
        config: &GatewayConfig,
        shutdown: watch::Receiver<bool>,
    ) -> Arc<GatewayManager> {
        Arc::new(GatewayManager {
            store,
            smtp,
            hostname,
            from_config: !config.code.trim().is_empty(),
            shutdown,
            services: OnceLock::new(),
            current: Arc::default(),
            tunnel_up: Arc::default(),
        })
    }

    /// Whether a gateway is paired, connected or not.
    pub fn is_paired(&self) -> bool {
        self.current.lock().expect("gateway poisoned").is_some()
    }

    /// Notified each time the tunnel comes up.
    pub fn tunnel_up(&self) -> Arc<Notify> {
        self.tunnel_up.clone()
    }

    /// Hand this to whatever turns networks away — SMTP, IMAP, the portal — and the gateway keeps
    /// them off its public ports too. Only this server sees who fails to log in: the gateway
    /// carries TLS it cannot read, so without this the guessing simply arrives again.
    ///
    /// The gateway refuses to ban the address its own tunnel comes from, so a mail app at home
    /// with the wrong password cannot take the household off its own gateway.
    pub fn reporter(self: &Arc<Self>) -> uwumail_smtp::Reporter {
        // Weak, so the manager is not kept alive by the services it hands this to.
        let manager = Arc::downgrade(self);
        Arc::new(move |ip, why: &str| {
            let Some(manager) = manager.upgrade() else {
                return;
            };
            let client =
                manager.current.lock().expect("gateway poisoned").as_ref().map(|current| current.client.clone());
            if let Some(client) = client {
                client.ban(ip, BAN_AT_GATEWAY, why);
            }
        })
    }

    /// Connects with the stored pairing, or pairs with the configured code, once the services
    /// for arriving connections exist.
    pub async fn start(&self, services: Services, config: &GatewayConfig) {
        let _ = self.services.set(services);
        match prepare(&self.store, config).await {
            Ok(Some(pairing)) => self.connect(pairing),
            Ok(None) => {}
            Err(err) => tracing::error!(error = %format!("{err:#}"), "the UwUMail Gateway pairing could not be used"),
        }
    }

    fn connect(&self, pairing: StoredPairing) {
        let Some(services) = self.services.get() else {
            return;
        };
        let settings = ClientSettings {
            addresses: pairing.addresses.clone(),
            gateway: pairing.gateway,
            identity: pairing.identity.clone(),
            hostname: self.hostname.clone(),
            software: format!("uwumail-server {}", env!("CARGO_PKG_VERSION")),
            services: Service::ALL.to_vec(),
            token: if pairing.confirmed { None } else { Token::from_text(&pairing.token) },
        };
        let client = TunnelClient::start(settings, Arc::new(services.clone()), self.shutdown.clone());
        // From now on mail to other servers only leaves through the gateway, also while it is away:
        // it waits in the queue instead of going out from here.
        self.smtp.set_connector(Some(Arc::new(ThroughGateway { client: client.clone() })));
        tracing::info!(gateway = %pairing.gateway, "mail to other servers goes through the UwUMail Gateway");

        let down_since = Arc::new(Mutex::new(Some(unix_now())));
        let current = Current { client: client.clone(), pairing: pairing.clone(), down_since: down_since.clone() };
        if let Some(previous) = self.current.lock().expect("gateway poisoned").replace(current) {
            previous.client.stop();
        }
        let tunnel_up = self.tunnel_up.clone();
        tokio::spawn(follow(self.store.clone(), self.current.clone(), client, pairing, down_since, tunnel_up));
    }

    async fn pair_with(&self, code: &str) -> Result<(), String> {
        let code = PairingCode::parse(code).map_err(|err| err.to_string())?;
        if self.services.get().is_none() {
            return Err("the server is still starting, try again in a moment".into());
        }
        // Keeping this server's key does no harm and lets a gateway that still knows it take it back.
        let identity = match load(&self.store).await.map_err(|err| format!("{err:#}"))? {
            Some(stored) => stored.identity,
            None => Identity::generate().map_err(|err| err.to_string())?,
        };
        let pairing = StoredPairing {
            addresses: code.addresses,
            gateway: code.fingerprint,
            identity,
            token: code.token.to_text(),
            confirmed: false,
        };
        save(&self.store, &pairing).await.map_err(|err| format!("{err:#}"))?;
        tracing::info!(gateway = %pairing.gateway, "pairing with a UwUMail Gateway from the portal");
        self.connect(pairing);
        Ok(())
    }

    async fn forget_gateway(&self) -> Result<(), String> {
        if let Some(previous) = self.current.lock().expect("gateway poisoned").take() {
            previous.client.stop();
        }
        self.smtp.set_connector(None);
        self.store.delete_setting(PAIRING_KEY).await.map_err(|err| err.to_string())?;
        tracing::warn!("forgot the UwUMail Gateway: mail leaves from this server again");
        Ok(())
    }
}

impl GatewayBackend for GatewayManager {
    fn view(&self) -> GatewayView {
        let current = self.current.lock().expect("gateway poisoned");
        let Some(current) = current.as_ref() else {
            return GatewayView { from_config: self.from_config, ..GatewayView::default() };
        };
        let mut view = GatewayView {
            state: GatewayState::Connecting,
            tunnel: current.pairing.addresses.iter().map(ToString::to_string).collect(),
            fingerprint: Some(current.pairing.gateway.to_string()),
            down_since: *current.down_since.lock().expect("gateway poisoned"),
            from_config: self.from_config,
            ..GatewayView::default()
        };
        match current.client.status() {
            Status::Connecting { error } => view.error = error,
            Status::Connected { welcome, since, .. } => {
                view.state = GatewayState::Connected;
                view.addresses = welcome.addresses.iter().map(ToString::to_string).collect();
                view.services = welcome.services.iter().map(|service| service.as_str().to_owned()).collect();
                view.outbound_ports = welcome.outbound_ports;
                view.software = Some(welcome.software);
                view.connected_since = Some(since);
                view.down_since = None;
                view.can_install = welcome.tasks;
                view.machine = current.client.gateway_status().map(machine_view);
            }
            Status::Refused { reason, message } => {
                view.state = GatewayState::Refused;
                view.refusal = serde_json::to_value(reason).ok().and_then(|value| value.as_str().map(str::to_owned));
                view.error = Some(message);
            }
            Status::Stopped => {}
        }
        view
    }

    fn pair<'a>(&'a self, code: &'a str) -> GatewayFuture<'a> {
        Box::pin(self.pair_with(code))
    }

    fn forget(&self) -> GatewayFuture<'_> {
        Box::pin(self.forget_gateway())
    }

    fn ask<'a>(
        &'a self,
        verb: &'a str,
        version: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let client = {
                let current = self.current.lock().expect("gateway poisoned");
                current.as_ref().map(|current| current.client.clone())
            };
            let client = client.ok_or_else(|| "no gateway is paired with this server".to_owned())?;
            if !matches!(client.status(), Status::Connected { ref welcome, .. } if welcome.tasks) {
                return Err("the gateway is not listening for this right now".into());
            }
            if client.gateway_status().and_then(|status| status.job).is_some_and(|job| job.state == "running") {
                return Err("something is already running on the gateway".into());
            }
            let id = job_id();
            // The ask goes down the same control stream as a ban, and like a ban it is never
            // waited for: what came of it arrives with the next status, which the gateway sends
            // every few seconds while something runs.
            if !client.task(&id, verb, version) {
                return Err("the gateway did not take it".into());
            }
            tracing::info!(%verb, %id, "asked the gateway's machine for a job");
            Ok(id)
        })
    }
}

/// Notes when the tunnel goes down and comes back, and confirms a new pairing once it is up.
async fn follow(
    store: Store,
    current: Arc<Mutex<Option<Current>>>,
    client: TunnelClient,
    pairing: StoredPairing,
    down_since: Arc<Mutex<Option<i64>>>,
    tunnel_up: Arc<Notify>,
) {
    let mut status = client.subscribe();
    let mut confirmed = pairing.confirmed;
    let mut was_connected = false;
    loop {
        let connected = match *status.borrow_and_update() {
            Status::Stopped => return,
            Status::Connected { .. } => true,
            _ => false,
        };
        if connected && !was_connected {
            // A certificate that could not be ordered while the tunnel was down can be now.
            tunnel_up.notify_one();
        }
        was_connected = connected;
        {
            let mut down = down_since.lock().expect("gateway poisoned");
            *down = if connected { None } else { down.or(Some(unix_now())) };
        }
        if connected && !confirmed {
            confirmed = true;
            // A pairing that was replaced in the meantime stays replaced.
            let still_current = current
                .lock()
                .expect("gateway poisoned")
                .as_ref()
                .is_some_and(|now| now.pairing.token == pairing.token && now.pairing.gateway == pairing.gateway);
            if still_current && let Err(err) = save(&store, &StoredPairing { confirmed: true, ..pairing.clone() }).await
            {
                tracing::error!(error = %format!("{err:#}"), "saving the gateway pairing failed");
            }
        }
        if status.changed().await.is_err() {
            return;
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

async fn load(store: &Store) -> anyhow::Result<Option<StoredPairing>> {
    match store.setting(PAIRING_KEY).await? {
        Some(raw) => Ok(Some(serde_json::from_str(&raw).context("the stored gateway pairing is damaged")?)),
        None => Ok(None),
    }
}

/// The pairing to use: the stored one, or a new one when the configuration has a code that was
/// not used yet.
pub(crate) async fn prepare(store: &Store, config: &GatewayConfig) -> anyhow::Result<Option<StoredPairing>> {
    let stored = load(store).await?;
    let code = config.code.trim();
    if code.is_empty() {
        return Ok(stored);
    }
    let code = PairingCode::parse(code).context("gateway.code")?;
    let token = code.token.to_text();
    if let Some(stored) = &stored
        && stored.gateway == code.fingerprint
        && stored.token == token
    {
        // The gateway keeps its token until it was used, so a corrected code, with other addresses
        // or another port, carries the same one. A confirmed pairing has proven its addresses.
        if stored.confirmed || stored.addresses == code.addresses {
            return Ok(Some(stored.clone()));
        }
        let pairing = StoredPairing { addresses: code.addresses, ..stored.clone() };
        save(store, &pairing).await?;
        tracing::info!(gateway = %pairing.gateway, "took the gateway's addresses from the corrected code");
        return Ok(Some(pairing));
    }
    // A new code. Keeping this server's key does no harm and lets a gateway that still knows it
    // take the server back.
    let identity = match stored {
        Some(stored) => stored.identity,
        None => Identity::generate()?,
    };
    let pairing =
        StoredPairing { addresses: code.addresses, gateway: code.fingerprint, identity, token, confirmed: false };
    save(store, &pairing).await?;
    tracing::info!(gateway = %pairing.gateway, "pairing with the UwUMail Gateway from the configured code");
    Ok(Some(pairing))
}

async fn save(store: &Store, pairing: &StoredPairing) -> anyhow::Result<()> {
    store.set_setting(PAIRING_KEY, &serde_json::to_string(pairing)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(gateway: &Identity, token: &Token) -> String {
        PairingCode {
            addresses: vec!["192.0.2.10:443".parse().unwrap()],
            fingerprint: gateway.fingerprint(),
            token: token.clone(),
        }
        .encode()
    }

    #[tokio::test]
    async fn a_code_pairs_once_and_a_new_code_pairs_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        assert!(prepare(&store, &GatewayConfig::default()).await.unwrap().is_none(), "no gateway, no pairing");

        let gateway = Identity::generate().unwrap();
        let token = Token::generate();
        let first = GatewayConfig { code: code(&gateway, &token) };
        let pairing = prepare(&store, &first).await.unwrap().unwrap();
        assert_eq!(pairing.gateway, gateway.fingerprint());
        assert!(!pairing.confirmed);

        // The gateway showed a wrong port at first: the corrected code has the same token, and
        // while the gateway has not accepted the pairing its addresses are taken over.
        let elsewhere: SocketAddr = "192.0.2.10:4433".parse().unwrap();
        let corrected =
            PairingCode { addresses: vec![elsewhere], fingerprint: gateway.fingerprint(), token: token.clone() };
        let moved = prepare(&store, &GatewayConfig { code: corrected.encode() }).await.unwrap().unwrap();
        assert_eq!(moved.addresses, [elsewhere]);
        assert_eq!(moved.identity.fingerprint(), pairing.identity.fingerprint(), "with the same key");
        let pairing = prepare(&store, &first).await.unwrap().unwrap();
        assert_eq!(pairing.addresses, ["192.0.2.10:443".parse::<SocketAddr>().unwrap()]);

        // Confirmed by the gateway; the same code in the configuration changes nothing after a restart.
        save(&store, &StoredPairing { confirmed: true, ..pairing.clone() }).await.unwrap();
        let again = prepare(&store, &first).await.unwrap().unwrap();
        assert!(again.confirmed);
        assert_eq!(again.identity.fingerprint(), pairing.identity.fingerprint());
        assert!(prepare(&store, &GatewayConfig::default()).await.unwrap().unwrap().confirmed);
        // A pairing that works has proven its addresses, so an old code with other ones changes nothing.
        let kept = prepare(&store, &GatewayConfig { code: corrected.encode() }).await.unwrap().unwrap();
        assert_eq!(kept.addresses, pairing.addresses);

        // After `uwumail-gateway unpair`: a new token for the same gateway pairs again, with the same key.
        let second = GatewayConfig { code: code(&gateway, &Token::generate()) };
        let repaired = prepare(&store, &second).await.unwrap().unwrap();
        assert!(!repaired.confirmed);
        assert_eq!(repaired.identity.fingerprint(), pairing.identity.fingerprint());
    }

    /// A gateway with web ports on this machine and its pairing code. Dropping the sender stops it.
    async fn test_gateway(dir: &std::path::Path) -> (uwumail_gateway::Running, PairingCode, watch::Sender<bool>) {
        use uwumail_gateway::config::{ListenConfig, OutboundConfig};

        let (running_until, never) = watch::channel(false);
        let config = uwumail_gateway::GatewayConfig {
            tunnel: "127.0.0.1:0".into(),
            state_dir: dir.join("gateway"),
            public_addresses: vec!["127.0.0.1".parse().unwrap()],
            listen: ListenConfig {
                smtp: String::new(),
                submission: String::new(),
                submissions: String::new(),
                http: "127.0.0.1:0".into(),
                https: "127.0.0.1:0".into(),
                imaps: "127.0.0.1:0".into(),
            },
            outbound: OutboundConfig { ports: vec![25], allow_private: true },
            ..uwumail_gateway::GatewayConfig::default()
        };
        let running = uwumail_gateway::start(config, never).await.unwrap();
        let state = uwumail_gateway::state::State::open(&dir.join("gateway")).unwrap();
        let token = loop {
            if let Some(token) = state.token().unwrap() {
                break token;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let fingerprint = state.identity().unwrap().unwrap().fingerprint();
        let code = PairingCode { addresses: vec![running.tunnel], fingerprint, token };
        (running, code, running_until)
    }

    /// This server's services with a self-signed certificate for `localhost`.
    async fn test_services(dir: &std::path::Path) -> (Services, Vec<u8>) {
        let store = Store::open(&dir.join("server")).await.unwrap();
        let smtp = Smtp::new(
            store,
            uwumail_smtp::SmtpSettings {
                hostname: "mail.example.com".into(),
                smtp: Default::default(),
                spam: Default::default(),
                delivery: Default::default(),
                tone: Default::default(),
                server_tls: None,
            },
        )
        .unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into());
        let mail_tls =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate.cert.der().clone()], key)
                .unwrap();
        let mut tls = mail_tls.clone();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let imap = uwumail_imap::Imap::new(smtp.store().clone(), 1024 * 1024);

        let state = http::HttpState {
            hostname: "mail.example.com".into(),
            challenges: Arc::default(),
            started: std::time::Instant::now(),
        };
        let who = Router::new().route(
            "/who",
            axum::routing::get(|axum::Extension(client): axum::Extension<uwumail_jmap::ClientInfo>| async move {
                format!("{} https={}", client.ip, client.https)
            }),
        );
        let services = Services {
            smtp,
            imap,
            mail_tls: Arc::new(mail_tls),
            https_tls: Arc::new(tls),
            https: http::app(state.clone(), Router::new(), who, Arc::default()),
            http: http::redirect_app(state),
        };
        (services, certificate.cert.der().to_vec())
    }

    /// A gateway with web ports, and this server's services behind it.
    async fn web_behind_gateway(
        dir: &std::path::Path,
    ) -> (uwumail_gateway::Running, TunnelClient, Vec<u8>, watch::Sender<bool>) {
        let (running, code, running_until) = test_gateway(dir).await;
        let (services, certificate) = test_services(dir).await;
        let settings = ClientSettings {
            addresses: code.addresses.clone(),
            gateway: code.fingerprint,
            identity: Identity::generate().unwrap(),
            hostname: "mail.example.com".into(),
            software: "test".into(),
            services: Service::ALL.to_vec(),
            token: Some(code.token.clone()),
        };
        let client = TunnelClient::start(settings, Arc::new(services), running_until.subscribe());
        let mut status = client.subscribe();
        tokio::time::timeout(Duration::from_secs(20), status.wait_for(|s| matches!(s, Status::Connected { .. })))
            .await
            .expect("the tunnel comes up")
            .unwrap();
        (running, client, certificate, running_until)
    }

    #[tokio::test]
    async fn the_portal_pairs_and_forgets_a_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let (_gateway, code, running_until) = test_gateway(dir.path()).await;
        let (services, _) = test_services(dir.path()).await;
        let (store, smtp) = (services.smtp.store().clone(), services.smtp.clone());
        let manager = GatewayManager::new(
            store.clone(),
            smtp.clone(),
            "mail.example.com".into(),
            &GatewayConfig::default(),
            running_until.subscribe(),
        );
        manager.start(services, &GatewayConfig::default()).await;
        assert_eq!(manager.view().state, GatewayState::None);
        assert!(manager.pair("uwugw1broken").await.is_err());
        assert!(!manager.is_paired());

        manager.pair(&code.encode()).await.unwrap();
        assert!(manager.is_paired());
        assert!(smtp.has_connector(), "mail goes through the gateway from the moment of pairing");
        let started = std::time::Instant::now();
        let view = loop {
            let view = manager.view();
            if view.state == GatewayState::Connected {
                break view;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "still {:?}", view.state);
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(view.addresses, ["127.0.0.1"]);
        assert!(view.down_since.is_none() && view.connected_since.is_some());
        // The certificate task hears about it, so it does not wait for its hourly retry.
        tokio::time::timeout(Duration::from_secs(5), manager.tunnel_up().notified())
            .await
            .expect("the tunnel coming up is announced");
        // Confirmed in the database, so a restart does not send the used token again.
        let confirmed = loop {
            if load(&store).await.unwrap().unwrap().confirmed {
                break true;
            }
            assert!(started.elapsed() < Duration::from_secs(20));
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(confirmed);

        manager.forget().await.unwrap();
        assert_eq!(manager.view().state, GatewayState::None);
        assert!(!smtp.has_connector(), "mail leaves from here again");
        assert!(load(&store).await.unwrap().is_none());
    }

    async fn http_get<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(mut stream: S, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let request = format!("GET {path} HTTP/1.1\r\nHost: mail.example.com\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    #[tokio::test]
    async fn web_requests_arrive_through_the_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let (gateway, _client, certificate, _running) = web_behind_gateway(dir.path()).await;

        // Port 80 sends browsers to HTTPS.
        let socket = tokio::net::TcpStream::connect(gateway.listener(Service::Http).unwrap()).await.unwrap();
        let response = http_get(socket, "/login").await;
        assert!(response.starts_with("HTTP/1.1 308"), "{response}");
        assert!(response.contains("location: https://mail.example.com/login"), "{response}");

        // TLS ends here, not at the gateway, and the app sees the browser's address.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls_pki_types::CertificateDer::from(certificate)).unwrap();
        let config =
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let socket = tokio::net::TcpStream::connect(gateway.listener(Service::Https).unwrap()).await.unwrap();
        let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
        let tls = connector.connect(name.clone(), socket).await.unwrap();
        let response = http_get(tls, "/who").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("127.0.0.1 https=true"), "{response}");

        // Mail apps reach IMAP on 993, encrypted end to end as well.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let socket = tokio::net::TcpStream::connect(gateway.listener(Service::Imaps).unwrap()).await.unwrap();
        let tls = connector.connect(name, socket).await.unwrap();
        let mut imap = tokio::io::BufReader::new(tls);
        let mut greeting = String::new();
        imap.read_line(&mut greeting).await.unwrap();
        assert!(greeting.starts_with("* OK [CAPABILITY IMAP4rev1"), "{greeting}");
        imap.get_mut().write_all(b"a LOGOUT\r\n").await.unwrap();
        let mut bye = String::new();
        imap.read_line(&mut bye).await.unwrap();
        assert!(bye.starts_with("* BYE"), "{bye}");
    }

    #[test]
    fn only_public_destinations_go_through_the_gateway() {
        assert!(through_gateway("8.8.8.8:25".parse().unwrap()));
        assert!(!through_gateway("192.168.1.20:25".parse().unwrap()));
        assert!(!through_gateway("[fd00::5]:25".parse().unwrap()));
    }
}
