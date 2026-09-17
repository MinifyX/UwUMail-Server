//! Management commands for the terminal. Changes are written to the change log as "cli".

use anyhow::bail;
use serde_json::{Value, json};
use uwumail_store::{
    AccountUpdate, AuditEntry, ListOwner, ListScope, NewAccount, NewSenderListEntry, PasswordLinkPurpose, Role,
    SenderKind, SenderList, SenderListEntry, Store,
};

use crate::cli::{
    AccountCommand, AliasCommand, BackupCommand, DomainCommand, ForwardCommand, GatewayCommand, QueueCommand,
    SenderArgs, SenderKindArg, SpamCommand, Switch, WordTarget, WordsCommand,
};
use crate::config::Config;

/// Password links from the command line work as long as those from the admin panel.
const PASSWORD_LINK_LIFETIME_SECS: i64 = 7 * 24 * 3600;

pub(crate) async fn audit(store: &Store, action: &str, target: &str, details: Value) {
    let entry = AuditEntry {
        actor_id: None,
        actor: "cli".into(),
        action: action.into(),
        target: target.into(),
        details,
        ip: String::new(),
    };
    if let Err(err) = store.record_audit(entry).await {
        eprintln!("(could not write the change log: {err})");
    }
}

fn password_from_env_or_generated() -> (String, bool) {
    match std::env::var("UWUMAIL_PASSWORD") {
        Ok(password) if !password.is_empty() => (password, false),
        _ => (generate_password(), true),
    }
}

/// 20 characters from an alphabet without look-alikes.
fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    // Only bytes below a multiple of the alphabet size, so every character is equally likely.
    let limit = (256 / ALPHABET.len() * ALPHABET.len()) as u8;
    let random = rustls::crypto::aws_lc_rs::default_provider().secure_random;
    let mut password = String::with_capacity(20);
    let mut bytes = [0u8; 32];
    while password.len() < 20 {
        random.fill(&mut bytes).expect("the system RNG failed");
        for byte in bytes.iter().filter(|b| **b < limit) {
            if password.len() < 20 {
                password.push(ALPHABET[*byte as usize % ALPHABET.len()] as char);
            }
        }
    }
    password
}

pub async fn domain(config: &Config, store: &Store, command: DomainCommand) -> anyhow::Result<()> {
    match command {
        DomainCommand::Add { name } => {
            let domain = store.create_domain(&name).await?;
            uwumail_smtp::dkim::ensure_domain_keys(store, &domain.name).await?;
            audit(store, "domain.create", &domain.name, json!({})).await;
            println!("Added {} (=^･ω･^=)", domain.name);
            println!();
            print_dns(config, store, &domain.name).await?;
        }
        DomainCommand::List => {
            for domain in store.domains().await? {
                match domain.catch_all {
                    Some(target) => println!("{}  (catch-all: {target})", domain.name),
                    None => println!("{}", domain.name),
                }
            }
        }
        DomainCommand::Remove { name } => {
            store.delete_domain(&name).await?;
            audit(store, "domain.remove", &name, json!({})).await;
            println!("Removed {name}");
        }
        DomainCommand::Dns { name } => print_dns(config, store, &name).await?,
        DomainCommand::CatchAll { domain, account } => {
            store.set_catch_all(&domain, account.as_deref()).await?;
            audit(store, "domain.catchAll", &domain, json!({ "account": account })).await;
            match account {
                Some(account) => println!("Unknown addresses at {domain} now go to {account}"),
                None => println!("Catch-all for {domain} is off"),
            }
        }
    }
    Ok(())
}

