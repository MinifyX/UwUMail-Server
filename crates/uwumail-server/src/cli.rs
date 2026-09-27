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
    /// Addresses without a mailbox that pass their mail on to other addresses.
    #[command(subcommand)]
    Forward(ForwardCommand),
    /// The outgoing mail queue.
    #[command(subcommand)]
    Queue(QueueCommand),
    /// Take over people, addresses and settings from another mail server.
    #[command(subcommand)]
    Import(ImportCommand),
    /// The UwUMail Gateway in front of this server.
    #[command(subcommand)]
    Gateway(GatewayCommand),
    /// The spam filter.
    #[command(subcommand)]
    Spam(SpamCommand),
    /// Server settings, the same ones the admin panel changes.
    #[command(subcommand)]
    Settings(SettingsCommand),
    /// Backups to an SFTP server, an S3 bucket or a folder, set up in the admin panel.
    #[command(subcommand)]
    Backup(BackupCommand),
}

#[derive(Debug, Subcommand)]
pub enum BackupCommand {
    /// Back up now. While the server runs, the button in the admin panel is the better way.
    Run,
    /// The snapshots on the backup server.
    List,
    /// Check that every part of the newest snapshot is on the backup server.
    Check,
    /// Restore a snapshot into an empty data directory, e.g. on a new machine. Needs no settings:
    /// the backups are named with --sftp, --s3 or --folder. The SSH password comes from
    /// UWUMAIL_BACKUP_SFTP_PASSWORD, the S3 keys from UWUMAIL_BACKUP_S3_ACCESS_KEY and
    /// UWUMAIL_BACKUP_S3_SECRET_KEY, the recovery key from UWUMAIL_BACKUP_KEY or the first line of
    /// standard input.
    #[command(group(clap::ArgGroup::new("from").required(true).args(["sftp", "s3", "folder"])))]
    Restore {
        /// Where the backups are on an SFTP server: user@host:/path.
        #[arg(long)]
        sftp: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// An OpenSSH private key to log in with, instead of a password.
        #[arg(long)]
        ssh_key: Option<std::path::PathBuf>,
        /// The host key's SHA256 fingerprint, to be sure it is the right server.
        #[arg(long)]
        host_key: Option<String>,
        /// Where the backups are in an S3 bucket: s3://bucket/folder.
        #[arg(long)]
        s3: Option<String>,
        /// The S3 server, e.g. https://s3.eu-central-1.amazonaws.com or http://minio.lan:9000.
        #[arg(long, default_value = "https://s3.amazonaws.com")]
        endpoint: String,
        #[arg(long, default_value = "us-east-1")]
        region: String,
        /// Put the bucket into the path (https://host/bucket/…), as MinIO wants it.
        #[arg(long)]
        path_style: bool,
        /// Where the backups are in a folder of this machine, e.g. a mounted disk.
        #[arg(long)]
        folder: Option<std::path::PathBuf>,
        /// A snapshot name from `backup list`, or `latest`.
        #[arg(long, default_value = "latest")]
        snapshot: String,
        /// The empty data directory to restore into.
        #[arg(long)]
        into: std::path::PathBuf,
    },
    /// Put one person's mail back from a snapshot, next to what they have now: into a new folder
    /// "Restored <date>" of their mailbox, with the folders below it as they were. Mail that is
    /// still there is not brought twice. Uses the backup settings of this server.
    RestoreMailbox {
        /// The person, by the address they logged in with when the snapshot was made.
        #[arg(long)]
        account: String,
        /// The mailbox here the mail goes into, when that is not the same address any more.
        #[arg(long)]
        into: Option<String>,
        /// A snapshot name from `backup list`, or `latest`.
        #[arg(long, default_value = "latest")]
        snapshot: String,
        /// Only this folder and the folders inside it, e.g. `Inbox` or `Projects/2025`. May be
        /// given more than once; all folders when left out.
        #[arg(long = "folder")]
        folders: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Every setting, its value, and where that value comes from.
    List {
        /// Only settings whose key starts with this, e.g. `spam`.
        prefix: Option<String>,
    },
    /// What one setting is set to.
    Get { key: String },
    /// Change a setting. A running server takes it from its next start.
    Set {
        key: String,
        /// The new value. `-` reads it from standard input, so a password stays out of the shell
        /// history.
        value: String,
    },
    /// Forget a setting made here, back to the config file or the default.
    Unset { key: String },
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
    /// Word lists: words, phrases and /regex/flags that count against a message.
    #[command(subcommand)]
    Words(WordsCommand),
    /// The built-in lists and how fetching them went.
    Feeds,
}

#[derive(Debug, Subcommand)]
pub enum WordsCommand {
    /// Show the server's and every domain's entries and subscribed lists, or one person's.
    List {
        #[arg(long)]
        account: Option<String>,
    },
    /// Add entries: words, phrases or /regex/flags.
    Add {
        #[arg(required = true)]
        entries: Vec<String>,
        #[command(flatten)]
        target: WordTarget,
    },
    /// Add every entry of a file, one per line, like an Rspamd map.
    Import {
        file: PathBuf,
        #[command(flatten)]
        target: WordTarget,
    },
    /// Remove an entry by the number `list` shows.
    Remove {
        id: i64,
        #[arg(long)]
        account: Option<String>,
    },
    /// Subscribe to a list by https link. The running server fetches it within ten minutes, then daily.
    Subscribe {
        url: String,
        #[command(flatten)]
        target: WordTarget,
        /// Look for the entries in the subject only.
        #[arg(long)]
        subject_only: bool,
    },
    /// Unsubscribe by the number `list` shows.
    Unsubscribe {
        id: i64,
        #[arg(long)]
        account: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct WordTarget {
    /// Only for mail to this one of our domains.
    #[arg(long, conflicts_with = "account")]
    pub domain: Option<String>,
    /// Only for this person.
    #[arg(long)]
    pub account: Option<String>,
    /// Points per matching entry; 2.5 when left out.
    #[arg(long)]
    pub points: Option<f32>,
}

#[derive(Debug, Args)]
pub struct SenderArgs {
    /// E.g. 192.0.2.10, 198.51.100.0/24, *.mail.example.com, someone@example.com, example.com or a
    /// pattern like *.tld or *newsletter*.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SenderKindArg {
    Ip,
    Host,
    Address,
    Domain,
    Pattern,
}

#[derive(Debug, Subcommand)]
pub enum GatewayCommand {
    /// Show the pairing with the gateway.
    Show,
    /// Pair with a gateway, for when the portal cannot be reached to do it there.
    ///
    /// Takes effect after a restart. With the stock compose file, `UWUMAIL_GATEWAY_CODE` in `.env`
    /// does the same.
    Pair {
        /// The pairing code the gateway shows (`uwugw1…`).
        code: String,
    },
    /// Forget the gateway, so mail leaves from this machine again after a restart.
    ///
    /// Remove `gateway.code` from the configuration too, or the server tries to pair again with a
    /// new key, which the gateway refuses. To pair again, run `uwumail-gateway unpair` on the VPS
    /// and use the new code.
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
        /// A mailbox for a program: no portal login, app passwords only, calendars and contacts
        /// off to begin with.
        #[arg(long, conflicts_with = "admin")]
        service: bool,
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
    /// Let someone manage the whole server, or take that away. The last admin cannot be taken away.
    Admin {
        address: String,
        state: Switch,
    },
    /// Turn a mailbox into one for a program, or back into a person's. The mail stays either way;
    /// on the way in, the password becomes an app password that does not expire.
    Service {
        address: String,
        state: Switch,
    },
    /// Turn a person or a service into a shared mailbox its members use from their own accounts,
    /// or a shared mailbox back into a plain service. Mail, addresses and app passwords stay.
    Shared {
        address: String,
        state: Switch,
        /// A member who reaches every folder of it. May be given more than once.
        #[arg(long = "member")]
        members: Vec<String>,
        /// A member who may also send with its addresses. May be given more than once.
        #[arg(long = "sender")]
        senders: Vec<String>,
    },
    /// Which protocols an account may use, and where its mail goes when it has no mailbox.
    Protocols {
        address: String,
        #[arg(long)]
        smtp: Option<Switch>,
        #[arg(long)]
        imap: Option<Switch>,
        #[arg(long)]
        jmap: Option<Switch>,
        #[arg(long)]
        calendar: Option<Switch>,
        #[arg(long)]
        contacts: Option<Switch>,
        /// Where mail goes while this account has no mailbox; an address of this server. Empty
        /// means its address takes no mail at all.
        #[arg(long)]
        redirect: Option<String>,
    },
    /// Let someone send as any address of these domains; without domains, only as their own again.
    SendAs {
        address: String,
        domains: Vec<String>,
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
pub enum ForwardCommand {
    /// Create a forwarding address or replace its targets.
    Set {
        address: String,
        /// Where its mail goes, here or elsewhere.
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(long, default_value = "")]
        note: String,
    },
    Remove {
        address: String,
    },
    /// All forwarding addresses, or those of one domain.
    List {
        #[arg(long)]
        domain: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ImportCommand {
    /// Import the file scripts/mailcow-export.sh wrote. Safe to run again: what exists stays.
    Mailcow {
        /// The export, or - to read it from standard input.
        file: std::path::PathBuf,
        /// Only this domain; repeat for more. All active domains when left out.
        #[arg(long)]
        domain: Vec<String>,
        /// Show what would happen without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Import an iCalendar file (.ics) into someone's calendar. Safe to run again: entries are
    /// matched by their UID.
    Ics {
        /// The file, or - to read it from standard input.
        file: std::path::PathBuf,
        /// Whose calendar: the person's login.
        #[arg(long)]
        account: String,
        /// The calendar to import into, by its URL name (e.g. personal). A new one when left out.
        #[arg(long, conflicts_with = "name")]
        calendar: Option<String>,
        /// The name of the new calendar; the one in the file or the file name when left out.
        #[arg(long)]
        name: Option<String>,
        /// Leave entries alone whose UID is there already, instead of overwriting them.
        #[arg(long)]
        only_new: bool,
        /// Only count what would be imported.
        #[arg(long)]
        dry_run: bool,
    },
    /// Import a vCard file (.vcf) into someone's address book. Safe to run again: cards are
    /// matched by their UID.
    Vcf {
        /// The file, or - to read it from standard input.
        file: std::path::PathBuf,
        /// Whose address book: the person's login.
        #[arg(long)]
        account: String,
        /// The address book to import into, by its URL name (e.g. contacts). A new one when left out.
        #[arg(long, conflicts_with = "name")]
        address_book: Option<String>,
        /// The name of the new address book; the file name when left out.
        #[arg(long)]
        name: Option<String>,
        /// Leave cards alone whose UID is there already, instead of overwriting them.
        #[arg(long)]
        only_new: bool,
        /// Only count what would be imported.
        #[arg(long)]
        dry_run: bool,
    },
    /// Copy mail over IMAP with TLS. Running it again only fetches what arrived since.
    Imap {
        /// The old server, e.g. 192.0.2.10:993.
        #[arg(long)]
        host: String,
        /// The name on the old server's certificate, when --host is an IP address or another name.
        #[arg(long)]
        tls_name: Option<String>,
        /// A dovecot master user, so no one's own password is needed.
        #[arg(long)]
        master_user: Option<String>,
        /// The password (the master password with --master-user). Read from the first line of standard
        /// input when not set.
        #[arg(long, env = "UWUMAIL_IMPORT_PASSWORD", hide_env_values = true)]
        password: Option<String>,
        /// A person to copy, with the same login on both servers; repeat for more.
        #[arg(long)]
        login: Vec<String>,
        /// Everyone on this domain here; repeat for more.
        #[arg(long)]
        domain: Vec<String>,
        /// Only count what would be copied.
        #[arg(long)]
        dry_run: bool,
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
