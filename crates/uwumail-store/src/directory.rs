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
    /// A mailbox that belongs to a program: it never signs in to the portal, and each protocol is
    /// switched on by itself. Stored as `kind = 'service'` beside `role = 'user'`; see migration 25.
    Service,
}

impl Role {
    /// What goes into `accounts.role`. A service is a user there, and its own column says the rest.
    fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User | Role::Service => "user",
        }
    }

    /// What goes into `accounts.kind`.
    fn kind_str(self) -> &'static str {
        match self {
            Role::Service => "service",
            _ => "person",
        }
    }

    fn parse(role: &str, kind: &str) -> Role {
        match (role, kind) {
            (_, "service") => Role::Service,
            ("admin", _) => Role::Admin,
            _ => Role::User,
        }
    }
}

/// Which protocols an account may use at all, whoever holds its password.
///
/// A person has all of them. A service is set up switch by switch, and with neither IMAP nor JMAP
/// it has no mailbox: mail to its address is refused, or sent on to [`Account::redirect_to`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Protocols {
    pub smtp: bool,
    pub imap: bool,
    pub jmap: bool,
    pub caldav: bool,
    pub carddav: bool,
}

impl Default for Protocols {
    fn default() -> Self {
        Protocols { smtp: true, imap: true, jmap: true, caldav: true, carddav: true }
    }
}

impl Protocols {
    /// What a new service starts with: mail in and out, no calendars and no address books.
    pub fn for_service() -> Protocols {
        Protocols { smtp: true, imap: true, jmap: true, caldav: false, carddav: false }
    }

    /// Whether mail can be stored for this account at all.
    pub fn has_mailbox(self) -> bool {
        self.imap || self.jmap
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
    /// Signs outgoing mail.
    pub active: bool,
    pub created_at: i64,
    /// No longer signs; its DNS record should stay a few days for mail still on its way.
    pub retired_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DkimKeyState {
    /// Created for a rotation, waiting for its DNS record before it signs.
    Pending,
    Active,
    Retired,
}

impl DkimKey {
    pub fn state(&self) -> DkimKeyState {
        if self.active {
            DkimKeyState::Active
        } else if self.retired_at.is_some() {
            DkimKeyState::Retired
        } else {
            DkimKeyState::Pending
        }
    }

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
            .field("state", &self.state())
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
    /// Locked out: cannot log in, but mail still arrives.
    pub disabled: bool,
    pub created_at: i64,
    /// In the trash since then: cannot log in and receives no mail.
    pub deleted_at: Option<i64>,
    /// When a password, app password or second factor last changed. Cached logins from before end.
    pub credentials_changed_at: i64,
    /// Which protocols this account may use at all.
    pub protocols: Protocols,
    /// Where mail goes for a service without a mailbox. Empty means its address refuses mail.
    pub redirect_to: String,
}

impl Account {
    /// Whether a password of this account counts at all, for any protocol.
    pub fn can_log_in(&self) -> bool {
        !self.disabled && self.deleted_at.is_none()
    }

    pub fn is_service(&self) -> bool {
        self.role == Role::Service
    }

    /// The web portal, and with it webmail, calendars and address books in the browser. A service
    /// never gets in: it has no password of its own, and this says so a second time.
    pub fn can_use_portal(&self) -> bool {
        self.can_log_in() && !self.is_service()
    }

    /// Whether mail is stored for this account. A send-only service has no mailbox at all.
    pub fn has_mailbox(&self) -> bool {
        self.protocols.has_mailbox()
    }
}

#[derive(Debug, Clone)]
pub struct NewAccount {
    pub address: String,
    pub display_name: String,
    /// A service has none of its own: its app passwords are the only way in.
    pub password: Option<String>,
    pub role: Role,
    /// 0 means unlimited.
    pub quota_bytes: i64,
    /// Left out means all of them for a person, and [`Protocols::for_service`] for a service.
    pub protocols: Option<Protocols>,
}

pub(crate) const ACCOUNT_COLUMNS: &str = "id, login, display_name, role, quota_bytes, used_bytes, disabled, \
     created_at, deleted_at, credentials_changed_at, kind, smtp_enabled, imap_enabled, jmap_enabled, \
     caldav_enabled, carddav_enabled, redirect_to";
/// Number of columns in [`ACCOUNT_COLUMNS`]; extra columns of a query start here.
pub(crate) const ACCOUNT_COLUMN_COUNT: usize = 17;

pub(crate) fn account_from_row(row: &Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        login: row.get(1)?,
        display_name: row.get(2)?,
        role: Role::parse(&row.get::<_, String>(3)?, &row.get::<_, String>(10)?),
        quota_bytes: row.get(4)?,
        used_bytes: row.get(5)?,
        disabled: row.get(6)?,
        created_at: row.get(7)?,
        deleted_at: row.get(8)?,
        credentials_changed_at: row.get(9)?,
        protocols: Protocols {
            smtp: row.get(11)?,
            imap: row.get(12)?,
            jmap: row.get(13)?,
            caldav: row.get(14)?,
            carddav: row.get(15)?,
        },
        redirect_to: row.get(16)?,
    })
}