async fn print_dns(config: &Config, store: &Store, name: &str) -> anyhow::Result<()> {
    let Some(domain) = store.domain(name).await? else {
        bail!("{name} is not hosted here");
    };
    let domain = domain.name;
    let host = if config.hostname.is_empty() { "<hostname>" } else { config.hostname.as_str() };
    let keys = uwumail_smtp::dkim::ensure_domain_keys(store, &domain).await?;

    println!("DNS records for {domain}:");
    println!();
    println!("  {domain}.  MX  10 {host}.");
    println!("  {domain}.  TXT \"v=spf1 mx -all\"");
    println!("  _dmarc.{domain}.  TXT \"v=DMARC1; p=quarantine; adkim=s; aspf=s; rua=mailto:dmarc-reports@{domain}\"");
    // New keys of a rotation are published before they sign.
    for key in keys.iter().filter(|k| k.state() != uwumail_store::DkimKeyState::Retired) {
        let (record_name, value) = key.dns_record();
        println!("  {record_name}.  TXT {}", zone_quoted(&value));
    }
    println!();
    println!("Recommended:");
    println!("  _smtp._tls.{domain}.  TXT \"v=TLSRPTv1; rua=mailto:tls-reports@{domain}\"");
    for (_, record_name, port) in uwumail_smtp::dnscheck::service_records(&domain) {
        println!("  {record_name}.  SRV 0 1 {port} {host}.");
    }
    if let Some(settings) = store.mta_sts(&domain).await? {
        let policy = uwumail_smtp::mta_sts::Policy::ours(settings.mode, &settings.mx);
        println!();
        println!("MTA-STS ({}):", policy.mode.as_str());
        println!("  _mta-sts.{domain}.  TXT \"{}\"", uwumail_smtp::mta_sts::txt_record(&policy));
        println!("  mta-sts.{domain}.  CNAME {host}.");
    }
    println!();
    println!("And for the server itself:");
    println!("  {host}.  A/AAAA  <the public IP addresses of this server>");
    println!("  Reverse DNS (PTR) of those addresses: {host}  (set at your hosting provider)");
    Ok(())
}

