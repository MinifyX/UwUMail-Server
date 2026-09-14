//! Domains, accounts, addresses and DKIM keys.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::{base_local_part, normalize_address, normalize_domain};
use crate::{Result, Store, StoreError, mail, now, password};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }

    fn parse(value: &str) -> Role {
        if value == "admin" { Role::Admin } else { Role::User }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Domain {
    pub id: i64,
    pub name: String,
    /// Login of the account that receives mail for unknown addresses.
    pub catch_all: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DkimKeyAlgorithm {
    #[serde(rename = "rsa-sha256")]
    RsaSha256,
    #[serde(rename = "ed25519-sha256")]
    Ed25519Sha256,
}

impl DkimKeyAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            DkimKeyAlgorithm::RsaSha256 => "rsa-sha256",
            DkimKeyAlgorithm::Ed25519Sha256 => "ed25519-sha256",
        }
    }

    /// The `k=` tag of the DNS record.
    pub fn dns_key_type(self) -> &'static str {
        match self {
            DkimKeyAlgorithm::RsaSha256 => "rsa",
            DkimKeyAlgorithm::Ed25519Sha256 => "ed25519",
        }
    }

    fn parse(value: &str) -> DkimKeyAlgorithm {
        if value == "ed25519-sha256" { DkimKeyAlgorithm::Ed25519Sha256 } else { DkimKeyAlgorithm::RsaSha256 }
    }
}

#[derive(Clone, Serialize)]
pub struct DkimKey {
    pub id: i64,
    pub domain: String,
    pub selector: String,
    pub algorithm: DkimKeyAlgorithm,
    /// PKCS#8 DER.
    #[serde(skip)]
    pub private_key: Vec<u8>,
    pub public_key: String,
    pub active: bool,
    pub created_at: i64,
}

impl DkimKey {
    /// Name and value of the TXT record that publishes this key.
    pub fn dns_record(&self) -> (String, String) {
        (
            format!("{}._domainkey.{}", self.selector, self.domain),
            format!("v=DKIM1; k={}; p={}", self.algorithm.dns_key_type(), self.public_key),
        )
    }
}

impl std::fmt::Debug for DkimKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DkimKey")
            .field("domain", &self.domain)
            .field("selector", &self.selector)
            .field("algorithm", &self.algorithm)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: i64,
    pub login: String,
    pub display_name: String,
    pub role: Role,
    pub quota_bytes: i64,
    pub used_bytes: i64,
    pub disabled: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewAccount {
    pub address: String,
    pub display_name: String,
    pub password: Option<String>,
    pub role: Role,
    /// 0 means unlimited.
    pub quota_bytes: i64,
}

const ACCOUNT_COLUMNS: &str = "id, login, display_name, role, quota_bytes, used_bytes, disabled, created_at";

fn account_from_row(row: &Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        login: row.get(1)?,
        display_name: row.get(2)?,
        role: Role::parse(&row.get::<_, String>(3)?),
        quota_bytes: row.get(4)?,
        used_bytes: row.get(5)?,
        disabled: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn domain_id(conn: &Connection, name: &str) -> Result<i64> {
    conn.query_row("SELECT id FROM domains WHERE name = ?1", [name], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("domain {name}")))
}

fn account_id(conn: &Connection, login: &str) -> Result<i64> {
    conn.query_row("SELECT id FROM accounts WHERE login = ?1", [login], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("account {login}")))
}

fn login_key(address: &str) -> Result<String> {
    let (local, domain) = normalize_address(address)?;
    Ok(format!("{local}@{domain}"))
}

