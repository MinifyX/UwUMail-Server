//! Forwarding addresses: addresses of our domains without a mailbox, whose mail goes straight on to
//! other addresses. Admins set them up, so unlike a person's forwarding the targets need no
//! confirmation.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::address::{base_local_part, normalize_address};
use crate::directory::{domain_id, resolve};
use crate::{Result, Store, StoreError, now};

/// How many addresses one forwarding address may pass mail on to.
pub const FORWARD_ADDRESS_MAX_TARGETS: usize = 20;
const NOTE_MAX_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardAddress {
    pub address: String,
    pub domain: String,
    pub targets: Vec<String>,
    pub note: String,
    pub created_at: i64,
}

/// Whether an address of one of our domains is already used by a mailbox, an alias or a forwarding
/// address.
pub(crate) fn address_in_use(conn: &Connection, local: &str, domain_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM addresses WHERE local_part = ?1 AND domain_id = ?2)
             OR EXISTS (SELECT 1 FROM forward_addresses WHERE local_part = ?1 AND domain_id = ?2)",
        params![local, domain_id],
        |row| row.get(0),
    )?)
}

/// The targets of the forwarding address that takes mail for `local`@`domain`: the address itself,
/// or its base without a `+tag` unless a mailbox has the full address.
pub(crate) fn forward_targets(conn: &Connection, local: &str, domain_id: i64) -> Result<Option<Vec<String>>> {
    let lookup = |local: &str| -> Result<Option<String>> {
        Ok(conn
            .query_row(
                "SELECT targets FROM forward_addresses WHERE local_part = ?1 AND domain_id = ?2",
                params![local, domain_id],
                |row| row.get(0),
            )
            .optional()?)
    };
    let found = match lookup(local)? {
        Some(targets) => Some(targets),
        None => {
            let base = base_local_part(local);
            let owned: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM addresses WHERE local_part = ?1 AND domain_id = ?2)",
                params![local, domain_id],
                |row| row.get(0),
            )?;
            if base != local && !owned { lookup(base)? } else { None }
        }
    };
    Ok(found.map(|targets| targets.lines().map(str::to_owned).collect()))
}

fn split(address: &str) -> Result<(String, String)> {
    normalize_address(address).map_err(|_| StoreError::Invalid(format!("'{address}' is not a valid email address")))
}

