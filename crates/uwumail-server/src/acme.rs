//! Let's Encrypt certificates through the ACME HTTP-01 challenge.
//!
//! The certificate names the server, plus the names of our domains whose DNS already points here:
//! `mta-sts.<domain>` for domains with MTA-STS on, so senders can fetch their policy, and the names
//! mail apps know from other servers, like `imap.<domain>` or `autoconfig.<domain>`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use tokio::sync::{Notify, watch};
use uwumail_store::Store;

use crate::config::Config;
use crate::tls::{CertStore, CertificateInfo, tls_dir, write_private};

/// Renew when the certificate expires in less than this.
const RENEW_BEFORE_SECS: i64 = 30 * 24 * 3600;
/// A name the CA could not validate is left out this long, to stay clear of its rate limits.
const FAILED_NAME_PAUSE_SECS: i64 = 24 * 3600;
/// How often to look for new names while the certificate is fine.
const NAME_CHECK_SECS: u64 = 15 * 60;
/// How long the first order waits for the tunnel to a paired gateway before it tries anyway.
const TUNNEL_WAIT_SECS: u64 = 60;
/// The tunnel coming up stops asking for an order after this many failed ones within an hour.
/// Let's Encrypt allows five failed validations per name and hour; this leaves room for the
/// retries on the clock, also with a tunnel that comes and goes.
const TUNNEL_ORDER_FAILURES: usize = 3;
const FAILURE_WINDOW: Duration = Duration::from_secs(3600);
/// Names under each of our domains that mail apps connect to or look up their settings at.
const CLIENT_NAMES: [&str; 5] = ["mail", "imap", "smtp", "autoconfig", "autodiscover"];

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

/// What the certificate task hears from a UwUMail Gateway. Behind one, the CA reaches this server
/// only while the tunnel is up.
#[derive(Clone, Default)]
pub struct Tunnel {
    /// Whether a gateway was paired when the server started.
    pub paired: bool,
    /// Notified each time the tunnel comes up.
    pub up: Arc<Notify>,
}

/// Keeps the certificate fresh and its names complete. Retries failures hourly, and sooner when
/// the tunnel to a gateway comes up.
pub async fn run(
    config: Config,
    certs: Arc<CertStore>,
    challenges: Arc<Challenges>,
    store: Store,
    tunnel: Tunnel,
    mut shutdown: watch::Receiver<bool>,
) {
    // Give the HTTP listener a moment to come up before the CA calls back. Behind a gateway the
    // CA gets nowhere before the tunnel is up, which wakes this task.
    let mut wait = Duration::from_secs(if tunnel.paired { TUNNEL_WAIT_SECS } else { 3 });
    let mut failed_names: HashMap<String, i64> = HashMap::new();
    let mut failures: VecDeque<Instant> = VecDeque::new();
    loop {
        let asleep_since = Instant::now();
        let tunnel_came_up = tokio::select! {
            _ = tokio::time::sleep(wait) => false,
            _ = tunnel.up.notified() => true,
            _ = shutdown.changed() => return,
        };
        if tunnel_came_up && recent_failures(&mut failures, Instant::now()) >= TUNNEL_ORDER_FAILURES {
            // An order may work now, but too many failed lately: a tunnel that comes and goes must
            // not use up the failed validations the CA allows. The retry on the clock stays due.
            tracing::info!(
                "the tunnel is up, but too many orders failed within the hour: the certificate waits for its retry"
            );
            wait = wait.saturating_sub(asleep_since.elapsed());
            continue;
        }
        let now = uwumail_now();
        let extra = extra_names(&config.hostname, &store).await;
        let names = names_to_request(&config.hostname, &extra, &failed_names, now);
        if !needs_certificate(certs.info().as_ref(), &names, now) {
            wait = Duration::from_secs(NAME_CHECK_SECS);
            continue;
        }
        tracing::info!(names = %names.join(", "), "requesting a certificate from Let's Encrypt");
        match order(&config, &certs, &challenges, &names).await {
            Ok(()) => {
                tracing::info!(names = %names.join(", "), "got a fresh certificate (=^･ω･^=)");
                wait = Duration::from_secs(NAME_CHECK_SECS);
            }
            Err(err) if names.len() > 1 => {
                // Try again soon without the extra names, which may simply not reach us yet.
                tracing::warn!(
                    error = %format!("{err:#}"),
                    "getting a certificate with the names of our domains failed, leaving them out for a day"
                );
                for name in &names[1..] {
                    failed_names.insert(name.clone(), now);
                }
                failures.push_back(Instant::now());
                wait = Duration::from_secs(60);
            }
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "getting a certificate failed, retrying in an hour");
                failures.push_back(Instant::now());
                wait = Duration::from_secs(3600);
            }
        }
        challenges.clear();
    }
}

/// The names under our domains that resolve to this server: `mta-sts.<domain>` of the domains with
/// MTA-STS on, and the names mail apps use for every domain.
async fn extra_names(hostname: &str, store: &Store) -> Vec<String> {
    let mta_sts = match store.mta_sts_domains().await {
        Ok(domains) => domains,
        Err(err) => {
            tracing::warn!(%err, "reading the MTA-STS domains failed");
            Vec::new()
        }
    };
    let domains = match store.domains().await {
        Ok(domains) => domains.into_iter().map(|domain| domain.name).collect(),
        Err(err) => {
            tracing::warn!(%err, "reading the domains failed");
            Vec::new()
        }
    };
    let candidates = candidate_names(hostname, &mta_sts, &domains);
    if candidates.is_empty() {
        return Vec::new();
    }
    let ours = addresses(hostname).await;
    let mut names = Vec::new();
    for name in candidates {
        if addresses(&name).await.iter().any(|ip| ours.contains(ip)) {
            names.push(name);
        }
    }
    names
}

