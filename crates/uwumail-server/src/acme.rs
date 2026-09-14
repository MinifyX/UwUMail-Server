//! Let's Encrypt certificates through the ACME HTTP-01 challenge.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use tokio::sync::watch;

use crate::config::Config;
use crate::tls::{CertStore, tls_dir, write_private};

/// Renew when the certificate expires in less than this.
const RENEW_BEFORE_SECS: i64 = 30 * 24 * 3600;

/// Tokens the HTTP listener on port 80 hands out during a challenge.
#[derive(Default)]
pub struct Challenges {
    tokens: RwLock<HashMap<String, String>>,
}

impl Challenges {
    pub fn answer(&self, token: &str) -> Option<String> {
        self.tokens.read().expect("challenges poisoned").get(token).cloned()
    }

    fn insert(&self, token: String, answer: String) {
        self.tokens.write().expect("challenges poisoned").insert(token, answer);
    }

    fn clear(&self) {
        self.tokens.write().expect("challenges poisoned").clear();
    }
}

/// Keeps the certificate fresh. Checks twice a day, retries failures hourly.
pub async fn run(
    config: Config,
    certs: Arc<CertStore>,
    challenges: Arc<Challenges>,
    mut shutdown: watch::Receiver<bool>,
) {
    // Give the HTTP listener a moment to come up before the CA calls back.
    let mut wait = Duration::from_secs(3);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => return,
        }
        let needs_certificate = match certs.info() {
            Some(info) => {
                info.self_signed
                    || !info.names.contains(&config.hostname)
                    || info.not_after - uwumail_now() < RENEW_BEFORE_SECS
            }
            None => true,
        };
        if !needs_certificate {
            wait = Duration::from_secs(12 * 3600);
            continue;
        }
        tracing::info!(hostname = %config.hostname, "requesting a certificate from Let's Encrypt");
        match order(&config, &certs, &challenges).await {
            Ok(()) => {
                tracing::info!(hostname = %config.hostname, "got a fresh certificate (=^･ω･^=)");
                wait = Duration::from_secs(12 * 3600);
            }
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "getting a certificate failed, retrying in an hour");
                wait = Duration::from_secs(3600);
            }
        }
        challenges.clear();
    }
}

fn uwumail_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

async fn account(config: &Config) -> anyhow::Result<Account> {
    let path = tls_dir(config).join("acme").join("account.json");
    if let Ok(saved) = tokio::fs::read(&path).await {
        let credentials: AccountCredentials = serde_json::from_slice(&saved).context("reading the ACME account")?;
        return Account::builder()?.from_credentials(credentials).await.context("loading the ACME account");
    }
    let contact = (!config.tls.acme_email.is_empty()).then(|| format!("mailto:{}", config.tls.acme_email));
    let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
    let (account, credentials) = Account::builder()?
        .create(
            &NewAccount { contact: &contacts, terms_of_service_agreed: true, only_return_existing: false },
            config.tls.acme_directory.clone(),
            None,
        )
        .await
        .context("creating the ACME account")?;
    tokio::fs::create_dir_all(path.parent().expect("has a parent")).await?;
    write_private(&path, &serde_json::to_vec_pretty(&credentials)?).await?;
    Ok(account)
}

async fn order(config: &Config, certs: &CertStore, challenges: &Challenges) -> anyhow::Result<()> {
    let account = account(config).await?;
    let identifiers = [Identifier::Dns(config.hostname.clone())];
    let mut order = account.new_order(&NewOrder::new(&identifiers)).await.context("creating the order")?;

    let mut authorizations = order.authorizations();
    while let Some(authorization) = authorizations.next().await {
        let mut authorization = authorization?;
        match authorization.status {
            AuthorizationStatus::Valid => continue,
            AuthorizationStatus::Pending => {}
            other => bail!("the authorization is {other:?}"),
        }
        let mut challenge = authorization
            .challenge(ChallengeType::Http01)
            .ok_or_else(|| anyhow!("the CA offers no HTTP-01 challenge"))?;
        challenges.insert(challenge.token.clone(), challenge.key_authorization().as_str().to_owned());
        challenge.set_ready().await?;
    }

    let retries = RetryPolicy::default().timeout(Duration::from_secs(120));
    let status = order.poll_ready(&retries).await.context("waiting for the challenge")?;
    if status != OrderStatus::Ready {
        bail!(
            "the certificate authority could not reach http://{}/.well-known/acme-challenge/ (order is {status:?}). \
             Is port 80 open and does the DNS record point here?",
            config.hostname
        );
    }
    let key_pem = order.finalize().await?;
    let cert_pem = order.poll_certificate(&retries).await?;

    certs.set_pem(cert_pem.as_bytes(), key_pem.as_bytes())?;
    let dir = tls_dir(config).join("acme");
    tokio::fs::create_dir_all(&dir).await?;
    write_private(&dir.join("key.pem"), key_pem.as_bytes()).await?;
    tokio::fs::write(dir.join("cert.pem"), cert_pem.as_bytes()).await?;
    Ok(())
}