/// Zone-file notation: TXT strings longer than 255 bytes are split into quoted chunks.
fn zone_quoted(value: &str) -> String {
    value
        .as_bytes()
        .chunks(255)
        .map(|chunk| format!("\"{}\"", String::from_utf8_lossy(chunk)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn account(config: &Config, store: &Store, command: AccountCommand) -> anyhow::Result<()> {
    match command {
        AccountCommand::Add { address, name, admin, quota_mb } => {
            let (password, generated) = password_from_env_or_generated();
            let account = store
                .create_account(NewAccount {
                    address,
                    display_name: name,
                    password: Some(password.clone()),
                    role: if admin { Role::Admin } else { Role::User },
                    quota_bytes: quota_mb.max(0) * 1024 * 1024,
                })
                .await?;
            audit(
                store,
                "account.create",
                &account.login,
                json!({ "role": account.role, "quotaBytes": account.quota_bytes }),
            )
            .await;
            println!("Created {} ✉", account.login);
            if generated {
                println!("Password: {password}");
                println!("(Shown only once. Mail apps log in with the address and this password.)");
            }
        }
        AccountCommand::List => {
            for account in store.accounts().await? {
                let quota = if account.quota_bytes > 0 {
                    format!("{} / {} MB", account.used_bytes / 1_048_576, account.quota_bytes / 1_048_576)
                } else {
                    format!("{} MB", account.used_bytes / 1_048_576)
                };
                let flags = [
                    (account.role == Role::Admin).then_some("admin"),
                    account.disabled.then_some("disabled"),
                    account.deleted_at.is_some().then_some("in the trash"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
                println!("{:<40} {:<12} {}", account.login, quota, flags);
            }
        }
        AccountCommand::Remove { address, yes } => {
            if !yes {
                bail!("this deletes {address} and all of its mail forever; add --yes if you are sure");
            }
            store.delete_account(&address).await?;
            audit(store, "account.purge", &address, json!({})).await;
            println!("Deleted {address}");
        }
        AccountCommand::Password { address } => {
            let (password, generated) = password_from_env_or_generated();
            store.set_password(&address, &password).await?;
            if let Some(account) = store.account(&address).await? {
                store.delete_web_sessions(account.id).await?;
            }
            audit(store, "account.passwordSet", &address, json!({})).await;
            if generated {
                println!("New password for {address}: {password}");
            } else {
                println!("Password for {address} changed");
            }
        }
        AccountCommand::Link { address } => {
            let Some(account) = store.account(&address).await? else {
                bail!("there is no account {address}");
            };
            if account.deleted_at.is_some() {
                bail!("{address} is in the trash; restore it in the admin panel first");
            }
            if account.disabled {
                store.update_account(&address, AccountUpdate { disabled: Some(false), ..Default::default() }).await?;
                println!("{address} was locked out and is active again");
            }
            let (token, _) = store
                .create_password_link(&address, PasswordLinkPurpose::Reset, None, PASSWORD_LINK_LIFETIME_SECS)
                .await?;
            audit(store, "account.passwordLink", &account.login, json!({ "purpose": "reset" })).await;
            let host = if config.hostname.is_empty() { "<hostname>" } else { config.hostname.as_str() };
            println!("Open this link within 7 days to choose a new password for {}:", account.login);
            println!("https://{host}/password/{token}");
        }
        AccountCommand::Reset2fa { address } => {
            let Some(account) = store.account(&address).await? else {
                bail!("there is no account {address}");
            };
            if !store.security_overview(account.id).await?.second_factor {
                println!("{} has no second factor", account.login);
                return Ok(());
            }
            store.reset_second_factors(account.id).await?;
            audit(store, "account.secondFactorsReset", &account.login, json!({})).await;
            let event = uwumail_store::SecurityEvent {
                kind: "secondFactorsReset".into(),
                actor: "cli".into(),
                ip: String::new(),
                details: json!({}),
            };
            store.record_security_event(account.id, event).await?;
            println!("{} logs in with the password only now", account.login);
        }
        AccountCommand::Disable { address } => {
            store.update_account(&address, AccountUpdate { disabled: Some(true), ..Default::default() }).await?;
            audit(store, "account.update", &address, json!({ "disabled": true })).await;
            println!("{address} can no longer log in (mail still arrives)");
        }
        AccountCommand::Enable { address } => {
            store.update_account(&address, AccountUpdate { disabled: Some(false), ..Default::default() }).await?;
            audit(store, "account.update", &address, json!({ "disabled": false })).await;
            println!("{address} is active again");
        }
        AccountCommand::Admin { address, state } => {
            let role = if state == Switch::On { Role::Admin } else { Role::User };
            let account =
                store.update_account(&address, AccountUpdate { role: Some(role), ..Default::default() }).await?;
            audit(store, "account.update", &account.login, json!({ "role": account.role })).await;
            match account.role {
                Role::Admin => println!("{} may manage the whole server now", account.login),
                Role::User => println!("{} is no admin anymore", account.login),
            }
        }
        AccountCommand::SendAs { address, domains } => {
            let account = store.account(&address).await?.ok_or_else(|| anyhow::anyhow!("no account {address}"))?;
            let domains = store.set_send_as_domains(account.id, domains).await?;
            audit(store, "account.sendAsDomains", &account.login, json!({ "domains": domains })).await;
            match domains.is_empty() {
                true => println!("{} sends only as their own addresses", account.login),
                false => println!("{} may send as any address of {}", account.login, domains.join(", ")),
            }
        }
    }
    Ok(())
}

pub async fn alias(store: &Store, command: AliasCommand) -> anyhow::Result<()> {
    match command {
        AliasCommand::Add { alias, account } => {
            store.add_alias(&alias, &account).await?;
            audit(store, "alias.add", &alias, json!({ "account": account })).await;
            println!("{alias} now delivers to {account}");
        }
        AliasCommand::Remove { alias } => {
            store.remove_alias(&alias).await?;
            audit(store, "alias.remove", &alias, json!({})).await;
            println!("Removed {alias}");
        }
        AliasCommand::List { account } => {
            for address in store.addresses(&account).await? {
                println!("{address}");
            }
        }
    }
    Ok(())
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

fn utc(time: i64) -> String {
    let days = time.div_euclid(86_400);
    let seconds = time.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02} UTC", seconds / 3600, seconds % 3600 / 60)
}

pub async fn backup(config: &Config, store: &Store, command: BackupCommand) -> anyhow::Result<()> {
    let backups = uwumail_backup::Backups::new(store.clone(), &config.hostname, env!("CARGO_PKG_VERSION"));
    match command {
        BackupCommand::Run => {
            let report = backups.run_now().await?;
            println!(
                "Snapshot {}: {} uploaded of {}; {} old snapshots and {} unused parts removed",
                report.snapshot,
                megabytes(report.uploaded),
                megabytes(report.total),
                report.removed_snapshots,
                report.removed_objects
            );
        }
        BackupCommand::List => {
            let snapshots = backups.snapshots().await?;
            if snapshots.is_empty() {
                println!("No snapshots yet.");
            }
            for (name, manifest) in snapshots {
                let total = manifest.database_size + manifest.blobs_size;
                println!(
                    "{name}  {}  {} mails  {}  (uploaded {})",
                    utc(manifest.created_at),
                    manifest.blobs.len(),
                    megabytes(total),
                    megabytes(manifest.uploaded)
                );
            }
        }
        BackupCommand::Check => {
            let snapshots = backups.snapshots().await?;
            let Some((name, _)) = snapshots.first() else { anyhow::bail!("there are no snapshots yet") };
            let mut settings = backups.settings().await?;
            let target = settings.target.take().ok_or_else(|| anyhow::anyhow!("no backup server is set up"))?;
            let key = settings.key.as_deref().map(uwumail_backup::RepoKey::from_recovery_text).transpose()?;
            let sftp = uwumail_backup::sftp::Sftp::connect(&target).await?;
            let repo = uwumail_backup::Repository::open_existing(uwumail_backup::Storage::Sftp(sftp), key).await?;
            let missing = uwumail_backup::check(&repo, name).await;
            repo.storage.close().await;
            let missing = missing?;
            if !missing.is_empty() {
                anyhow::bail!("{} parts of snapshot {name} are missing on the backup server", missing.len());
            }
            println!("Snapshot {name} is complete (=^･ω･^=)");
        }
        BackupCommand::Restore { sftp, port, ssh_key, host_key, snapshot, into } => {
            let (user, rest) = sftp.split_once('@').ok_or_else(|| anyhow::anyhow!("--sftp needs user@host:/path"))?;
            let (host, path) = rest.split_once(':').ok_or_else(|| anyhow::anyhow!("--sftp needs user@host:/path"))?;
            let login = match ssh_key {
                Some(file) => uwumail_backup::Login::Key { private_key: std::fs::read_to_string(file)? },
                None => uwumail_backup::Login::Password {
                    password: std::env::var("UWUMAIL_BACKUP_SFTP_PASSWORD")
                        .map_err(|_| anyhow::anyhow!("give --ssh-key or set UWUMAIL_BACKUP_SFTP_PASSWORD"))?,
                },
            };
            let target = uwumail_backup::Target {
                host: host.into(),
                port,
                user: user.into(),
                path: path.into(),
                login,
                host_key,
            };
            let connection = uwumail_backup::sftp::Sftp::connect(&target).await?;
            println!("Connected to {host}, host key {}", connection.host_key);
            let storage = uwumail_backup::Storage::Sftp(connection);
            let key = if uwumail_backup::Repository::is_encrypted(&storage).await? {
                let text = match std::env::var("UWUMAIL_BACKUP_KEY") {
                    Ok(text) => text,
                    Err(_) => {
                        eprintln!("Recovery key:");
                        let mut line = String::new();
                        std::io::stdin().read_line(&mut line)?;
                        line
                    }
                };
                Some(uwumail_backup::RepoKey::from_recovery_text(&text)?)
            } else {
                None
            };
            let repo = uwumail_backup::Repository::open_existing(storage, key).await?;
            let result = async {
                let name = match snapshot.as_str() {
                    "latest" => {
                        repo.snapshots().await?.pop().ok_or_else(|| anyhow::anyhow!("there are no snapshots"))?
                    }
                    name => name.to_owned(),
                };
                let manifest = uwumail_backup::restore(&repo, &name, &into).await?;
                anyhow::Ok((name, manifest))
            }
            .await;
            repo.storage.close().await;
            let (name, manifest) = result?;
            println!(
                "Restored snapshot {name} of {} from {} into {} (=^･ω･^=)",
                manifest.hostname,
                utc(manifest.created_at),
                into.display()
            );
        }
    }
    Ok(())
}

pub async fn forward(store: &Store, command: ForwardCommand) -> anyhow::Result<()> {
    match command {
        ForwardCommand::Set { address, targets, note } => {
            let saved = store.set_forward_address(&address, targets, &note).await?;
            audit(store, "domain.forwardAddress", &saved.address, json!({ "targets": saved.targets })).await;
            println!("{} now forwards to {}", saved.address, saved.targets.join(", "));
        }
        ForwardCommand::Remove { address } => {
            store.remove_forward_address(&address).await?;
            audit(store, "domain.forwardAddressRemove", &address, json!({})).await;
            println!("Removed {address}");
        }
        ForwardCommand::List { domain } => {
            for forward in store.forward_addresses(domain).await? {
                println!("{} -> {}", forward.address, forward.targets.join(", "));
            }
        }
    }
    Ok(())
}

pub async fn spam(store: &Store, command: SpamCommand) -> anyhow::Result<()> {
    match command {
        SpamCommand::Learn { account } => {
            let accounts = match account {
                Some(login) => vec![store.account(&login).await?.ok_or_else(|| anyhow::anyhow!("no account {login}"))?],
                None => store.accounts().await?,
            };
            let (mut spam, mut ham) = (0, 0);
            for account in &accounts {
                let (found_spam, found_ham) = store
                    .queue_bayes_from_folders(
                        account.id,
                        uwumail_store::BAYES_WANTED_AFTER_SECS,
                        uwumail_store::BAYES_FOLDER_LIMIT,
                    )
                    .await?;
                spam += found_spam;
                ham += found_ham;
            }
            audit(store, "spam.learnFromFolders", "server", json!({ "spam": spam, "ham": ham })).await;
            println!("Queued {spam} spam and {ham} wanted messages; the running server learns them in the background.");
        }
        SpamCommand::Stats => {
            let server = store.bayes_totals(None).await?;
            let queued = store.bayes_queue_length().await?;
            println!("Learned for the whole server: {} spam, {} wanted ({queued} waiting)", server.spam, server.ham);
            println!("The Bayes filter counts once both reach {}.", uwumail_store::BAYES_MIN_LEARNED);
        }
        SpamCommand::Allow(args) => add_sender(store, SenderList::Allow, args).await?,
        SpamCommand::Block(args) => add_sender(store, SenderList::Block, args).await?,
        SpamCommand::Senders { account } => {
            let entries = match account {
                Some(login) => store.sender_list(ListScope::Account(account_id(store, &login).await?)).await?,
                None => store.admin_sender_lists().await?,
            };
            if entries.is_empty() {
                println!("No senders listed.");
            }
            for entry in entries {
                let scope = entry.domain.clone().unwrap_or_else(|| "server".into());
                let scope = if matches!(entry.scope, ListScope::Account(_)) { "own".into() } else { scope };
                let note = if entry.note.is_empty() { String::new() } else { format!("  ({})", entry.note) };
                println!(
                    "{:>5}  {:<5}  {:<7}  {:<20}  {}{note}",
                    entry.id,
                    list_name(&entry),
                    kind_name(entry.kind),
                    scope,
                    entry.value
                );
            }
        }
        SpamCommand::Unlist { id, account } => {
            let owner = match account {
                Some(login) => ListOwner::Account(account_id(store, &login).await?),
                None => ListOwner::Admin,
            };
            let entry = store.remove_sender_list_entry(owner, id).await?;
            if owner == ListOwner::Admin {
                audit(store, "spam.senderRemove", &entry.value, sender_details(&entry)).await;
            }
            println!("Took {} off the {} list.", entry.value, list_name(&entry));
        }
        SpamCommand::Words(command) => words(store, command).await?,
        SpamCommand::Feeds => {
            let states = store.feed_states().await?;
            for feed in uwumail_smtp::FEEDS {
                let state = states.iter().find(|state| state.key == feed.key);
                let status = match state {
                    None => "not fetched yet".to_owned(),
                    Some(state) => match &state.error {
                        Some(error) => format!("{} entries, last attempt failed: {error}", state.entries),
                        None => format!("{} entries", state.entries),
                    },
                };
                let key = if feed.needs_key { " (needs spam.feeds.abuse_ch_key)" } else { "" };
                println!("{:<15} {:<24} {status}{key}", feed.key, feed.source);
            }
        }
    }
    Ok(())
}

async fn word_scope(store: &Store, target: &WordTarget) -> anyhow::Result<ListScope> {
    Ok(match (&target.domain, &target.account) {
        (Some(name), _) => {
            ListScope::Domain(store.domain(name).await?.ok_or_else(|| anyhow::anyhow!("no domain {name}"))?.id)
        }
        (None, Some(login)) => ListScope::Account(account_id(store, login).await?),
        (None, None) => ListScope::Server,
    })
}

async fn word_owner(store: &Store, account: Option<String>) -> anyhow::Result<ListOwner> {
    Ok(match account {
        Some(login) => ListOwner::Account(account_id(store, &login).await?),
        None => ListOwner::Admin,
    })
}

async fn add_words(store: &Store, target: WordTarget, text: String) -> anyhow::Result<()> {
    let scope = word_scope(store, &target).await?;
    let report = store.add_words(scope, text, target.points, String::new(), "cli".into()).await?;
    if report.added > 0 && !matches!(scope, ListScope::Account(_)) {
        let details = json!({ "added": report.added, "domain": target.domain });
        audit(store, "spam.wordsAdd", target.domain.as_deref().unwrap_or("server"), details).await;
    }
    println!("Added {}, {} already listed, {} refused.", report.added, report.duplicates, report.refused_count);
    for refused in report.refused {
        println!("  {}: {}", refused.line, refused.reason);
    }
    Ok(())
}

pub async fn words(store: &Store, command: WordsCommand) -> anyhow::Result<()> {
    match command {
        WordsCommand::List { account } => {
            let (entries, sources) = match account {
                Some(login) => {
                    let scope = ListScope::Account(account_id(store, &login).await?);
                    (store.word_entries(scope).await?, store.word_sources(scope).await?)
                }
                None => (store.admin_word_entries().await?, store.admin_word_sources().await?),
            };
            if entries.is_empty() && sources.is_empty() {
                println!("No word lists.");
            }
            for entry in entries {
                let scope = entry.domain.clone().unwrap_or_else(|| "server".into());
                let points = entry.points.map_or(String::new(), |points| format!("  ({points} points)"));
                println!("{:>5}  {:<20}  {}{points}", entry.id, scope, entry.pattern);
            }
            for source in sources {
                let scope = source.domain.clone().unwrap_or_else(|| "server".into());
                let state = match &source.error {
                    Some(error) => format!("failed: {error}"),
                    None if source.fetched_at.is_none() => "not fetched yet".into(),
                    None => format!("{} entries", source.entries),
                };
                println!("{:>5}  {:<20}  {}  [{state}]", source.id, scope, source.url);
            }
        }
        WordsCommand::Add { entries, target } => add_words(store, target, entries.join("\n")).await?,
        WordsCommand::Import { file, target } => {
            let text = std::fs::read_to_string(&file)
                .map_err(|err| anyhow::anyhow!("could not read {}: {err}", file.display()))?;
            add_words(store, target, text).await?;
        }
        WordsCommand::Remove { id, account } => {
            let owner = word_owner(store, account).await?;
            let entry = store.remove_word_entry(owner, id).await?;
            if owner == ListOwner::Admin {
                audit(store, "spam.wordRemove", &entry.pattern, json!({ "domain": entry.domain })).await;
            }
            println!("Removed {}.", entry.pattern);
        }
        WordsCommand::Subscribe { url, target, subject_only } => {
            uwumail_smtp::Smtp::check_list_link(&url).map_err(|reason| anyhow::anyhow!("{reason}"))?;
            let scope = word_scope(store, &target).await?;
            let source = store.add_word_source(scope, url, subject_only, target.points, "cli".into()).await?;
            if !matches!(scope, ListScope::Account(_)) {
                audit(store, "spam.wordSourceAdd", &source.url, json!({ "domain": target.domain })).await;
            }
            println!("Subscribed as number {}; the running server fetches it within ten minutes.", source.id);
        }
        WordsCommand::Unsubscribe { id, account } => {
            let owner = word_owner(store, account).await?;
            let source = store.remove_word_source(owner, id).await?;
            if owner == ListOwner::Admin {
                audit(store, "spam.wordSourceRemove", &source.url, json!({ "domain": source.domain })).await;
            }
            println!("Unsubscribed from {}.", source.url);
        }
    }
    Ok(())
}

async fn account_id(store: &Store, login: &str) -> anyhow::Result<i64> {
    Ok(store.account(login).await?.ok_or_else(|| anyhow::anyhow!("no account {login}"))?.id)
}

fn list_name(entry: &SenderListEntry) -> &'static str {
    match entry.list {
        SenderList::Allow => "allow",
        SenderList::Block => "block",
    }
}

fn kind_name(kind: SenderKind) -> &'static str {
    match kind {
        SenderKind::Ip => "ip",
        SenderKind::Host => "host",
        SenderKind::Address => "address",
        SenderKind::Domain => "domain",
        SenderKind::Pattern => "pattern",
    }
}

fn sender_details(entry: &SenderListEntry) -> Value {
    json!({ "list": entry.list, "kind": entry.kind, "domain": entry.domain })
}

async fn add_sender(store: &Store, list: SenderList, args: SenderArgs) -> anyhow::Result<()> {
    let scope = match (&args.domain, &args.account) {
        (Some(name), _) => {
            ListScope::Domain(store.domain(name).await?.ok_or_else(|| anyhow::anyhow!("no domain {name}"))?.id)
        }
        (None, Some(login)) => ListScope::Account(account_id(store, login).await?),
        (None, None) => ListScope::Server,
    };
    let kind = args.kind.map(|kind| match kind {
        SenderKindArg::Ip => SenderKind::Ip,
        SenderKindArg::Host => SenderKind::Host,
        SenderKindArg::Address => SenderKind::Address,
        SenderKindArg::Domain => SenderKind::Domain,
        SenderKindArg::Pattern => SenderKind::Pattern,
    });
    let new = NewSenderListEntry { scope, list, kind, value: args.value, note: args.note, created_by: "cli".into() };
    let entry = store.add_sender_list_entry(new).await?;
    if !matches!(scope, ListScope::Account(_)) {
        audit(store, "spam.senderAdd", &entry.value, sender_details(&entry)).await;
    }
    println!(
        "Put {} ({}) on the {} list as number {}.",
        entry.value,
        kind_name(entry.kind),
        list_name(&entry),
        entry.id
    );
    Ok(())
}

pub async fn queue(store: &Store, command: QueueCommand) -> anyhow::Result<()> {
    match command {
        QueueCommand::List => {
            let entries = store.queue_entries().await?;
            if entries.is_empty() {
                println!("The queue is empty (˘ᵕ˘)");
            }
            for entry in entries {
                let from = if entry.message.return_path.is_empty() { "<>" } else { &entry.message.return_path };
                println!("#{} from {from}, {} bytes", entry.message.id, entry.message.size);
                for recipient in entry.recipients {
                    println!(
                        "    {:<40} {:?}, {} attempts{}",
                        recipient.address,
                        recipient.status,
                        recipient.attempts,
                        recipient.last_error.map(|e| format!(": {e}")).unwrap_or_default()
                    );
                }
            }
        }
        QueueCommand::Retry { id } => {
            store.retry_queue_message(id).await?;
            println!("Message #{id} will be retried within a minute");
        }
        QueueCommand::Drop { id } => {
            store.delete_queue_message(id).await?;
            println!("Dropped message #{id}");
        }
    }
    Ok(())
}

pub async fn gateway(config: &Config, store: &Store, command: GatewayCommand) -> anyhow::Result<()> {
    let stored = match store.setting(crate::gateway::PAIRING_KEY).await? {
        Some(raw) => Some(serde_json::from_str::<crate::gateway::StoredPairing>(&raw)?),
        None => None,
    };
    match command {
        GatewayCommand::Show => {
            let Some(pairing) = stored else {
                println!("No UwUMail Gateway: mail leaves from this machine.");
                return Ok(());
            };
            let addresses = pairing.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            println!("UwUMail Gateway at {addresses}");
            println!("  gateway certificate  {}", pairing.gateway);
            println!("  this server's key    {}", pairing.identity.fingerprint());
            let state = if pairing.confirmed { "paired" } else { "waiting for the gateway to accept the code" };
            println!("  pairing              {state}");
        }
        GatewayCommand::Forget => {
            if stored.is_none() {
                println!("There is no gateway to forget.");
                return Ok(());
            }
            store.delete_setting(crate::gateway::PAIRING_KEY).await?;
            audit(store, "gateway.forget", "", json!({})).await;
            println!("Forgot the gateway. Restart the server; mail then leaves from this machine again.");
            if !config.gateway.code.trim().is_empty() {
                println!("Also remove `gateway.code` from the configuration, or the server pairs again on start.");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_passwords_are_long_and_readable() {
        let password = generate_password();
        assert_eq!(password.len(), 20);
        assert!(!password.contains(['0', 'O', 'l', '1', 'I']));
        assert_ne!(password, generate_password());
    }
}