/// Every name that may belong on the certificate, before asking DNS whether it points here.
fn candidate_names(hostname: &str, mta_sts: &[String], domains: &[String]) -> Vec<String> {
    let mut names: Vec<String> = mta_sts.iter().map(|domain| format!("mta-sts.{domain}")).collect();
    for domain in domains {
        for prefix in CLIENT_NAMES {
            let name = format!("{prefix}.{domain}");
            if name != hostname && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

async fn addresses(host: &str) -> Vec<std::net::IpAddr> {
    match tokio::net::lookup_host((host, 443)).await {
        Ok(found) => found.map(|addr| addr.ip()).collect(),
        Err(_) => Vec::new(),
    }
}

/// The server's own name first, then the extra names that did not fail recently.
fn names_to_request(hostname: &str, extra: &[String], failed: &HashMap<String, i64>, now: i64) -> Vec<String> {
    let mut names = vec![hostname.to_owned()];
    for name in extra {
        let paused = failed.get(name).is_some_and(|at| now - at < FAILED_NAME_PAUSE_SECS);
        if !paused && !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// How many orders failed within the last hour. Older ones are forgotten.
fn recent_failures(failures: &mut VecDeque<Instant>, now: Instant) -> usize {
    while failures.front().is_some_and(|at| now.saturating_duration_since(*at) >= FAILURE_WINDOW) {
        failures.pop_front();
    }
    failures.len()
}

fn needs_certificate(current: Option<&CertificateInfo>, names: &[String], now: i64) -> bool {
    match current {
        Some(info) => {
            info.self_signed
                || names.iter().any(|name| !info.names.contains(name))
                || info.not_after - now < RENEW_BEFORE_SECS
        }
        None => true,
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

async fn order(config: &Config, certs: &CertStore, challenges: &Challenges, names: &[String]) -> anyhow::Result<()> {
    let account = account(config).await?;
    let identifiers: Vec<Identifier> = names.iter().map(|name| Identifier::Dns(name.clone())).collect();
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
             Is port 80 open and does the DNS record point here? Behind a UwUMail Gateway it has to point to \
             the gateway, and the tunnel has to be connected.",
            names.join(", http://")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn info(names: &[&str], days_left: i64, self_signed: bool) -> CertificateInfo {
        CertificateInfo {
            not_after: 1_000_000 + days_left * 24 * 3600,
            names: names.iter().map(|name| name.to_string()).collect(),
            self_signed,
        }
    }

    #[test]
    fn candidates_cover_mta_sts_and_the_names_mail_apps_use() {
        let domains = ["example.de".to_owned(), "verein.de".to_owned()];
        let names = candidate_names("mail.example.de", &domains[1..], &domains);
        assert_eq!(names[0], "mta-sts.verein.de");
        assert!(names.contains(&"imap.verein.de".to_owned()) && names.contains(&"autodiscover.example.de".to_owned()));
        assert!(!names.contains(&"mail.example.de".to_owned()), "the server's own name comes first anyway");
        assert_eq!(names.len(), 1 + 2 * CLIENT_NAMES.len() - 1);
    }

    #[test]
    fn extra_names_join_unless_they_failed_lately() {
        let now = 1_000_000;
        let extra = vec!["mta-sts.example.de".to_owned(), "mta-sts.verein.de".to_owned()];
        let failed = HashMap::from([("mta-sts.verein.de".to_owned(), now - 3600)]);
        assert_eq!(
            names_to_request("mail.example.de", &extra, &failed, now),
            ["mail.example.de", "mta-sts.example.de"]
        );
        let later = now + FAILED_NAME_PAUSE_SECS;
        assert_eq!(names_to_request("mail.example.de", &extra, &failed, later).len(), 3, "tried again a day later");
    }

    #[test]
    fn failed_orders_count_for_an_hour() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut failures = VecDeque::from([at(0), at(60), at(1800)]);
        assert_eq!(recent_failures(&mut failures, at(1801)), 3, "the tunnel stops asking here");
        assert_eq!(recent_failures(&mut failures, at(3599)), 3);
        assert_eq!(recent_failures(&mut failures, at(3600)), 2, "the first one is an hour old");
        assert_eq!(recent_failures(&mut failures, at(3700)), 1);
        assert_eq!(recent_failures(&mut failures, at(2 * 3600)), 0);
        assert!(failures.is_empty());
    }

    #[test]
    fn a_certificate_is_ordered_when_names_are_missing_or_it_expires() {
        let now = 1_000_000;
        let names = ["mail.example.de".to_owned(), "mta-sts.example.de".to_owned()];
        assert!(needs_certificate(None, &names[..1], now));
        assert!(!needs_certificate(Some(&info(&["mail.example.de"], 60, false)), &names[..1], now));
        assert!(needs_certificate(Some(&info(&["mail.example.de"], 60, false)), &names, now), "a new name");
        assert!(needs_certificate(Some(&info(&["mail.example.de", "mta-sts.example.de"], 10, false)), &names, now));
        assert!(needs_certificate(Some(&info(&["mail.example.de"], 60, true)), &names[..1], now));
    }
}
