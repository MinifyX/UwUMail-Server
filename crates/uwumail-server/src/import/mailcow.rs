//! Taking over a mailcow installation from the file `scripts/mailcow-export.sh` writes: domains,
//! mailboxes with their password hashes, aliases and forwarding, app passwords, send-as rights,
//! allowed and blocked senders, spam limits, DKIM keys, calendars and contacts. Mail itself comes
//! over IMAP.
//!
//! Running it again is safe: what exists is left alone. A person's own settings (forwarding, app
//! passwords, send-as rights, sender lists, spam limits, calendars and contacts) are only taken over
//! together with their mailbox, so whoever changed something here keeps it.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::json;
use uwumail_store::{
    AccountUpdate, AppScope, DavKind, DavPrecondition, DavWrite, DavWriteOutcome, ListScope, NewAccount,
    NewDavCollection, NewSenderListEntry, Role, SenderList, SpamLimits, Store, StoreError,
};

/// One line of the export.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum Entry {
    Domain {
        domain: String,
        active: i64,
    },
    AliasDomain {
        alias_domain: String,
        target_domain: String,
    },
    Mailbox {
        username: String,
        name: Option<String>,
        password: String,
        quota: i64,
        active: i64,
        domain: String,
    },
    Alias {
        address: String,
        goto: String,
        active: i64,
    },
    AppPassword {
        mailbox: String,
        name: String,
        password: String,
        active: i64,
        imap: Option<i64>,
        smtp: Option<i64>,
        dav: Option<i64>,
    },
    SenderAcl {
        logged_in_as: String,
        send_as: String,
    },
    Filter {
        object: String,
        option: String,
        value: String,
    },
    Dkim {
        domain: String,
        selector: String,
        private_key: String,
    },
    DavFolder {
        id: i64,
        owner: String,
        path: Option<String>,
        name: Option<String>,
        kind: String,
    },
    DavObject {
        folder: i64,
        name: String,
        content: String,
    },
    #[serde(other)]
    Unknown,
}

/// What an import did, or would do in a dry run.
#[derive(Default)]
struct Report {
    counts: HashMap<&'static str, usize>,
    notes: Vec<String>,
}

impl Report {
    fn count(&mut self, what: &'static str) {
        *self.counts.entry(what).or_default() += 1;
    }

    fn note(&mut self, note: String) {
        self.notes.push(note);
    }
}

struct Importer<'a> {
    store: &'a Store,
    dav: uwumail_dav::Dav,
    dry_run: bool,
    report: Report,
    /// Logins whose mailbox this run creates, or would create in a dry run.
    created: BTreeSet<String>,
    /// Domains with a mailcow DKIM key, taken over now or before (a dry run stores none).
    keyed: BTreeSet<String>,
}

fn lower(value: &str) -> String {
    value.trim().to_lowercase()
}

fn domain_of(address: &str) -> &str {
    address.rsplit_once('@').map_or("", |(_, domain)| domain)
}

/// A name that works in a DAV URL: other characters become `-`.
fn url_segment(name: &str) -> String {
    let segment: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.@+~".contains(c) { c } else { '-' })
        .take(200)
        .collect();
    if segment.is_empty() || segment == "." || segment == ".." { "imported".into() } else { segment }
}

