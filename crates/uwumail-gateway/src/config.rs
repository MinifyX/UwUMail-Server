//! Configuration from an optional TOML file, overridable with `UWUMAIL_GATEWAY_*` environment
//! variables. Nested keys use a double underscore: `UWUMAIL_GATEWAY_LISTEN__HTTP=`.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;
use uwumail_tunnel::{Service, net};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    /// UDP address the UwUMail server connects to for the tunnel.
    pub tunnel: String,
    /// Where the gateway keeps its key and the pairing.
    pub state_dir: PathBuf,
    /// The gateway's public addresses, for the pairing code and the DNS records. Empty: the
    /// addresses this machine sends from.
    pub public_addresses: Vec<IpAddr>,
    pub listen: ListenConfig,
    pub outbound: OutboundConfig,
    pub limits: LimitsConfig,
    pub log: LogConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            tunnel: "[::]:443".into(),
            state_dir: PathBuf::from("/var/lib/uwumail-gateway"),
            public_addresses: Vec::new(),
            listen: ListenConfig::default(),
            outbound: OutboundConfig::default(),
            limits: LimitsConfig::default(),
            log: LogConfig::default(),
        }
    }
}

/// Public TCP ports and the service behind each. An empty string turns one off.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListenConfig {
    pub smtp: String,
    pub submission: String,
    pub submissions: String,
    pub http: String,
    pub https: String,
    pub imaps: String,
}

impl Default for ListenConfig {
    fn default() -> Self {
        ListenConfig {
            smtp: "[::]:25".into(),
            submission: "[::]:587".into(),
            submissions: "[::]:465".into(),
            http: "[::]:80".into(),
            https: "[::]:443".into(),
            imaps: "[::]:993".into(),
        }
    }
}

impl ListenConfig {
    pub fn addresses(&self) -> Vec<(Service, &str)> {
        [
            (Service::Smtp, &self.smtp),
            (Service::Submission, &self.submission),
            (Service::Submissions, &self.submissions),
            (Service::Http, &self.http),
            (Service::Https, &self.https),
            (Service::Imaps, &self.imaps),
        ]
        .into_iter()
        .filter(|(_, address)| !address.is_empty())
        .map(|(service, address)| (service, address.as_str()))
        .collect()
    }
}

/// Connections the gateway makes for the server: only to mail ports of public addresses, so it
/// can never be used to reach anything else.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutboundConfig {
    pub ports: Vec<u16>,
    /// Also connect to private and local addresses. Only for tests.
    pub allow_private: bool,
}

impl Default for OutboundConfig {
    fn default() -> Self {
        OutboundConfig { ports: vec![25, 465, 587], allow_private: false }
    }
}

impl OutboundConfig {
    pub fn allows(&self, address: SocketAddr) -> Result<(), String> {
        if !self.ports.contains(&address.port()) {
            return Err(format!("port {} is not one of the mail ports", address.port()));
        }
        if !self.allow_private && !net::is_global(address.ip()) {
            return Err(format!("{} is not a public address", address.ip()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    /// Connections carried at once.
    pub max_connections: usize,
    /// Connections at once from one IPv4 address or IPv6 /64.
    pub max_connections_per_ip: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig { max_connections: 1000, max_connections_per_ip: 50 }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    pub format: LogFormat,
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig { format: LogFormat::Text, level: "info".into() }
    }
}

/// Where `install.sh` puts the configuration. The commands find it there by themselves, so that
/// `uwumail-gateway code` shows the same addresses and port the running service uses.
pub const INSTALLED_CONFIG: &str = "/etc/uwumail-gateway/gateway.toml";

/// The configuration file to read: the one that was named, else the installed one if it exists.
pub fn config_file(named: Option<PathBuf>, installed: &Path) -> Option<PathBuf> {
    named.or_else(|| installed.exists().then(|| installed.to_path_buf()))
}

impl GatewayConfig {
    pub fn load(path: Option<&Path>) -> anyhow::Result<GatewayConfig> {
        let mut figment = Figment::new();
        if let Some(path) = path {
            if !path.exists() {
                bail!("the config file {} does not exist", path.display());
            }
            figment = figment.merge(Toml::file(path));
        }
        figment = figment.merge(Env::prefixed("UWUMAIL_GATEWAY_").split("__").ignore(&["config"]));
        figment.extract().context("the configuration is invalid")
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        self.tunnel.parse::<SocketAddr>().with_context(|| format!("`tunnel` '{}' is not an address", self.tunnel))?;
        for (service, address) in self.listen.addresses() {
            address
                .parse::<SocketAddr>()
                .with_context(|| format!("`listen.{}` '{address}' is not an address", service.as_str()))?;
        }
        if self.outbound.ports.is_empty() {
            bail!("`outbound.ports` is empty, so the server could not send any mail");
        }
        if self.limits.max_connections == 0 || self.limits.max_connections_per_ip == 0 {
            bail!("the connection limits must be at least 1");
        }
        Ok(())
    }

    /// The addresses to put into the pairing code and the server's DNS records.
    pub fn public_addresses(&self) -> Vec<IpAddr> {
        if self.public_addresses.is_empty() { net::public_addresses() } else { self.public_addresses.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let config = GatewayConfig::default();
        config.validate().unwrap();
        assert_eq!(config.listen.addresses().len(), 6);
    }

    #[test]
    fn file_turns_listeners_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.toml");
        std::fs::write(&path, "tunnel = \"0.0.0.0:4433\"\n[listen]\nhttp = \"\"\n[outbound]\nports = [25]\n").unwrap();
        let config = GatewayConfig::load(Some(&path)).unwrap();
        config.validate().unwrap();
        assert_eq!(config.tunnel, "0.0.0.0:4433");
        assert!(!config.listen.addresses().iter().any(|(service, _)| *service == Service::Http));
        assert_eq!(config.outbound.ports, [25]);
    }

    #[test]
    fn the_installed_file_is_found_unless_another_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("gateway.toml");
        assert_eq!(config_file(None, &installed), None, "no file, so only defaults and the environment");

        std::fs::write(&installed, "public_addresses = [\"192.0.2.10\"]\n").unwrap();
        assert_eq!(config_file(None, &installed), Some(installed.clone()));
        let config = GatewayConfig::load(config_file(None, &installed).as_deref()).unwrap();
        assert_eq!(config.public_addresses(), ["192.0.2.10".parse::<IpAddr>().unwrap()]);

        let named = dir.path().join("other.toml");
        assert_eq!(config_file(Some(named.clone()), &installed), Some(named));
    }

    #[test]
    fn outbound_only_to_public_mail_ports() {
        let outbound = OutboundConfig::default();
        assert!(outbound.allows("8.8.8.8:25".parse().unwrap()).is_ok());
        assert!(outbound.allows("8.8.8.8:22".parse().unwrap()).is_err());
        assert!(outbound.allows("10.0.0.5:25".parse().unwrap()).is_err());
        assert!(outbound.allows("169.254.169.254:587".parse().unwrap()).is_err());
        assert!(outbound.allows("[::1]:25".parse().unwrap()).is_err());
    }
}
