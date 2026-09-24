//! SMTP for the UwUMail server.
//!
//! - [`serve`] accepts connections on the MX port (mail from other servers)
//!   and the submission ports (mail from our own people).
//! - [`run_queue`] delivers queued mail to other servers, retries, and bounces.
//! - [`dkim`] creates and uses the signing keys of hosted domains.

pub mod autoconfig;
mod checks;
pub mod clamav;
mod client;
pub mod config;
pub mod dkim;
mod dns;
pub mod dnscheck;
mod dsn;
pub mod egress;
mod fetch;
mod fetched;
mod forward;
mod headers;
pub mod health;
pub mod https;
mod inbound;
mod limiter;
pub mod mta_sts;
mod outbound;
pub mod palette;
pub mod pictures;
pub mod reachability;
mod relay;
mod reports;
mod rules;
mod sender_lists;
pub mod servercheck;
pub mod sieve;
mod spam;
mod srs;
mod stream;
mod submission;
mod texts;
mod tls;
mod vacation;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use mail_auth::MessageAuthenticator;
use tokio::sync::Semaphore;
use uwumail_store::Store;

pub use client::{Connector, connect_directly};
pub use config::{
    AntivirusConfig, BrandConfig, DeliveryConfig, ExternalTone, FeedsConfig, InternalTone, Language, RelayConfig,
    RelaySecurity, SmtpConfig, SpamConfig, SpamLogConfig, ToneConfig,
};
pub use dns::DnsCaches;
pub use fetch::is_public;
pub use fetched::Mailbox as FetchedMailbox;
/// The value of one header of a raw message, for callers that fetch mail and hand it in here.
pub use headers::first_value as header_value;
pub use inbound::{ListenerKind, Taken, deliver_fetched, serve, serve_stream};
pub use limiter::{AuthLimiter, Reporter};
pub use outbound::run_queue;
pub use relay::IpNetwork;
pub use spam::{FEEDS, Feed, feed, run_learning, run_list_updates};
pub use stream::{BoxIo, Io};
pub use submission::{Submission, SubmissionRecipient, SubmitError, Submitted};

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
    /// Settings that can change while the server runs, e.g. from the admin panel.
    live: RwLock<Arc<Live>>,
    pub server_tls: Option<Arc<rustls::ServerConfig>>,
    pub client_tls: tls::ClientTls,
    /// For MTA-STS policies of other domains.
    pub https: https::Https,
    pub authenticator: MessageAuthenticator,
    pub dns: DnsCaches,
    pub auth_limiter: limiter::AuthLimiter,
    /// Recent blocklist answers about sending servers.
    pub blocklist_cache: spam::BlocklistCache,
    /// Recent domain blocklist answers about link domains.
    pub domain_cache: spam::DomainCache,
    /// The key Bayes tokens are hashed with, loaded or made on first use.
    pub bayes_key: tokio::sync::OnceCell<[u8; 32]>,
    /// The word lists and built-in lists, compiled again when they change.
    pub(crate) lists: spam::CompiledLists,
    /// Fetches lists from the web.
    pub(crate) fetcher: fetch::Fetcher,
    /// Sized at start; changing these limits takes a restart.
    pub connections: Arc<Semaphore>,
    pub delivery_permits: Arc<Semaphore>,
    /// Unpacking and reading a report is the most work a stranger can ask for with one message, so
    /// only a few are read at a time and the rest are let go.
    pub reports: Arc<Semaphore>,
    pub inflight: Mutex<HashSet<i64>>,
    pub stats: health::DeliveryStats,
    /// Where connections to other servers start; `None` is this machine.
    connector: RwLock<Option<Arc<dyn Connector>>>,
    /// Name, colour and mascot the server shows; changed in place from the admin panel.
    brand: RwLock<Arc<BrandConfig>>,
}

/// The settings in effect right now. Take a snapshot per connection or delivery.
pub(crate) struct Live {
    pub smtp: SmtpConfig,
    pub spam: SpamConfig,
    pub delivery: DeliveryConfig,
    pub tone: ToneConfig,
    pub trusted_relays: Vec<relay::IpNetwork>,
}

impl Live {
    fn new(smtp: SmtpConfig, spam: SpamConfig, delivery: DeliveryConfig, tone: ToneConfig) -> Result<Live, SmtpError> {
        let trusted_relays = relay::parse_networks(&smtp.trusted_relays)
            .map_err(|err| SmtpError::Config(format!("smtp.trusted_relays: {err}")))?;
        Ok(Live { smtp, spam, delivery, tone, trusted_relays })
    }
}

impl Context {
    pub fn live(&self) -> Arc<Live> {
        self.live.read().expect("settings poisoned").clone()
    }

    pub fn connector(&self) -> Option<Arc<dyn Connector>> {
        self.connector.read().expect("connector poisoned").clone()
    }

    pub fn brand(&self) -> Arc<BrandConfig> {
        self.brand.read().expect("brand poisoned").clone()
    }

    /// The tone as set, made plain when the mascot is switched off.
    pub fn tone(&self) -> ToneConfig {
        let tone = self.live().tone;
        ToneConfig { internal: self.brand().internal_tone(tone.internal), ..tone }
    }

    /// The route of mail that does not go through a relay.
    pub fn direct_route(&self) -> health::Route {
        if self.connector().is_some() { health::Route::Gateway } else { health::Route::Direct }
    }
}

