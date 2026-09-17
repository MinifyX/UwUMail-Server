use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Receiving and submission.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpConfig {
    /// Largest accepted message in bytes.
    pub max_message_size: usize,
    pub max_recipients: usize,
    /// Only offer AUTH after STARTTLS (or on the implicit TLS port).
    pub require_tls_for_auth: bool,
    /// Idle time before a connection is closed.
    pub timeout_secs: u64,
    pub max_connections: usize,
    /// Check SPF, DKIM and DMARC for mail from other servers.
    pub verify_senders: bool,
    /// Reject mail that fails DMARC for domains with `p=reject` (otherwise it goes to Junk).
    pub enforce_dmarc_reject: bool,
    /// Put the client's IP address into the Received header of submitted mail.
    pub reveal_client_ip: bool,
    /// IP addresses or networks (CIDR) of mail servers that receive mail for us and forward it,
    /// like an existing mail server in front. Sender checks use the address those servers saw.
    pub trusted_relays: Vec<String>,
    /// People may forward their mail to addresses on other servers (after the owner confirmed).
    pub allow_external_forwarding: bool,
}

impl Default for SmtpConfig {
    fn default() -> Self {
        SmtpConfig {
            max_message_size: 50 * 1024 * 1024,
            max_recipients: 100,
            require_tls_for_auth: true,
            timeout_secs: 300,
            max_connections: 500,
            verify_senders: true,
            enforce_dmarc_reject: true,
            reveal_client_ip: false,
            trusted_relays: Vec::new(),
            allow_external_forwarding: true,
        }
    }
}

/// The spam filter for mail from other servers.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpamConfig {
    /// Score incoming mail at all. Off means only the DMARC verdict decides.
    pub enabled: bool,
    /// Ask DNS blocklists about the sending server.
    pub blocklists: bool,
    /// Learn from Spam / Not spam and clear cases, and let the Bayes filter score mail once it learned
    /// enough.
    pub bayes: bool,
    /// From this score on, a message goes into Junk instead of the inbox.
    pub junk_score: f32,
    /// Senders scoring at least this much, but below `junk_score`, are asked to come back later.
    /// Mail that is filed as junk anyway is not delayed, and neither is anything that looks fine,
    /// so confirmation codes from well-behaved servers arrive at once.
    pub greylist_score: f32,
    /// How long a greylisted sender has to wait before a retry is let through.
    pub greylist_delay_secs: u64,
    /// From this score on, mail is refused in the SMTP dialogue. Off unless set: a young filter
    /// is wrong now and then, and Junk loses nothing while a refusal does.
    pub reject_score: Option<f32>,
    /// Built-in lists the server fetches itself.
    pub feeds: FeedsConfig,
}

impl Default for SpamConfig {
    fn default() -> Self {
        SpamConfig {
            enabled: true,
            blocklists: true,
            bayes: true,
            junk_score: 5.0,
            greylist_score: 2.0,
            greylist_delay_secs: 300,
            reject_score: None,
            feeds: FeedsConfig::default(),
        }
    }
}

/// Built-in lists the server fetches itself; see docs/spam-filter.md.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeedsConfig {
    /// Links to malware that is online right now (abuse.ch URLhaus). Needs `abuse_ch_key`.
    pub urlhaus: bool,
    /// Files seen as malware in the last two days (abuse.ch MalwareBazaar). Needs `abuse_ch_key`.
    pub malware_bazaar: bool,
    /// Subjects of spam waves, as regular expressions (mailcow).
    pub bad_subjects: bool,
    /// Domains of throwaway addresses (Rspamd).
    pub disposable: bool,
    /// Freemail providers, for replies that are meant to go somewhere else (Rspamd).
    pub freemail: bool,
    /// Link shorteners and redirectors (Rspamd).
    pub redirectors: bool,
    /// One's own Auth-Key from auth.abuse.ch; free for non-commercial use only.
    pub abuse_ch_key: Option<String>,
}

impl Default for FeedsConfig {
    fn default() -> Self {
        FeedsConfig {
            urlhaus: true,
            malware_bazaar: true,
            bad_subjects: true,
            disposable: true,
            freemail: true,
            redirectors: true,
            abuse_ch_key: None,
        }
    }
}

/// Delivery to other servers.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeliveryConfig {
    /// Parallel outgoing deliveries.
    pub concurrency: usize,
    /// Give up and bounce after this many hours.
    pub max_lifetime_hours: u64,
    pub connect_timeout_secs: u64,
    pub command_timeout_secs: u64,
    /// Port used when connecting to MX hosts. Only change this for testing.
    pub mx_port: u16,
    /// Refuse to deliver without TLS.
    pub require_tls: bool,
    /// Send everything through this server instead of looking up MX records.
    pub relay: Option<RelayConfig>,
    /// Fixed `host:port` per recipient domain, checked before the relay and DNS.
    pub routes: HashMap<String, String>,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        DeliveryConfig {
            concurrency: 16,
            max_lifetime_hours: 120,
            connect_timeout_secs: 30,
            command_timeout_secs: 300,
            mx_port: 25,
            require_tls: false,
            relay: None,
            routes: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub host: String,
    #[serde(default = "RelayConfig::default_port")]
    pub port: u16,
    #[serde(default)]
    pub security: RelaySecurity,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl RelayConfig {
    fn default_port() -> u16 {
        587
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RelaySecurity {
    /// STARTTLS with a valid certificate.
    #[default]
    Starttls,
    /// Implicit TLS (usually port 465).
    Tls,
    /// No encryption. Only for relays in a trusted network.
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    De,
    En,
}

/// How mail to the server's own people sounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InternalTone {
    #[default]
    Playful,
    Neutral,
}

/// How mail to everyone else sounds (bounces, later vacation replies).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalTone {
    #[default]
    Neutral,
    /// Friendly and a little playful, never over the top.
    Light,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToneConfig {
    pub language: Language,
    pub internal: InternalTone,
    pub external: ExternalTone,
}
