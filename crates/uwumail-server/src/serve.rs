//! Starts every listener and background task, and stops them on SIGTERM / Ctrl+C.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use uwumail_smtp::{ListenerKind, Smtp, SmtpSettings};
use uwumail_store::Store;

use crate::acme::{self, Challenges};
use crate::config::{Config, TlsMode};
use crate::http::{self, HttpState};
use crate::tls::{self, CertStore};

async fn bind(address: &str, what: &str) -> anyhow::Result<Option<TcpListener>> {
    if address.is_empty() {
        return Ok(None);
    }
    let listener = match TcpListener::bind(address).await {
        Ok(listener) => Ok(listener),
        // Containers without IPv6 cannot bind [::], so fall back to all IPv4 addresses.
        Err(_) if address.starts_with("[::]:") => TcpListener::bind(address.replacen("[::]", "0.0.0.0", 1)).await,
        Err(err) => Err(err),
    }
    .with_context(|| {
        format!("could not listen on {address} for {what} (is another mail or web server already using it?)")
    })?;
    tracing::info!(address = %listener.local_addr()?, "listening for {what}");
    Ok(Some(listener))
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    config.validate()?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), hostname = %config.hostname, "UwUMail Server is waking up (=^･ω･^=)");

    let store = Store::open(&config.data_dir).await.context("opening the data directory")?;
    if store.domains().await?.is_empty() {
        tracing::warn!("no domains yet, add one with: uwumail-server domain add example.com");
    }

    let certs = Arc::new(CertStore::default());
    tls::load_initial(&config, &certs).await.context("loading the TLS certificate")?;
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: config.hostname.clone(),
            smtp: config.smtp.clone(),
            delivery: config.delivery.clone(),
            tone: config.tone,
            server_tls: Some(tls::mail_server_config(certs.clone())?),
        },
    )?;

    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();

    for (address, kind, what) in [
        (&config.listen.smtp, ListenerKind::Mx, "mail from other servers (SMTP)"),
        (&config.listen.submission, ListenerKind::Submission, "mail apps (submission with STARTTLS)"),
        (&config.listen.submissions, ListenerKind::SubmissionTls, "mail apps (submission with TLS)"),
    ] {
        if let Some(listener) = bind(address, what).await? {
            tasks.spawn(uwumail_smtp::serve(smtp.clone(), listener, kind, shutdown_rx.clone()));
        }
    }
    tasks.spawn(uwumail_smtp::run_queue(smtp.clone(), shutdown_rx.clone()));

    let jmap = uwumail_jmap::Jmap::new(smtp.clone()).router();
    let web = uwumail_web::Web::new(
        store.clone(),
        uwumail_web::WebSettings { hostname: config.hostname.clone(), started: Instant::now() },
    )
    .router();
    let trusted_proxies = Arc::new(
        uwumail_smtp::IpNetwork::parse_list(&config.http.trusted_proxies)
            .map_err(|err| anyhow::anyhow!("http.trusted_proxies: {err}"))?,
    );
    let challenges = Arc::new(Challenges::default());
    let state =
        HttpState { hostname: config.hostname.clone(), challenges: challenges.clone(), started: Instant::now() };
    if let Some(listener) = bind(&config.listen.http, "HTTP (certificate challenges, redirect to HTTPS)").await? {
        tasks.spawn(http::serve(listener, None, http::redirect_app(state.clone()), shutdown_rx.clone()));
    }
    if let Some(listener) = bind(&config.listen.https, "HTTPS").await? {
        let tls = tls::https_server_config(certs.clone())?;
        let app = http::app(state.clone(), jmap.clone(), web.clone(), trusted_proxies.clone());
        tasks.spawn(http::serve(listener, Some(tls), app, shutdown_rx.clone()));
    }
    if let Some(listener) = bind(&config.listen.proxy, "HTTP behind a reverse proxy").await? {
        let app = http::app(state.clone(), jmap.clone(), web.clone(), trusted_proxies.clone());
        tasks.spawn(http::serve(listener, None, app, shutdown_rx.clone()));
    }

    match config.tls.mode {
        TlsMode::Acme => {
            tasks.spawn(acme::run(config.clone(), certs.clone(), challenges, shutdown_rx.clone()));
        }
        TlsMode::Files => {
            tasks.spawn(tls::watch_files(config.clone(), certs.clone(), shutdown_rx.clone()));
        }
        TlsMode::SelfSigned => tracing::warn!("using a self-signed certificate: fine for testing, not for real mail"),
    }

    tasks.spawn(collect_garbage(store.clone(), shutdown_rx.clone()));

    tracing::info!("ready ✉");
    wait_for_signal().await;
    tracing::info!("shutting down, see you soon");
    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(10), async { while tasks.join_next().await.is_some() {} }).await;
    Ok(())
}

async fn collect_garbage(store: Store, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
            _ = shutdown.changed() => return,
        }
        // People in the trash for 30 days go first, so their message files are cleaned up right after.
        match store.purge_trash(uwumail_store::TRASH_RETENTION_SECS).await {
            Ok(purged) => {
                for login in purged {
                    tracing::info!(%login, "removed a person from the trash for good");
                    let entry = uwumail_store::AuditEntry {
                        actor_id: None,
                        actor: "system".into(),
                        action: "account.purge".into(),
                        target: login,
                        details: serde_json::json!({ "reason": "trash" }),
                        ip: String::new(),
                    };
                    if let Err(err) = store.record_audit(entry).await {
                        tracing::warn!(%err, "writing the change log failed");
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "emptying the trash failed"),
        }
        match store.collect_garbage(3600).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "removed unused message files"),
            Err(err) => tracing::warn!(%err, "cleaning up message files failed"),
        }
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("installing the SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
