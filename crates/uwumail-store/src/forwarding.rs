//! Forwarding: a person's mail also goes to other addresses. Addresses on this server work at
//! once; addresses elsewhere only after their owner confirmed through a link.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::address::normalize_address;
use crate::directory::{ACCOUNT_COLUMNS, account_from_row, resolve};
use crate::{Account, Result, Store, StoreError, now, random_bytes};

pub const MAX_FORWARD_TARGETS: i64 = 5;
/// A confirmation link works this long.
pub const FORWARD_LINK_LIFETIME_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardTarget {
    pub id: i64,
    pub address: String,
    /// The address belongs to someone on this server.
    pub local: bool,
    pub created_at: i64,
    pub confirmed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Forwarding {
    pub keep_copy: bool,
    /// An admin forbade forwarding to other servers for this person.
    pub external_blocked: bool,
    pub targets: Vec<ForwardTarget>,
}

/// Where mail for an account goes right now.
#[derive(Debug, Clone, Default)]
pub struct ActiveForwarding {
    pub keep_copy: bool,
    /// Confirmed targets; `Some(account id)` for people on this server.
    pub targets: Vec<(String, Option<i64>)>,
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn target_from_row(conn: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<ForwardTarget> {
    let address: String = row.get(1)?;
    let local = matches!(resolve(conn, &address), Ok(Some(_)));
    Ok(ForwardTarget { id: row.get(0)?, address, local, created_at: row.get(2)?, confirmed_at: row.get(3)? })
}

fn targets(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<ForwardTarget>> {
    let mut stmt = conn.prepare(
        "SELECT id, address, created_at, confirmed_at FROM forward_targets WHERE account_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([account_id], |row| target_from_row(conn, row))?;
    rows.collect()
}

impl Store {
    pub async fn forwarding(&self, account_id: i64) -> Result<Forwarding> {
        self.read(move |conn| {
            let (keep_copy, external_blocked) = conn.query_row(
                "SELECT forward_keep_copy, external_forwarding_blocked FROM accounts WHERE id = ?1",
                [account_id],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
            )?;
            Ok(Forwarding { keep_copy, external_blocked, targets: targets(conn, account_id)? })
        })
        .await
    }

    /// Adds a forwarding target. Returns the confirmation token for addresses on other servers.
    pub async fn add_forward_target(
        &self,
        account_id: i64,
        address: &str,
        external_allowed: bool,
    ) -> Result<(ForwardTarget, Option<String>)> {
        let (local_part, domain) = normalize_address(address)?;
        let address = format!("{local_part}@{domain}");
        self.write(move |tx| {
            let target_account = resolve(tx, &address)?;
            if target_account == Some(account_id) {
                return Err(StoreError::Rule {
                    code: "forwardToSelf",
                    message: "this address is already yours".into(),
                });
            }
            let blocked: bool =
                tx.query_row("SELECT external_forwarding_blocked FROM accounts WHERE id = ?1", [account_id], |row| {
                    row.get(0)
                })?;
            if target_account.is_none() && (blocked || !external_allowed) {
                return Err(StoreError::Rule {
                    code: "forwardingBlocked",
                    message: "forwarding to other servers is not allowed".into(),
                });
            }
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM forward_targets WHERE account_id = ?1", [account_id], |r| r.get(0))?;
            if count >= MAX_FORWARD_TARGETS {
                return Err(StoreError::Rule {
                    code: "tooManyForwardTargets",
                    message: format!("at most {MAX_FORWARD_TARGETS} forwarding addresses"),
                });
            }
            let created_at = now();
            let token = target_account.is_none().then(|| hex::encode(random_bytes::<32>()));
            tx.execute(
                "INSERT INTO forward_targets (account_id, address, token_hash, created_at, confirmed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    account_id,
                    address,
                    token.as_deref().map(token_hash),
                    created_at,
                    target_account.map(|_| created_at),
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                    StoreError::Conflict(format!("forwarding to {address}"))
                }
                other => other.into(),
            })?;
            let target = ForwardTarget {
                id: tx.last_insert_rowid(),
                address,
                local: target_account.is_some(),
                created_at,
                confirmed_at: target_account.map(|_| created_at),
            };
            Ok((target, token))
        })
        .await
    }

    pub async fn remove_forward_target(&self, account_id: i64, id: i64) -> Result<ForwardTarget> {
        self.write(move |tx| {
            let target = tx
                .query_row(
                    "SELECT id, address, created_at, confirmed_at FROM forward_targets WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                    |row| target_from_row(tx, row),
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("forwarding {id}")))?;
            tx.execute("DELETE FROM forward_targets WHERE id = ?1", [id])?;
            Ok(target)
        })
        .await
    }

    pub async fn set_forward_keep_copy(&self, account_id: i64, keep: bool) -> Result<()> {
        self.write(move |tx| {
            tx.execute("UPDATE accounts SET forward_keep_copy = ?1 WHERE id = ?2", params![keep, account_id])?;
            Ok(())
        })
        .await
    }

    pub async fn set_external_forwarding_blocked(&self, account_id: i64, blocked: bool) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "UPDATE accounts SET external_forwarding_blocked = ?1 WHERE id = ?2",
                params![blocked, account_id],
            )?;
            Ok(())
        })
        .await
    }

    /// The unconfirmed target behind a link, with the person who wants to forward.
    pub async fn forward_link(&self, token: &str) -> Result<Option<(ForwardTarget, Account)>> {
        let hash = token_hash(token);
        self.read(move |conn| {
            let found = conn
                .query_row(
                    "SELECT id, address, created_at, confirmed_at, account_id FROM forward_targets
                     WHERE token_hash = ?1 AND confirmed_at IS NULL AND created_at > ?2",
                    params![hash, now() - FORWARD_LINK_LIFETIME_SECS],
                    |row| Ok((target_from_row(conn, row)?, row.get::<_, i64>(4)?)),
                )
                .optional()?;
            let Some((target, account_id)) = found else {
                return Ok(None);
            };
            let account = conn
                .query_row(
                    &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1 AND deleted_at IS NULL"),
                    [account_id],
                    account_from_row,
                )
                .optional()?;
            Ok(account.map(|account| (target, account)))
        })
        .await
    }

    /// The owner of the address agreed: forwarding starts.
    pub async fn confirm_forward_link(&self, token: &str) -> Result<(ForwardTarget, Account)> {
        let (target, account) =
            self.forward_link(token).await?.ok_or_else(|| StoreError::NotFound("forwarding link".into()))?;
        let id = target.id;
        let confirmed_at = self
            .write(move |tx| {
                let at = now();
                let changed = tx.execute(
                    "UPDATE forward_targets SET confirmed_at = ?1, token_hash = NULL WHERE id = ?2 AND confirmed_at IS NULL",
                    params![at, id],
                )?;
                if changed == 0 {
                    return Err(StoreError::NotFound("forwarding link".into()));
                }
                Ok(at)
            })
            .await?;
        Ok((ForwardTarget { confirmed_at: Some(confirmed_at), ..target }, account))
    }

    /// The owner of the address does not want the mail: the target is removed.
    pub async fn decline_forward_link(&self, token: &str) -> Result<(ForwardTarget, Account)> {
        let (target, account) =
            self.forward_link(token).await?.ok_or_else(|| StoreError::NotFound("forwarding link".into()))?;
        let id = target.id;
        self.write(move |tx| {
            tx.execute("DELETE FROM forward_targets WHERE id = ?1", [id])?;
            Ok(())
        })
        .await?;
        Ok((target, account))
    }

    /// Confirmed targets for delivery. Addresses elsewhere are left out while an admin blocks them.
    pub async fn active_forwarding(&self, account_id: i64) -> Result<ActiveForwarding> {
        self.read(move |conn| {
            let (keep_copy, blocked) = conn.query_row(
                "SELECT forward_keep_copy, external_forwarding_blocked FROM accounts WHERE id = ?1",
                [account_id],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
            )?;
            let mut stmt = conn.prepare(
                "SELECT address FROM forward_targets WHERE account_id = ?1 AND confirmed_at IS NOT NULL ORDER BY id",
            )?;
            let addresses =
                stmt.query_map([account_id], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            let mut targets = Vec::new();
            for address in addresses {
                let local = resolve(conn, &address)?;
                if local == Some(account_id) || (local.is_none() && blocked) {
                    continue;
                }
                targets.push((address, local));
            }
            Ok(ActiveForwarding { keep_copy, targets })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    async fn account(store: &Store, address: &str) -> Account {
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
    }

    #[tokio::test]
    async fn local_targets_work_at_once_and_others_after_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("example.de").await.unwrap();
        let leni = account(&store, "leni@example.de").await;
        let ami = account(&store, "ami@example.de").await;

        let self_target = store.add_forward_target(leni.id, "LENI@example.de", true).await;
        assert!(matches!(self_target, Err(StoreError::Rule { code: "forwardToSelf", .. })));
        let (local, token) = store.add_forward_target(leni.id, "ami@example.de", true).await.unwrap();
        assert!(local.local && local.confirmed_at.is_some() && token.is_none());
        let blocked = store.add_forward_target(leni.id, "leni@elsewhere.example", false).await;
        assert!(matches!(blocked, Err(StoreError::Rule { code: "forwardingBlocked", .. })));

        let (external, token) = store.add_forward_target(leni.id, "leni@elsewhere.example", true).await.unwrap();
        let token = token.expect("a confirmation link");
        assert!(!external.local && external.confirmed_at.is_none());
        let active = store.active_forwarding(leni.id).await.unwrap();
        assert_eq!(active.targets, vec![("ami@example.de".to_owned(), Some(ami.id))]);

        let (_, owner) = store.forward_link(&token).await.unwrap().unwrap();
        assert_eq!(owner.id, leni.id);
        store.confirm_forward_link(&token).await.unwrap();
        assert!(store.forward_link(&token).await.unwrap().is_none(), "a link works once");
        assert_eq!(store.active_forwarding(leni.id).await.unwrap().targets.len(), 2);

        store.set_external_forwarding_blocked(leni.id, true).await.unwrap();
        assert_eq!(store.active_forwarding(leni.id).await.unwrap().targets.len(), 1, "the admin lock applies at once");
        store.set_forward_keep_copy(leni.id, false).await.unwrap();
        let forwarding = store.forwarding(leni.id).await.unwrap();
        assert!(!forwarding.keep_copy && forwarding.external_blocked && forwarding.targets.len() == 2);
    }
}
