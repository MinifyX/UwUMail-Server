//! Moving to UwUMail from another mail server.

pub mod imap;
mod mailcow;

pub use mailcow::mailcow;

use uwumail_store::Store;

/// Copies the mail of each person over IMAP and prints how it went. Everyone on the given domains
/// here, plus the given logins; the login is the same on both servers.
pub async fn imap(
    store: &Store,
    source: imap::Source,
    logins: &[String],
    domains: &[String],
    dry_run: bool,
) -> anyhow::Result<()> {
    let mut people: Vec<String> = logins.iter().map(|login| login.trim().to_lowercase()).collect();
    if !domains.is_empty() {
        let domains: Vec<String> = domains.iter().map(|domain| format!("@{}", domain.trim().to_lowercase())).collect();
        for account in store.accounts().await? {
            if account.deleted_at.is_none()
                && domains.iter().any(|domain| account.login.ends_with(domain.as_str()))
                && !people.contains(&account.login)
            {
                people.push(account.login);
            }
        }
    }
    if people.is_empty() {
        anyhow::bail!("name the people to copy with --login or --domain");
    }
    let mut failed = 0;
    let mut total = imap::Copied::default();
    for login in &people {
        println!("{login}");
        let mut show = |line: &str| println!("  {line}");
        match imap::copy_mail(store, &source, login, login, dry_run, &mut show).await {
            Ok(copied) => {
                println!(
                    "  {} messages ({} MB) from {} folders",
                    copied.messages,
                    copied.bytes / 1_000_000,
                    copied.folders
                );
                total.messages += copied.messages;
                total.bytes += copied.bytes;
            }
            Err(err) => {
                failed += 1;
                println!("  (╥﹏╥) {err:#}");
            }
        }
    }
    let verb = if dry_run { "Would copy" } else { "Copied" };
    println!(
        "{verb} {} messages ({} MB) for {} people.",
        total.messages,
        total.bytes / 1_000_000,
        people.len() - failed
    );
    if failed > 0 {
        anyhow::bail!(
            "{failed} of {} people could not be copied; running it again continues where it stopped",
            people.len()
        );
    }
    if !dry_run {
        crate::commands::audit(store, "import.imap", &source.address, serde_json::json!({ "people": people })).await;
    }
    Ok(())
}
