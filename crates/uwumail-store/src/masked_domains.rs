//! Masked-only domains and the policy of who may make masked addresses where
//! (docs/jmap-masked-email.md).
//!
//! A domain is a mail domain or a masked-only domain. A masked-only domain carries masked
//! addresses and nothing else: every way to put a person, an alias, a group, a forwarding address
//! or a catch-all on a domain asks [`ensure_mail_domain`] first.
//!
//! Each mail domain says where its users (the accounts whose login is on it) may make masked
//! addresses: on the domain itself, on some masked-only domains, both, or nowhere. An admin can
//! set any part of that differently for one account. Only new masked addresses follow the policy;
//! the ones already made keep working whatever it says later.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::address::normalize_domain;
use crate::directory::domain_id;
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DomainKind {
    /// People, aliases, groups and everything else, as always.
    #[default]
    Mail,
    /// Only masked addresses.
    Masked,
}

impl DomainKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DomainKind::Mail => "mail",
            DomainKind::Masked => "masked",
        }
    }

    pub fn parse(value: &str) -> Option<DomainKind> {
        match value {
            "mail" => Some(DomainKind::Mail),
            "masked" => Some(DomainKind::Masked),
            _ => None,
        }
    }
}

/// Where the users of a domain may make masked addresses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaskedMode {
    #[default]
    Off,
    /// On their own domain.
    Own,
    /// On the masked-only domains the policy names.
    Dedicated,
    Both,
}

impl MaskedMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MaskedMode::Off => "off",
            MaskedMode::Own => "own",
            MaskedMode::Dedicated => "dedicated",
            MaskedMode::Both => "both",
        }
    }

    pub fn parse(value: &str) -> Option<MaskedMode> {
        match value {
            "off" => Some(MaskedMode::Off),
            "own" => Some(MaskedMode::Own),
            "dedicated" => Some(MaskedMode::Dedicated),
            "both" => Some(MaskedMode::Both),
            _ => None,
        }
    }

    fn own(self) -> bool {
        matches!(self, MaskedMode::Own | MaskedMode::Both)
    }

    fn dedicated(self) -> bool {
        matches!(self, MaskedMode::Dedicated | MaskedMode::Both)
    }
}

/// The policy of a mail domain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainMaskedPolicy {
    pub mode: MaskedMode,
    /// The masked-only domains its users may use, by name.
    pub masked_domains: Vec<String>,
    /// The domain taken when none is named; `None` picks one by itself.
    pub default_domain: Option<String>,
}

/// What an admin set differently for one account; `None` is "as the domain", part by part.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountMaskedPolicy {
    pub mode: Option<MaskedMode>,
    pub masked_domains: Option<Vec<String>>,
    pub default_domain: Option<String>,
}

/// What holds for an account: its own settings where there are any, its domain's otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveMaskedPolicy {
    pub mode: MaskedMode,
    pub masked_domains: Vec<String>,
    /// Where the account may make masked addresses, by name. Empty: nowhere.
    pub domains: Vec<String>,
    /// Where a new one goes when none is named: always one of `domains`, or `None` without any.
    pub default_domain: Option<String>,
}

/// What keeps a mail domain from becoming masked-only. Accounts in the trash count, as they can
/// come back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindBlockers {
    /// Accounts whose login is on the domain: people, services and shared mailboxes.
    pub accounts: i64,
    pub aliases: i64,
    pub groups: i64,
    pub forwards: i64,
    pub catch_all: bool,
    /// Accounts that may send as any address of the domain.
    pub send_as: i64,
}

impl KindBlockers {
    pub fn is_empty(&self) -> bool {
        *self == KindBlockers::default()
    }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (count, what) in [
            (self.accounts, "accounts"),
            (self.aliases, "aliases"),
            (self.groups, "groups"),
            (self.forwards, "forwarding addresses"),
            (self.send_as, "people who may send as any of its addresses"),
        ] {
            if count > 0 {
                parts.push(format!("{count} {what}"));
            }
        }
        if self.catch_all {
            parts.push("a catch-all".into());
        }
        parts.join(", ")
    }
}

/// What changing a domain's kind did besides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindChange {
    pub kind: DomainKind,
    /// Mail domains whose policy named the domain, which no longer does (masked-only → mail).
    pub removed_from_domains: Vec<String>,
    /// Accounts whose own policy named the domain, which no longer does (masked-only → mail).
    pub removed_from_accounts: Vec<String>,
}

/// A number that goes up with every write that can change where anyone may make masked addresses:
/// a domain's kind or policy, an account's own policy, a domain going away. Clients see it in the
/// JMAP session state instead of a hash over each account's policy, which would cost several
/// queries on every request.
const VERSION_KEY: &str = "maskedPolicyVersion";

pub(crate) fn bump_version(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, '1')
         ON CONFLICT (key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [VERSION_KEY],
    )?;
    Ok(())
}

fn rule(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Rule { code, message: message.into() }
}

fn kind_of(conn: &Connection, domain_id: i64) -> Result<(String, DomainKind)> {
    conn.query_row("SELECT name, kind FROM domains WHERE id = ?1", [domain_id], |row| {
        Ok((row.get::<_, String>(0)?, DomainKind::parse(&row.get::<_, String>(1)?).unwrap_or_default()))
    })
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("domain {domain_id}")))
}