/// Looks up the account an address delivers to, following sub-addresses and catch-alls.
pub(crate) fn resolve(conn: &Connection, address: &str) -> Result<Option<i64>> {
    let Ok((local, domain)) = normalize_address(address) else {
        return Ok(None);
    };
    let Some((domain_id, catch_all)) = conn
        .query_row("SELECT id, catch_all_account_id FROM domains WHERE name = ?1", [&domain], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
        })
        .optional()?
    else {
        return Ok(None);
    };
    let lookup = |local: &str| -> Result<Option<i64>> {
        Ok(conn
            .query_row(
                "SELECT a.account_id FROM addresses a JOIN accounts acc ON acc.id = a.account_id
                 WHERE a.local_part = ?1 AND a.domain_id = ?2 AND acc.disabled = 0",
                params![local, domain_id],
                |row| row.get(0),
            )
            .optional()?)
    };
    if let Some(id) = lookup(&local)? {
        return Ok(Some(id));
    }
    let base = base_local_part(&local);
    if base != local
        && let Some(id) = lookup(base)?
    {
        return Ok(Some(id));
    }
    if catch_all.is_some() {
        return Ok(catch_all);
    }
    // RFC 5321 requires postmaster; abuse is expected by blocklist operators.
    if matches!(base, "postmaster" | "abuse") {
        return Ok(conn
            .query_row("SELECT id FROM accounts WHERE role = 'admin' AND disabled = 0 ORDER BY id LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?);
    }
    Ok(None)
}

impl Store {
    pub async fn create_domain(&self, name: &str) -> Result<Domain> {
        let name = normalize_domain(name)?;
        self.write(move |tx| {
            let exists: bool =
                tx.query_row("SELECT EXISTS (SELECT 1 FROM domains WHERE name = ?1)", [&name], |r| r.get(0))?;
            if exists {
                return Err(StoreError::Conflict(format!("domain {name}")));
            }
            let created_at = now();
            tx.execute("INSERT INTO domains (name, created_at) VALUES (?1, ?2)", params![name, created_at])?;
            Ok(Domain { id: tx.last_insert_rowid(), name, catch_all: None, created_at })
        })
        .await
    }

    pub async fn domains(&self) -> Result<Vec<Domain>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT d.id, d.name, a.login, d.created_at FROM domains d
                 LEFT JOIN accounts a ON a.id = d.catch_all_account_id ORDER BY d.name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(Domain { id: row.get(0)?, name: row.get(1)?, catch_all: row.get(2)?, created_at: row.get(3)? })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn domain(&self, name: &str) -> Result<Option<Domain>> {
        let name = normalize_domain(name)?;
        Ok(self.domains().await?.into_iter().find(|d| d.name == name))
    }

    /// Deletes a domain. Refuses while addresses still use it.
    pub async fn delete_domain(&self, name: &str) -> Result<()> {
        let name = normalize_domain(name)?;
        self.write(move |tx| {
            let id = domain_id(tx, &name)?;
            let in_use: i64 =
                tx.query_row("SELECT count(*) FROM addresses WHERE domain_id = ?1", [id], |r| r.get(0))?;
            if in_use > 0 {
                return Err(StoreError::Invalid(format!("{in_use} addresses still use {name}, remove them first")));
            }
            tx.execute("DELETE FROM domains WHERE id = ?1", [id])?;
            Ok(())
        })
        .await
    }

    pub async fn set_catch_all(&self, domain: &str, login: Option<&str>) -> Result<()> {
        let domain = normalize_domain(domain)?;
        let login = login.map(login_key).transpose()?;
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let account = login.as_deref().map(|login| account_id(tx, login)).transpose()?;
            tx.execute("UPDATE domains SET catch_all_account_id = ?1 WHERE id = ?2", params![account, domain_id])?;
            Ok(())
        })
        .await
    }

    pub async fn is_local_domain(&self, domain: &str) -> Result<bool> {
        let Ok(domain) = normalize_domain(domain) else {
            return Ok(false);
        };
        self.read(move |conn| {
            Ok(conn.query_row("SELECT EXISTS (SELECT 1 FROM domains WHERE name = ?1)", [domain], |r| r.get(0))?)
        })
        .await
    }

    pub async fn add_dkim_key(
        &self,
        domain: &str,
        selector: &str,
        algorithm: DkimKeyAlgorithm,
        private_key: Vec<u8>,
        public_key: String,
    ) -> Result<DkimKey> {
        let domain = normalize_domain(domain)?;
        let selector = selector.to_owned();
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let created_at = now();
            tx.execute(
                "INSERT INTO dkim_keys (domain_id, selector, algorithm, private_key, public_key, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![domain_id, selector, algorithm.as_str(), private_key, public_key, created_at],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                    StoreError::Conflict(format!("DKIM selector {selector} for {domain}"))
                }
                other => other.into(),
            })?;
            Ok(DkimKey {
                id: tx.last_insert_rowid(),
                domain,
                selector,
                algorithm,
                private_key,
                public_key,
                active: true,
                created_at,
            })
        })
        .await
    }

    /// All DKIM keys of a domain, newest first.
    pub async fn dkim_keys(&self, domain: &str) -> Result<Vec<DkimKey>> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT k.id, d.name, k.selector, k.algorithm, k.private_key, k.public_key, k.active, k.created_at
                 FROM dkim_keys k JOIN domains d ON d.id = k.domain_id
                 WHERE d.name = ?1 ORDER BY k.created_at DESC, k.id DESC",
            )?;
            let rows = stmt.query_map([domain], |row| {
                Ok(DkimKey {
                    id: row.get(0)?,
                    domain: row.get(1)?,
                    selector: row.get(2)?,
                    algorithm: DkimKeyAlgorithm::parse(&row.get::<_, String>(3)?),
                    private_key: row.get(4)?,
                    public_key: row.get(5)?,
                    active: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn create_account(&self, new: NewAccount) -> Result<Account> {
        let (local, domain) = normalize_address(&new.address)?;
        if new.quota_bytes < 0 {
            return Err(StoreError::Invalid("the quota cannot be negative".into()));
        }
        let password_hash = match new.password {
            Some(password) => Some(
                tokio::task::spawn_blocking(move || password::hash(&password))
                    .await
                    .map_err(|err| StoreError::Internal(err.to_string()))??,
            ),
            None => None,
        };
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let login = format!("{local}@{domain}");
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM addresses WHERE local_part = ?1 AND domain_id = ?2)
                     OR EXISTS (SELECT 1 FROM accounts WHERE login = ?3)",
                params![local, domain_id, login],
                |r| r.get(0),
            )?;
            if taken {
                return Err(StoreError::Conflict(format!("address {login}")));
            }
            let created_at = now();
            tx.execute(
                "INSERT INTO accounts (login, display_name, password_hash, role, quota_bytes, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![login, new.display_name.trim(), password_hash, new.role.as_str(), new.quota_bytes, created_at],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO addresses (local_part, domain_id, account_id, kind, created_at) VALUES (?1, ?2, ?3, 'primary', ?4)",
                params![local, domain_id, id, created_at],
            )?;
            mail::create_default_mailboxes(tx, id)?;
            Ok(Account {
                id,
                login,
                display_name: new.display_name.trim().to_owned(),
                role: new.role,
                quota_bytes: new.quota_bytes,
                used_bytes: 0,
                disabled: false,
                created_at,
            })
        })
        .await
    }

    pub async fn accounts(&self) -> Result<Vec<Account>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(&format!("SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY login"))?;
            let rows = stmt.query_map([], account_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn account(&self, login: &str) -> Result<Option<Account>> {
        let login = login_key(login)?;
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE login = ?1"),
                    [login],
                    account_from_row,
                )
                .optional()?)
        })
        .await
    }

    pub async fn account_by_id(&self, id: i64) -> Result<Option<Account>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(&format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1"), [id], account_from_row)
                .optional()?)
        })
        .await
    }

    pub async fn delete_account(&self, login: &str) -> Result<()> {
        let login = login_key(login)?;
        self.write(move |tx| {
            let id = account_id(tx, &login)?;
            // Threads are only referenced by emails of the same account.
            tx.execute("DELETE FROM emails WHERE account_id = ?1", [id])?;
            tx.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
            Ok(())
        })
        .await
    }

    pub async fn set_password(&self, login: &str, new_password: &str) -> Result<()> {
        let login = login_key(login)?;
        let new_password = new_password.to_owned();
        let hash = tokio::task::spawn_blocking(move || password::hash(&new_password))
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))??;
        self.write(move |tx| {
            let changed =
                tx.execute("UPDATE accounts SET password_hash = ?1 WHERE login = ?2", params![hash, login])?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("account {login}")));
            }
            Ok(())
        })
        .await
    }

    pub async fn set_account_disabled(&self, login: &str, disabled: bool) -> Result<()> {
        let login = login_key(login)?;
        self.write(move |tx| {
            let changed = tx.execute("UPDATE accounts SET disabled = ?1 WHERE login = ?2", params![disabled, login])?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("account {login}")));
            }
            Ok(())
        })
        .await
    }

    /// Checks a login and password. Unknown logins take as long as wrong passwords.
    pub async fn authenticate(&self, login: &str, password: &str) -> Result<Option<Account>> {
        let login = login_key(login).unwrap_or_default();
        let found = self
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        &format!("SELECT {ACCOUNT_COLUMNS}, password_hash FROM accounts WHERE login = ?1"),
                        [login],
                        |row| Ok((account_from_row(row)?, row.get::<_, Option<String>>(8)?)),
                    )
                    .optional()?)
            })
            .await?;
        let password = password.to_owned();
        tokio::task::spawn_blocking(move || {
            let (account, hash) = match found {
                Some((account, hash)) => (Some(account), hash),
                None => (None, None),
            };
            let valid = password::verify(&password, hash.as_deref());
            Ok(account.filter(|account| valid && !account.disabled))
        })
        .await
        .map_err(|err| StoreError::Internal(err.to_string()))?
    }

    pub async fn add_alias(&self, address: &str, login: &str) -> Result<()> {
        let (local, domain) = normalize_address(address)?;
        let login = login_key(login)?;
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let account_id = account_id(tx, &login)?;
            tx.execute(
                "INSERT INTO addresses (local_part, domain_id, account_id, kind, created_at) VALUES (?1, ?2, ?3, 'alias', ?4)",
                params![local, domain_id, account_id, now()],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                    StoreError::Conflict(format!("address {local}@{domain}"))
                }
                other => other.into(),
            })?;
            Ok(())
        })
        .await
    }

    pub async fn remove_alias(&self, address: &str) -> Result<()> {
        let (local, domain) = normalize_address(address)?;
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let removed = tx.execute(
                "DELETE FROM addresses WHERE local_part = ?1 AND domain_id = ?2 AND kind = 'alias'",
                params![local, domain_id],
            )?;
            if removed == 0 {
                return Err(StoreError::NotFound(format!("alias {local}@{domain}")));
            }
            Ok(())
        })
        .await
    }

    /// All addresses of an account, primary address first.
    pub async fn addresses(&self, login: &str) -> Result<Vec<String>> {
        let login = login_key(login)?;
        self.read(move |conn| {
            let id = account_id(conn, &login)?;
            let mut stmt = conn.prepare(
                "SELECT a.local_part || '@' || d.name FROM addresses a JOIN domains d ON d.id = a.domain_id
                 WHERE a.account_id = ?1 ORDER BY a.kind = 'primary' DESC, 1",
            )?;
            let rows = stmt.query_map([id], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// The account that receives mail for `address`, if it is hosted here.
    pub async fn resolve_recipient(&self, address: &str) -> Result<Option<i64>> {
        let address = address.to_owned();
        self.read(move |conn| resolve(conn, &address)).await
    }

    /// Whether the account may send as `address` (its own addresses and their sub-addresses).
    pub async fn account_owns_address(&self, account_id: i64, address: &str) -> Result<bool> {
        let Ok((local, domain)) = normalize_address(address) else {
            return Ok(false);
        };
        self.read(move |conn| {
            let base = base_local_part(&local).to_owned();
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM addresses a JOIN domains d ON d.id = a.domain_id
                 WHERE a.account_id = ?1 AND d.name = ?2 AND a.local_part IN (?3, ?4))",
                params![account_id, domain, local, base],
                |r| r.get(0),
            )?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;

    fn person(address: &str) -> NewAccount {
        NewAccount {
            address: address.into(),
            display_name: "Mini".into(),
            password: Some("katzenpfote".into()),
            role: Role::User,
            quota_bytes: 0,
        }
    }

    #[tokio::test]
    async fn domains_and_accounts() {
        let (store, _dir) = store().await;
        store.create_domain("Example.DE").await.unwrap();
        assert!(matches!(store.create_domain("example.de").await, Err(StoreError::Conflict(_))));
        assert!(matches!(store.create_account(person("mini@nowhere.de")).await, Err(StoreError::NotFound(_))));

        let mini = store.create_account(person("Mini@example.de")).await.unwrap();
        assert_eq!(mini.login, "mini@example.de");
        assert!(matches!(store.create_account(person("mini@example.de")).await, Err(StoreError::Conflict(_))));
        assert_eq!(store.accounts().await.unwrap().len(), 1);
        assert!(matches!(store.delete_domain("example.de").await, Err(StoreError::Invalid(_))));

        assert!(store.authenticate("MINI@example.de", "katzenpfote").await.unwrap().is_some());
        assert!(store.authenticate("mini@example.de", "wrong").await.unwrap().is_none());
        assert!(store.authenticate("ghost@example.de", "katzenpfote").await.unwrap().is_none());

        store.set_password("mini@example.de", "neues-passwort").await.unwrap();
        assert!(store.authenticate("mini@example.de", "neues-passwort").await.unwrap().is_some());

        store.set_account_disabled("mini@example.de", true).await.unwrap();
        assert!(store.authenticate("mini@example.de", "neues-passwort").await.unwrap().is_none());
        assert_eq!(store.resolve_recipient("mini@example.de").await.unwrap(), None);
    }

    #[tokio::test]
    async fn recipient_resolution() {
        let (store, _dir) = store().await;
        store.create_domain("example.de").await.unwrap();
        let mini = store.create_account(person("mini@example.de")).await.unwrap();
        let ami = store.create_account(NewAccount { role: Role::Admin, ..person("ami@example.de") }).await.unwrap();
        store.add_alias("kontakt@example.de", "mini@example.de").await.unwrap();

        assert_eq!(store.resolve_recipient("mini@example.de").await.unwrap(), Some(mini.id));
        assert_eq!(store.resolve_recipient("<MINI+shop@Example.de>").await.unwrap(), Some(mini.id));
        assert_eq!(store.resolve_recipient("kontakt@example.de").await.unwrap(), Some(mini.id));
        assert_eq!(store.resolve_recipient("postmaster@example.de").await.unwrap(), Some(ami.id));
        assert_eq!(store.resolve_recipient("ghost@example.de").await.unwrap(), None);
        assert_eq!(store.resolve_recipient("mini@elsewhere.de").await.unwrap(), None);

        store.set_catch_all("example.de", Some("ami@example.de")).await.unwrap();
        assert_eq!(store.resolve_recipient("ghost@example.de").await.unwrap(), Some(ami.id));

        assert!(store.account_owns_address(mini.id, "kontakt+x@example.de").await.unwrap());
        assert!(!store.account_owns_address(mini.id, "ami@example.de").await.unwrap());
        assert_eq!(store.addresses("mini@example.de").await.unwrap(), vec!["mini@example.de", "kontakt@example.de"]);

        store.remove_alias("kontakt@example.de").await.unwrap();
        assert_eq!(store.resolve_recipient("kontakt@example.de").await.unwrap(), Some(ami.id));
    }

    #[tokio::test]
    async fn dkim_keys_round_trip() {
        let (store, _dir) = store().await;
        store.create_domain("example.de").await.unwrap();
        let key = store
            .add_dkim_key("example.de", "uwu1", DkimKeyAlgorithm::Ed25519Sha256, vec![1, 2, 3], "cHVibGlj".into())
            .await
            .unwrap();
        assert_eq!(key.dns_record(), ("uwu1._domainkey.example.de".into(), "v=DKIM1; k=ed25519; p=cHVibGlj".into()));
        let keys = store.dkim_keys("example.de").await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].private_key, vec![1, 2, 3]);
    }
}
