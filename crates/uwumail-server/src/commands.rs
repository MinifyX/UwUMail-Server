//! Management commands for the terminal, until the admin panel exists.

use anyhow::bail;
use uwumail_store::{NewAccount, Role, Store};

use crate::cli::{AccountCommand, AliasCommand, DomainCommand, QueueCommand};
use crate::config::Config;

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
            println!("Removed {name}");
        }
        DomainCommand::Dns { name } => print_dns(config, store, &name).await?,
        DomainCommand::CatchAll { domain, account } => {
            store.set_catch_all(&domain, account.as_deref()).await?;
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
    println!("  _dmarc.{domain}.  TXT \"v=DMARC1; p=quarantine; adkim=s; aspf=s; rua=mailto:postmaster@{domain}\"");
    for key in keys.iter().filter(|k| k.active) {
        let (record_name, value) = key.dns_record();
        println!("  {record_name}.  TXT {}", zone_quoted(&value));
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

pub async fn account(store: &Store, command: AccountCommand) -> anyhow::Result<()> {
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
                let flags = [(account.role == Role::Admin).then_some("admin"), account.disabled.then_some("disabled")]
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
            println!("Deleted {address}");
        }
        AccountCommand::Password { address } => {
            let (password, generated) = password_from_env_or_generated();
            store.set_password(&address, &password).await?;
            if generated {
                println!("New password for {address}: {password}");
            } else {
                println!("Password for {address} changed");
            }
        }
        AccountCommand::Disable { address } => {
            store.set_account_disabled(&address, true).await?;
            println!("{address} can no longer log in or receive mail");
        }
        AccountCommand::Enable { address } => {
            store.set_account_disabled(&address, false).await?;
            println!("{address} is active again");
        }
    }
    Ok(())
}

pub async fn alias(store: &Store, command: AliasCommand) -> anyhow::Result<()> {
    match command {
        AliasCommand::Add { alias, account } => {
            store.add_alias(&alias, &account).await?;
            println!("{alias} now delivers to {account}");
        }
        AliasCommand::Remove { alias } => {
            store.remove_alias(&alias).await?;
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