pub(crate) fn domain_id(conn: &Connection, name: &str) -> Result<i64> {
    conn.query_row("SELECT id FROM domains WHERE name = ?1", [name], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("domain {name}")))
}

pub(crate) fn account_id(conn: &Connection, login: &str) -> Result<i64> {
    conn.query_row("SELECT id FROM accounts WHERE login = ?1", [login], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("account {login}")))
}

pub(crate) fn login_key(address: &str) -> Result<String> {
    let (local, domain) = normalize_address(address)?;
    Ok(format!("{local}@{domain}"))
}

/// Looks up the account an address delivers to, following sub-addresses and catch-alls. A forwarding
/// address has no account, and no catch-all takes its mail.
///
/// Disabled accounts still receive mail; accounts in the trash do not.
pub(crate) fn resolve(conn: &Connection, address: &str) -> Result<Option<i64>> {
    let Ok((local, domain)) = normalize_address(address) else {
        return Ok(None);
    };
    let Some((domain_id, catch_all)) = conn
        .query_row(
            "SELECT d.id, a.id FROM domains d
             LEFT JOIN accounts a ON a.id = d.catch_all_account_id AND a.deleted_at IS NULL
             WHERE d.name = ?1",
            [&domain],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let lookup = |local: &str| -> Result<Option<i64>> {
        Ok(conn
            .query_row(
                "SELECT a.account_id FROM addresses a JOIN accounts acc ON acc.id = a.account_id
                 WHERE a.local_part = ?1 AND a.domain_id = ?2 AND acc.deleted_at IS NULL",
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
    if crate::forward_addresses::forward_targets(conn, &local, domain_id)?.is_some() {
        return Ok(None);
    }
    if catch_all.is_some() {
        return Ok(catch_all);
    }
    // RFC 5321 requires postmaster; abuse is expected by blocklist operators.
    if matches!(base, "postmaster" | "abuse") {
        return Ok(conn
            .query_row(
                "SELECT id FROM accounts WHERE role = 'admin' AND disabled = 0 AND deleted_at IS NULL ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )
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
            let in_use: i64 = tx.query_row(
                "SELECT (SELECT count(*) FROM addresses WHERE domain_id = ?1)
                      + (SELECT count(*) FROM forward_addresses WHERE domain_id = ?1)",
                [id],
                |r| r.get(0),
            )?;
            if in_use > 0 {
                return Err(StoreError::Invalid(format!("{in_use} addresses still use {name}, remove them first")));
            }
            tx.execute("DELETE FROM domains WHERE id = ?1", [id])?;
            Ok(())
        })
        .await
    }

    /// Finishes a rotation: the pending keys sign from now on, the keys they replace are retired.
    pub async fn activate_dkim_keys(&self, domain: &str) -> Result<()> {
        let domain = normalize_domain(domain)?;
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let pending: Vec<String> = tx
                .prepare("SELECT algorithm FROM dkim_keys WHERE domain_id = ?1 AND active = 0 AND retired_at IS NULL")?
                .query_map([domain_id], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            if pending.is_empty() {
                return Err(StoreError::Rule {
                    code: "noPendingKeys",
                    message: format!("{domain} has no new keys to switch to"),
                });
            }
            let now = now();
            for algorithm in pending {
                tx.execute(
                    "UPDATE dkim_keys SET active = 0, retired_at = ?1 WHERE domain_id = ?2 AND algorithm = ?3 AND active = 1",
                    params![now, domain_id, algorithm],
                )?;
                tx.execute(
                    "UPDATE dkim_keys SET active = 1 WHERE domain_id = ?1 AND algorithm = ?2 AND active = 0 AND retired_at IS NULL",
                    params![domain_id, algorithm],
                )?;
            }
            Ok(())
        })
        .await
    }

    /// Removes a pending or retired key. Active keys cannot be removed.
    pub async fn remove_dkim_key(&self, domain: &str, selector: &str) -> Result<()> {
        let domain = normalize_domain(domain)?;
        let selector = selector.to_owned();
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let active: Option<bool> = tx
                .query_row(
                    "SELECT active FROM dkim_keys WHERE domain_id = ?1 AND selector = ?2",
                    params![domain_id, selector],
                    |row| row.get(0),
                )
                .optional()?;
            match active {
                None => Err(StoreError::NotFound(format!("DKIM key {selector} of {domain}"))),
                Some(true) => Err(StoreError::Rule {
                    code: "keyActive",
                    message: format!("{selector} still signs mail for {domain}"),
                }),
                Some(false) => {
                    tx.execute(
                        "DELETE FROM dkim_keys WHERE domain_id = ?1 AND selector = ?2",
                        params![domain_id, selector],
                    )?;
                    Ok(())
                }
            }
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

    /// Adds a signing key; `active: false` prepares it for a rotation.
    pub async fn add_dkim_key(
        &self,
        domain: &str,
        selector: &str,
        algorithm: DkimKeyAlgorithm,
        private_key: Vec<u8>,
        public_key: String,
        active: bool,
    ) -> Result<DkimKey> {
        let domain = normalize_domain(domain)?;
        let selector = selector.to_owned();
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let created_at = now();
            tx.execute(
                "INSERT INTO dkim_keys (domain_id, selector, algorithm, private_key, public_key, active, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![domain_id, selector, algorithm.as_str(), private_key, public_key, active, created_at],
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
                active,
                created_at,
                retired_at: None,
            })
        })
        .await
    }

    /// All DKIM keys of a domain, newest first.
    pub async fn dkim_keys(&self, domain: &str) -> Result<Vec<DkimKey>> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT k.id, d.name, k.selector, k.algorithm, k.private_key, k.public_key, k.active, k.created_at, k.retired_at
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
                    retired_at: row.get(8)?,
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
        // A person may use everything; a service starts with mail only, and the rest is switched on
        // one by one.
        let protocols = new.protocols.unwrap_or(match new.role {
            Role::Service => Protocols::for_service(),
            _ => Protocols::default(),
        });
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let login = format!("{local}@{domain}");
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM accounts WHERE login = ?1)",
                params![login],
                |r| r.get(0),
            )?;
            if taken || crate::forward_addresses::address_in_use(tx, &local, domain_id)? {
                return Err(StoreError::Conflict(format!("address {login}")));
            }
            let created_at = now();
            tx.execute(
                "INSERT INTO accounts (login, display_name, password_hash, role, kind, quota_bytes, created_at,
                                       smtp_enabled, imap_enabled, jmap_enabled, caldav_enabled, carddav_enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    login,
                    new.display_name.trim(),
                    password_hash,
                    new.role.as_str(),
                    new.role.kind_str(),
                    new.quota_bytes,
                    created_at,
                    protocols.smtp,
                    protocols.imap,
                    protocols.jmap,
                    protocols.caldav,
                    protocols.carddav
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO addresses (local_part, domain_id, account_id, kind, created_at) VALUES (?1, ?2, ?3, 'primary', ?4)",
                params![local, domain_id, id, created_at],
            )?;
            // A service that only sends has no mailbox, so it gets no folders either.
            if protocols.has_mailbox() {
                mail::create_default_mailboxes(tx, id)?;
            }
            Ok(Account {
                id,
                login,
                display_name: new.display_name.trim().to_owned(),
                role: new.role,
                quota_bytes: new.quota_bytes,
                used_bytes: 0,
                disabled: false,
                created_at,
                deleted_at: None,
                credentials_changed_at: 0,
                protocols,
                redirect_to: String::new(),
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

    /// Which account a message for this one is stored under: itself, or the single address a
    /// mailbox-less service hands its mail to. `None` means the address takes no mail at all.
    ///
    /// One hop only: a redirect into another account without a mailbox is no redirect. Every way
    /// in asks this, so the answer at the door and the answer at delivery cannot drift apart.
    pub async fn delivery_target(&self, account_id: i64) -> Result<Option<i64>> {
        let Some(account) = self.account_by_id(account_id).await? else { return Ok(None) };
        if account.has_mailbox() {
            return Ok(Some(account.id));
        }
        let to = account.redirect_to.trim().to_owned();
        if to.is_empty() {
            return Ok(None);
        }
        let Some(target) = self.resolve_recipient(&to).await? else { return Ok(None) };
        match self.account_by_id(target).await? {
            Some(target) if target.has_mailbox() => Ok(Some(target.id)),
            _ => Ok(None),
        }
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
            let changed = tx.execute(
                "UPDATE accounts SET password_hash = ?1, credentials_changed_at = ?2 WHERE login = ?3",
                params![hash, now(), login],
            )?;
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
                        |row| Ok((account_from_row(row)?, row.get::<_, Option<String>>(ACCOUNT_COLUMN_COUNT)?)),
                    )
                    .optional()?)
            })
            .await?;
        let (typed, stored) = (password.to_owned(), found.as_ref().and_then(|(_, hash)| hash.clone()));
        let valid = tokio::task::spawn_blocking(move || password::verify(&typed, stored.as_deref()))
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))?;
        let Some((account, hash)) = found.filter(|(account, _)| valid && account.can_log_in()) else {
            return Ok(None);
        };
        if let Some(old) = hash.filter(|hash| password::is_imported(hash)) {
            self.upgrade_imported_hash(account.id, old, password.to_owned()).await;
        }
        Ok(Some(account))
    }

    pub async fn add_alias(&self, address: &str, login: &str) -> Result<()> {
        let (local, domain) = normalize_address(address)?;
        let login = login_key(login)?;
        self.write(move |tx| {
            let domain_id = domain_id(tx, &domain)?;
            let account_id = account_id(tx, &login)?;
            // Someone who deleted this alias themselves keeps it for a while.
            let reserved_for: Option<i64> = tx
                .query_row(
                    "SELECT account_id FROM released_addresses WHERE local_part = ?1 AND domain_id = ?2 AND released_at >= ?3",
                    params![local, domain_id, now() - crate::RELEASED_ADDRESS_SECS],
                    |row| row.get(0),
                )
                .optional()?;
            if reserved_for.is_some_and(|owner| owner != account_id) {
                return Err(StoreError::Rule {
                    code: "addressReserved",
                    message: format!("{local}@{domain} was deleted recently and is still reserved"),
                });
            }
            if crate::forward_addresses::address_in_use(tx, &local, domain_id)? {
                return Err(StoreError::Conflict(format!("address {local}@{domain}")));
            }
            tx.execute("DELETE FROM released_addresses WHERE local_part = ?1 AND domain_id = ?2", params![local, domain_id])?;
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

    /// Whether the account may send as `address`: its own addresses and their sub-addresses, and every
    /// address of the domains it may send as.
    pub async fn account_owns_address(&self, account_id: i64, address: &str) -> Result<bool> {
        let address = address.to_owned();
        self.read(move |conn| crate::extras::owns(conn, account_id, &address)).await
    }

    /// The domains an account may send as with any address.
    pub async fn send_as_domains(&self, account_id: i64) -> Result<Vec<String>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT d.name FROM send_as_domains s JOIN domains d ON d.id = s.domain_id
                 WHERE s.account_id = ?1 ORDER BY d.name",
            )?;
            let rows = stmt.query_map([account_id], |row| row.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Replaces the domains an account may send as with any address.
    pub async fn set_send_as_domains(&self, account_id: i64, domains: Vec<String>) -> Result<Vec<String>> {
        let domains = domains.iter().map(|name| normalize_domain(name)).collect::<Result<Vec<_>>>()?;
        self.write(move |tx| {
            let ids = domains.iter().map(|name| domain_id(tx, name)).collect::<Result<Vec<_>>>()?;
            tx.execute("DELETE FROM send_as_domains WHERE account_id = ?1", [account_id])?;
            for id in ids {
                tx.execute(
                    "INSERT OR IGNORE INTO send_as_domains (account_id, domain_id, created_at) VALUES (?1, ?2, ?3)",
                    params![account_id, id, now()],
                )?;
            }
            Ok(())
        })
        .await?;
        self.send_as_domains(account_id).await
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
            protocols: None,
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
        assert_eq!(
            store.resolve_recipient("mini@example.de").await.unwrap(),
            Some(mini.id),
            "disabled people still get mail"
        );
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
        store.create_domain("verein.de").await.unwrap();
        let domains = vec!["Verein.de".into(), "verein.de".into()];
        assert_eq!(store.set_send_as_domains(mini.id, domains).await.unwrap(), vec!["verein.de"]);
        assert!(store.account_owns_address(mini.id, "vorstand@verein.de").await.unwrap(), "any address of it");
        assert!(!store.account_owns_address(mini.id, "ami@example.de").await.unwrap(), "not other domains");
        assert!(store.set_send_as_domains(mini.id, vec!["elsewhere.de".into()]).await.is_err());
        assert!(store.set_send_as_domains(mini.id, vec![]).await.unwrap().is_empty());
        assert!(!store.account_owns_address(mini.id, "vorstand@verein.de").await.unwrap());
        assert_eq!(store.addresses("mini@example.de").await.unwrap(), vec!["mini@example.de", "kontakt@example.de"]);

        store.remove_alias("kontakt@example.de").await.unwrap();
        assert_eq!(store.resolve_recipient("kontakt@example.de").await.unwrap(), Some(ami.id));
    }

    #[tokio::test]
    async fn dkim_keys_round_trip() {
        let (store, _dir) = store().await;
        store.create_domain("example.de").await.unwrap();
        let key = store
            .add_dkim_key("example.de", "uwu1", DkimKeyAlgorithm::Ed25519Sha256, vec![1, 2, 3], "cHVibGlj".into(), true)
            .await
            .unwrap();
        assert_eq!(key.dns_record(), ("uwu1._domainkey.example.de".into(), "v=DKIM1; k=ed25519; p=cHVibGlj".into()));
        let keys = store.dkim_keys("example.de").await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].private_key, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn dkim_rotation() {
        let (store, _dir) = store().await;
        store.create_domain("example.de").await.unwrap();
        let add = |selector: &'static str, active| {
            let store = store.clone();
            async move {
                store
                    .add_dkim_key("example.de", selector, DkimKeyAlgorithm::RsaSha256, vec![1], "a2V5".into(), active)
                    .await
                    .unwrap()
            }
        };
        add("old", true).await;
        let refused = store.activate_dkim_keys("example.de").await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "noPendingKeys", .. })));
        assert_eq!(add("new", false).await.state(), DkimKeyState::Pending);

        store.activate_dkim_keys("example.de").await.unwrap();
        let keys = store.dkim_keys("example.de").await.unwrap();
        let states: Vec<_> = keys.iter().map(|k| (k.selector.as_str(), k.state())).collect();
        assert!(states.contains(&("new", DkimKeyState::Active)));
        assert!(states.contains(&("old", DkimKeyState::Retired)));

        let refused = store.remove_dkim_key("example.de", "new").await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "keyActive", .. })));
        store.remove_dkim_key("example.de", "old").await.unwrap();
        assert_eq!(store.dkim_keys("example.de").await.unwrap().len(), 1);
    }
}