impl Store {
    /// The forwarding addresses of one domain, or of all of them.
    pub async fn forward_addresses(&self, domain: Option<String>) -> Result<Vec<ForwardAddress>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT f.local_part || '@' || d.name, d.name, f.targets, f.note, f.created_at
                 FROM forward_addresses f JOIN domains d ON d.id = f.domain_id
                 WHERE ?1 IS NULL OR d.name = ?1 ORDER BY d.name, f.local_part",
            )?;
            let rows = stmt.query_map([domain], |row| {
                let targets: String = row.get(2)?;
                Ok(ForwardAddress {
                    address: row.get(0)?,
                    domain: row.get(1)?,
                    targets: targets.lines().map(str::to_owned).collect(),
                    note: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Creates a forwarding address or replaces its targets. The address may not belong to a
    /// mailbox or alias; the targets may be anywhere except the address itself.
    pub async fn set_forward_address(&self, address: &str, targets: Vec<String>, note: &str) -> Result<ForwardAddress> {
        let (local, domain) = split(address)?;
        let own = format!("{local}@{domain}");
        let mut normalized: Vec<String> = Vec::new();
        for target in &targets {
            let (target_local, target_domain) = split(target.trim())?;
            let target = format!("{target_local}@{target_domain}");
            if target == own {
                return Err(StoreError::Invalid(format!("{own} cannot forward to itself")));
            }
            if !normalized.contains(&target) {
                normalized.push(target);
            }
        }
        if normalized.is_empty() || normalized.len() > FORWARD_ADDRESS_MAX_TARGETS {
            return Err(StoreError::Invalid(format!(
                "a forwarding address needs between 1 and {FORWARD_ADDRESS_MAX_TARGETS} targets"
            )));
        }
        let note = note.trim().to_owned();
        if note.chars().count() > NOTE_MAX_CHARS {
            return Err(StoreError::Invalid(format!("the note may have at most {NOTE_MAX_CHARS} characters")));
        }
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let mailbox: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM addresses WHERE local_part = ?1 AND domain_id = ?2)",
                params![local, domain_id],
                |row| row.get(0),
            )?;
            if mailbox {
                return Err(StoreError::Conflict(format!("address {own}")));
            }
            let created_at = now();
            tx.execute(
                "INSERT INTO forward_addresses (local_part, domain_id, targets, note, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (local_part, domain_id) DO UPDATE SET targets = excluded.targets, note = excluded.note",
                params![local, domain_id, normalized.join("\n"), note, created_at],
            )?;
            let created_at = tx.query_row(
                "SELECT created_at FROM forward_addresses WHERE local_part = ?1 AND domain_id = ?2",
                params![local, domain_id],
                |row| row.get(0),
            )?;
            Ok(ForwardAddress { address: own, domain, targets: normalized, note, created_at })
        })
        .await
    }

    pub async fn remove_forward_address(&self, address: &str) -> Result<()> {
        let (local, domain) = split(address)?;
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let removed = tx.execute(
                "DELETE FROM forward_addresses WHERE local_part = ?1 AND domain_id = ?2",
                params![local, domain_id],
            )?;
            if removed == 0 {
                return Err(StoreError::NotFound(format!("forwarding address {local}@{domain}")));
            }
            Ok(())
        })
        .await
    }

    /// Where mail for `address` goes if it is a forwarding address: each target with `Some(account
    /// id)` when it is hosted here.
    pub async fn forward_address_targets(&self, address: &str) -> Result<Option<Vec<(String, Option<i64>)>>> {
        let Ok((local, domain)) = normalize_address(address) else {
            return Ok(None);
        };
        self.read(move |conn| {
            let Some(domain_id) =
                conn.query_row("SELECT id FROM domains WHERE name = ?1", [&domain], |row| row.get(0)).optional()?
            else {
                return Ok(None);
            };
            let Some(targets) = forward_targets(conn, &local, domain_id)? else {
                return Ok(None);
            };
            targets
                .into_iter()
                .map(|target| {
                    let account = resolve(conn, &target)?;
                    Ok((target, account))
                })
                .collect::<Result<Vec<_>>>()
                .map(Some)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    #[tokio::test]
    async fn forwarding_addresses_pass_mail_on_and_keep_their_address_to_themselves() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = store
            .create_account(NewAccount {
                address: "mini@example.org".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();

        let targets = vec!["Verein@Example.net".into(), "mini@example.org".into(), "verein@example.net".into()];
        let created = store.set_forward_address("Kasse@example.org", targets, "Kassenwart").await.unwrap();
        assert_eq!(created.targets, vec!["verein@example.net", "mini@example.org"]);
        assert_eq!(
            store.forward_address_targets("kasse+2026@example.org").await.unwrap(),
            Some(vec![("verein@example.net".into(), None), ("mini@example.org".into(), Some(mini.id))])
        );
        assert_eq!(store.forward_address_targets("mini@example.org").await.unwrap(), None);

        // A catch-all does not swallow it, and no mailbox or alias can take the address.
        store.set_catch_all("example.org", Some("mini@example.org")).await.unwrap();
        assert_eq!(store.resolve_recipient("kasse@example.org").await.unwrap(), None);
        assert_eq!(store.resolve_recipient("irgendwer@example.org").await.unwrap(), Some(mini.id));
        assert!(matches!(store.add_alias("kasse@example.org", "mini@example.org").await, Err(StoreError::Conflict(_))));
        let refused = store.set_forward_address("mini@example.org", vec!["a@example.net".into()], "").await;
        assert!(matches!(refused, Err(StoreError::Conflict(_))));
        let itself = store.set_forward_address("kasse@example.org", vec!["kasse@example.org".into()], "").await;
        assert!(matches!(itself, Err(StoreError::Invalid(_))));

        let replaced =
            store.set_forward_address("kasse@example.org", vec!["neu@example.net".into()], "").await.unwrap();
        assert_eq!((replaced.targets.len(), replaced.created_at), (1, created.created_at));
        assert_eq!(store.forward_addresses(Some("example.org".into())).await.unwrap(), vec![replaced]);
        assert!(store.delete_domain("example.org").await.is_err(), "the domain is still in use");
        store.remove_forward_address("kasse@example.org").await.unwrap();
        assert!(store.forward_addresses(None).await.unwrap().is_empty());
    }
}
