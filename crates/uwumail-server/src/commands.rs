//! Management commands for the terminal. Changes are written to the change log as "cli".

use anyhow::bail;
use serde_json::{Value, json};
use uwumail_store::{AccountUpdate, AuditEntry, NewAccount, PasswordLinkPurpose, Role, Store};

use crate::cli::{AccountCommand, AliasCommand, DomainCommand, GatewayCommand, QueueCommand, SpamCommand};
use crate::config::Config;

/// Password links from the command line work as long as those from the admin panel.
const PASSWORD_LINK_LIFETIME_SECS: i64 = 7 * 24 * 3600;

async fn audit(store: &Store, action: &str, target: &str, details: Value) {
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
    }
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