pub struct SmtpSettings {
    pub hostname: String,
    pub smtp: SmtpConfig,
    pub spam: SpamConfig,
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
        let SmtpSettings { hostname, smtp, spam, delivery, tone, server_tls } = settings;
        Ok(Smtp {
            inner: Arc::new(Context {
                store,
                hostname: hostname.to_ascii_lowercase(),
                connections: Arc::new(Semaphore::new(smtp.max_connections.max(1))),
                delivery_permits: Arc::new(Semaphore::new(delivery.concurrency.max(1))),
                reports: Arc::new(Semaphore::new(reports::AT_ONCE)),
                live: RwLock::new(Arc::new(Live::new(smtp, spam, delivery, tone)?)),
                server_tls,
                client_tls: tls::ClientTls::new()?,
                https: https::Https::new(),
                authenticator,
                dns: DnsCaches::default(),
                auth_limiter: limiter::AuthLimiter::default(),
                blocklist_cache: spam::BlocklistCache::default(),
                domain_cache: spam::DomainCache::default(),
                bayes_key: tokio::sync::OnceCell::new(),
                lists: Default::default(),
                fetcher: fetch::Fetcher::new(),
                inflight: Mutex::new(HashSet::new()),
                stats: health::DeliveryStats::default(),
                connector: RwLock::new(None),
                brand: RwLock::new(Arc::new(BrandConfig::default())),
            }),
        })
    }

    /// Lets `connector` make every connection to other servers from now on, e.g. through the
    /// UwUMail Gateway. `None` connects from this machine again.
    pub fn set_connector(&self, connector: Option<Arc<dyn Connector>>) {
        *self.inner.connector.write().expect("connector poisoned") = connector;
    }

    /// Whether connections to other servers go through a [`Connector`].
    pub fn has_connector(&self) -> bool {
        self.inner.connector().is_some()
    }

    /// Hands every network this turns away to `reporter` as well, so it can be kept out further
    /// away than this server — at the UwUMail Gateway, where it never reaches the house at all.
    pub fn report_blocks_to(&self, reporter: Option<Reporter>) {
        self.inner.auth_limiter.report_to(reporter);
    }

    /// Switches to new settings at once: new connections and deliveries use them, running ones finish
    /// with the old. Connection and delivery limits keep their size until a restart.
    pub fn update_settings(
        &self,
        smtp: SmtpConfig,
        spam: SpamConfig,
        delivery: DeliveryConfig,
        tone: ToneConfig,
    ) -> Result<(), SmtpError> {
        let live = Live::new(smtp, spam, delivery, tone)?;
        *self.inner.live.write().expect("settings poisoned") = Arc::new(live);
        Ok(())
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// The virus scanner's settings as they are right now.
    pub fn antivirus(&self) -> AntivirusConfig {
        self.inner.live().spam.antivirus.clone()
    }

    /// What the virus scanner says about itself; `None` while it is switched off.
    pub async fn virus_status(&self) -> Option<Result<clamav::Status, String>> {
        let config = self.antivirus();
        if !config.enabled {
            return None;
        }
        Some(clamav::Clamav::new(&config).status().await)
    }

    /// Hands the scanner the harmless test file every scanner knows, so an admin can see that the
    /// two of them really talk to each other.
    pub async fn virus_selftest(&self) -> Result<clamav::Scan, String> {
        clamav::Clamav::new(&self.antivirus()).scan(clamav::test_file().as_bytes()).await
    }

    /// Fetches a built-in list now. Returns how many entries it holds, or why it failed.
    pub async fn refresh_feed(&self, feed: &Feed) -> Result<usize, String> {
        spam::refresh_feed(&self.inner, feed).await
    }

    /// Fetches a subscribed word list now. Returns how many entries it holds, or why it failed.
    pub async fn refresh_word_source(&self, source: &uwumail_store::WordSource) -> Result<usize, String> {
        spam::refresh_word_source(&self.inner, source).await
    }

    /// Why a link cannot be subscribed to, if it cannot.
    pub fn check_list_link(url: &str) -> Result<(), String> {
        fetch::check_url(url, false).map(|_| ())
    }

    /// The spam filter settings in effect right now.
    pub fn spam_settings(&self) -> SpamConfig {
        self.inner.live().spam.clone()
    }

    pub fn hostname(&self) -> &str {
        &self.inner.hostname
    }

    /// DNS answers used for SPF, DKIM, DMARC and MX lookups. Tests pre-fill it.
    pub fn dns_cache(&self) -> &DnsCaches {
        &self.inner.dns
    }

    /// Language and tone of mail the server writes itself, as currently set. Without the mascot the
    /// internal tone is always neutral.
    pub fn tone(&self) -> ToneConfig {
        self.inner.tone()
    }

    /// The name, colour and mascot as currently set.
    pub fn brand(&self) -> Arc<BrandConfig> {
        self.inner.brand()
    }

    /// Takes a new brand into use at once.
    pub fn set_brand(&self, brand: BrandConfig) {
        *self.inner.brand.write().expect("brand poisoned") = Arc::new(brand);
    }

    /// Whether people may forward mail to addresses on other servers.
    pub fn allow_external_forwarding(&self) -> bool {
        self.inner.live().smtp.allow_external_forwarding
    }

    /// The relay outgoing mail leaves through, if one is configured.
    pub fn relay_host(&self) -> Option<String> {
        self.inner.live().delivery.relay.as_ref().map(|relay| relay.host.clone())
    }

    /// Whether another mail server receives mail first and hands it to us.
    pub fn behind_upstream_server(&self) -> bool {
        !self.inner.live().trusted_relays.is_empty()
    }

    /// What the spam history is set to keep, for the job that clears it out and for the portal.
    pub fn spam_log_settings(&self) -> config::SpamLogConfig {
        self.inner.live().spam.log.clone()
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
