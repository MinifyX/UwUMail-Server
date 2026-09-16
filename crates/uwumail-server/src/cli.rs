use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "uwumail-server", version, about = "UwUMail Server: your own cute mail server (=^･ω･^=)")]
pub struct Cli {
    /// Path to the TOML configuration. Environment variables (UWUMAIL_*) override it.
    #[arg(long, short, global = true, env = "UWUMAIL_CONFIG")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the server.
    Serve,
    /// Check the configuration and exit.
    CheckConfig,
    /// Exit successfully if the running server answers (for container health checks).
    Health,
    /// Mail domains hosted here.
    #[command(subcommand)]
    Domain(DomainCommand),
    /// People and their mailboxes.
    #[command(subcommand)]
    Account(AccountCommand),
    /// Extra addresses that deliver into an account.
    #[command(subcommand)]
    Alias(AliasCommand),
    /// The outgoing mail queue.
    #[command(subcommand)]
    Queue(QueueCommand),
    /// The UwUMail Gateway in front of this server.
    #[command(subcommand)]
    Gateway(GatewayCommand),
    /// The spam filter.
    #[command(subcommand)]
    Spam(SpamCommand),
}

#[derive(Debug, Subcommand)]
pub enum SpamCommand {
    /// Learn once from mail that is already sorted: what lies in Junk as spam, read mail older than two
    /// weeks in the inbox and archive as wanted mail. The running server learns it in the background.
    Learn {
        /// Only this person's mail; everyone's when left out.
        account: Option<String>,
    },
    /// What the Bayes filter has learned so far.
    Stats,
    /// Always let a sender through: an IP address or network, a host name, an email address or a domain.
    Allow(SenderArgs),
    /// Keep a sender out. Blocked for the server or a domain, their mail is refused; for a person it goes
    /// to Junk.
    Block(SenderArgs),
    /// Show allowed and blocked senders: the server's and every domain's, or one person's.
    Senders {
        #[arg(long)]
        account: Option<String>,
    },
    /// Take a sender off a list by the number `senders` shows.
    Unlist {
        id: i64,
        /// The person the entry belongs to, for personal entries.
        #[arg(long)]
        account: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct SenderArgs {
    /// E.g. 192.0.2.10, 198.51.100.0/24, *.mail.example.com, someone@example.com or example.com.
    pub value: String,
    /// What the value is; guessed when left out. A single host name has to be given as host.
    #[arg(long, value_enum)]
    pub kind: Option<SenderKindArg>,
    /// Only for mail to this one of our domains.
    #[arg(long, conflicts_with = "account")]
    pub domain: Option<String>,
    /// Only for this person.
    #[arg(long)]
    pub account: Option<String>,
    #[arg(long, default_value = "")]
    pub note: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SenderKindArg {
    Ip,
    Host,
    Address,
    Domain,
}

#[derive(Debug, Subcommand)]
pub enum GatewayCommand {
    /// Show the pairing with the gateway.
    Show,
    /// Forget the gateway, so mail leaves from this machine again after a restart. Remove
    /// `gateway.code` from the configuration too, or the server pairs again.
    Forget,
}

#[derive(Debug, Subcommand)]
pub enum DomainCommand {
    /// Add a domain and create its DKIM keys.
    Add {
        name: String,
    },
    List,
    /// Remove a domain that no address uses anymore.
    Remove {
        name: String,
    },
    /// Show the DNS records the domain needs.
    Dns {
        name: String,
    },
    /// Deliver mail for unknown addresses to an account (or stop doing so).
    CatchAll {
        domain: String,
        /// Leave out to turn the catch-all off.
        account: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Create an account. Prints a generated password unless UWUMAIL_PASSWORD is set.
    Add {
        address: String,
        #[arg(long, default_value = "")]
        name: String,
        /// May manage the whole server.
        #[arg(long)]
        admin: bool,
        /// Storage limit in megabytes, 0 for none.
        #[arg(long, default_value_t = 0)]
        quota_mb: i64,
    },
    List,
    /// Delete an account and all of its mail.
    Remove {
        address: String,
        /// Required, because this cannot be undone.
        #[arg(long)]
        yes: bool,
    },
    /// Set a new password. Prints a generated one unless UWUMAIL_PASSWORD is set.
    Password {
        address: String,
    },
    /// Print a one-time link (valid 7 days) to choose a new password in the browser.
    Link {
        address: String,
    },
    /// Remove the authenticator app, passkeys and recovery codes, e.g. after a lost phone.
    Reset2fa {
        address: String,
    },
    /// Lock someone out; mail to them still arrives.
    Disable {
        address: String,
    },
    Enable {
        address: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AliasCommand {
    Add {
        alias: String,
        account: String,
    },
    Remove {
        alias: String,
    },
    /// All addresses of an account.
    List {
        account: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum QueueCommand {
    List,
    /// Try a queued message again right away.
    Retry {
        id: i64,
    },
    /// Delete a queued message without bouncing it.
    Drop {
        id: i64,
    },
}
