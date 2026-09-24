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
use crate::gateway;
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

pub async fn run(
    config: Config,
    config_path: Option<std::path::PathBuf>,
    logs: Arc<uwumail_web::LogBuffer>,
) -> anyhow::Result<()> {
    config.validate()?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), hostname = %config.hostname, "UwUMail Server is waking up (=^･ω･^=)");

    // Before anything opens the data directory: a snapshot the portal fetched may be waiting to
    // take the place of what is here. This is the only moment those files belong to nobody.
    let waiting = crate::restore::waiting(&config.data_dir);
    // Read while the old database is still the one in place: the gateway in the snapshot belongs
    // to the machine that made it, and this one is very likely its replacement.
    let pairing_here = match &waiting {
        Some(ready) if ready.keep_gateway => crate::restore::pairing_here(&config.data_dir).await,
        _ => None,
    };
    let restored = crate::restore::take_over(&config.data_dir).await;

    let store = Store::open(&config.data_dir).await.context("opening the data directory")?;
    if let Some(done) = &restored {
        crate::restore::after(&store, done, pairing_here, &config.hostname).await;
    }
    // Settings changed in the admin panel, underneath the config file and environment.
    let overlay = store
        .setting(uwumail_web::SETTINGS_OVERLAY_KEY)
        .await?
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_default();
    let config = match Config::load_with_overlay(config_path.as_deref(), &overlay) {
        Ok(merged) => merged,
        Err(err) => {
            tracing::warn!(error = %format!("{err:#}"), "ignoring the settings from the admin panel");
            config
        }
    };

    let certs = Arc::new(CertStore::default());
    tls::load_initial(&config, &certs).await.context("loading the TLS certificate")?;
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: config.hostname.clone(),
            smtp: config.smtp.clone(),
            spam: config.spam.clone(),
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
    // Mail apps may append messages as big as they may send.
    let imap = uwumail_imap::Imap::new(store.clone(), config.smtp.max_message_size);
    let mail_tls = tls::mail_server_config(certs.clone())?;
    if let Some(listener) = bind(&config.listen.imaps, "mail apps (IMAP with TLS)").await? {
        tasks.spawn(imap.clone().serve(listener, mail_tls.clone(), shutdown_rx.clone()));
    }
    // Mail rules for apps that manage Sieve scripts; the same logins and lockouts as IMAP.
    if let Some(listener) = bind(&config.listen.managesieve, "mail rules (ManageSieve with STARTTLS)").await? {
        let managesieve = uwumail_imap::ManageSieve::new(&imap);
        tasks.spawn(managesieve.serve(listener, mail_tls.clone(), shutdown_rx.clone()));
    }
    tasks.spawn(uwumail_smtp::run_queue(smtp.clone(), shutdown_rx.clone()));
    tasks.spawn(uwumail_smtp::run_learning(smtp.clone(), shutdown_rx.clone()));
    tasks.spawn(uwumail_smtp::run_list_updates(smtp.clone(), shutdown_rx.clone()));
    // Mailboxes at other providers, emptied into the mailboxes here that asked for them.
    tasks.spawn(crate::fetch::run_fetchers(store.clone(), smtp.clone(), shutdown_rx.clone()));

    // Calendars and contacts (CalDAV, CardDAV) live next to JMAP on the same HTTPS port.
    let names = match config.tone.language {
        uwumail_smtp::Language::De => ("Kalender", "Kontakte"),
        _ => ("Calendar", "Contacts"),
    };
    let dav = uwumail_dav::Dav::new(
        store.clone(),
        uwumail_dav::DavSettings { calendar_name: names.0.into(), addressbook_name: names.1.into() },
    );
    // One switch for the whole server, shared by everything that has to honour it: the page
    // under /mail, JMAP's session login, and the admin panel that flips it.
    let webmail = Arc::new(std::sync::atomic::AtomicBool::new(config.http.webmail));
    // A message's remote pictures, fetched here instead of by the reader; through a VPN when one is set.
    let egress = uwumail_smtp::egress::Egress::new(&config.egress).map_err(anyhow::Error::msg)?;
    if egress.proxied() {
        tracing::info!(fallback = ?config.egress.fallback, "remote pictures leave through the egress proxy");
    }
    let jmap = uwumail_jmap::Jmap::with_webmail(smtp.clone(), webmail.clone())
        .with_egress(egress.clone())
        .router()
        .merge(dav.router());
    // The log to Grafana Loki, when the config or the admin panel asks for it; the admin panel
    // switches it on, over and off while the server runs.
    let loki = uwumail_web::Loki::new();
    logs.forward_to(loki.clone());
    match config.log.loki.target(&config.hostname) {
        Ok(target) => loki.set_target(target),
        Err(err) => tracing::warn!(%err, "not sending the log to Loki"),
    }
    tasks.spawn(loki.clone().run(shutdown_rx.clone()));
    let certificate: uwumail_web::CertificateSource = {
        let (certs, automatic, config) = (certs.clone(), config.tls.mode == TlsMode::Acme, config.clone());
        Arc::new(move || {
            certs.info().map(|info| uwumail_web::CertificateStatus {
                not_after: info.not_after,
                names: info.names,
                self_signed: info.self_signed,
                automatic,
                lets_encrypt_account: crate::acme::lets_encrypt_account(&config),
            })
        })
    };
    let web = uwumail_web::Web::new(
        smtp.clone(),
        uwumail_web::WebSettings {
            hostname: config.hostname.clone(),
            started: Instant::now(),
            logs: Some(logs.clone()),
            loki: Some(loki.clone()),
            config: Some(Arc::new(crate::settings::ServerSettings {
                path: config_path,
                smtp: smtp.clone(),
                loki,
                webmail: webmail.clone(),
            })),
            certificate: Some(certificate),
            webmail,
        },
    );
    web.set_egress(egress);
    tasks.spawn(web.clone().run_health_checks(shutdown_rx.clone()));
    let setup_code = web.open_setup().await;
    let gateway = gateway::GatewayManager::new(
        store.clone(),
        smtp.clone(),
        config.hostname.clone(),
        &config.gateway,
        shutdown_rx.clone(),
    );
    web.set_gateway(gateway.clone());
    // What the gateway logs shows up next to this server's lines, in the portal and in Loki.
    gateway.log_to(logs.clone());
    // A network this server turns away is kept off the gateway's public ports too, so the next
    // try does not reach the house at all. Only this side can see who fails to log in.
    let report_blocks = gateway.reporter();
    smtp.report_blocks_to(Some(report_blocks.clone()));
    imap.report_blocks_to(Some(report_blocks.clone()));
    web.report_blocks_to(Some(report_blocks));
    // Only there when someone installed the helper beside us (deploy/host/). Without it the portal
    // shows the commands to copy, the way it always did.
    if let Some(bridge) = crate::host::HostBridge::find() {
        web.set_host(Arc::new(bridge));
    }
    let backups = uwumail_backup::Backups::new(store.clone(), &config.hostname, env!("CARGO_PKG_VERSION"));
    web.set_backups(backups.clone());
    // The one thing a restore needs that the backup service cannot have by itself: where the data
    // lives, and a way to stop the server once the snapshot is here. It cannot put the files in
    // place while running on the database it would replace.
    {
        let stop = shutdown.clone();
        backups.restores_into(
            &config.data_dir,
            Box::new(move || {
                let _ = stop.send(true);
            }),
        );
    }
    // After the helper and the backups, not before: the first thing this does is ask the helper how
    // the update that replaced the container it is starting in turned out.
    tasks.spawn(web.clone().run_updates(shutdown_rx.clone()));
    let web = web.router();
    let trusted_proxies = Arc::new(
        uwumail_smtp::IpNetwork::parse_list(&config.http.trusted_proxies)
            .map_err(|err| anyhow::anyhow!("http.trusted_proxies: {err}"))?,
    );
    let challenges = Arc::new(Challenges::default());
    let state =
        HttpState { hostname: config.hostname.clone(), challenges: challenges.clone(), started: Instant::now() };
    let redirect = http::redirect_app(state.clone());
    let https_tls = tls::https_server_config(certs.clone())?;
    let https = http::app(state.clone(), jmap.clone(), web.clone(), trusted_proxies.clone())
        .layer(axum::middleware::from_fn_with_state(certs.clone(), http::strict_transport_security));
    let connections = http::Connections::new();
    if let Some(listener) = bind(&config.listen.http, "HTTP (certificate challenges, redirect to HTTPS)").await? {
        tasks.spawn(http::serve(listener, None, redirect.clone(), connections.clone(), true, shutdown_rx.clone()));
    }
    if let Some(listener) = bind(&config.listen.https, "HTTPS").await? {
        tasks.spawn(http::serve(
            listener,
            Some(https_tls.clone()),
            https.clone(),
            connections.clone(),
            true,
            shutdown_rx.clone(),
        ));
    }
    if let Some(listener) = bind(&config.listen.proxy, "HTTP behind a reverse proxy").await? {
        let app = http::app(state.clone(), jmap.clone(), web.clone(), trusted_proxies.clone());
        // Every connection there comes from the proxy, so only the total counts.
        tasks.spawn(http::serve(listener, None, app, connections.clone(), false, shutdown_rx.clone()));
    }
    // Connections that arrive through a UwUMail Gateway reach the same services.
    let services = gateway::Services {
        smtp: smtp.clone(),
        imap,
        mail_tls,
        https_tls,
        https,
        http: redirect,
        http_connections: connections,
    };
    gateway.start(services, &config.gateway).await;

    if let Some(code) = setup_code {
        let hostname = &config.hostname;
        tracing::warn!("no admin yet: open https://{hostname}/setup and enter the one-time code {code}");
        if gateway.is_paired() {
            tracing::info!(
                "this server uses a UwUMail Gateway: https://{hostname}/setup works through it as soon as the \
                 tunnel is connected"
            );
        } else if !config.listen.https.is_empty() {
            tracing::info!(
                "if {hostname} does not reach this server yet, https://<address of this machine>/setup works too \
                 (with the port, when 443 is published on another one)"
            );
        }
    }

    match config.tls.mode {
        TlsMode::Acme => {
            let tunnel = acme::Tunnel { paired: gateway.is_paired(), up: gateway.tunnel_up() };
            tasks.spawn(acme::run(
                config.clone(),
                certs.clone(),
                challenges,
                store.clone(),
                tunnel,
                shutdown_rx.clone(),
            ));
        }
        TlsMode::Files => {
            tasks.spawn(tls::watch_files(config.clone(), certs.clone(), shutdown_rx.clone()));
        }
        TlsMode::SelfSigned => tracing::warn!("using a self-signed certificate: fine for testing, not for real mail"),
    }

    tasks.spawn(collect_garbage(store.clone(), smtp.clone(), shutdown_rx.clone()));
    tasks.spawn(backups.clone().run(shutdown_rx.clone()));

    tracing::info!("ready ✉");
    wait_for_signal().await;
    tracing::info!("shutting down, see you soon");
    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(10), async { while tasks.join_next().await.is_some() {} }).await;
    Ok(())
}

