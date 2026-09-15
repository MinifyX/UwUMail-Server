//! This server's side of a UwUMail Gateway: the pairing, the tunnel, connections that arrive
//! through the gateway and connections to other servers from there.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use uwumail_smtp::{BoxIo, Connector, ListenerKind, Smtp};
use uwumail_store::Store;
use uwumail_tunnel::{
    ClientSettings, Fingerprint, Identity, Inbound, Open, PairingCode, Service, Status, Token, TunnelClient,
    TunnelStream,
};

use crate::config::GatewayConfig;
use crate::http;

/// Where the pairing lives in the settings table.
pub const PAIRING_KEY: &str = "gateway.pairing";

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
pub struct Services {
    pub smtp: Smtp,
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

/// Servers in the own network, like fixed routes to a private address, are reached directly:
/// that reveals nothing, and the gateway would not connect there anyway.
fn through_gateway(address: SocketAddr) -> bool {
    uwumail_tunnel::net::is_global(address.ip())
}

/// Pairs from the configured code if there is a new one, then keeps the tunnel up until
/// `shutdown`. Without a pairing it does nothing.
pub async fn run(
    store: Store,
    config: GatewayConfig,
    hostname: String,
    services: Services,
    mut shutdown: watch::Receiver<bool>,
) {
    let pairing = match prepare(&store, &config).await {
        Ok(Some(pairing)) => pairing,
        Ok(None) => return,
        Err(err) => {
            tracing::error!(error = %format!("{err:#}"), "the UwUMail Gateway pairing could not be used");
            return;
        }
    };
    let smtp = services.smtp.clone();
    let settings = ClientSettings {
        addresses: pairing.addresses.clone(),
        gateway: pairing.gateway,
        identity: pairing.identity.clone(),
        hostname,
        software: format!("uwumail-server {}", env!("CARGO_PKG_VERSION")),
        token: if pairing.confirmed { None } else { Token::from_text(&pairing.token) },
    };
    let client = TunnelClient::start(settings, Arc::new(services), shutdown.clone());
    // From now on mail to other servers only leaves through the gateway, also while it is away:
    // it waits in the queue instead of going out from here.
    smtp.set_connector(Some(Arc::new(ThroughGateway { client: client.clone() })));
    tracing::info!(gateway = %pairing.gateway, "mail to other servers goes through the UwUMail Gateway");

    if pairing.confirmed {
        return;
    }
    let mut status = client.subscribe();
    loop {
        if matches!(*status.borrow_and_update(), Status::Connected { .. }) {
            let confirmed = StoredPairing { confirmed: true, ..pairing };
            if let Err(err) = save(&store, &confirmed).await {
                tracing::error!(error = %format!("{err:#}"), "saving the gateway pairing failed");
            }
            return;
        }
        tokio::select! {
            changed = status.changed() => if changed.is_err() { return },
            _ = shutdown.changed() => return,
        }
    }
}

/// The pairing to use: the stored one, or a new one when the configuration has a code that was
/// not used yet.
async fn prepare(store: &Store, config: &GatewayConfig) -> anyhow::Result<Option<StoredPairing>> {
    let stored = match store.setting(PAIRING_KEY).await? {
        Some(raw) => {
            Some(serde_json::from_str::<StoredPairing>(&raw).context("the stored gateway pairing is damaged")?)
        }
        None => None,
    };
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
        return Ok(Some(stored.clone()));
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
        let first = GatewayConfig { code: code(&gateway, &Token::generate()) };
        let pairing = prepare(&store, &first).await.unwrap().unwrap();
        assert_eq!(pairing.gateway, gateway.fingerprint());
        assert!(!pairing.confirmed);

        // Confirmed by the gateway; the same code in the configuration changes nothing after a restart.
        save(&store, &StoredPairing { confirmed: true, ..pairing.clone() }).await.unwrap();
        let again = prepare(&store, &first).await.unwrap().unwrap();
        assert!(again.confirmed);
        assert_eq!(again.identity.fingerprint(), pairing.identity.fingerprint());
        assert!(prepare(&store, &GatewayConfig::default()).await.unwrap().unwrap().confirmed);

        // After `uwumail-gateway unpair`: a new token for the same gateway pairs again, with the same key.
        let second = GatewayConfig { code: code(&gateway, &Token::generate()) };
        let repaired = prepare(&store, &second).await.unwrap().unwrap();
        assert!(!repaired.confirmed);
        assert_eq!(repaired.identity.fingerprint(), pairing.identity.fingerprint());
    }

    /// A gateway with web ports, and this server's services behind it.
    async fn web_behind_gateway(
        dir: &std::path::Path,
    ) -> (uwumail_gateway::Running, TunnelClient, Vec<u8>, watch::Sender<bool>) {
        use uwumail_gateway::config::{ListenConfig, OutboundConfig};

        // Dropping the sender would stop the gateway and the tunnel right away.
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
            },
            outbound: OutboundConfig { ports: vec![25], allow_private: true },
            ..uwumail_gateway::GatewayConfig::default()
        };
        let running = uwumail_gateway::start(config, never.clone()).await.unwrap();
        let state = uwumail_gateway::state::State::open(&dir.join("gateway")).unwrap();
        let token = loop {
            if let Some(token) = state.token().unwrap() {
                break token;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let gateway = state.identity().unwrap().unwrap().fingerprint();

        let store = Store::open(&dir.join("server")).await.unwrap();
        let smtp = Smtp::new(
            store,
            uwumail_smtp::SmtpSettings {
                hostname: "mail.example.com".into(),
                smtp: Default::default(),
                delivery: Default::default(),
                tone: Default::default(),
                server_tls: None,
            },
        )
        .unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into());
        let mut tls =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate.cert.der().clone()], key)
                .unwrap();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

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
            https_tls: Arc::new(tls),
            https: http::app(state.clone(), Router::new(), who, Arc::default()),
            http: http::redirect_app(state),
        };
        let settings = ClientSettings {
            addresses: vec![running.tunnel],
            gateway,
            identity: Identity::generate().unwrap(),
            hostname: "mail.example.com".into(),
            software: "test".into(),
            token: Some(token),
        };
        let client = TunnelClient::start(settings, Arc::new(services), never);
        let mut status = client.subscribe();
        tokio::time::timeout(Duration::from_secs(20), status.wait_for(|s| matches!(s, Status::Connected { .. })))
            .await
            .expect("the tunnel comes up")
            .unwrap();
        (running, client, certificate.cert.der().to_vec(), running_until)
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
        let socket = tokio::net::TcpStream::connect(gateway.listener(Service::Https).unwrap()).await.unwrap();
        let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
        let tls = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, socket).await.unwrap();
        let response = http_get(tls, "/who").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("127.0.0.1 https=true"), "{response}");
    }

    #[test]
    fn only_public_destinations_go_through_the_gateway() {
        assert!(through_gateway("8.8.8.8:25".parse().unwrap()));
        assert!(!through_gateway("192.168.1.20:25".parse().unwrap()));
        assert!(!through_gateway("[fd00::5]:25".parse().unwrap()));
    }
}
