//! Managing people from the admin panel: changes, the trash, links to choose a
//! password, and the change log.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::directory::{ACCOUNT_COLUMN_COUNT, ACCOUNT_COLUMNS, account_from_row, account_id, login_key};
use crate::{Account, Result, Role, Store, StoreError, now, password, random_bytes};

/// People stay in the trash this long before they are removed for good.
pub const TRASH_RETENTION_SECS: i64 = 30 * 24 * 3600;

#[derive(Debug, Clone, Default)]
pub struct AccountUpdate {
    pub display_name: Option<String>,
    pub role: Option<Role>,
    /// 0 means unlimited.
    pub quota_bytes: Option<i64>,
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordLinkPurpose {
    /// A new person chooses their first password.
    Invite,
    /// Someone who forgot their password chooses a new one.
    Reset,
}

impl PasswordLinkPurpose {
    fn as_str(self) -> &'static str {
        match self {
            PasswordLinkPurpose::Invite => "invite",
            PasswordLinkPurpose::Reset => "reset",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PasswordLink {
    pub account: Account,
    pub purpose: PasswordLinkPurpose,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressInfo {
    pub address: String,
    /// "primary" or "alias".
    pub kind: String,
    pub created_at: i64,
}

/// A person as the admin panel shows them.
#[derive(Debug, Clone)]
pub struct Person {
    pub account: Account,
    /// False until an invited person chose their password.
    pub has_password: bool,
    pub addresses: Vec<AddressInfo>,
}

/// One change for the change log. `details` must never contain passwords or mail content.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub actor_id: Option<i64>,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub details: Value,
    pub ip: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    pub id: i64,
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub details: Value,
    pub ip: String,
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn load_account(conn: &Connection, login: &str) -> Result<Account> {
    conn.query_row(&format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE login = ?1"), [login], account_from_row)
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("account {login}")))
}

fn active_admin(account: &Account) -> bool {
    account.role == Role::Admin && account.can_log_in()
}

/// Refuses a change that would leave the server without anyone who can manage it.
fn keep_an_admin(conn: &Connection, before: &Account, after_is_active_admin: bool) -> Result<()> {
    if !active_admin(before) || after_is_active_admin {
        return Ok(());
    }
    let others: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE role = 'admin' AND disabled = 0 AND deleted_at IS NULL AND id <> ?1",
        [before.id],
        |row| row.get(0),
    )?;
    if others == 0 {
        return Err(StoreError::Rule {
            code: "lastAdmin",
            message: format!("{} is the last admin; make someone else an admin first", before.login),
        });
    }
    Ok(())
}

impl Store {
    pub async fn update_account(&self, login: &str, update: AccountUpdate) -> Result<Account> {
        let login = login_key(login)?;
        if update.quota_bytes.is_some_and(|quota| quota < 0) {
            return Err(StoreError::Invalid("the quota cannot be negative".into()));
        }
        self.write(move |tx| {
            let before = load_account(tx, &login)?;
            if before.deleted_at.is_some() {
                return Err(StoreError::Invalid(format!("{login} is in the trash; restore it first")));
            }
            let mut after = before.clone();
            if let Some(name) = &update.display_name {
                after.display_name = name.trim().to_owned();
            }
            if let Some(role) = update.role {
                after.role = role;
            }
            if let Some(quota) = update.quota_bytes {
                after.quota_bytes = quota;
            }
            if let Some(disabled) = update.disabled {
                after.disabled = disabled;
            }
            keep_an_admin(tx, &before, active_admin(&after))?;
            tx.execute(
                "UPDATE accounts SET display_name = ?1, role = ?2, quota_bytes = ?3, disabled = ?4 WHERE id = ?5",
                params![
                    after.display_name,
                    if after.role == Role::Admin { "admin" } else { "user" },
                    after.quota_bytes,
                    after.disabled,
                    after.id
                ],
            )?;
            if after.disabled && !before.disabled {
                tx.execute("DELETE FROM web_sessions WHERE account_id = ?1", [after.id])?;
            }
            Ok(after)
        })
        .await
    }

    /// Moves a person to the trash: logged out everywhere, no more mail, addresses reserved.
    pub async fn trash_account(&self, login: &str) -> Result<Account> {
        let login = login_key(login)?;
        self.write(move |tx| {
            let mut account = load_account(tx, &login)?;
            if account.deleted_at.is_some() {
                return Ok(account);
            }
            keep_an_admin(tx, &account, false)?;
            let at = now();
            tx.execute("UPDATE accounts SET deleted_at = ?1 WHERE id = ?2", params![at, account.id])?;
            tx.execute("DELETE FROM web_sessions WHERE account_id = ?1", [account.id])?;
            tx.execute("DELETE FROM password_links WHERE account_id = ?1", [account.id])?;
            tx.execute("UPDATE domains SET catch_all_account_id = NULL WHERE catch_all_account_id = ?1", [account.id])?;
            account.deleted_at = Some(at);
            Ok(account)
        })
        .await
    }

