//! SMTP for the UwUMail server.
//!
//! - [`serve`] accepts connections on the MX port (mail from other servers)
//!   and the submission ports (mail from our own people).
//! - [`run_queue`] delivers queued mail to other servers, retries, and bounces.
//! - [`dkim`] creates and uses the signing keys of hosted domains.

mod checks;
mod client;
pub mod config;
pub mod dkim;
mod dns;
mod dsn;
mod headers;
mod inbound;
mod limiter;
mod outbound;
mod relay;
mod stream;
mod texts;
mod tls;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use mail_auth::MessageAuthenticator;
use tokio::sync::Semaphore;
use uwumail_store::Store;

pub use config::{
    DeliveryConfig, ExternalTone, InternalTone, Language, RelayConfig, RelaySecurity, SmtpConfig, ToneConfig,
};
pub use dns::DnsCaches;
pub use inbound::{ListenerKind, serve};
pub use outbound::run_queue;

#[derive(Debug, thiserror::Error)]
pub enum SmtpError {
    #[error("storage: {0}")]
    Store(#[from] uwumail_store::StoreError),
    #[error("TLS: {0}")]
    Tls(#[from] rustls::Error),
    #[error("DKIM: {0}")]
    Dkim(String),
    #[error("DNS resolver: {0}")]
    Dns(String),
    #[error("configuration: {0}")]
    Config(String),
}

/// Everything the SMTP services share. Cheap to clone.
#[derive(Clone)]
pub struct Smtp {
    inner: Arc<Context>,
}

pub(crate) struct Context {
    pub store: Store,
    pub hostname: String,
    pub smtp: SmtpConfig,
    pub delivery: DeliveryConfig,
    pub tone: ToneConfig,
    pub server_tls: Option<Arc<rustls::ServerConfig>>,
    pub client_tls: tls::ClientTls,
    pub authenticator: MessageAuthenticator,
    pub dns: DnsCaches,
    pub auth_limiter: limiter::AuthLimiter,
    pub trusted_relays: Vec<relay::IpNetwork>,
    pub connections: Arc<Semaphore>,
    pub delivery_permits: Arc<Semaphore>,
    pub inflight: Mutex<HashSet<i64>>,
}

pub struct SmtpSettings {
    pub hostname: String,
    pub smtp: SmtpConfig,
    pub delivery: DeliveryConfig,
    pub tone: ToneConfig,
    /// Certificates for STARTTLS and implicit TLS. Without it, no TLS is offered.
    pub server_tls: Option<Arc<rustls::ServerConfig>>,
}

impl Smtp {
    pub fn new(store: Store, settings: SmtpSettings) -> Result<Smtp, SmtpError> {
        let authenticator = MessageAuthenticator::new_system_conf()
            .or_else(|err| {
                tracing::warn!(%err, "no usable system DNS configuration, falling back to Quad9");
                MessageAuthenticator::new_quad9_tls()
            })
            .map_err(|err| SmtpError::Dns(err.to_string()))?;
        let SmtpSettings { hostname, smtp, delivery, tone, server_tls } = settings;
        let trusted_relays = relay::parse_networks(&smtp.trusted_relays)
            .map_err(|err| SmtpError::Config(format!("smtp.trusted_relays: {err}")))?;
        Ok(Smtp {
            inner: Arc::new(Context {
                store,
                hostname: hostname.to_ascii_lowercase(),
                connections: Arc::new(Semaphore::new(smtp.max_connections.max(1))),
                delivery_permits: Arc::new(Semaphore::new(delivery.concurrency.max(1))),
                smtp,
                delivery,
                tone,
                server_tls,
                client_tls: tls::ClientTls::new()?,
                authenticator,
                dns: DnsCaches::default(),
                auth_limiter: limiter::AuthLimiter::default(),
                trusted_relays,
                inflight: Mutex::new(HashSet::new()),
            }),
        })
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn hostname(&self) -> &str {
        &self.inner.hostname
    }

    /// DNS answers used for SPF, DKIM, DMARC and MX lookups. Tests pre-fill it.
    pub fn dns_cache(&self) -> &DnsCaches {
        &self.inner.dns
    }
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

pub(crate) fn random_id() -> String {
    use aws_lc_rs::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    aws_lc_rs::rand::SystemRandom::new().fill(&mut bytes).expect("the system RNG failed");
    hex::encode(bytes)
}
