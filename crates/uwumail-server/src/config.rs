//! Configuration from a TOML file, overridable with `UWUMAIL_*` environment variables.
//!
//! Nested keys use a double underscore: `UWUMAIL_TLS__MODE=self-signed`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;
use uwumail_smtp::{DeliveryConfig, SmtpConfig, ToneConfig};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Public name of this server, e.g. `mail.example.com`. Used in SMTP greetings,
    /// Received headers, MX records and the TLS certificate.
    pub hostname: String,
    pub data_dir: PathBuf,
    pub listen: ListenConfig,
    pub tls: TlsConfig,
    pub smtp: SmtpConfig,
    pub delivery: DeliveryConfig,
    pub tone: ToneConfig,
    pub log: LogConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: String::new(),
            data_dir: PathBuf::from("/data"),
            listen: ListenConfig::default(),
            tls: TlsConfig::default(),
            smtp: SmtpConfig::default(),
            delivery: DeliveryConfig::default(),
            tone: ToneConfig::default(),
            log: LogConfig::default(),
        }
    }
}

/// Addresses to listen on. An empty string turns a listener off.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListenConfig {
    pub smtp: String,
    pub submission: String,
    pub submissions: String,
    pub http: String,
    pub https: String,
    /// Plain HTTP for running behind a reverse proxy that terminates TLS.
    pub proxy: String,
}

impl Default for ListenConfig {
    fn default() -> Self {
        ListenConfig {
            smtp: "[::]:25".into(),
            submission: "[::]:587".into(),
            submissions: "[::]:465".into(),
            http: "[::]:80".into(),
            https: "[::]:443".into(),
            proxy: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// Let's Encrypt (or another ACME CA) through the HTTP challenge on port 80.
    #[default]
    Acme,
    /// Certificate and key files managed elsewhere, e.g. by a reverse proxy. Reloaded on change.
    Files,
    /// A generated certificate. Only for testing: other servers and apps will complain.
    SelfSigned,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsConfig {
    pub mode: TlsMode,
    /// Contact for expiry notices from the certificate authority.
    pub acme_email: String,
    pub acme_directory: String,
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}

impl Default for TlsConfig {
    fn default() -> Self {
        TlsConfig {
            mode: TlsMode::Acme,
            acme_email: String::new(),
            acme_directory: "https://acme-v02.api.letsencrypt.org/directory".into(),
            cert_file: PathBuf::new(),
            key_file: PathBuf::new(),
        }
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

impl Config {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let mut figment = Figment::new();
        if let Some(path) = path {
            if !path.exists() {
                bail!("the config file {} does not exist", path.display());
            }
            figment = figment.merge(Toml::file(path));
        }
        figment = figment.merge(Env::prefixed("UWUMAIL_").split("__").ignore(&["config"]));
        let mut config: Config = figment.extract().context("the configuration is invalid")?;
        config.hostname = config.hostname.trim().trim_end_matches('.').to_ascii_lowercase();
        Ok(config)
    }

    /// Checks what cannot be expressed in types.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.hostname.is_empty() {
            bail!("set `hostname` (or UWUMAIL_HOSTNAME) to the public name of this server, e.g. mail.example.com");
        }
        uwumail_store::normalize_domain(&self.hostname)
            .map_err(|_| anyhow::anyhow!("`hostname` '{}' is not a valid host name", self.hostname))?;
        if self.tls.mode == TlsMode::Files
            && (self.tls.cert_file.as_os_str().is_empty() || self.tls.key_file.as_os_str().is_empty())
        {
            bail!("TLS mode `files` needs `tls.cert_file` and `tls.key_file`");
        }
        // Behind a reverse proxy the challenge arrives through the proxy listener instead of port 80.
        if self.tls.mode == TlsMode::Acme && self.listen.http.is_empty() && self.listen.proxy.is_empty() {
            bail!(
                "TLS mode `acme` needs the HTTP listener on port 80 (or a proxy listener) for the certificate challenge"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_and_environment_merge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uwumail.toml");
        std::fs::write(
            &path,
            "hostname = \"Mail.Example.DE.\"\n[tls]\nmode = \"self-signed\"\n[delivery.routes]\n\"b.test\" = \"127.0.0.1:2525\"\n",
        )
        .unwrap();
        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(config.hostname, "mail.example.de");
        assert_eq!(config.tls.mode, TlsMode::SelfSigned);
        assert_eq!(config.delivery.routes["b.test"], "127.0.0.1:2525");
        assert_eq!(config.listen.smtp, "[::]:25");
        config.validate().unwrap();
    }

    #[test]
    fn missing_hostname_is_explained() {
        let error = Config::default().validate().unwrap_err().to_string();
        assert!(error.contains("hostname"));
    }
}