async fn collect_garbage(store: Store, smtp: uwumail_smtp::Smtp, mut shutdown: watch::Receiver<bool>) {
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
        match store.purge_reports(uwumail_store::REPORT_RETENTION_SECS).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "removed old DMARC and TLS reports"),
            Err(err) => tracing::warn!(%err, "removing old reports failed"),
        }
        let spam_history = store.prune_spam_history(
            uwumail_store::GREYLIST_WAITING_SECS,
            uwumail_store::GREYLIST_PASSED_SECS,
            uwumail_store::REPUTATION_RETENTION_SECS,
        );
        match spam_history.await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "forgot old greylisting and sender reputation entries"),
            Err(err) => tracing::warn!(%err, "cleaning up greylisting and sender reputation failed"),
        }
        // The spam history is the one table an admin sets the age of, so it is read fresh each round.
        let log = smtp.spam_log_settings();
        match store.prune_spam_log(i64::from(log.retention_days.max(1)) * 24 * 3600).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "removed old entries from the spam history"),
            Err(err) => tracing::warn!(%err, "cleaning up the spam history failed"),
        }
        // Greylisted messages nobody came back for, and everything at all once an admin switched
        // keeping them off. Before the message files are cleaned up, so both go in the same round.
        let held = if smtp.spam_settings().greylist_hold {
            store.prune_greylist_holds().await
        } else {
            store.clear_greylist_holds().await
        };
        match held {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "removed greylisted messages nobody decided about"),
            Err(err) => tracing::warn!(%err, "cleaning up greylisted messages failed"),
        }
        match store.prune_bayes(uwumail_store::BAYES_RARE_TOKEN_SECS, uwumail_store::BAYES_LEARNED_SECS).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "forgot rare and old Bayes filter entries"),
            Err(err) => tracing::warn!(%err, "cleaning up the Bayes filter failed"),
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