/// Reads the export (`-` for standard input) and imports the chosen domains; all active domains when
/// none are chosen.
pub async fn mailcow(
    store: &Store,
    dav: uwumail_dav::Dav,
    file: &Path,
    domains: &[String],
    dry_run: bool,
) -> anyhow::Result<()> {
    let text = if file == Path::new("-") {
        tokio::task::spawn_blocking(|| std::io::read_to_string(std::io::stdin()))
            .await?
            .context("reading the export from standard input")?
    } else {
        tokio::fs::read_to_string(file).await.with_context(|| format!("reading {}", file.display()))?
    };
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty()) {
        let entry: Entry = serde_json::from_str(line)
            .with_context(|| format!("line {} of {} is not an export entry", index + 1, file.display()))?;
        entries.push(entry);
    }

    let chosen: BTreeSet<String> = if domains.is_empty() {
        entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Domain { domain, active } if *active != 0 => Some(lower(domain)),
                _ => None,
            })
            .collect()
    } else {
        domains.iter().map(|domain| lower(domain)).collect()
    };
    let known: BTreeSet<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Domain { domain, .. } => Some(lower(domain)),
            _ => None,
        })
        .collect();
    if let Some(missing) = chosen.iter().find(|domain| !known.contains(*domain)) {
        anyhow::bail!("{missing} is not in the export");
    }

    let mut importer =
        Importer { store, dav, dry_run, report: Report::default(), created: BTreeSet::new(), keyed: BTreeSet::new() };
    importer.run(&entries, &chosen).await?;

    let report = importer.report;
    let verb = if dry_run { "Would import" } else { "Imported" };
    println!("{verb} {} domain(s): {}", chosen.len(), chosen.iter().cloned().collect::<Vec<_>>().join(", "));
    let mut counts: Vec<_> = report.counts.into_iter().collect();
    counts.sort();
    for (what, count) in counts {
        println!("  {what}: {count}");
    }
    if !report.notes.is_empty() {
        println!("Worth a look:");
        for note in &report.notes {
            println!("  - {note}");
        }
    }
    if dry_run {
        println!("Nothing was changed. Run again without --dry-run to import.");
    } else {
        crate::commands::audit(store, "import.mailcow", &file.display().to_string(), json!({ "domains": chosen }))
            .await;
        println!("Next: copy the mail over IMAP, then point DNS here.");
    }
    Ok(())
}

