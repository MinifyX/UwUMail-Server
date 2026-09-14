//! Certificates for HTTPS and the mail ports, swappable at runtime.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, anyhow};
use rustls::ServerConfig;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::{Config, TlsMode};

#[derive(Debug, Clone)]
pub struct CertificateInfo {
    pub not_after: i64,
    pub names: Vec<String>,
    pub self_signed: bool,
}

/// The certificate currently served. ACME renewals and file reloads replace it in place.
#[derive(Debug, Default)]
pub struct CertStore {
    current: RwLock<Option<(Arc<CertifiedKey>, CertificateInfo)>>,
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.read().expect("cert store poisoned").as_ref().map(|(key, _)| key.clone())
    }
}

impl CertStore {
    pub fn info(&self) -> Option<CertificateInfo> {
        self.current.read().expect("cert store poisoned").as_ref().map(|(_, info)| info.clone())
    }

    pub fn set_pem(&self, cert_pem: &[u8], key_pem: &[u8]) -> anyhow::Result<CertificateInfo> {
        let certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(cert_pem).collect::<Result<_, _>>().context("reading the certificate")?;
        let leaf = certs.first().ok_or_else(|| anyhow!("the certificate file contains no certificate"))?;
        let info = inspect(leaf)?;
        let key = PrivateKeyDer::from_pem_slice(key_pem).context("reading the private key")?;
        let signing_key =
            rustls::crypto::aws_lc_rs::sign::any_supported_type(&key).context("loading the private key")?;
        let certified = Arc::new(CertifiedKey::new(certs, signing_key));
        *self.current.write().expect("cert store poisoned") = Some((certified, info.clone()));
        Ok(info)
    }
}

fn inspect(cert: &CertificateDer<'_>) -> anyhow::Result<CertificateInfo> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref()).context("parsing the certificate")?;
    let names = parsed
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|san| {
            san.value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    x509_parser::extensions::GeneralName::DNSName(dns) => Some(dns.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(CertificateInfo {
        not_after: parsed.validity().not_after.timestamp(),
        names,
        self_signed: parsed.issuer() == parsed.subject(),
    })
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// TLS settings for SMTP (STARTTLS and port 465).
pub fn mail_server_config(certs: Arc<CertStore>) -> anyhow::Result<Arc<ServerConfig>> {
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(certs);
    Ok(Arc::new(config))
}

/// TLS settings for HTTPS with HTTP/2.
pub fn https_server_config(certs: Arc<CertStore>) -> anyhow::Result<Arc<ServerConfig>> {
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(certs);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

pub fn tls_dir(config: &Config) -> PathBuf {
    config.data_dir.join("tls")
}

/// Loads whatever certificate is available at startup. For ACME this may be a temporary
/// self-signed one until the first real certificate arrives.
pub async fn load_initial(config: &Config, certs: &CertStore) -> anyhow::Result<()> {
    match config.tls.mode {
        TlsMode::Files => {
            let cert = tokio::fs::read(&config.tls.cert_file)
                .await
                .with_context(|| format!("reading {}", config.tls.cert_file.display()))?;
            let key = tokio::fs::read(&config.tls.key_file)
                .await
                .with_context(|| format!("reading {}", config.tls.key_file.display()))?;
            certs.set_pem(&cert, &key)?;
        }
        TlsMode::Acme => {
            let dir = tls_dir(config).join("acme");
            match (tokio::fs::read(dir.join("cert.pem")).await, tokio::fs::read(dir.join("key.pem")).await) {
                (Ok(cert), Ok(key)) => {
                    certs.set_pem(&cert, &key)?;
                }
                _ => load_self_signed(config, certs).await?,
            }
        }
        TlsMode::SelfSigned => load_self_signed(config, certs).await?,
    }
    Ok(())
}

async fn load_self_signed(config: &Config, certs: &CertStore) -> anyhow::Result<()> {
    let dir = tls_dir(config);
    let (cert_path, key_path) = (dir.join("self-signed.crt"), dir.join("self-signed.key"));
    if let (Ok(cert), Ok(key)) = (tokio::fs::read(&cert_path).await, tokio::fs::read(&key_path).await)
        && let Ok(info) = certs.set_pem(&cert, &key)
        && info.names.contains(&config.hostname)
    {
        return Ok(());
    }
    let generated = rcgen::generate_simple_self_signed(vec![config.hostname.clone(), "localhost".into()])?;
    let (cert, key) = (generated.cert.pem(), generated.signing_key.serialize_pem());
    tokio::fs::create_dir_all(&dir).await?;
    write_private(&key_path, key.as_bytes()).await?;
    tokio::fs::write(&cert_path, cert.as_bytes()).await?;
    certs.set_pem(cert.as_bytes(), key.as_bytes())?;
    tracing::info!(hostname = %config.hostname, "generated a self-signed certificate");
    Ok(())
}

/// Writes a file only the server user can read.
pub async fn write_private(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, contents).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await?;
    }
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Reloads certificate files when they change (`files` mode).
pub async fn watch_files(config: Config, certs: Arc<CertStore>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut last: (Option<SystemTime>, Option<SystemTime>) =
        (modified(&config.tls.cert_file), modified(&config.tls.key_file));
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = shutdown.changed() => return,
        }
        let now = (modified(&config.tls.cert_file), modified(&config.tls.key_file));
        if now == last {
            continue;
        }
        last = now;
        if let Err(err) = load_initial(&config, &certs).await {
            tracing::warn!(%err, "reloading the certificate files failed, keeping the old certificate");
        } else {
            tracing::info!("reloaded the certificate files");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn self_signed_certificates_are_created_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let config =
            Config { hostname: "mail.example.de".into(), data_dir: dir.path().to_path_buf(), ..Config::default() };
        let certs = CertStore::default();
        load_self_signed(&config, &certs).await.unwrap();
        let first = certs.info().unwrap();
        assert!(first.names.contains(&"mail.example.de".to_string()));
        assert!(first.self_signed);

        let reloaded = CertStore::default();
        load_self_signed(&config, &reloaded).await.unwrap();
        assert_eq!(reloaded.info().unwrap().not_after, first.not_after);
    }
}
