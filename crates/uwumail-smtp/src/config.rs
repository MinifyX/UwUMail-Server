use std::collections::HashMap;

use serde::Deserialize;

/// Receiving and submission.
#[derive(Debug, Clone, Deserialize)]
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
        }
    }
}

/// Delivery to other servers.
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    De,
    En,
}

/// How mail to the server's own people sounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InternalTone {
    #[default]
    Playful,
    Neutral,
}

/// How mail to everyone else sounds (bounces, later vacation replies).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalTone {
    #[default]
    Neutral,
    /// Friendly and a little playful, never over the top.
    Light,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToneConfig {
    pub language: Language,
    pub internal: InternalTone,
    pub external: ExternalTone,
}