/// Refuses anything but a masked address on a masked-only domain.
pub(crate) fn ensure_mail_domain(conn: &Connection, domain_id: i64) -> Result<()> {
    let (name, kind) = kind_of(conn, domain_id)?;
    if kind == DomainKind::Masked {
        return Err(rule("maskedOnlyDomain", format!("{name} only carries masked addresses")));
    }
    Ok(())
}

/// The ids of masked-only domains, by name; anything else is refused.
fn masked_domain_ids(conn: &Connection, names: &[String]) -> Result<Vec<(i64, String)>> {
    let mut ids: Vec<(i64, String)> = Vec::new();
    for name in names {
        let name = normalize_domain(name)?;
        let id = domain_id(conn, &name)?;
        if kind_of(conn, id)?.1 != DomainKind::Masked {
            return Err(rule("notMaskedDomain", format!("{name} is not a domain for masked addresses only")));
        }
        if !ids.iter().any(|(known, _)| *known == id) {
            ids.push((id, name));
        }
    }
    ids.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(ids)
}

fn names_of(conn: &Connection, sql: &str, id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn domain_list(conn: &Connection, domain_id: i64) -> Result<Vec<String>> {
    names_of(
        conn,
        "SELECT d.name FROM domain_masked_domains j JOIN domains d ON d.id = j.masked_domain_id
         WHERE j.domain_id = ?1 AND d.kind = 'masked' ORDER BY d.name",
        domain_id,
    )
}

fn account_list(conn: &Connection, account_id: i64) -> Result<Vec<String>> {
    names_of(
        conn,
        "SELECT d.name FROM account_masked_domains j JOIN domains d ON d.id = j.masked_domain_id
         WHERE j.account_id = ?1 AND d.kind = 'masked' ORDER BY d.name",
        account_id,
    )
}

fn load_domain_policy(conn: &Connection, domain_id: i64) -> Result<DomainMaskedPolicy> {
    let (mode, default_domain): (String, Option<String>) = conn.query_row(
        "SELECT d.masked_mode, x.name FROM domains d LEFT JOIN domains x ON x.id = d.masked_default_domain_id
         WHERE d.id = ?1",
        [domain_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(DomainMaskedPolicy {
        mode: MaskedMode::parse(&mode).unwrap_or_default(),
        masked_domains: domain_list(conn, domain_id)?,
        default_domain,
    })
}

fn load_account_policy(conn: &Connection, account_id: i64) -> Result<AccountMaskedPolicy> {
    let (mode, custom, default_domain): (Option<String>, bool, Option<String>) = conn
        .query_row(
            "SELECT a.masked_mode, a.masked_domains_custom, x.name FROM accounts a
             LEFT JOIN domains x ON x.id = a.masked_default_domain_id WHERE a.id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
    Ok(AccountMaskedPolicy {
        mode: mode.as_deref().and_then(MaskedMode::parse),
        masked_domains: if custom { Some(account_list(conn, account_id)?) } else { None },
        default_domain,
    })
}

/// The domains a policy allows: the own domain for `own` and `both` (when it is a mail domain),
/// the named masked-only domains for `dedicated` and `both`. Sorted by name.
fn allowed(mode: MaskedMode, own: Option<&str>, masked_domains: &[String]) -> Vec<String> {
    let mut domains: Vec<String> = Vec::new();
    if mode.own()
        && let Some(own) = own
    {
        domains.push(own.to_owned());
    }
    if mode.dedicated() {
        domains.extend(masked_domains.iter().filter(|name| Some(name.as_str()) != own).cloned());
    }
    domains.sort();
    domains.dedup();
    domains
}

/// The first stored default that is still allowed (the account's own, then its domain's);
/// otherwise the own domain when allowed, else the first allowed one by name.
fn pick_default(domains: &[String], own: Option<&str>, stored: &[Option<&str>]) -> Option<String> {
    let allowed = |name: &&str| domains.iter().any(|domain| domain == name);
    stored
        .iter()
        .flatten()
        .copied()
        .find(allowed)
        .or(own.filter(allowed))
        .or(domains.first().map(String::as_str))
        .map(str::to_owned)
}

/// The login domain of an account, when it is a mail domain here, with its id.
fn own_domain(conn: &Connection, account_id: i64) -> Result<Option<(i64, String)>> {
    let login: String = conn
        .query_row("SELECT login FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
    let name = login.rsplit_once('@').map(|(_, domain)| domain.to_owned()).unwrap_or_default();
    Ok(conn
        .query_row("SELECT id FROM domains WHERE name = ?1 AND kind = 'mail'", [&name], |row| row.get(0))
        .optional()?
        .map(|id| (id, name)))
}

fn resolve(
    own: Option<(i64, String)>,
    domain: DomainMaskedPolicy,
    person: AccountMaskedPolicy,
) -> EffectiveMaskedPolicy {
    let own = own.map(|(_, name)| name);
    let mode = person.mode.unwrap_or(domain.mode);
    let masked_domains = person.masked_domains.unwrap_or(domain.masked_domains);
    let domains = allowed(mode, own.as_deref(), &masked_domains);
    let stored = [person.default_domain.as_deref(), domain.default_domain.as_deref()];
    let default_domain = pick_default(&domains, own.as_deref(), &stored);
    EffectiveMaskedPolicy { mode, masked_domains, domains, default_domain }
}

/// What holds for an account.
pub(crate) fn effective(conn: &Connection, account_id: i64) -> Result<EffectiveMaskedPolicy> {
    let own = own_domain(conn, account_id)?;
    let domain = match &own {
        Some((id, _)) => load_domain_policy(conn, *id)?,
        None => DomainMaskedPolicy::default(),
    };
    Ok(resolve(own, domain, load_account_policy(conn, account_id)?))
}

/// The mail domains and the accounts whose policy names a masked-only domain.
fn users(conn: &Connection, domain_id: i64) -> Result<(Vec<String>, Vec<String>)> {
    let domains = names_of(
        conn,
        "SELECT d.name FROM domains d WHERE d.id IN (
             SELECT domain_id FROM domain_masked_domains WHERE masked_domain_id = ?1
             UNION SELECT id FROM domains WHERE masked_default_domain_id = ?1)
           AND d.id <> ?1
         ORDER BY d.name",
        domain_id,
    )?;
    let accounts = names_of(
        conn,
        "SELECT a.login FROM accounts a WHERE a.id IN (
             SELECT account_id FROM account_masked_domains WHERE masked_domain_id = ?1
             UNION SELECT id FROM accounts WHERE masked_default_domain_id = ?1)
         ORDER BY a.login",
        domain_id,
    )?;
    Ok((domains, accounts))
}

fn blockers(conn: &Connection, domain_id: i64, name: &str) -> Result<KindBlockers> {
    Ok(conn.query_row(
        "SELECT (SELECT count(*) FROM accounts WHERE substr(login, instr(login, '@') + 1) = ?2),
                (SELECT count(*) FROM addresses WHERE domain_id = ?1 AND kind = 'alias'),
                (SELECT count(*) FROM groups WHERE domain_id = ?1),
                (SELECT count(*) FROM forward_addresses WHERE domain_id = ?1),
                (SELECT catch_all_account_id IS NOT NULL FROM domains WHERE id = ?1),
                (SELECT count(*) FROM send_as_domains WHERE domain_id = ?1)",
        params![domain_id, name],
        |row| {
            Ok(KindBlockers {
                accounts: row.get(0)?,
                aliases: row.get(1)?,
                groups: row.get(2)?,
                forwards: row.get(3)?,
                catch_all: row.get(4)?,
                send_as: row.get(5)?,
            })
        },
    )?)
}

impl Store {
    pub async fn domain_kind(&self, domain: &str) -> Result<DomainKind> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| Ok(kind_of(conn, domain_id(conn, &domain)?)?.1)).await
    }

    /// Mail domains that hold masked addresses nobody can make more of: open to their own people
    /// ('own' or 'both') while nobody's login is on them. Upgrading to 0.16.0 left a domain that only
    /// ever carried masked addresses like this, for every user of other domains to lose it
    /// (security-audit-0.16.0 MD-1); the health overview names them, with how many addresses each
    /// holds, until the admin makes them masked-only and chooses them for the mail domains.
    pub async fn stranded_masked_domains(&self) -> Result<Vec<(String, i64)>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT d.name, count(*) FROM domains d
                 JOIN masked_addresses x ON x.domain_id = d.id AND x.state <> 'deleted'
                 WHERE d.kind = 'mail' AND d.masked_mode IN ('own', 'both')
                   AND NOT EXISTS (SELECT 1 FROM accounts a
                                   WHERE a.deleted_at IS NULL AND substr(a.login, instr(a.login, '@') + 1) = d.name)
                 GROUP BY d.id ORDER BY d.name",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// What keeps a mail domain from becoming masked-only; empty when nothing does.
    pub async fn domain_kind_blockers(&self, domain: &str) -> Result<KindBlockers> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| blockers(conn, domain_id(conn, &domain)?, &domain)).await
    }

    /// The mail domains and the accounts whose policy names a masked-only domain: what turning
    /// it back into a mail domain takes it out of.
    pub async fn masked_domain_users(&self, domain: &str) -> Result<(Vec<String>, Vec<String>)> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| users(conn, domain_id(conn, &domain)?)).await
    }

    /// Makes a mail domain masked-only, or a masked-only domain a mail domain again.
    ///
    /// A mail domain becomes masked-only only while nothing but masked addresses is on it (see
    /// [`KindBlockers`]); its own policy is reset, and aliases people make themselves are closed
    /// there. A masked-only domain can always become a mail domain; every policy that named it
    /// stops doing so.
    pub async fn set_domain_kind(&self, domain: &str, kind: DomainKind) -> Result<KindChange> {
        let domain = normalize_domain(domain)?;
        self.write(move |tx| {
            let id = domain_id(tx, &domain)?;
            let mut change = KindChange { kind, ..Default::default() };
            if kind_of(tx, id)?.1 == kind {
                return Ok(change);
            }
            match kind {
                DomainKind::Masked => {
                    let blockers = blockers(tx, id, &domain)?;
                    if !blockers.is_empty() {
                        return Err(rule(
                            "kindChangeBlocked",
                            format!("{domain} still has {}; remove them first", blockers.describe()),
                        ));
                    }
                    tx.execute(
                        "UPDATE domains SET kind = 'masked', masked_mode = 'off', masked_default_domain_id = NULL,
                             self_service_aliases = 0
                         WHERE id = ?1",
                        [id],
                    )?;
                    tx.execute("DELETE FROM domain_masked_domains WHERE domain_id = ?1", [id])?;
                    tx.execute(
                        "UPDATE domains SET masked_default_domain_id = NULL WHERE masked_default_domain_id = ?1",
                        [id],
                    )?;
                    tx.execute(
                        "UPDATE accounts SET masked_default_domain_id = NULL WHERE masked_default_domain_id = ?1",
                        [id],
                    )?;
                }
                DomainKind::Mail => {
                    (change.removed_from_domains, change.removed_from_accounts) = users(tx, id)?;
                    tx.execute("DELETE FROM domain_masked_domains WHERE masked_domain_id = ?1", [id])?;
                    tx.execute("DELETE FROM account_masked_domains WHERE masked_domain_id = ?1", [id])?;
                    tx.execute(
                        "UPDATE domains SET masked_default_domain_id = NULL WHERE masked_default_domain_id = ?1",
                        [id],
                    )?;
                    tx.execute(
                        "UPDATE accounts SET masked_default_domain_id = NULL WHERE masked_default_domain_id = ?1",
                        [id],
                    )?;
                    tx.execute("UPDATE domains SET kind = 'mail' WHERE id = ?1", [id])?;
                }
            }
            bump_version(tx)?;
            Ok(change)
        })
        .await
    }

    pub async fn domain_masked_policy(&self, domain: &str) -> Result<DomainMaskedPolicy> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| load_domain_policy(conn, domain_id(conn, &domain)?)).await
    }

    /// Sets where the users of a mail domain may make masked addresses. The masked domains have to
    /// be masked-only, and the default one of the domains the policy allows.
    pub async fn set_domain_masked_policy(
        &self,
        domain: &str,
        policy: DomainMaskedPolicy,
    ) -> Result<DomainMaskedPolicy> {
        let domain = normalize_domain(domain)?;
        let wanted_default = policy.default_domain.as_deref().map(normalize_domain).transpose()?;
        self.write(move |tx| {
            let id = domain_id(tx, &domain)?;
            ensure_mail_domain(tx, id)?;
            let masked = masked_domain_ids(tx, &policy.masked_domains)?;
            let names: Vec<String> = masked.iter().map(|(_, name)| name.clone()).collect();
            let default_id = match &wanted_default {
                Some(wanted) => {
                    if !allowed(policy.mode, Some(&domain), &names).contains(wanted) {
                        return Err(rule("maskedDefault", format!("{wanted} is not one of the allowed domains")));
                    }
                    Some(domain_id(tx, wanted)?)
                }
                None => None,
            };
            tx.execute(
                "UPDATE domains SET masked_mode = ?1, masked_default_domain_id = ?2 WHERE id = ?3",
                params![policy.mode.as_str(), default_id, id],
            )?;
            tx.execute("DELETE FROM domain_masked_domains WHERE domain_id = ?1", [id])?;
            for (masked_id, _) in &masked {
                tx.execute(
                    "INSERT INTO domain_masked_domains (domain_id, masked_domain_id) VALUES (?1, ?2)",
                    params![id, masked_id],
                )?;
            }
            bump_version(tx)?;
            load_domain_policy(tx, id)
        })
        .await
    }

    /// What an admin set differently for one account.
    pub async fn account_masked_policy(&self, account_id: i64) -> Result<AccountMaskedPolicy> {
        self.read(move |conn| load_account_policy(conn, account_id)).await
    }

    /// Sets an account's own policy, part by part; `None` goes back to what its domain says. A
    /// default has to be one of the domains the account may use with it.
    pub async fn set_account_masked_policy(
        &self,
        account_id: i64,
        policy: AccountMaskedPolicy,
    ) -> Result<AccountMaskedPolicy> {
        let wanted_default = policy.default_domain.as_deref().map(normalize_domain).transpose()?;
        self.write(move |tx| {
            let masked = policy.masked_domains.as_deref().map(|names| masked_domain_ids(tx, names)).transpose()?;
            let own = own_domain(tx, account_id)?;
            let default_id = match &wanted_default {
                Some(wanted) => {
                    let domain = match &own {
                        Some((id, _)) => load_domain_policy(tx, *id)?,
                        None => DomainMaskedPolicy::default(),
                    };
                    let mode = policy.mode.unwrap_or(domain.mode);
                    let names = match &masked {
                        Some(masked) => masked.iter().map(|(_, name)| name.clone()).collect(),
                        None => domain.masked_domains,
                    };
                    let own_name = own.as_ref().map(|(_, name)| name.as_str());
                    if !allowed(mode, own_name, &names).contains(wanted) {
                        return Err(rule("maskedDefault", format!("{wanted} is not one of the allowed domains")));
                    }
                    Some(domain_id(tx, wanted)?)
                }
                None => None,
            };
            tx.execute(
                "UPDATE accounts SET masked_mode = ?1, masked_domains_custom = ?2, masked_default_domain_id = ?3
                 WHERE id = ?4",
                params![policy.mode.map(MaskedMode::as_str), masked.is_some(), default_id, account_id],
            )?;
            tx.execute("DELETE FROM account_masked_domains WHERE account_id = ?1", [account_id])?;
            for (masked_id, _) in masked.iter().flatten() {
                tx.execute(
                    "INSERT INTO account_masked_domains (account_id, masked_domain_id) VALUES (?1, ?2)",
                    params![account_id, masked_id],
                )?;
            }
            bump_version(tx)?;
            load_account_policy(tx, account_id)
        })
        .await
    }

    /// Goes up whenever any masked address policy may have changed.
    pub async fn masked_policy_version(&self) -> Result<i64> {
        self.read(|conn| {
            Ok(crate::db::get_setting(conn, VERSION_KEY)?.and_then(|value| value.parse().ok()).unwrap_or(0))
        })
        .await
    }

    /// Where an account may make masked addresses, and where a new one goes by default.
    pub async fn effective_masked_policy(&self, account_id: i64) -> Result<EffectiveMaskedPolicy> {
        self.read(move |conn| effective(conn, account_id)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, NewGroup, NewMaskedAddress, Role, WhoMaySend};

    fn new_account(address: &str, role: Role) -> NewAccount {
        NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: None,
            role,
            quota_bytes: 0,
            protocols: None,
        }
    }

    async fn person(store: &Store, address: &str) -> i64 {
        store.create_account(new_account(address, Role::User)).await.unwrap().id
    }

    fn code<T: std::fmt::Debug>(result: Result<T>) -> &'static str {
        match result {
            Err(StoreError::Rule { code, .. }) => code,
            other => panic!("expected a rule, got {other:?}"),
        }
    }

    fn policy(mode: MaskedMode, masked: &[&str], default: Option<&str>) -> DomainMaskedPolicy {
        DomainMaskedPolicy {
            mode,
            masked_domains: masked.iter().map(|name| name.to_string()).collect(),
            default_domain: default.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn domains_nobody_can_add_masked_addresses_to_are_named() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        store.create_domain("masks.example").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        store.set_domain_masked_policy("example.org", policy(MaskedMode::Own, &[], None)).await.unwrap();
        let masked = store.create_masked_address(mini, NewMaskedAddress::default()).await.unwrap();
        assert!(store.stranded_masked_domains().await.unwrap().is_empty(), "Mini's own domain");

        // As 0.16.0 left a domain that only carried masked addresses of people from elsewhere.
        store
            .write(move |tx| {
                tx.execute(
                    "UPDATE masked_addresses SET domain_id = (SELECT id FROM domains WHERE name = 'masks.example')
                     WHERE id = ?1",
                    [masked.id],
                )?;
                tx.execute("UPDATE domains SET masked_mode = 'own' WHERE name = 'masks.example'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(store.stranded_masked_domains().await.unwrap(), [("masks.example".to_owned(), 1)]);

        // Made masked-only, as the hint says, it is no longer stranded.
        store.set_domain_kind("masks.example", DomainKind::Masked).await.unwrap();
        assert!(store.stranded_masked_domains().await.unwrap().is_empty());
    }

    #[test]
    fn policies_resolve_part_by_part() {
        let own = Some((1, "example.org".to_owned()));
        let domain = policy(MaskedMode::Both, &["b.example", "a.example"], Some("b.example"));
        let effective = resolve(own.clone(), domain.clone(), AccountMaskedPolicy::default());
        assert_eq!(effective.domains, vec!["a.example", "b.example", "example.org"]);
        assert_eq!(effective.default_domain.as_deref(), Some("b.example"));

        // Only the mode differs: the domain's list and default stay, as far as they are allowed.
        let person = AccountMaskedPolicy { mode: Some(MaskedMode::Own), ..Default::default() };
        let effective = resolve(own.clone(), domain.clone(), person);
        assert_eq!(effective.domains, vec!["example.org"]);
        assert_eq!(effective.default_domain.as_deref(), Some("example.org"), "a stale default falls back");

        // Only the list differs.
        let person = AccountMaskedPolicy { masked_domains: Some(vec!["c.example".into()]), ..Default::default() };
        let effective = resolve(own.clone(), domain.clone(), person);
        assert_eq!(effective.domains, vec!["c.example", "example.org"]);
        assert_eq!(effective.default_domain.as_deref(), Some("example.org"), "own domain first");

        // Only the default differs.
        let person = AccountMaskedPolicy { default_domain: Some("a.example".into()), ..Default::default() };
        assert_eq!(resolve(own.clone(), domain.clone(), person).default_domain.as_deref(), Some("a.example"));

        // A person's default that is no longer allowed gives way to the domain's, then to automatic.
        let person = AccountMaskedPolicy {
            masked_domains: Some(vec!["b.example".into()]),
            default_domain: Some("a.example".into()),
            ..Default::default()
        };
        assert_eq!(resolve(own.clone(), domain.clone(), person.clone()).default_domain.as_deref(), Some("b.example"));
        let other_default = policy(MaskedMode::Both, &["b.example"], Some("c.example"));
        assert_eq!(resolve(own.clone(), other_default, person).default_domain.as_deref(), Some("example.org"));

        // Dedicated without an own domain in it: the first masked domain by name.
        let dedicated = policy(MaskedMode::Dedicated, &["b.example", "a.example"], None);
        let effective = resolve(own.clone(), dedicated, AccountMaskedPolicy::default());
        assert_eq!((effective.domains.len(), effective.default_domain.as_deref()), (2, Some("a.example")));

        // Off, or a custom empty list: nowhere.
        let person = AccountMaskedPolicy { mode: Some(MaskedMode::Off), ..Default::default() };
        let effective = resolve(own.clone(), domain.clone(), person);
        assert!(effective.domains.is_empty() && effective.default_domain.is_none());
        let person = AccountMaskedPolicy {
            mode: Some(MaskedMode::Dedicated),
            masked_domains: Some(Vec::new()),
            ..Default::default()
        };
        assert!(resolve(own, domain.clone(), person).domains.is_empty());

        // Without a mail domain of its own, "own" allows nothing.
        let effective = resolve(None, policy(MaskedMode::Own, &[], None), AccountMaskedPolicy::default());
        assert!(effective.domains.is_empty());
    }

    #[tokio::test]
    async fn nothing_but_masked_addresses_on_a_masked_domain() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let domain = store.create_domain_with_kind("masked.example", DomainKind::Masked).await.unwrap();
        assert_eq!(domain.kind, DomainKind::Masked);
        let mini = person(&store, "mini@example.org").await;

        // Every way to put something else there is refused.
        for role in [Role::User, Role::Admin, Role::Service] {
            let refused = store.create_account(new_account("nyu@masked.example", role)).await;
            assert_eq!(code(refused), "maskedOnlyDomain");
        }
        let shared = crate::NewSharedMailbox {
            address: "team@masked.example".into(),
            name: "Team".into(),
            quota_bytes: 0,
            members: Vec::new(),
        };
        assert_eq!(code(store.create_shared_mailbox(shared).await), "maskedOnlyDomain");
        assert_eq!(code(store.add_alias("hello@masked.example", "mini@example.org").await), "maskedOnlyDomain");
        assert_eq!(code(store.set_domain_self_service("masked.example", true).await), "maskedOnlyDomain");
        assert_eq!(code(store.create_own_alias(mini, "hello@masked.example").await), "maskedOnlyDomain");
        let group = NewGroup {
            address: "team@masked.example".into(),
            name: "Team".into(),
            who_may_send: WhoMaySend::Anyone,
            members_may_send_as: false,
            members: vec!["mini@example.org".into()],
        };
        assert_eq!(code(store.create_group(group).await), "maskedOnlyDomain");
        let targets = vec!["someone@example.net".to_owned()];
        assert_eq!(code(store.set_forward_address("fwd@masked.example", targets, "").await), "maskedOnlyDomain");
        assert_eq!(code(store.set_catch_all("masked.example", Some("mini@example.org")).await), "maskedOnlyDomain");
        store.set_catch_all("masked.example", None).await.unwrap();
        assert_eq!(code(store.set_send_as_domains(mini, vec!["masked.example".into()]).await), "maskedOnlyDomain");
        assert_eq!(
            code(store.set_domain_masked_policy("masked.example", DomainMaskedPolicy::default()).await),
            "maskedOnlyDomain"
        );

        // Masked addresses are fine, and only they take mail there.
        let dedicated = policy(MaskedMode::Dedicated, &["masked.example"], None);
        store.set_domain_masked_policy("example.org", dedicated).await.unwrap();
        let masked = store.create_masked_address(mini, NewMaskedAddress::default()).await.unwrap();
        assert!(masked.email.ends_with("@masked.example"));
        assert_eq!(store.resolve_recipient(&masked.email).await.unwrap(), Some(mini));
        assert_eq!(store.resolve_recipient("ghost@masked.example").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_domain_turns_masked_only_when_nothing_else_is_on_it() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        store.create_domain("masked.example").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let nyu = person(&store, "nyu@masked.example").await;
        store.add_alias("hi@masked.example", "mini@example.org").await.unwrap();
        store.set_catch_all("masked.example", Some("mini@example.org")).await.unwrap();
        store.set_send_as_domains(mini, vec!["masked.example".into()]).await.unwrap();
        store.set_forward_address("fwd@masked.example", vec!["someone@example.net".into()], "").await.unwrap();
        let group = NewGroup {
            address: "team@masked.example".into(),
            name: "Team".into(),
            who_may_send: WhoMaySend::Anyone,
            members_may_send_as: false,
            members: vec!["mini@example.org".into()],
        };
        store.create_group(group).await.unwrap();
        store.set_domain_self_service("masked.example", true).await.unwrap();
        let blockers = store.domain_kind_blockers("masked.example").await.unwrap();
        assert_eq!(
            blockers,
            KindBlockers { accounts: 1, aliases: 1, groups: 1, forwards: 1, catch_all: true, send_as: 1 }
        );
        assert_eq!(code(store.set_domain_kind("masked.example", DomainKind::Masked).await), "kindChangeBlocked");

        // Someone in the trash still counts: they could come back.
        store.trash_account("nyu@masked.example").await.unwrap();
        store.remove_alias("hi@masked.example").await.unwrap();
        store.set_catch_all("masked.example", None).await.unwrap();
        store.set_send_as_domains(mini, Vec::new()).await.unwrap();
        store.remove_forward_address("fwd@masked.example").await.unwrap();
        store.delete_group("team@masked.example").await.unwrap();
        assert_eq!(store.domain_kind_blockers("masked.example").await.unwrap().accounts, 1);
        assert_eq!(code(store.set_domain_kind("masked.example", DomainKind::Masked).await), "kindChangeBlocked");
        assert!(store.account_by_id(nyu).await.unwrap().is_some());
        store.delete_account("nyu@masked.example").await.unwrap();
        assert!(store.domain_kind_blockers("masked.example").await.unwrap().is_empty());

        // Its own masked addresses may stay; its own policy goes, and so does self-service.
        store.set_domain_masked_policy("masked.example", policy(MaskedMode::Own, &[], None)).await.unwrap();
        store.set_domain_kind("masked.example", DomainKind::Masked).await.unwrap();
        assert_eq!(store.domain_kind("masked.example").await.unwrap(), DomainKind::Masked);
        assert!(!store.domain_self_service("masked.example").await.unwrap());
        assert_eq!(store.domain_masked_policy("masked.example").await.unwrap(), DomainMaskedPolicy::default());
    }

    #[tokio::test]
    async fn back_to_mail_clears_every_policy_that_named_it() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        store.create_domain("example.net").await.unwrap();
        store.create_domain_with_kind("a.example", DomainKind::Masked).await.unwrap();
        store.create_domain_with_kind("b.example", DomainKind::Masked).await.unwrap();
        let mini = person(&store, "mini@example.net").await;
        let both = policy(MaskedMode::Both, &["a.example", "b.example"], Some("a.example"));
        store.set_domain_masked_policy("example.org", both).await.unwrap();
        let custom = AccountMaskedPolicy {
            mode: Some(MaskedMode::Dedicated),
            masked_domains: Some(vec!["A.example".into()]),
            default_domain: Some("a.example".into()),
        };
        assert_eq!(
            store.set_account_masked_policy(mini, custom).await.unwrap().masked_domains.unwrap(),
            vec!["a.example"]
        );
        let masked = store.create_masked_address(mini, NewMaskedAddress::default()).await.unwrap();
        assert!(masked.email.ends_with("@a.example"));

        let users = store.masked_domain_users("a.example").await.unwrap();
        assert_eq!(users, (vec!["example.org".to_owned()], vec!["mini@example.net".to_owned()]));
        let change = store.set_domain_kind("a.example", DomainKind::Mail).await.unwrap();
        assert_eq!(change.removed_from_domains, vec!["example.org"]);
        assert_eq!(change.removed_from_accounts, vec!["mini@example.net"]);
        let org = store.domain_masked_policy("example.org").await.unwrap();
        assert_eq!(org, policy(MaskedMode::Both, &["b.example"], None));
        let own = store.account_masked_policy(mini).await.unwrap();
        assert_eq!(own.masked_domains, Some(Vec::new()));
        assert_eq!(own.default_domain, None);
        assert!(store.effective_masked_policy(mini).await.unwrap().domains.is_empty());
        // The masked address made there keeps working, and the domain takes people again.
        assert_eq!(store.resolve_recipient(&masked.email).await.unwrap(), Some(mini));
        person(&store, "nyu@a.example").await;
        assert_eq!(code(store.set_domain_kind("a.example", DomainKind::Masked).await), "kindChangeBlocked");
    }

    #[tokio::test]
    async fn policies_are_checked_and_only_new_addresses_follow_them() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        store.create_domain("example.net").await.unwrap();
        store.create_domain_with_kind("a.example", DomainKind::Masked).await.unwrap();
        store.create_domain_with_kind("b.example", DomainKind::Masked).await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.net").await;

        // Off by default: nowhere.
        let version = store.masked_policy_version().await.unwrap();
        assert_eq!(code(store.create_masked_address(mini, NewMaskedAddress::default()).await), "maskedDomain");
        assert_eq!(store.effective_masked_policy(mini).await.unwrap(), EffectiveMaskedPolicy::default());

        // The list takes masked-only domains, and the default has to be allowed.
        let refused =
            store.set_domain_masked_policy("example.org", policy(MaskedMode::Dedicated, &["example.net"], None));
        assert_eq!(code(refused.await), "notMaskedDomain");
        let refused =
            store.set_domain_masked_policy("example.org", policy(MaskedMode::Own, &["a.example"], Some("a.example")));
        assert_eq!(code(refused.await), "maskedDefault");
        let refused = store.set_domain_masked_policy("example.org", policy(MaskedMode::Off, &[], Some("example.org")));
        assert_eq!(code(refused.await), "maskedDefault");
        assert!(matches!(
            store.set_domain_masked_policy("example.org", policy(MaskedMode::Both, &["nowhere.invalid"], None)).await,
            Err(StoreError::NotFound(_))
        ));
        let both = policy(MaskedMode::Both, &["b.example", "a.example"], Some("b.example"));
        let saved = store.set_domain_masked_policy("example.org", both).await.unwrap();
        assert_eq!(saved.masked_domains, vec!["a.example", "b.example"]);
        assert!(store.masked_policy_version().await.unwrap() > version, "clients hear of it");

        let effective = store.effective_masked_policy(mini).await.unwrap();
        assert_eq!(effective.domains, vec!["a.example", "b.example", "example.org"]);
        assert_eq!(effective.default_domain.as_deref(), Some("b.example"));
        let first = store.create_masked_address(mini, NewMaskedAddress::default()).await.unwrap();
        assert!(first.email.ends_with("@b.example"), "the default");
        let wanted = NewMaskedAddress { domain: Some("Example.org".into()), ..Default::default() };
        assert!(store.create_masked_address(mini, wanted).await.unwrap().email.ends_with("@example.org"));
        let wanted = NewMaskedAddress { domain: Some("example.net".into()), ..Default::default() };
        assert_eq!(code(store.create_masked_address(mini, wanted).await), "maskedDomain", "not allowed");
        let wanted = NewMaskedAddress { domain: Some("nowhere.invalid".into()), ..Default::default() };
        assert_eq!(code(store.create_masked_address(mini, wanted).await), "maskedDomain", "unknown");

        // Leni's domain says nothing: example.org's policy is not hers, not even "own" on it.
        assert_eq!(code(store.create_masked_address(leni, NewMaskedAddress::default()).await), "maskedDomain");
        let wanted = NewMaskedAddress { domain: Some("example.org".into()), ..Default::default() };
        assert_eq!(code(store.create_masked_address(leni, wanted).await), "maskedDomain");

        // An admin gives Leni b.example only; a default outside of that is refused.
        let refused = AccountMaskedPolicy {
            mode: Some(MaskedMode::Dedicated),
            masked_domains: Some(vec!["b.example".into()]),
            default_domain: Some("a.example".into()),
        };
        assert_eq!(code(store.set_account_masked_policy(leni, refused).await), "maskedDefault");
        let custom = AccountMaskedPolicy {
            mode: Some(MaskedMode::Dedicated),
            masked_domains: Some(vec!["b.example".into()]),
            default_domain: None,
        };
        store.set_account_masked_policy(leni, custom.clone()).await.unwrap();
        assert_eq!(store.account_masked_policy(leni).await.unwrap(), custom);
        let made = store.create_masked_address(leni, NewMaskedAddress::default()).await.unwrap();
        assert!(made.email.ends_with("@b.example"));

        // Switched off later: nothing new, but what exists keeps taking mail and changing state.
        store.set_domain_masked_policy("example.org", policy(MaskedMode::Off, &[], None)).await.unwrap();
        store.set_account_masked_policy(leni, AccountMaskedPolicy::default()).await.unwrap();
        assert_eq!(code(store.create_masked_address(mini, NewMaskedAddress::default()).await), "maskedDomain");
        assert_eq!(store.resolve_recipient(&first.email).await.unwrap(), Some(mini));
        assert!(store.account_owns_address(mini, &first.email).await.unwrap(), "still sends as it");
        assert_eq!(store.resolve_recipient(&made.email).await.unwrap(), Some(leni));
        let disabled = crate::MaskedUpdate { state: Some(crate::MaskedState::Disabled), ..Default::default() };
        store.update_masked_address(mini, first.id, disabled).await.unwrap();
        let deleted = crate::MaskedUpdate { state: Some(crate::MaskedState::Deleted), ..Default::default() };
        store.update_masked_address(mini, first.id, deleted).await.unwrap();
        let enabled = crate::MaskedUpdate { state: Some(crate::MaskedState::Enabled), ..Default::default() };
        store.update_masked_address(mini, first.id, enabled).await.unwrap();

        // A person-level default that is no longer allowed falls back by itself.
        store.set_domain_masked_policy("example.org", policy(MaskedMode::Both, &["a.example"], None)).await.unwrap();
        let pinned = AccountMaskedPolicy { default_domain: Some("a.example".into()), ..Default::default() };
        store.set_account_masked_policy(mini, pinned).await.unwrap();
        assert_eq!(store.effective_masked_policy(mini).await.unwrap().default_domain.as_deref(), Some("a.example"));
        store.set_domain_masked_policy("example.org", policy(MaskedMode::Own, &["a.example"], None)).await.unwrap();
        let effective = store.effective_masked_policy(mini).await.unwrap();
        assert_eq!(effective.domains, vec!["example.org"]);
        assert_eq!(effective.default_domain.as_deref(), Some("example.org"));
        assert_eq!(store.account_masked_policy(mini).await.unwrap().default_domain.as_deref(), Some("a.example"));

        // With the domain's own default still allowed, that one comes first.
        let both = policy(MaskedMode::Both, &["a.example", "b.example"], Some("b.example"));
        store.set_domain_masked_policy("example.org", both).await.unwrap();
        assert_eq!(store.effective_masked_policy(mini).await.unwrap().default_domain.as_deref(), Some("a.example"));
        let narrowed = AccountMaskedPolicy { masked_domains: Some(vec!["b.example".into()]), ..Default::default() };
        store.set_account_masked_policy(mini, narrowed).await.unwrap();
        store
            .write(move |tx| {
                // As left behind when the list shrank after the default was chosen.
                tx.execute(
                    "UPDATE accounts SET masked_default_domain_id = (SELECT id FROM domains WHERE name = 'a.example')
                     WHERE id = ?1",
                    [mini],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let effective = store.effective_masked_policy(mini).await.unwrap();
        assert_eq!(effective.domains, vec!["b.example", "example.org"]);
        assert_eq!(effective.default_domain.as_deref(), Some("b.example"), "the domain's default, not automatic");
    }

    #[tokio::test]
    async fn a_trashed_account_on_a_masked_domain_cannot_come_back() {
        // Cannot happen through the rules above; a database edited by hand still keeps it out.
        let (store, _dir) = store().await;
        store.create_domain("masked.example").await.unwrap();
        person(&store, "nyu@masked.example").await;
        store.trash_account("nyu@masked.example").await.unwrap();
        store
            .write(|tx| {
                tx.execute("UPDATE domains SET kind = 'masked' WHERE name = 'masked.example'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(code(store.restore_account("nyu@masked.example").await), "maskedOnlyDomain");
    }
}