    pub async fn restore_account(&self, login: &str) -> Result<Account> {
        let login = login_key(login)?;
        self.write(move |tx| {
            let mut account = load_account(tx, &login)?;
            tx.execute("UPDATE accounts SET deleted_at = NULL WHERE id = ?1", [account.id])?;
            account.deleted_at = None;
            Ok(account)
        })
        .await
    }

    /// Removes people who have been in the trash for longer than `older_than_secs`, with all their mail.
    pub async fn purge_trash(&self, older_than_secs: i64) -> Result<Vec<String>> {
        let cutoff = now() - older_than_secs;
        let logins: Vec<String> = self
            .read(move |conn| {
                let mut stmt =
                    conn.prepare("SELECT login FROM accounts WHERE deleted_at IS NOT NULL AND deleted_at <= ?1")?;
                let rows = stmt.query_map([cutoff], |row| row.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        for login in &logins {
            self.delete_account(login).await?;
        }
        Ok(logins)
    }

    /// Creates a one-time link for a person to choose a password. Older links of that person stop working.
    pub async fn create_password_link(
        &self,
        login: &str,
        purpose: PasswordLinkPurpose,
        created_by: Option<i64>,
        lifetime_secs: i64,
    ) -> Result<(String, i64)> {
        let login = login_key(login)?;
        let token = hex::encode(random_bytes::<32>());
        let hash = token_hash(&token);
        let expires_at = self
            .write(move |tx| {
                let account = load_account(tx, &login)?;
                if !account.can_log_in() {
                    return Err(StoreError::Invalid(format!("{login} is disabled or in the trash")));
                }
                let now = now();
                tx.execute(
                    "DELETE FROM password_links WHERE account_id = ?1 OR expires_at <= ?2",
                    params![account.id, now],
                )?;
                tx.execute(
                    "INSERT INTO password_links (token_hash, account_id, purpose, created_by, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![hash, account.id, purpose.as_str(), created_by, now, now + lifetime_secs],
                )?;
                Ok(now + lifetime_secs)
            })
            .await?;
        Ok((token, expires_at))
    }

    /// The person a password link belongs to, if the link is still valid.
    pub async fn password_link(&self, token: &str) -> Result<Option<PasswordLink>> {
        let hash = token_hash(token);
        self.read(move |conn| {
            let columns = ACCOUNT_COLUMNS.split(", ").map(|c| format!("a.{c}")).collect::<Vec<_>>().join(", ");
            let found = conn
                .query_row(
                    &format!(
                        "SELECT {columns}, l.purpose, l.expires_at FROM password_links l
                         JOIN accounts a ON a.id = l.account_id WHERE l.token_hash = ?1"
                    ),
                    [hash],
                    |row| {
                        Ok((
                            account_from_row(row)?,
                            row.get::<_, String>(ACCOUNT_COLUMN_COUNT)?,
                            row.get::<_, i64>(ACCOUNT_COLUMN_COUNT + 1)?,
                        ))
                    },
                )
                .optional()?;
            Ok(found.and_then(|(account, purpose, expires_at)| {
                (expires_at > now() && account.can_log_in()).then(|| PasswordLink {
                    account,
                    purpose: if purpose == "invite" { PasswordLinkPurpose::Invite } else { PasswordLinkPurpose::Reset },
                    expires_at,
                })
            }))
        })
        .await
    }

    /// Sets the password through a link. The link is used up and the person is logged out everywhere.
    pub async fn use_password_link(&self, token: &str, new_password: &str) -> Result<Account> {
        let Some(link) = self.password_link(token).await? else {
            return Err(StoreError::NotFound("password link".into()));
        };
        let new_password = new_password.to_owned();
        let hash = tokio::task::spawn_blocking(move || password::hash(&new_password))
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))??;
        let token_hash = token_hash(token);
        self.write(move |tx| {
            // The link may have been used a moment ago by another request.
            let removed = tx.execute("DELETE FROM password_links WHERE token_hash = ?1", [token_hash])?;
            if removed == 0 {
                return Err(StoreError::NotFound("password link".into()));
            }
            tx.execute("UPDATE accounts SET password_hash = ?1 WHERE id = ?2", params![hash, link.account.id])?;
            tx.execute("DELETE FROM password_links WHERE account_id = ?1", [link.account.id])?;
            tx.execute("DELETE FROM web_sessions WHERE account_id = ?1", [link.account.id])?;
            Ok(link.account)
        })
        .await
    }

    /// Everyone with their addresses, in the trash too; sorted by login.
    pub async fn people(&self) -> Result<Vec<Person>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ACCOUNT_COLUMNS}, password_hash IS NOT NULL FROM accounts ORDER BY login"
            ))?;
            let accounts = stmt
                .query_map([], |row| Ok((account_from_row(row)?, row.get::<_, bool>(ACCOUNT_COLUMN_COUNT)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut stmt = conn.prepare(
                "SELECT a.account_id, a.local_part || '@' || d.name, a.kind, a.created_at FROM addresses a
                 JOIN domains d ON d.id = a.domain_id ORDER BY a.kind = 'primary' DESC, 2",
            )?;
            let mut addresses: std::collections::HashMap<i64, Vec<AddressInfo>> = Default::default();
            for row in stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    AddressInfo { address: row.get(1)?, kind: row.get(2)?, created_at: row.get(3)? },
                ))
            })? {
                let (account_id, address) = row?;
                addresses.entry(account_id).or_default().push(address);
            }
            Ok(accounts
                .into_iter()
                .map(|(account, has_password)| Person {
                    addresses: addresses.remove(&account.id).unwrap_or_default(),
                    account,
                    has_password,
                })
                .collect())
        })
        .await
    }

    pub async fn person(&self, login: &str) -> Result<Option<Person>> {
        let login = login_key(login)?;
        Ok(self.people().await?.into_iter().find(|person| person.account.login == login))
    }

    /// Per domain: how many people have their main address there, and how many aliases it has.
    pub async fn domain_address_counts(&self) -> Result<std::collections::HashMap<String, (i64, i64)>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT d.name, SUM(a.kind = 'primary'), SUM(a.kind = 'alias') FROM domains d
                 JOIN addresses a ON a.domain_id = d.id GROUP BY d.name",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, (row.get(1)?, row.get(2)?))))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Addresses of an account with their kind, primary address first.
    pub async fn account_addresses(&self, login: &str) -> Result<Vec<AddressInfo>> {
        let login = login_key(login)?;
        self.read(move |conn| {
            let id = account_id(conn, &login)?;
            let mut stmt = conn.prepare(
                "SELECT a.local_part || '@' || d.name, a.kind, a.created_at FROM addresses a
                 JOIN domains d ON d.id = a.domain_id
                 WHERE a.account_id = ?1 ORDER BY a.kind = 'primary' DESC, 1",
            )?;
            let rows = stmt.query_map([id], |row| {
                Ok(AddressInfo { address: row.get(0)?, kind: row.get(1)?, created_at: row.get(2)? })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn record_audit(&self, entry: AuditEntry) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO audit_log (at, actor_id, actor, action, target, details, ip) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![now(), entry.actor_id, entry.actor, entry.action, entry.target, entry.details.to_string(), entry.ip],
            )?;
            Ok(())
        })
        .await
    }

    /// The newest changes first; pass the smallest id seen as `before` for the next page.
    pub async fn audit_log(&self, limit: usize, before: Option<i64>) -> Result<Vec<AuditRecord>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, at, actor, action, target, details, ip FROM audit_log
                 WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![before.unwrap_or(i64::MAX), limit.min(500) as i64], |row| {
                Ok(AuditRecord {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    actor: row.get(2)?,
                    action: row.get(3)?,
                    target: row.get(4)?,
                    details: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(Value::Null),
                    ip: row.get(6)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::NewAccount;
    use crate::test_support::store;

    async fn people(store: &Store) -> (Account, Account) {
        store.create_domain("example.de").await.unwrap();
        let new = |address: &str, role| NewAccount {
            address: address.into(),
            display_name: "Someone".into(),
            password: None,
            role,
            quota_bytes: 0,
        };
        let nyu = store.create_account(new("nyu@example.de", Role::Admin)).await.unwrap();
        let leni = store.create_account(new("leni@example.de", Role::User)).await.unwrap();
        (nyu, leni)
    }

    #[tokio::test]
    async fn the_last_admin_stays() {
        let (store, _dir) = store().await;
        let (nyu, _) = people(&store).await;
        let demote = AccountUpdate { role: Some(Role::User), ..Default::default() };
        let refused = store.update_account(&nyu.login, demote.clone()).await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "lastAdmin", .. })), "{refused:?}");
        let disable = AccountUpdate { disabled: Some(true), ..Default::default() };
        assert!(store.update_account(&nyu.login, disable).await.is_err());
        assert!(store.trash_account(&nyu.login).await.is_err());

        let promote = AccountUpdate { role: Some(Role::Admin), quota_bytes: Some(1024), ..Default::default() };
        let leni = store.update_account("leni@example.de", promote).await.unwrap();
        assert_eq!((leni.role, leni.quota_bytes), (Role::Admin, 1024));
        assert_eq!(store.update_account(&nyu.login, demote).await.unwrap().role, Role::User);
    }

    #[tokio::test]
    async fn the_trash_keeps_addresses_and_refuses_mail() {
        let (store, _dir) = store().await;
        let (_, leni) = people(&store).await;
        store.add_alias("hallo@example.de", &leni.login).await.unwrap();
        store.set_catch_all("example.de", Some(&leni.login)).await.unwrap();

        let trashed = store.trash_account(&leni.login).await.unwrap();
        assert!(trashed.deleted_at.is_some() && !trashed.can_log_in());
        assert_eq!(store.resolve_recipient("leni@example.de").await.unwrap(), None);
        assert_eq!(store.resolve_recipient("hallo@example.de").await.unwrap(), None);
        assert!(matches!(store.add_alias("hallo@example.de", "nyu@example.de").await, Err(StoreError::Conflict(_))));
        assert_eq!(store.purge_trash(TRASH_RETENTION_SECS).await.unwrap(), Vec::<String>::new());

        store.restore_account(&leni.login).await.unwrap();
        assert_eq!(store.resolve_recipient("hallo@example.de").await.unwrap(), Some(leni.id));
        assert_eq!(store.domain("example.de").await.unwrap().unwrap().catch_all, None, "the catch-all is not restored");

        store.trash_account(&leni.login).await.unwrap();
        assert_eq!(store.purge_trash(-1).await.unwrap(), vec!["leni@example.de".to_owned()]);
        assert!(store.account(&leni.login).await.unwrap().is_none());
        store.add_alias("hallo@example.de", "nyu@example.de").await.unwrap();
    }

    #[tokio::test]
    async fn password_links_work_once() {
        let (store, _dir) = store().await;
        let (nyu, leni) = people(&store).await;
        assert!(store.authenticate(&leni.login, "ein-gutes-passwort").await.unwrap().is_none());

        let (old, _) =
            store.create_password_link(&leni.login, PasswordLinkPurpose::Invite, Some(nyu.id), 3600).await.unwrap();
        let (token, expires_at) =
            store.create_password_link(&leni.login, PasswordLinkPurpose::Invite, Some(nyu.id), 3600).await.unwrap();
        assert!(store.password_link(&old).await.unwrap().is_none(), "a new link replaces the old one");
        let link = store.password_link(&token).await.unwrap().unwrap();
        assert_eq!(
            (link.account.login.as_str(), link.purpose, link.expires_at),
            ("leni@example.de", PasswordLinkPurpose::Invite, expires_at)
        );

        store.use_password_link(&token, "ein-gutes-passwort").await.unwrap();
        assert!(store.authenticate(&leni.login, "ein-gutes-passwort").await.unwrap().is_some());
        assert!(matches!(store.use_password_link(&token, "noch-ein-passwort").await, Err(StoreError::NotFound(_))));

        let (expired, _) = store.create_password_link(&leni.login, PasswordLinkPurpose::Reset, None, -1).await.unwrap();
        assert!(store.password_link(&expired).await.unwrap().is_none());
        store.update_account(&leni.login, AccountUpdate { disabled: Some(true), ..Default::default() }).await.unwrap();
        assert!(store.create_password_link(&leni.login, PasswordLinkPurpose::Reset, None, 3600).await.is_err());
    }

    #[tokio::test]
    async fn the_change_log_pages_backwards() {
        let (store, _dir) = store().await;
        for n in 0..3 {
            store
                .record_audit(AuditEntry {
                    actor_id: None,
                    actor: "cli".into(),
                    action: "account.create".into(),
                    target: format!("p{n}@example.de"),
                    details: json!({ "role": "user" }),
                    ip: String::new(),
                })
                .await
                .unwrap();
        }
        let first = store.audit_log(2, None).await.unwrap();
        assert_eq!(first.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(), ["p2@example.de", "p1@example.de"]);
        let next = store.audit_log(2, Some(first[1].id)).await.unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].details["role"], "user");
    }
}