impl Importer<'_> {
    async fn run(&mut self, entries: &[Entry], chosen: &BTreeSet<String>) -> anyhow::Result<()> {
        let in_scope = |address: &str| chosen.contains(&lower(domain_of(address)));

        for domain in chosen {
            self.domain(domain).await?;
        }
        for entry in entries {
            if let Entry::Dkim { domain, selector, private_key } = entry
                && chosen.contains(&lower(domain))
            {
                self.dkim(&lower(domain), selector, private_key).await?;
            }
        }
        for domain in chosen {
            let has_keys =
                self.keyed.contains(domain) || !self.store.dkim_keys(domain).await.unwrap_or_default().is_empty();
            if !has_keys {
                if !self.dry_run {
                    uwumail_smtp::dkim::ensure_domain_keys(self.store, domain).await?;
                }
                self.report.note(format!("{domain} had no DKIM key in mailcow: new keys need DNS records"));
            }
        }

        let mailboxes: BTreeSet<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Mailbox { username, .. } => Some(lower(username)),
                _ => None,
            })
            .collect();
        for entry in entries {
            match entry {
                Entry::Mailbox { username, name, password, quota, active, domain }
                    if chosen.contains(&lower(domain)) =>
                {
                    self.mailbox(&lower(username), name.as_deref().unwrap_or(""), password, *quota, *active).await?;
                }
                Entry::AliasDomain { alias_domain, target_domain } if chosen.contains(&lower(target_domain)) => {
                    self.report.note(format!(
                        "{alias_domain} mirrors {target_domain} in mailcow; UwUMail has no alias domains, add aliases instead"
                    ));
                }
                _ => {}
            }
        }

        let mut send_as: HashMap<String, BTreeSet<String>> = HashMap::new();
        for entry in entries {
            match entry {
                Entry::Alias { address, goto, active } if in_scope(address) => {
                    if *active == 0 {
                        self.report.note(format!("{address} is switched off in mailcow and was left out"));
                        continue;
                    }
                    self.alias(&lower(address), goto, &mailboxes).await?;
                }
                Entry::AppPassword { mailbox, name, password, active, imap, smtp, dav } if in_scope(mailbox) => {
                    if *active == 0 {
                        continue;
                    }
                    let mut scopes = Vec::new();
                    for (flag, scope) in [(imap, AppScope::Mail), (smtp, AppScope::Smtp), (dav, AppScope::Dav)] {
                        if flag.unwrap_or(1) != 0 {
                            scopes.push(scope);
                        }
                    }
                    self.app_password(&lower(mailbox), name, password, scopes).await?;
                }
                Entry::SenderAcl { logged_in_as, send_as: target } if in_scope(logged_in_as) => {
                    match target.trim().strip_prefix('@') {
                        Some(domain) => {
                            send_as.entry(lower(logged_in_as)).or_default().insert(lower(domain));
                        }
                        None => self.report.note(format!(
                            "{logged_in_as} may send as {target} in mailcow; give them that address as an alias instead"
                        )),
                    }
                }
                Entry::Filter { object, option, value } if in_scope(object) || chosen.contains(&lower(object)) => {
                    self.filter(&lower(object), option, value).await?;
                }
                _ => {}
            }
        }
        for (login, domains) in send_as {
            self.send_as(&login, domains).await?;
        }

        let folders: HashMap<i64, (String, Option<String>, Option<String>, String)> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::DavFolder { id, owner, path, name, kind } if in_scope(owner) => {
                    Some((*id, (lower(owner), path.clone(), name.clone(), kind.clone())))
                }
                _ => None,
            })
            .collect();
        let mut collections: HashMap<i64, Option<(i64, i64, DavKind)>> = HashMap::new();
        for entry in entries {
            if let Entry::DavObject { folder, name, content } = entry
                && let Some((owner, path, display, kind)) = folders.get(folder)
            {
                if !collections.contains_key(folder) {
                    let found = self.collection(owner, path.as_deref(), display.as_deref(), kind).await?;
                    collections.insert(*folder, found);
                }
                if let Some(Some(target)) = collections.get(folder) {
                    self.dav_object(*target, name, content).await?;
                }
            }
        }
        Ok(())
    }

    async fn domain(&mut self, domain: &str) -> anyhow::Result<()> {
        if self.store.domain(domain).await?.is_some() {
            return Ok(());
        }
        if !self.dry_run {
            self.store.create_domain(domain).await?;
        }
        self.report.count("domains");
        Ok(())
    }

    async fn dkim(&mut self, domain: &str, selector: &str, pem: &str) -> anyhow::Result<()> {
        self.keyed.insert(domain.to_owned());
        if self.store.dkim_keys(domain).await.unwrap_or_default().iter().any(|key| key.selector == selector) {
            return Ok(());
        }
        let key = uwumail_smtp::dkim::import_rsa_key(selector, pem)?;
        if !self.dry_run {
            self.store
                .add_dkim_key(domain, &key.selector, key.algorithm, key.private_key, key.public_key, true)
                .await?;
        }
        self.report.count("DKIM keys (the DNS record stays as it is)");
        Ok(())
    }

    async fn mailbox(&mut self, login: &str, name: &str, hash: &str, quota: i64, active: i64) -> anyhow::Result<()> {
        if self.store.account(login).await?.is_some() {
            self.report.note(format!("{login} already exists here and was left as it is"));
            return Ok(());
        }
        if let Err(err) = uwumail_store::normalize_imported_password_hash(hash) {
            self.report.note(format!("{login}: {err}; they need a password link"));
        }
        if !self.dry_run {
            let new = NewAccount {
                address: login.to_owned(),
                display_name: name.to_owned(),
                password: None,
                role: Role::User,
                quota_bytes: quota.max(0),
            };
            let account = self.store.create_account(new).await?;
            if uwumail_store::normalize_imported_password_hash(hash).is_ok() {
                self.store.import_password_hash(account.id, hash).await?;
            }
            if active != 1 {
                let update = AccountUpdate { disabled: Some(true), ..Default::default() };
                self.store.update_account(login, update).await?;
            }
        }
        self.created.insert(login.to_owned());
        if active != 1 {
            self.report.note(format!("{login} could not log in to mailcow and is locked here too"));
        }
        self.report.count("mailboxes");
        Ok(())
    }

    /// mailcow keeps several things in its alias table: a mailbox's own address (with forwarding
    /// when there are more targets), catch-alls (`@domain`), plain aliases and forwarding addresses.
    async fn alias(&mut self, address: &str, goto: &str, mailboxes: &BTreeSet<String>) -> anyhow::Result<()> {
        let mut targets: Vec<String> = goto.split(',').map(lower).filter(|target| !target.is_empty()).collect();
        let special: Vec<String> = targets.iter().filter(|target| target.ends_with("@localhost")).cloned().collect();
        if !special.is_empty() {
            self.report
                .note(format!("{address} goes to {} in mailcow, which UwUMail does not have", special.join(", ")));
            targets.retain(|target| !target.ends_with("@localhost"));
        }
        if targets.is_empty() {
            return Ok(());
        }

        if mailboxes.contains(address) {
            let keep_copy = targets.iter().any(|target| target == address);
            targets.retain(|target| target != address);
            return self.forwarding(address, &targets, keep_copy).await;
        }

        if let Some(domain) = address.strip_prefix('@') {
            match &targets[..] {
                [target] if self.local_account(target).await? => {
                    let current = self.store.domain(domain).await?.and_then(|domain| domain.catch_all);
                    if current.is_none() {
                        if !self.dry_run {
                            self.store.set_catch_all(domain, Some(target)).await?;
                        }
                        self.report.count("catch-all addresses");
                    }
                }
                _ => self.report.note(format!(
                    "the catch-all of {domain} goes to {}; UwUMail sends it to one person only",
                    targets.join(", ")
                )),
            }
            return Ok(());
        }

        if let [target] = &targets[..]
            && self.local_account(target).await?
        {
            let owner = self.store.resolve_recipient(address).await?;
            let target_id = self.store.account(target).await?.map(|account| account.id);
            if owner.is_some() && owner == target_id && self.store.addresses(target).await?.iter().any(|a| a == address)
            {
                return Ok(());
            }
            if self.store.forward_address_targets(address).await?.is_some() && !self.dry_run {
                // Taken over earlier as forwarding while its person was not here yet.
                self.store.remove_forward_address(address).await?;
            }
            if !self.dry_run {
                match self.store.add_alias(address, target).await {
                    Ok(()) | Err(StoreError::Conflict(_)) => {}
                    Err(err) => return Err(err.into()),
                }
            }
            self.report.count("aliases");
            return Ok(());
        }

        let existing = self.store.forward_address_targets(address).await?;
        let unchanged = existing.is_some_and(|existing| {
            existing.iter().map(|(target, _)| target.clone()).collect::<BTreeSet<_>>()
                == targets.iter().cloned().collect::<BTreeSet<_>>()
        });
        if !unchanged {
            if !self.dry_run {
                self.store.set_forward_address(address, targets.clone(), "from mailcow").await?;
            }
            self.report.count("forwarding addresses");
        }
        Ok(())
    }

    async fn local_account(&self, address: &str) -> anyhow::Result<bool> {
        Ok(self.created.contains(address)
            || self.store.account(address).await?.is_some_and(|account| account.deleted_at.is_none()))
    }

    async fn forwarding(&mut self, login: &str, targets: &[String], keep_copy: bool) -> anyhow::Result<()> {
        if targets.is_empty() || !self.created.contains(login) {
            return Ok(());
        }
        let Some(account) = self.store.account(login).await? else {
            // In a dry run the person does not exist yet.
            self.report.count("forwardings");
            return Ok(());
        };
        let existing = self.store.forwarding(account.id).await?;
        let mut added = false;
        for target in targets {
            if existing.targets.iter().any(|known| &known.address == target) {
                continue;
            }
            added = true;
            if self.dry_run {
                continue;
            }
            match self.store.add_forward_target(account.id, target, true).await {
                // Set up by an admin in mailcow already: no confirmation mail.
                Ok((_, Some(token))) => {
                    self.store.confirm_forward_link(&token).await?;
                }
                Ok((_, None)) => {}
                Err(err) => self.report.note(format!("{login} could not forward to {target}: {err}")),
            }
        }
        if added {
            if !self.dry_run {
                self.store.set_forward_keep_copy(account.id, keep_copy).await?;
            }
            self.report.count("forwardings");
        }
        Ok(())
    }

    async fn app_password(&mut self, login: &str, name: &str, hash: &str, scopes: Vec<AppScope>) -> anyhow::Result<()> {
        if scopes.is_empty() || !self.created.contains(login) {
            return Ok(());
        }
        if !self.dry_run {
            let Some(account) = self.store.account(login).await? else { return Ok(()) };
            if let Err(err) = self.store.import_app_password(account.id, name, hash, scopes).await {
                self.report.note(format!("app password \"{name}\" of {login}: {err}"));
                return Ok(());
            }
        }
        self.report.count("app passwords");
        Ok(())
    }

    async fn send_as(&mut self, login: &str, domains: BTreeSet<String>) -> anyhow::Result<()> {
        if !self.created.contains(login) {
            return Ok(());
        }
        let mut wanted = Vec::new();
        for domain in domains {
            if self.store.domain(&domain).await?.is_some() || self.dry_run {
                wanted.push(domain);
            } else {
                self.report.note(format!("{login} may send as {domain} in mailcow, which is not hosted here yet"));
            }
        }
        if wanted.is_empty() {
            return Ok(());
        }
        if !self.dry_run {
            let Some(account) = self.store.account(login).await? else { return Ok(()) };
            let mut all = self.store.send_as_domains(account.id).await?;
            all.extend(wanted);
            self.store.set_send_as_domains(account.id, all).await?;
        }
        self.report.count("send-as rights");
        Ok(())
    }

    async fn filter(&mut self, object: &str, option: &str, value: &str) -> anyhow::Result<()> {
        if object.contains('@') && !self.created.contains(object) {
            return Ok(());
        }
        let scope = if object.contains('@') {
            match self.store.account(object).await? {
                Some(account) => Some(ListScope::Account(account.id)),
                None if self.dry_run => None,
                None => return Ok(()),
            }
        } else {
            match self.store.domain(object).await? {
                Some(domain) => Some(ListScope::Domain(domain.id)),
                None if self.dry_run => None,
                None => return Ok(()),
            }
        };
        match option {
            "whitelist_from" | "blacklist_from" => {
                let list = if option == "whitelist_from" { SenderList::Allow } else { SenderList::Block };
                if let Some(scope) = scope {
                    let entry = NewSenderListEntry {
                        scope,
                        list,
                        kind: None,
                        value: value.to_owned(),
                        note: "from mailcow".into(),
                        created_by: "import".into(),
                    };
                    match self.store.add_sender_list_entry(entry).await {
                        Ok(_) => {}
                        Err(StoreError::Conflict(_)) | Err(StoreError::Rule { code: "senderListed", .. }) => {
                            return Ok(());
                        }
                        Err(err) => {
                            self.report.note(format!("{value} for {object} could not be listed: {err}"));
                            return Ok(());
                        }
                    }
                } else {
                    uwumail_store::normalize_sender(uwumail_store::guess_sender_kind(value), value)
                        .map_err(|err| self.report.note(format!("{value} for {object} could not be listed: {err}")))
                        .ok();
                }
                self.report.count("allowed and blocked senders");
            }
            "lowspamlevel" | "highspamlevel" => {
                let Ok(points) = value.trim().parse::<f32>() else {
                    self.report.note(format!("{object}: {option} {value} is not a number"));
                    return Ok(());
                };
                let Some(ListScope::Account(account_id)) = scope else {
                    if object.contains('@') {
                        self.report.count("spam limits");
                    } else {
                        self.report
                            .note(format!("{object} has its own spam limits in mailcow; UwUMail has them per person"));
                    }
                    return Ok(());
                };
                let mut limits = self.store.spam_limits(account_id).await?;
                if option == "lowspamlevel" {
                    limits.junk = Some(points);
                } else {
                    limits.reject = Some(points);
                }
                if let Err(err) = self.store.set_spam_limits(account_id, limits).await {
                    // The other limit may not be there yet; order does not matter once both are.
                    if !matches!(err, StoreError::Rule { .. }) {
                        self.report.note(format!("spam limits of {object}: {err}"));
                    }
                    let partial = SpamLimits { junk: limits.junk, reject: None };
                    let _ = self.store.set_spam_limits(account_id, partial).await;
                }
                self.report.count("spam limits");
            }
            other => self.report.note(format!("{object}: the mailcow filter setting {other} was left out")),
        }
        Ok(())
    }

    /// The calendar or address book a SOGo folder goes into: the person's default one for SOGo's
    /// "personal" folder, otherwise one of its own.
    async fn collection(
        &mut self,
        owner: &str,
        path: Option<&str>,
        display: Option<&str>,
        kind: &str,
    ) -> anyhow::Result<Option<(i64, i64, DavKind)>> {
        let kind = match kind {
            "Appointment" => DavKind::Calendar,
            "Contact" => DavKind::Addressbook,
            _ => return Ok(None),
        };
        if !self.created.contains(owner) {
            return Ok(None);
        }
        let Some(account) = self.store.account(owner).await? else {
            if !self.dry_run {
                return Ok(None);
            }
            // The mailbox only exists in a dry run's imagination: count what would land in it.
            self.report.count(if kind == DavKind::Calendar { "calendars" } else { "address books" });
            return Ok(Some((0, 0, kind)));
        };
        let default = self.dav.default_collection(kind);
        let slug = match path {
            None | Some("personal") => default.slug.clone(),
            Some(path) => url_segment(path),
        };
        let existing = self.store.dav_collections(account.id, kind, default.clone()).await?;
        if let Some(found) = existing.iter().find(|collection| collection.slug == slug) {
            return Ok(Some((account.id, found.id, kind)));
        }
        self.report.count(if kind == DavKind::Calendar { "calendars" } else { "address books" });
        if self.dry_run {
            return Ok(None);
        }
        let new = NewDavCollection {
            slug,
            display_name: display.filter(|name| !name.trim().is_empty()).unwrap_or(&default.display_name).to_owned(),
            components: default.components.clone(),
            color: default.color.clone(),
            ..Default::default()
        };
        let created = self.store.dav_create_collection(account.id, kind, new).await?;
        Ok(Some((account.id, created.id, kind)))
    }

    async fn dav_object(
        &mut self,
        (account_id, collection_id, kind): (i64, i64, DavKind),
        name: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let name = url_segment(name);
        let checked = match kind {
            DavKind::Calendar => {
                let allowed = self.dav.default_collection(kind).components;
                uwumail_dav::objects::check_calendar(content, &allowed)
            }
            DavKind::Addressbook => uwumail_dav::objects::check_contact(content, &name),
        };
        let checked = match checked {
            Ok(checked) => checked,
            Err(err) => {
                self.report.note(format!("{name} could not be taken over: {err:?}"));
                return Ok(());
            }
        };
        if self.dry_run {
            self.report.count("calendar entries and contacts");
            return Ok(());
        }
        let write = DavWrite {
            name: name.clone(),
            content: content.to_owned(),
            uid: checked.uid,
            component: checked.component,
            starts_at: checked.starts_at,
            ends_at: checked.ends_at,
        };
        match self.store.dav_put(account_id, collection_id, write, DavPrecondition::default()).await? {
            DavWriteOutcome::Created { .. } => self.report.count("calendar entries and contacts"),
            DavWriteOutcome::Updated { .. } => {}
            other => self.report.note(format!("{name} could not be taken over: {other:?}")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use uwumail_store::{MailAuth, SenderKind};

    use super::*;

    fn bcrypt(secret: &str) -> String {
        let parts = bcrypt::hash_with_result(secret, 4).unwrap();
        format!("{{BLF-CRYPT}}{}", parts.format_for_version(bcrypt::Version::TwoY))
    }

    fn dkim_pem() -> String {
        let key = uwumail_smtp::dkim::generate_keys("202609").unwrap().remove(0);
        let body = base64::engine::general_purpose::STANDARD.encode(&key.private_key);
        format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n")
    }

    /// An export like the script writes, with made-up people.
    fn export() -> String {
        let event = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:treffen-1\r\n\
            DTSTAMP:20260901T100000Z\r\nDTSTART:20260920T100000Z\r\nDTEND:20260920T110000Z\r\nSUMMARY:Treffen\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:erika-1\r\nFN:Erika Beispiel\r\nEND:VCARD\r\n";
        let lines = [
            json!({ "type": "domain", "domain": "example.de", "active": 1 }),
            json!({ "type": "domain", "domain": "verein.de", "active": 1 }),
            json!({ "type": "domain", "domain": "alt.de", "active": 0 }),
            json!({ "type": "mailbox", "username": "mini@example.de", "name": "Mini",
                    "password": bcrypt("katzenpfote-123"), "quota": 0, "active": 1, "domain": "example.de" }),
            json!({ "type": "mailbox", "username": "leni@verein.de", "name": null,
                    "password": bcrypt("seifenblase-99"), "quota": 1048576, "active": 2, "domain": "verein.de" }),
            json!({ "type": "alias", "address": "kontakt@example.de", "goto": "mini@example.de", "active": 1 }),
            json!({ "type": "alias", "address": "mini@example.de", "goto": "mini@example.de,oma@example.org",
                    "active": 1 }),
            json!({ "type": "alias", "address": "@verein.de", "goto": "leni@verein.de", "active": 1 }),
            json!({ "type": "alias", "address": "kasse@verein.de", "goto": "kassenwart@example.org", "active": 1 }),
            json!({ "type": "alias", "address": "spam@example.de", "goto": "null@localhost", "active": 1 }),
            json!({ "type": "appPassword", "mailbox": "mini@example.de", "name": "Telefon",
                    "password": bcrypt("altes-app-pw"), "active": 1, "imap": 1, "smtp": 1, "dav": 0 }),
            json!({ "type": "senderAcl", "loggedInAs": "mini@example.de", "sendAs": "@verein.de", "external": 0 }),
            json!({ "type": "filter", "object": "mini@example.de", "option": "blacklist_from",
                    "value": "*@werbung.example" }),
            json!({ "type": "filter", "object": "mini@example.de", "option": "whitelist_from",
                    "value": "oma@example.org" }),
            json!({ "type": "filter", "object": "mini@example.de", "option": "highspamlevel", "value": "20" }),
            json!({ "type": "filter", "object": "mini@example.de", "option": "lowspamlevel", "value": "8" }),
            json!({ "type": "dkim", "domain": "example.de", "selector": "dkim", "privateKey": dkim_pem() }),
            json!({ "type": "davFolder", "id": 7, "owner": "mini@example.de", "path": "personal",
                    "name": "Persönlich", "kind": "Appointment" }),
            json!({ "type": "davFolder", "id": 8, "owner": "mini@example.de", "path": "family", "name": "Familie",
                    "kind": "Contact" }),
            json!({ "type": "davObject", "folder": 7, "name": "treffen-1.ics", "content": event }),
            json!({ "type": "davObject", "folder": 8, "name": "erika 1.vcf", "content": card }),
            json!({ "type": "somethingNew", "value": 1 }),
        ];
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    fn dav(store: &Store) -> uwumail_dav::Dav {
        uwumail_dav::Dav::new(
            store.clone(),
            uwumail_dav::DavSettings { calendar_name: "Kalender".into(), addressbook_name: "Kontakte".into() },
        )
    }

    #[tokio::test]
    async fn a_mailcow_export_is_taken_over_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("data")).await.unwrap();
        let file = dir.path().join("export.jsonl");
        std::fs::write(&file, export()).unwrap();

        mailcow(&store, dav(&store), &file, &[], true).await.unwrap();
        assert!(store.domains().await.unwrap().is_empty(), "a dry run changes nothing");
        let entries: Vec<Entry> = export().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let mut dry = Importer {
            store: &store,
            dav: dav(&store),
            dry_run: true,
            report: Report::default(),
            created: BTreeSet::new(),
            keyed: BTreeSet::new(),
        };
        dry.run(&entries, &["example.de".into(), "verein.de".into()].into()).await.unwrap();
        let missing: Vec<_> = dry.report.notes.iter().filter(|note| note.contains("no DKIM key")).collect();
        assert_eq!(missing, ["verein.de had no DKIM key in mailcow: new keys need DNS records"]);
        assert_eq!(dry.report.counts.get("calendar entries and contacts"), Some(&2), "a dry run counts DAV objects");

        mailcow(&store, dav(&store), &file, &[], false).await.unwrap();
        let names: Vec<_> = store.domains().await.unwrap().into_iter().map(|domain| domain.name).collect();
        assert_eq!(names, ["example.de", "verein.de"], "inactive domains stay behind");

        let mini = store.account("mini@example.de").await.unwrap().unwrap();
        assert!(store.authenticate("mini@example.de", "katzenpfote-123").await.unwrap().is_some());
        let leni = store.account("leni@verein.de").await.unwrap().unwrap();
        assert!(leni.disabled && leni.quota_bytes == 1048576, "mailcow's login lock stays");
        let app = store.authenticate_mail("mini@example.de", "altes-app-pw", AppScope::Smtp, "smtp", "").await;
        assert!(matches!(app.unwrap(), MailAuth::Ok { app_password: Some(_), .. }));

        assert_eq!(store.resolve_recipient("kontakt@example.de").await.unwrap(), Some(mini.id));
        assert_eq!(store.resolve_recipient("irgendwer@verein.de").await.unwrap(), Some(leni.id), "catch-all");
        let kasse = store.forward_address_targets("kasse@verein.de").await.unwrap().unwrap();
        assert_eq!(kasse, [("kassenwart@example.org".to_owned(), None)]);
        let forwarding = store.forwarding(mini.id).await.unwrap();
        assert!(forwarding.keep_copy);
        assert_eq!(forwarding.targets.len(), 1);
        assert!(forwarding.targets[0].confirmed_at.is_some(), "no confirmation mail for what mailcow already did");

        assert!(store.account_owns_address(mini.id, "vorstand@verein.de").await.unwrap());
        let senders = store.sender_list(ListScope::Account(mini.id)).await.unwrap();
        assert!(senders.iter().any(|entry| entry.kind == SenderKind::Pattern && entry.list == SenderList::Block));
        assert_eq!(senders.len(), 2);
        let limits = store.spam_limits(mini.id).await.unwrap();
        assert_eq!((limits.junk, limits.reject), (Some(8.0), Some(20.0)));
        let keys = store.dkim_keys("example.de").await.unwrap();
        assert_eq!(keys.iter().map(|key| key.selector.as_str()).collect::<Vec<_>>(), ["dkim"]);

        let calendars = store.dav_collections(mini.id, DavKind::Calendar, NewDavCollection::default()).await.unwrap();
        assert_eq!((calendars.len(), calendars[0].slug.as_str(), calendars[0].resources), (1, "personal", 1));
        let books = store.dav_collections(mini.id, DavKind::Addressbook, NewDavCollection::default()).await.unwrap();
        let family = books.iter().find(|book| book.slug == "family").unwrap();
        assert_eq!((family.display_name.as_str(), family.resources), ("Familie", 1));
        let cards = store.dav_resource_contents(mini.id, family.id, None).await.unwrap();
        assert_eq!(cards[0].info.name, "erika-1.vcf");

        // Again: nothing doubles, and what people changed here stays.
        store.set_spam_limits(mini.id, SpamLimits { junk: Some(6.0), reject: None }).await.unwrap();
        mailcow(&store, dav(&store), &file, &["example.de".into()], false).await.unwrap();
        assert_eq!(store.forwarding(mini.id).await.unwrap().targets.len(), 1);
        assert_eq!(store.spam_limits(mini.id).await.unwrap().junk, Some(6.0), "their own change stays");
        assert_eq!(store.sender_list(ListScope::Account(mini.id)).await.unwrap().len(), 2);
        assert_eq!(store.dkim_keys("example.de").await.unwrap().len(), 1);
        assert!(mailcow(&store, dav(&store), &file, &["unbekannt.de".into()], false).await.is_err());
    }
}
