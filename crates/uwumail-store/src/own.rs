//! What people manage themselves: aliases on domains an admin opened for it, and how much
//! space their folders take.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::address::{base_local_part, normalize_address};
use crate::directory::domain_id;
use crate::mail::MailboxRole;
use crate::mutate::{EmailUpdate, MailboxesChange};
use crate::{Result, Store, StoreError, now};

/// An alias its owner deleted stays theirs this long.
pub const RELEASED_ADDRESS_SECS: i64 = 30 * 24 * 3600;

/// Names people cannot take themselves, because others rely on what they mean.
const RESERVED: &[&str] = &[
    "postmaster",
    "abuse",
    "hostmaster",
    "webmaster",
    "mailer-daemon",
    "root",
    "admin",
    "administrator",
    "security",
    "noreply",
    "no-reply",
    "dmarc",
    "tls-rpt",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnAddress {
    pub address: String,
    pub kind: String,
    /// Created by the person, so they may delete it again.
    pub own: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleasedAddress {
    pub address: String,
    pub released_at: i64,
    /// Until then it can be taken back.
    pub reserved_until: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnAddresses {
    pub addresses: Vec<OwnAddress>,
    /// Domains where people may create aliases.
    pub domains: Vec<String>,
    pub limit: i64,
    pub used: i64,
    pub released: Vec<ReleasedAddress>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxUsage {
    pub id: i64,
    pub name: String,
    pub role: Option<MailboxRole>,
    pub emails: i64,
    pub size_bytes: i64,
}

fn forget_old_releases(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM released_addresses WHERE released_at < ?1", [now() - RELEASED_ADDRESS_SECS])?;
    Ok(())
}

fn rule(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Rule { code, message: message.into() }
}

impl Store {
    pub async fn own_addresses(&self, account_id: i64) -> Result<OwnAddresses> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT a.local_part || '@' || d.name, a.kind, a.created_by_owner, a.created_at
                 FROM addresses a JOIN domains d ON d.id = a.domain_id
                 WHERE a.account_id = ?1 ORDER BY a.kind = 'primary' DESC, 1",
            )?;
            let addresses = stmt
                .query_map([account_id], |row| {
                    Ok(OwnAddress {
                        address: row.get(0)?,
                        kind: row.get(1)?,
                        own: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut stmt = conn.prepare("SELECT name FROM domains WHERE self_service_aliases = 1 ORDER BY name")?;
            let domains = stmt.query_map([], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
            let limit: i64 =
                conn.query_row("SELECT alias_limit FROM accounts WHERE id = ?1", [account_id], |r| r.get(0))?;
            let mut stmt = conn.prepare(
                "SELECT r.local_part || '@' || d.name, r.released_at FROM released_addresses r
                 JOIN domains d ON d.id = r.domain_id
                 WHERE r.account_id = ?1 AND r.released_at >= ?2 ORDER BY r.released_at DESC",
            )?;
            let released = stmt
                .query_map(params![account_id, now() - RELEASED_ADDRESS_SECS], |row| {
                    let released_at: i64 = row.get(1)?;
                    Ok(ReleasedAddress {
                        address: row.get(0)?,
                        released_at,
                        reserved_until: released_at + RELEASED_ADDRESS_SECS,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let used = addresses.iter().filter(|address| address.own).count() as i64;
            Ok(OwnAddresses { addresses, domains, limit, used, released })
        })
        .await
    }

    /// Creates an alias the person asked for. Taking back an address released within 30 days
    /// works the same way.
    pub async fn create_own_alias(&self, account_id: i64, address: &str) -> Result<OwnAddress> {
        let (local, domain) = normalize_address(address)?;
        if local.contains('+') {
            return Err(rule("aliasInvalid", "an alias cannot contain +"));
        }
        if RESERVED.contains(&base_local_part(&local)) {
            return Err(rule("aliasReserved", format!("{local} is kept for the server")));
        }
        self.write(move |tx| {
            forget_old_releases(tx)?;
            let allowed: bool = tx
                .query_row("SELECT self_service_aliases FROM domains WHERE name = ?1", [&domain], |row| row.get(0))
                .optional()?
                .unwrap_or(false);
            if !allowed {
                return Err(rule("aliasDomain", format!("aliases on {domain} are not open")));
            }
            let domain_id = domain_id(tx, &domain)?;
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM addresses WHERE local_part = ?1 AND domain_id = ?2)
                     OR EXISTS (SELECT 1 FROM released_addresses WHERE local_part = ?1 AND domain_id = ?2 AND account_id != ?3)
                     OR EXISTS (SELECT 1 FROM accounts WHERE login = ?4)",
                params![local, domain_id, account_id, format!("{local}@{domain}")],
                |row| row.get(0),
            )?;
            if taken {
                return Err(rule("addressTaken", format!("{local}@{domain} is taken")));
            }
            let (used, limit): (i64, i64) = tx.query_row(
                "SELECT (SELECT COUNT(*) FROM addresses WHERE account_id = ?1 AND created_by_owner = 1),
                        (SELECT alias_limit FROM accounts WHERE id = ?1)",
                [account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if used >= limit {
                return Err(rule("aliasLimit", format!("at most {limit} own aliases")));
            }
            let created_at = now();
            tx.execute(
                "INSERT INTO addresses (local_part, domain_id, account_id, kind, created_at, created_by_owner)
                 VALUES (?1, ?2, ?3, 'alias', ?4, 1)",
                params![local, domain_id, account_id, created_at],
            )?;
            tx.execute(
                "DELETE FROM released_addresses WHERE local_part = ?1 AND domain_id = ?2",
                params![local, domain_id],
            )?;
            Ok(OwnAddress { address: format!("{local}@{domain}"), kind: "alias".into(), own: true, created_at })
        })
        .await
    }

    /// Deletes an alias the person created. The address stays reserved for them for 30 days.
    pub async fn delete_own_alias(&self, account_id: i64, address: &str) -> Result<()> {
        let (local, domain) = normalize_address(address)?;
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let own: Option<bool> = tx
                .query_row(
                    "SELECT created_by_owner FROM addresses
                     WHERE local_part = ?1 AND domain_id = ?2 AND account_id = ?3 AND kind = 'alias'",
                    params![local, domain_id, account_id],
                    |row| row.get(0),
                )
                .optional()?;
            match own {
                None => return Err(StoreError::NotFound(format!("alias {local}@{domain}"))),
                Some(false) => return Err(rule("aliasNotYours", "an admin created this alias")),
                Some(true) => {}
            }
            tx.execute("DELETE FROM addresses WHERE local_part = ?1 AND domain_id = ?2", params![local, domain_id])?;
            tx.execute(
                "INSERT OR REPLACE INTO released_addresses (local_part, domain_id, account_id, released_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![local, domain_id, account_id, now()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn set_domain_self_service(&self, domain: &str, on: bool) -> Result<()> {
        let (_, domain) = normalize_address(&format!("x@{domain}"))?;
        self.write(move |tx| {
            let changed =
                tx.execute("UPDATE domains SET self_service_aliases = ?1 WHERE name = ?2", params![on, domain])?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("domain {domain}")));
            }
            Ok(())
        })
        .await
    }

    pub async fn domain_self_service(&self, domain: &str) -> Result<bool> {
        let domain = domain.to_ascii_lowercase();
        self.read(move |conn| {
            Ok(conn
                .query_row("SELECT self_service_aliases FROM domains WHERE name = ?1", [domain], |row| row.get(0))
                .optional()?
                .unwrap_or(false))
        })
        .await
    }

    pub async fn set_alias_limit(&self, account_id: i64, limit: i64) -> Result<()> {
        if !(0..=1000).contains(&limit) {
            return Err(StoreError::Invalid("the alias limit must be between 0 and 1000".into()));
        }
        self.write(move |tx| {
            tx.execute("UPDATE accounts SET alias_limit = ?1 WHERE id = ?2", params![limit, account_id])?;
            Ok(())
        })
        .await
    }

    /// Size and number of messages per folder. A message in two folders counts in both.
    pub async fn mailbox_usage(&self, account_id: i64) -> Result<Vec<MailboxUsage>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.name, m.role, COUNT(e.id), COALESCE(SUM(e.size), 0)
                 FROM mailboxes m
                 LEFT JOIN email_mailboxes em ON em.mailbox_id = m.id
                 LEFT JOIN emails e ON e.id = em.email_id
                 WHERE m.account_id = ?1
                 GROUP BY m.id ORDER BY m.sort_order, m.name",
            )?;
            let rows = stmt
                .query_map([account_id], |row| {
                    Ok(MailboxUsage {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        role: row.get::<_, Option<String>>(2)?.as_deref().and_then(MailboxRole::parse),
                        emails: row.get(3)?,
                        size_bytes: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Empties the Trash or Junk folder. Messages that are in another folder too only leave this one.
    /// Returns how many messages were removed from the folder.
    pub async fn empty_mailbox(&self, account_id: i64, role: MailboxRole) -> Result<usize> {
        if !matches!(role, MailboxRole::Trash | MailboxRole::Junk) {
            return Err(rule("notEmptiable", "only Trash and Junk can be emptied"));
        }
        let role_name = role.as_str().to_owned();
        let (mailbox, emails) = self
            .read(move |conn| {
                let Some(mailbox) = conn
                    .query_row(
                        "SELECT id FROM mailboxes WHERE account_id = ?1 AND role = ?2",
                        params![account_id, role_name],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                else {
                    return Ok((None, Vec::new()));
                };
                let mut stmt = conn.prepare(
                    "SELECT em.email_id, (SELECT COUNT(*) FROM email_mailboxes other WHERE other.email_id = em.email_id)
                     FROM email_mailboxes em WHERE em.mailbox_id = ?1",
                )?;
                let emails = stmt
                    .query_map([mailbox], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((Some(mailbox), emails))
            })
            .await?;
        let Some(mailbox) = mailbox else {
            return Ok(0);
        };
        let (elsewhere, only_here): (Vec<_>, Vec<_>) = emails.into_iter().partition(|(_, folders)| *folders > 1);
        let count = elsewhere.len() + only_here.len();
        if !only_here.is_empty() {
            self.destroy_emails(account_id, only_here.into_iter().map(|(id, _)| id).collect()).await?;
        }
        if !elsewhere.is_empty() {
            let updates = elsewhere
                .into_iter()
                .map(|(id, _)| EmailUpdate {
                    id,
                    mailboxes: MailboxesChange::Patch(vec![(mailbox, false)]),
                    ..Default::default()
                })
                .collect();
            self.update_emails(account_id, updates).await?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IngestRequest, MailboxTarget, NewAccount, Role};

    async fn account(store: &Store, address: &str) -> i64 {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn own_aliases_follow_the_rules_and_stay_reserved() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("example.de").await.unwrap();
        let leni = account(&store, "leni@example.de").await;
        let ami = account(&store, "ami@example.de").await;
        let code = |result: Result<OwnAddress>| match result {
            Err(StoreError::Rule { code, .. }) => code,
            other => panic!("expected a rule, got {other:?}"),
        };

        assert_eq!(code(store.create_own_alias(leni, "katze@example.de").await), "aliasDomain");
        store.set_domain_self_service("example.de", true).await.unwrap();
        assert_eq!(code(store.create_own_alias(leni, "postmaster@example.de").await), "aliasReserved");
        assert_eq!(code(store.create_own_alias(leni, "ami@example.de").await), "addressTaken");
        store.create_own_alias(leni, "Katze@example.de").await.unwrap();
        store.set_alias_limit(leni, 1).await.unwrap();
        assert_eq!(code(store.create_own_alias(leni, "hund@example.de").await), "aliasLimit");
        assert_eq!(store.resolve_recipient("katze@example.de").await.unwrap(), Some(leni));

        store.delete_own_alias(leni, "katze@example.de").await.unwrap();
        assert_eq!(store.resolve_recipient("katze@example.de").await.unwrap(), None);
        assert_eq!(code(store.create_own_alias(ami, "katze@example.de").await), "addressTaken", "reserved for leni");
        let own = store.own_addresses(leni).await.unwrap();
        assert_eq!((own.used, own.released.len()), (0, 1));
        store.create_own_alias(leni, "katze@example.de").await.unwrap();
        assert!(store.own_addresses(leni).await.unwrap().released.is_empty(), "taken back");

        store.add_alias("chef@example.de", "leni@example.de").await.unwrap();
        assert!(matches!(
            store.delete_own_alias(leni, "chef@example.de").await,
            Err(StoreError::Rule { code: "aliasNotYours", .. })
        ));
    }

    #[tokio::test]
    async fn trash_empties_but_keeps_messages_filed_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("example.de").await.unwrap();
        let leni = account(&store, "leni@example.de").await;
        let ingest = |targets: Vec<MailboxTarget>, subject: &str| IngestRequest {
            account_id: leni,
            raw: format!("From: a@example.org\r\nSubject: {subject}\r\n\r\nHallo\r\n").into_bytes(),
            mailboxes: targets,
            keywords: vec![],
            received_at: None,
        };
        store.ingest(ingest(vec![MailboxTarget::Role(MailboxRole::Trash)], "Weg")).await.unwrap();
        store
            .ingest(ingest(
                vec![MailboxTarget::Role(MailboxRole::Trash), MailboxTarget::Role(MailboxRole::Archive)],
                "Bleibt",
            ))
            .await
            .unwrap();
        let usage = store.mailbox_usage(leni).await.unwrap();
        let trash = usage.iter().find(|m| m.role == Some(MailboxRole::Trash)).unwrap();
        assert_eq!(trash.emails, 2);
        assert!(trash.size_bytes > 0);

        assert_eq!(store.empty_mailbox(leni, MailboxRole::Trash).await.unwrap(), 2);
        let usage = store.mailbox_usage(leni).await.unwrap();
        assert_eq!(usage.iter().find(|m| m.role == Some(MailboxRole::Trash)).unwrap().emails, 0);
        assert_eq!(usage.iter().find(|m| m.role == Some(MailboxRole::Archive)).unwrap().emails, 1);
        assert!(store.empty_mailbox(leni, MailboxRole::Inbox).await.is_err());
    }
}
