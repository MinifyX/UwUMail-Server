//! UwUMail Gateway: a fixed public address for a UwUMail server at home.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;
use uwumail_gateway::GatewayConfig;
use uwumail_gateway::config::LogFormat;
use uwumail_gateway::state::State;

#[derive(Debug, Parser)]
#[command(
    name = "uwumail-gateway",
    version,
    about = "UwUMail Gateway: a fixed public address for a UwUMail server at home (=^･ω･^=)"
)]
struct Cli {
    /// Path to the TOML configuration. Environment variables (UWUMAIL_GATEWAY_*) override it.
    #[arg(long, short, global = true, env = "UWUMAIL_GATEWAY_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the gateway (the default).
    Serve,
    /// Show the pairing code for the UwUMail server.
    Code,
    /// Forget the paired server so another one can pair. The running gateway disconnects it.
    Unpair,
    /// Check the configuration and exit.
    CheckConfig,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match GatewayConfig::load(cli.config.as_deref()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("(╥﹏╥) {err:#}");
            return ExitCode::FAILURE;
        }
    };
    let command = cli.command.unwrap_or(Command::Serve);
    init_logging(&config, matches!(command, Command::Serve));

    let result = match command {
        Command::Serve => serve(config).await,
        Command::Code => show_code(&config),
        Command::Unpair => unpair(&config).await,
        Command::CheckConfig => config.validate().map(|()| println!("The configuration looks good (=^･ω･^=)")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("(╥﹏╥) {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(config: &GatewayConfig, serving: bool) {
    // Commands only print what matters; the gateway logs at the configured level.
    let level = if serving { config.log.level.as_str() } else { "warn" };
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    match config.log.format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Text => builder.with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout())).init(),
    }
}

async fn serve(config: GatewayConfig) -> anyhow::Result<()> {
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "UwUMail Gateway is waking up (=^･ω･^=)");
    let (shutdown, shutdown_rx) = watch::channel(false);
    let running = uwumail_gateway::start(config, shutdown_rx).await?;
    tracing::info!(fingerprint = %running.fingerprint, "ready ✉");
    wait_for_signal().await;
    tracing::info!("shutting down, see you soon");
    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(10), running.wait()).await;
    Ok(())
}

fn show_code(config: &GatewayConfig) -> anyhow::Result<()> {
    let state = State::open(&config.state_dir)?;
    if let Some(pairing) = state.pairing()? {
        println!("This gateway is paired with {}.", pairing.hostname);
        println!("To pair another server instead: uwumail-gateway unpair");
        return Ok(());
    }
    let (Some(identity), Some(token)) = (state.identity()?, state.token()?) else {
        anyhow::bail!("there is no pairing code yet: start the gateway first (systemctl start uwumail-gateway)");
    };
    let port = config.tunnel.parse::<SocketAddr>().map(|address| address.port())?;
    let Some(code) = uwumail_gateway::pairing_code(&config.public_addresses(), port, &identity, &token) else {
        anyhow::bail!("found no public address for this gateway: set public_addresses in the configuration");
    };
    println!("Enter this pairing code on your UwUMail server under Server → Setup:\n\n  {code}\n");
    Ok(())
}

async fn unpair(config: &GatewayConfig) -> anyhow::Result<()> {
    let state = State::open(&config.state_dir)?;
    match state.pairing()? {
        Some(pairing) => {
            state.remove_pairing()?;
            state.remove_token()?;
            println!("Forgot {}. The running gateway disconnects it in a moment.", pairing.hostname);
        }
        None => println!("No server was paired."),
    }
    // The running gateway makes the new code; give it a moment so it can be shown right away.
    for _ in 0..10 {
        if state.token()?.is_some() {
            return show_code(config);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!("Start the gateway, then show the new pairing code with: uwumail-gateway code");
    Ok(())
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
