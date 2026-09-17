//! UwUMail Server: your own cute mail server.

mod acme;
mod cli;
mod commands;
mod config;
mod gateway;
mod http;
mod import;
mod serve;
mod settings;
mod tls;

use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use uwumail_web::LogBuffer;

use crate::cli::{Cli, Command};
use crate::config::{Config, LogFormat};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let config = match Config::load(cli.config.as_deref()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("(╥﹏╥) {err:#}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let logs = init_logging(&config, matches!(cli.command, Command::Serve));

    match run(cli.command, config, cli.config, logs).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("(╥﹏╥) {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Logs to stdout and keeps the newest lines for the admin panel.
fn init_logging(config: &Config, serving: bool) -> Arc<LogBuffer> {
    // Management commands only print what matters; the server logs everything at the configured level.
    let level = if serving { config.log.level.as_str() } else { "warn" };
    // While resolving from the root servers, the recursor warns about every missing NS record on the way,
    // which is normal for names below a zone cut. DNS problems show up in the check results instead.
    let quiet = "hickory_resolver::recursor=error";
    let filter =
        EnvFilter::try_new(format!("{level},{quiet}")).unwrap_or_else(|_| EnvFilter::new(format!("info,{quiet}")));
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let logs = LogBuffer::new(2000);
    let output = match config.log.format {
        LogFormat::Json => tracing_subscriber::fmt::layer().json().with_target(false).boxed(),
        LogFormat::Text => tracing_subscriber::fmt::layer().with_target(false).with_ansi(ansi).boxed(),
    };
    tracing_subscriber::registry().with(filter).with(output).with(logs.layer()).init();
    logs
}

async fn run(
    command: Command,
    config: Config,
    config_path: Option<std::path::PathBuf>,
    logs: Arc<LogBuffer>,
) -> anyhow::Result<()> {
    if matches!(command, Command::Serve) {
        return serve::run(config, config_path, logs).await;
    }
    if matches!(command, Command::CheckConfig) {
        config.validate()?;
        println!("The configuration looks good (=^･ω･^=)");
        return Ok(());
    }
    if matches!(command, Command::Health) {
        return health(&config).await;
    }
    let store = uwumail_store::Store::open(&config.data_dir).await?;
    match command {
        Command::Domain(command) => commands::domain(&config, &store, command).await,
        Command::Account(command) => commands::account(&config, &store, command).await,
        Command::Alias(command) => commands::alias(&store, command).await,
        Command::Forward(command) => commands::forward(&store, command).await,
        Command::Import(crate::cli::ImportCommand::Mailcow { file, domain, dry_run }) => {
            let names = match config.tone.language {
                uwumail_smtp::Language::De => ("Kalender", "Kontakte"),
                _ => ("Calendar", "Contacts"),
            };
            let settings = uwumail_dav::DavSettings { calendar_name: names.0.into(), addressbook_name: names.1.into() };
            let dav = uwumail_dav::Dav::new(store.clone(), settings);
            import::mailcow(&store, dav, &file, &domain, dry_run).await
        }
        Command::Import(crate::cli::ImportCommand::Imap {
            host,
            tls_name,
            master_user,
            password,
            login,
            domain,
            dry_run,
        }) => {
            let password = match password {
                Some(password) => password,
                None => {
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim_end_matches(['\r', '\n']).to_owned()
                }
            };
            let source = import::imap::Source { address: host, tls_name, roots: None, master_user, password };
            import::imap(&store, source, &login, &domain, dry_run).await
        }
        Command::Queue(command) => commands::queue(&store, command).await,
        Command::Gateway(command) => commands::gateway(&config, &store, command).await,
        Command::Spam(command) => commands::spam(&store, command).await,
        Command::Backup(command) => commands::backup(&config, &store, command).await,
        Command::Serve | Command::CheckConfig | Command::Health => unreachable!("handled above"),
    }
}

/// Container health check: the SMTP listener greets within a few seconds.
async fn health(config: &Config) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;

    let listen = [&config.listen.smtp, &config.listen.submission, &config.listen.submissions]
        .into_iter()
        .find(|address| !address.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no SMTP listener is configured"))?;
    let port = listen.rsplit_once(':').map(|(_, port)| port).unwrap_or("25");
    let check = async {
        let mut socket = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await?;
        let mut greeting = [0u8; 3];
        socket.read_exact(&mut greeting).await?;
        anyhow::ensure!(&greeting == b"220", "unexpected greeting");
        anyhow::Ok(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), check)
        .await
        .map_err(|_| anyhow::anyhow!("timed out"))??;
    Ok(())
}
