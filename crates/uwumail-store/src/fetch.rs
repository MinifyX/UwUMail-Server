//! Fetch accounts: mailboxes at other providers that this server empties into someone's mailbox.
//!
//! What is kept here is the settings, where each folder stands and what came before. The fetching
//! itself is in the server crate, and what happens to a fetched message is the same thing that
//! happens to a message another server hands in: it goes through the spam filter and lands in the
//! inbox or in Junk.
//!
//! The provider's password has to be sent to the provider, so unlike our own passwords it cannot be
//! hashed. It is sealed with AES-256-GCM under a key of this server. The key sits in the settings
//! table of the same database, so this keeps the password out of an extract, a log line or a glance
//! at the table -- it is not a defence against somebody who holds the whole database.

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::normalize_address;
use crate::db::{get_setting, set_setting};
use crate::{Result, Store, StoreError, now, random_bytes};

/// How many mailboxes one person may have this server empty for them.
pub const MAX_FETCH_ACCOUNTS: usize = 10;
/// Asking a provider more often than this is neither polite nor useful.
pub const MIN_FETCH_INTERVAL_SECS: i64 = 60;
pub const MAX_FETCH_INTERVAL_SECS: i64 = 6 * 3600;
pub const DEFAULT_FETCH_INTERVAL_SECS: i64 = 300;
/// A message that the server keeps asking to bring later is stepped over after this, so one
/// message can never block a folder for good.
pub const FETCH_HOLD_LIMIT_SECS: i64 = 24 * 3600;
/// How long a fetched message is remembered by name, so a provider that renumbers its folders does
/// not deliver everything a second time.
pub const FETCH_SEEN_SECS: i64 = 30 * 24 * 3600;
/// Where the key for sealing provider passwords is kept.
const SECRET_KEY: &str = "fetch.secret_key";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchSecurity {
    /// TLS from the first byte, port 993.
    Tls,
    /// A plain connection that is upgraded with STARTTLS, port 143.
    Starttls,
}

impl FetchSecurity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::Starttls => "starttls",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tls" => Some(Self::Tls),
            "starttls" => Some(Self::Starttls),
            _ => None,
        }
    }
}

/// How the provider's outgoing server is reached. Never unencrypted: this sends a password across
/// the internet, not across a machine room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SendSecurity {
    /// A plain connection upgraded with STARTTLS, usually port 587.
    Starttls,
    /// TLS from the first byte, usually port 465.
    Tls,
}

impl SendSecurity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starttls => "starttls",
            Self::Tls => "tls",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "starttls" => Some(Self::Starttls),
            "tls" => Some(Self::Tls),
            _ => None,
        }
    }
}

/// Where a fetched address sends its mail, and with which login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchSender {
    pub account_id: i64,
    pub address: String,
    pub host: String,
    pub port: u16,
    pub security: SendSecurity,
    pub username: String,
    pub password: String,
}

/// What happens to a message at the provider once this server has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AfterFetch {
    /// Marked as read and left where it is. Nothing is lost if a run goes wrong.
    MarkRead,
    /// Deleted there. Good for a mailbox one only keeps because a service insists on it.
    Delete,
}

impl AfterFetch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MarkRead => "mark_read",
            Self::Delete => "delete",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mark_read" => Some(Self::MarkRead),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchAccount {
    pub id: i64,
    pub account_id: i64,
    pub address: String,
    pub host: String,
    pub port: u16,
    pub security: FetchSecurity,
    pub username: String,
    pub after_fetch: AfterFetch,
    pub fetch_junk: bool,
    pub interval_secs: i64,
    pub enabled: bool,
    pub auth_serv_id: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: SendSecurity,
    pub send_enabled: bool,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_error: String,
    pub last_fetched: i64,
    pub total_fetched: i64,
}

#[derive(Debug, Clone)]
pub struct NewFetchAccount {
    pub account_id: i64,
    pub address: String,
    pub host: String,
    pub port: u16,
    pub security: FetchSecurity,
    pub username: String,
    pub password: String,
    pub after_fetch: AfterFetch,
    pub fetch_junk: bool,
    pub interval_secs: i64,
    pub auth_serv_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct FetchAccountUpdate {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub security: Option<FetchSecurity>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub after_fetch: Option<AfterFetch>,
    pub fetch_junk: Option<bool>,
    pub interval_secs: Option<i64>,
    pub enabled: Option<bool>,
    pub auth_serv_id: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_security: Option<SendSecurity>,
    pub send_enabled: Option<bool>,
}

/// Where one folder of a fetch account stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FetchFolder {
    pub uid_validity: i64,
    pub last_uid: i64,
    /// A message the server asked us to bring later; the folder waits at it.
    pub held_uid: Option<i64>,
    pub held_since: Option<i64>,
}

impl FetchFolder {
    /// Whether a held message has been waiting so long that it should be stepped over.
    pub fn hold_expired(&self, at: i64) -> bool {
        self.held_since.is_some_and(|since| at - since > FETCH_HOLD_LIMIT_SECS)
    }
}

const COLUMNS: &str = "id, account_id, address, host, port, security, username, after_fetch, fetch_junk, \
                       interval_secs, enabled, auth_serv_id, smtp_host, smtp_port, smtp_security, send_enabled, \
                       created_at, last_run_at, last_ok_at, last_error, last_fetched, total_fetched";

fn from_row(row: &Row<'_>) -> rusqlite::Result<FetchAccount> {
    let security: String = row.get(5)?;
    let after: String = row.get(7)?;
    let sending: String = row.get(14)?;
    Ok(FetchAccount {
        id: row.get(0)?,
        account_id: row.get(1)?,
        address: row.get(2)?,
        host: row.get(3)?,
        port: row.get::<_, i64>(4)? as u16,
        security: FetchSecurity::parse(&security).unwrap_or(FetchSecurity::Tls),
        username: row.get(6)?,
        after_fetch: AfterFetch::parse(&after).unwrap_or(AfterFetch::MarkRead),
        fetch_junk: row.get(8)?,
        interval_secs: row.get(9)?,
        enabled: row.get(10)?,
        auth_serv_id: row.get(11)?,
        smtp_host: row.get(12)?,
        smtp_port: row.get::<_, i64>(13)? as u16,
        smtp_security: SendSecurity::parse(&sending).unwrap_or(SendSecurity::Starttls),
        send_enabled: row.get(15)?,
        created_at: row.get(16)?,
        last_run_at: row.get(17)?,
        last_ok_at: row.get(18)?,
        last_error: row.get(19)?,
        last_fetched: row.get(20)?,
        total_fetched: row.get(21)?,
    })
}

/// The key that seals provider passwords, made on first use.
fn secret_key(conn: &Connection) -> Result<[u8; 32]> {
    if let Some(value) = get_setting(conn, SECRET_KEY)?
        && let Ok(bytes) = hex::decode(value)
        && let Ok(key) = <[u8; 32]>::try_from(bytes)
    {
        return Ok(key);
    }
    let key = random_bytes::<32>();
    set_setting(conn, SECRET_KEY, &hex::encode(key))?;
    Ok(key)
}

fn cipher(key: &[u8; 32]) -> Result<LessSafeKey> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| StoreError::Internal("the key for provider passwords is unusable".into()))?;
    Ok(LessSafeKey::new(unbound))
}

/// Seals a password: a fresh nonce, then the sealed bytes behind it.
fn seal(conn: &Connection, password: &str) -> Result<Vec<u8>> {
    let key = cipher(&secret_key(conn)?)?;
    let nonce = random_bytes::<NONCE_LEN>();
    let mut sealed = password.as_bytes().to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut sealed)
        .map_err(|_| StoreError::Internal("sealing a provider password failed".into()))?;
    let mut out = nonce.to_vec();
    out.append(&mut sealed);
    Ok(out)
}

fn unseal(conn: &Connection, sealed: &[u8]) -> Result<String> {
    if sealed.len() <= NONCE_LEN {
        return Err(StoreError::Internal("a stored provider password is too short to be one".into()));
    }
    let key = cipher(&secret_key(conn)?)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&sealed[..NONCE_LEN]);
    let mut buffer = sealed[NONCE_LEN..].to_vec();
    let plain = key
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buffer)
        .map_err(|_| StoreError::Internal("a stored provider password could not be read".into()))?;
    String::from_utf8(plain.to_vec()).map_err(|_| StoreError::Internal("a stored provider password is not text".into()))
}

/// Keeps a provider's error message short enough for a table and a page.
fn shorten(value: &str, max: usize) -> String {
    let value = value.trim();
    match value.char_indices().nth(max) {
        Some((cut, _)) => format!("{}...", &value[..cut]),
        None => value.to_owned(),
    }
}

fn check_interval(secs: i64) -> Result<i64> {
    if !(MIN_FETCH_INTERVAL_SECS..=MAX_FETCH_INTERVAL_SECS).contains(&secs) {
        return Err(StoreError::Invalid(format!(
            "fetch every {MIN_FETCH_INTERVAL_SECS} to {MAX_FETCH_INTERVAL_SECS} seconds"
        )));
    }
    Ok(secs)
}

/// Whether an IP address is on the open internet, not this machine, the local network or a reserved
/// range. A fetched mailbox and its outgoing server live somewhere else, so a private or loopback
/// address would only point the worker at this host or the LAN (security-audit-0.5.2 S-10).
fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip.to_canonical() {
        std::net::IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (b & 0xc0) == 64))
        }
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

fn check_host(host: &str) -> Result<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || host.contains(char::is_whitespace) {
        return Err(StoreError::Invalid(format!("'{host}' is not a server name")));
    }
    // An IP literal must be public. A name is re-checked when the connection is made, so it cannot
    // resolve to a private address either.
    if let Ok(ip) = host.parse::<std::net::IpAddr>()
        && !is_public_ip(ip)
    {
        return Err(StoreError::Invalid(format!("'{host}' is not a public address")));
    }
    Ok(host)
}

impl Store {
    /// The fetch accounts of one person, or of everyone.
    pub async fn fetch_accounts(&self, account_id: Option<i64>) -> Result<Vec<FetchAccount>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM fetch_accounts
                 WHERE ?1 IS NULL OR account_id = ?1 ORDER BY address"
            ))?;
            let rows = stmt.query_map(params![account_id], from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// One fetched mailbox of this person. Every single-row function takes whose it is and asks for
    /// both, so a row id from a URL can never reach somebody else's mailbox -- not even through a
    /// caller that forgot to check.
    pub async fn fetch_account(&self, account_id: i64, id: i64) -> Result<Option<FetchAccount>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM fetch_accounts WHERE id = ?1 AND account_id = ?2"),
                    params![id, account_id],
                    from_row,
                )
                .optional()?)
        })
        .await
    }

    /// The accounts whose turn it is, oldest run first. A run that failed waits the same interval.
    pub async fn fetch_accounts_due(&self) -> Result<Vec<FetchAccount>> {
        let at = now();
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM fetch_accounts
                 WHERE enabled = 1
                   AND account_id IN (SELECT id FROM accounts WHERE deleted_at IS NULL)
                   AND (last_run_at IS NULL OR last_run_at + interval_secs <= ?1)
                 ORDER BY last_run_at IS NOT NULL, last_run_at"
            ))?;
            let rows = stmt.query_map(params![at], from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// The password to send to the provider. It opens a mailbox somewhere else, so this one asks
    /// whose it is even more than the others do.
    pub async fn fetch_password(&self, account_id: i64, id: i64) -> Result<Option<String>> {
        self.read(move |conn| {
            let sealed: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT password FROM fetch_accounts WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                    |row| row.get(0),
                )
                .optional()?;
            sealed.map(|sealed| unseal(conn, &sealed)).transpose()
        })
        .await
    }

    pub async fn create_fetch_account(&self, new: NewFetchAccount) -> Result<FetchAccount> {
        let (local, domain) = normalize_address(&new.address)
            .map_err(|_| StoreError::Invalid(format!("'{}' is not a valid email address", new.address)))?;
        let address = format!("{local}@{domain}");
        // A fetched mailbox is one somewhere else. Refuse an address of a domain hosted here: it
        // would let an account register (and, once send is on, send as) a local address it does not
        // own -- the admin's, or another person's -- signed with this server's own key.
        if self.is_local_domain(&domain).await? {
            return Err(StoreError::Invalid(
                "this address is hosted on this server; a fetched mailbox is for a mailbox elsewhere".into(),
            ));
        }
        let host = check_host(&new.host)?;
        let interval = check_interval(new.interval_secs)?;
        let username = new.username.trim().to_owned();
        if username.is_empty() {
            return Err(StoreError::Invalid("the provider needs a user name".into()));
        }
        if new.password.is_empty() {
            return Err(StoreError::Invalid("the provider needs a password".into()));
        }
        let at = now();
        self.write(move |tx| {
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM fetch_accounts WHERE account_id = ?1",
                params![new.account_id],
                |row| row.get(0),
            )?;
            if count as usize >= MAX_FETCH_ACCOUNTS {
                return Err(StoreError::Invalid(format!("at most {MAX_FETCH_ACCOUNTS} fetched mailboxes")));
            }
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM fetch_accounts WHERE account_id = ?1 AND address = ?2)",
                params![new.account_id, address],
                |row| row.get(0),
            )?;
            if taken {
                return Err(StoreError::Invalid(format!("{address} is already fetched")));
            }
            let sealed = seal(tx, &new.password)?;
            tx.execute(
                "INSERT INTO fetch_accounts
                     (account_id, address, host, port, security, username, password, after_fetch, fetch_junk,
                      interval_secs, auth_serv_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    new.account_id,
                    address,
                    host,
                    new.port as i64,
                    new.security.as_str(),
                    username,
                    sealed,
                    new.after_fetch.as_str(),
                    new.fetch_junk,
                    interval,
                    new.auth_serv_id.trim().to_ascii_lowercase(),
                    at,
                ],
            )?;
            let id = tx.last_insert_rowid();
            Ok(tx.query_row(&format!("SELECT {COLUMNS} FROM fetch_accounts WHERE id = ?1"), params![id], from_row)?)
        })
        .await
    }

    /// Changes what was given and leaves the rest. A new password replaces the old one; the state of
    /// the folders stays, because the mailbox is the same one.
    pub async fn update_fetch_account(
        &self,
        account_id: i64,
        id: i64,
        update: FetchAccountUpdate,
    ) -> Result<FetchAccount> {
        let host = update.host.as_deref().map(check_host).transpose()?;
        let smtp_host = update.smtp_host.as_deref().map(check_host).transpose()?;
        let interval = update.interval_secs.map(check_interval).transpose()?;
        self.write(move |tx| {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM fetch_accounts WHERE id = ?1 AND account_id = ?2)",
                params![id, account_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StoreError::NotFound(format!("fetched mailbox {id}")));
            }
            // Every statement below asks for both, so a wrong owner changes nothing even if the
            // check above were ever removed.
            let set = |column: &str, value: &dyn rusqlite::ToSql| -> Result<()> {
                tx.execute(
                    &format!("UPDATE fetch_accounts SET {column} = ?1 WHERE id = ?2 AND account_id = ?3"),
                    params![value, id, account_id],
                )?;
                Ok(())
            };
            if let Some(host) = host {
                set("host", &host)?;
            }
            if let Some(port) = update.port {
                set("port", &(port as i64))?;
            }
            if let Some(security) = update.security {
                set("security", &security.as_str())?;
            }
            if let Some(username) = &update.username {
                let username = username.trim();
                if username.is_empty() {
                    return Err(StoreError::Invalid("the provider needs a user name".into()));
                }
                set("username", &username)?;
            }
            if let Some(password) = &update.password {
                if password.is_empty() {
                    return Err(StoreError::Invalid("the provider needs a password".into()));
                }
                let sealed = seal(tx, password)?;
                set("password", &sealed)?;
            }
            if let Some(after) = update.after_fetch {
                set("after_fetch", &after.as_str())?;
            }
            if let Some(junk) = update.fetch_junk {
                set("fetch_junk", &junk)?;
            }
            if let Some(interval) = interval {
                set("interval_secs", &interval)?;
            }
            if let Some(enabled) = update.enabled {
                set("enabled", &enabled)?;
                // Switching it back on should not wait for the interval to pass.
                if enabled {
                    set("last_run_at", &None::<i64>)?;
                }
            }
            if let Some(name) = &update.auth_serv_id {
                set("auth_serv_id", &name.trim().to_ascii_lowercase())?;
            }
            if let Some(host) = smtp_host {
                set("smtp_host", &host)?;
            }
            if let Some(port) = update.smtp_port {
                set("smtp_port", &(port as i64))?;
            }
            if let Some(security) = update.smtp_security {
                set("smtp_security", &security.as_str())?;
            }
            if let Some(send) = update.send_enabled {
                // Sending needs somewhere to send to: a server name is what makes the difference
                // between an address that can answer and one that only claims it can. And it needs
                // proof that this account can read the mailbox -- one successful fetch -- so that a
                // row alone can never grant the right to send as an address nobody has opened.
                let (host, last_ok_at): (String, Option<i64>) = tx.query_row(
                    "SELECT smtp_host, last_ok_at FROM fetch_accounts WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                if send && host.trim().is_empty() {
                    return Err(StoreError::Invalid(
                        "sending from this address needs the provider's outgoing server".into(),
                    ));
                }
                if send && last_ok_at.is_none() {
                    return Err(StoreError::Invalid(
                        "sending from this address needs one successful fetch first, to prove the mailbox is yours"
                            .into(),
                    ));
                }
                set("send_enabled", &send)?;
            }
            Ok(tx.query_row(
                &format!("SELECT {COLUMNS} FROM fetch_accounts WHERE id = ?1 AND account_id = ?2"),
                params![id, account_id],
                from_row,
            )?)
        })
        .await
    }

    pub async fn delete_fetch_account(&self, account_id: i64, id: i64) -> Result<()> {
        self.write(move |tx| {
            let gone =
                tx.execute("DELETE FROM fetch_accounts WHERE id = ?1 AND account_id = ?2", params![id, account_id])?;
            if gone == 0 {
                return Err(StoreError::NotFound(format!("fetched mailbox {id}")));
            }
            Ok(())
        })
        .await
    }

    /// How a run went: what it brought, and what went wrong if anything did.
    pub async fn note_fetch_run(&self, id: i64, fetched: i64, error: Option<String>) -> Result<()> {
        let at = now();
        self.write(move |tx| {
            match &error {
                None => tx.execute(
                    "UPDATE fetch_accounts
                     SET last_run_at = ?2, last_ok_at = ?2, last_error = '', last_fetched = ?3,
                         total_fetched = total_fetched + ?3
                     WHERE id = ?1",
                    params![id, at, fetched],
                )?,
                Some(error) => tx.execute(
                    "UPDATE fetch_accounts
                     SET last_run_at = ?2, last_error = ?3, last_fetched = ?4,
                         total_fetched = total_fetched + ?4
                     WHERE id = ?1",
                    params![id, at, shorten(error, 500), fetched],
                )?,
            };
            Ok(())
        })
        .await
    }

    /// Where mail from this address has to leave, when it is a fetched address that may send.
    ///
    /// A reply from a free mail address only holds up if it goes out through that provider: its
    /// DMARC policy is what makes the address worth anything, and our server is not in it. Asked
    /// once per delivery attempt, by the sending account and the envelope sender together: the
    /// route hangs on the account, not on the address alone, so two people who fetch the same
    /// provider can never leave through each other's server. Mail with no account (bounces, system
    /// mail) has no fetched sender.
    pub async fn fetch_sender(&self, account_id: i64, address: &str) -> Result<Option<FetchSender>> {
        let Ok((local, domain)) = normalize_address(address) else {
            return Ok(None);
        };
        let address = format!("{local}@{domain}");
        self.read(move |conn| {
            let found = conn
                .query_row(
                    "SELECT account_id, address, smtp_host, smtp_port, smtp_security, username, password
                     FROM fetch_accounts
                     WHERE account_id = ?2 AND address = ?1 AND send_enabled = 1 AND smtp_host <> ''",
                    params![address, account_id],
                    |row| {
                        let security: String = row.get(4)?;
                        let sealed: Vec<u8> = row.get(6)?;
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)? as u16,
                            SendSecurity::parse(&security).unwrap_or(SendSecurity::Starttls),
                            row.get::<_, String>(5)?,
                            sealed,
                        ))
                    },
                )
                .optional()?;
            let Some((account_id, address, host, port, security, username, sealed)) = found else {
                return Ok(None);
            };
            Ok(Some(FetchSender {
                account_id,
                address,
                host,
                port,
                security,
                username,
                password: unseal(conn, &sealed)?,
            }))
        })
        .await
    }

    /// Lets the next run start at once, whatever the interval says.
    pub async fn fetch_account_due_now(&self, account_id: i64, id: i64) -> Result<()> {
        self.write(move |tx| {
            let changed = tx.execute(
                "UPDATE fetch_accounts SET last_run_at = NULL WHERE id = ?1 AND account_id = ?2",
                params![id, account_id],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("fetched mailbox {id}")));
            }
            Ok(())
        })
        .await
    }

    pub async fn fetch_folder(&self, fetch_id: i64, folder: String) -> Result<Option<FetchFolder>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT uid_validity, last_uid, held_uid, held_since FROM fetch_state
                     WHERE fetch_id = ?1 AND folder = ?2",
                    params![fetch_id, folder],
                    |row| {
                        Ok(FetchFolder {
                            uid_validity: row.get(0)?,
                            last_uid: row.get(1)?,
                            held_uid: row.get(2)?,
                            held_since: row.get(3)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    /// Remembers how far a folder was read. A folder that was renumbered starts over.
    pub async fn set_fetch_folder(&self, fetch_id: i64, folder: String, state: FetchFolder) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO fetch_state (fetch_id, folder, uid_validity, last_uid, held_uid, held_since)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (fetch_id, folder) DO UPDATE SET
                     uid_validity = excluded.uid_validity,
                     last_uid = excluded.last_uid,
                     held_uid = excluded.held_uid,
                     held_since = excluded.held_since",
                params![fetch_id, folder, state.uid_validity, state.last_uid, state.held_uid, state.held_since],
            )?;
            Ok(())
        })
        .await
    }

    /// Remembers a message and says whether this mailbox brought it before. One step, so two runs
    /// at once cannot both decide that a message is new.
    /// Whether this mailbox has already brought a message with this key, without recording anything.
    /// The worker asks this before delivering, and only records it as seen once it was really taken.
    pub async fn is_fetch_seen(&self, fetch_id: i64, key: String) -> Result<bool> {
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM fetch_seen WHERE fetch_id = ?1 AND key = ?2)",
                params![fetch_id, key],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn mark_fetch_seen(&self, fetch_id: i64, key: String) -> Result<bool> {
        let at = now();
        self.write(move |tx| {
            let added = tx.execute(
                "INSERT INTO fetch_seen (fetch_id, key, seen_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (fetch_id, key) DO NOTHING",
                params![fetch_id, key, at],
            )?;
            Ok(added == 0)
        })
        .await
    }

    /// Forgets the names of messages nobody will offer again.
    pub async fn prune_fetch_seen(&self) -> Result<usize> {
        let before = now() - FETCH_SEEN_SECS;
        self.write(move |tx| Ok(tx.execute("DELETE FROM fetch_seen WHERE seen_at < ?1", params![before])?)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    /// A store with one person in it, who is about to have mail fetched for them.
    async fn store_with_person() -> (Store, tempfile::TempDir, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("uwu.test").await.unwrap();
        let account = NewAccount {
            address: "mini@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let id = store.create_account(account).await.unwrap().id;
        (store, dir, id)
    }

    fn new_account(account_id: i64) -> NewFetchAccount {
        NewFetchAccount {
            account_id,
            address: "Mini@Example.COM".into(),
            host: "imap.example.com".into(),
            port: 993,
            security: FetchSecurity::Tls,
            username: "mini@example.com".into(),
            password: "secret-at-the-provider".into(),
            after_fetch: AfterFetch::MarkRead,
            fetch_junk: true,
            interval_secs: DEFAULT_FETCH_INTERVAL_SECS,
            auth_serv_id: String::new(),
        }
    }

    #[tokio::test]
    async fn the_password_comes_back_but_is_not_in_the_table() {
        let (store, _dir, account_id) = store_with_person().await;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();
        assert_eq!(fetched.address, "mini@example.com", "the address is normalized");

        assert_eq!(
            store.fetch_password(account_id, fetched.id).await.unwrap().as_deref(),
            Some("secret-at-the-provider")
        );
        let stored: Vec<u8> = store
            .read(move |conn| {
                Ok(conn.query_row("SELECT password FROM fetch_accounts WHERE id = ?1", params![fetched.id], |row| {
                    row.get(0)
                })?)
            })
            .await
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&stored).contains("secret-at-the-provider"),
            "the password is not in the table as text"
        );
    }

    #[tokio::test]
    async fn a_second_mailbox_for_the_same_address_is_refused() {
        let (store, _dir, account_id) = store_with_person().await;
        store.create_fetch_account(new_account(account_id)).await.unwrap();
        let again = store.create_fetch_account(new_account(account_id)).await;
        assert!(matches!(again, Err(StoreError::Invalid(_))));
    }

    #[tokio::test]
    async fn only_mailboxes_whose_turn_it_is_are_due() {
        let (store, _dir, account_id) = store_with_person().await;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();
        assert_eq!(store.fetch_accounts_due().await.unwrap().len(), 1, "a mailbox that never ran is due");

        store.note_fetch_run(fetched.id, 3, None).await.unwrap();
        assert!(store.fetch_accounts_due().await.unwrap().is_empty(), "not again before the interval");
        assert_eq!(store.fetch_account(account_id, fetched.id).await.unwrap().unwrap().total_fetched, 3);

        store.fetch_account_due_now(account_id, fetched.id).await.unwrap();
        assert_eq!(store.fetch_accounts_due().await.unwrap().len(), 1, "asking for it now works");

        store
            .update_fetch_account(
                account_id,
                fetched.id,
                FetchAccountUpdate { enabled: Some(false), ..Default::default() },
            )
            .await
            .unwrap();
        assert!(store.fetch_accounts_due().await.unwrap().is_empty(), "a mailbox that is off is never due");
    }

    #[tokio::test]
    async fn a_folder_remembers_where_it_stood_and_what_it_held() {
        let (store, _dir, account_id) = store_with_person().await;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();

        let state = FetchFolder { uid_validity: 7, last_uid: 42, held_uid: None, held_since: None };
        store.set_fetch_folder(fetched.id, "INBOX".into(), state).await.unwrap();
        assert_eq!(store.fetch_folder(fetched.id, "INBOX".into()).await.unwrap(), Some(state));

        let held = FetchFolder { held_uid: Some(43), held_since: Some(now()), ..state };
        store.set_fetch_folder(fetched.id, "INBOX".into(), held).await.unwrap();
        let read = store.fetch_folder(fetched.id, "INBOX".into()).await.unwrap().unwrap();
        assert_eq!(read.held_uid, Some(43));
        assert!(!read.hold_expired(now()), "a message that just arrived is not stuck yet");
        assert!(read.hold_expired(now() + FETCH_HOLD_LIMIT_SECS + 1), "one that waited a day is");
    }

    #[tokio::test]
    async fn a_message_is_only_brought_once() {
        let (store, _dir, account_id) = store_with_person().await;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();

        let one = "<one@example.com>".to_owned();
        assert!(!store.mark_fetch_seen(fetched.id, one.clone()).await.unwrap(), "the first time it is new");
        assert!(store.mark_fetch_seen(fetched.id, one).await.unwrap(), "the second time it is known");
        assert!(
            !store.mark_fetch_seen(fetched.id, "<two@example.com>".into()).await.unwrap(),
            "another message is its own"
        );
    }

    #[tokio::test]
    async fn deleting_the_mailbox_takes_its_state_with_it() {
        let (store, _dir, account_id) = store_with_person().await;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();
        let state = FetchFolder { uid_validity: 1, last_uid: 5, held_uid: None, held_since: None };
        store.set_fetch_folder(fetched.id, "INBOX".into(), state).await.unwrap();
        store.mark_fetch_seen(fetched.id, "<one@example.com>".into()).await.unwrap();

        store.delete_fetch_account(account_id, fetched.id).await.unwrap();
        assert!(store.fetch_folder(fetched.id, "INBOX".into()).await.unwrap().is_none());
        let remembered: i64 = store
            .read(move |conn| {
                Ok(conn.query_row("SELECT COUNT(*) FROM fetch_seen WHERE fetch_id = ?1", params![fetched.id], {
                    |row| row.get(0)
                })?)
            })
            .await
            .unwrap();
        assert_eq!(remembered, 0, "and what it remembered is gone with it");
    }

    #[tokio::test]
    async fn nobody_reaches_another_persons_fetched_mailbox() {
        let (store, _dir, account_id) = store_with_person().await;
        let other = store
            .create_account(NewAccount {
                address: "leni@uwu.test".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();

        // Knowing the row id is not enough: every single-row function asks whose it is.
        assert!(store.fetch_account(other, fetched.id).await.unwrap().is_none());
        assert!(store.fetch_password(other, fetched.id).await.unwrap().is_none(), "least of all the password");
        assert!(matches!(
            store.update_fetch_account(other, fetched.id, FetchAccountUpdate::default()).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(store.fetch_account_due_now(other, fetched.id).await, Err(StoreError::NotFound(_))));
        assert!(matches!(store.delete_fetch_account(other, fetched.id).await, Err(StoreError::NotFound(_))));

        // Nothing of it was changed or taken away by trying.
        let mine = store.fetch_account(account_id, fetched.id).await.unwrap().unwrap();
        assert_eq!(mine.host, "imap.example.com");
        assert_eq!(
            store.fetch_password(account_id, fetched.id).await.unwrap().as_deref(),
            Some("secret-at-the-provider")
        );

        // Turning the host into one's own server is the point of asking: that is where the
        // provider's password would be sent on the next run.
        let hijack = FetchAccountUpdate { host: Some("imap.attacker.example".into()), ..Default::default() };
        assert!(store.update_fetch_account(other, fetched.id, hijack).await.is_err());
        assert_eq!(store.fetch_account(account_id, fetched.id).await.unwrap().unwrap().host, "imap.example.com");

        // And a listing only ever shows one's own.
        assert!(store.fetch_accounts(Some(other)).await.unwrap().is_empty());
        assert_eq!(store.fetch_accounts(Some(account_id)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn answering_from_a_fetched_address_needs_a_server_and_one_owner() {
        let (store, _dir, account_id) = store_with_person().await;
        let other = store
            .create_account(NewAccount {
                address: "leni@uwu.test".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();
        let address = fetched.address.clone();

        // Fetching alone says nothing about sending: the address cannot be sent from yet.
        assert!(!store.account_owns_address(account_id, &address).await.unwrap());
        assert!(store.fetch_sender(account_id, &address).await.unwrap().is_none());

        // And it cannot be switched on without somewhere to send to.
        let just_on = FetchAccountUpdate { send_enabled: Some(true), ..Default::default() };
        assert!(matches!(
            store.update_fetch_account(account_id, fetched.id, just_on).await,
            Err(StoreError::Invalid(_))
        ));
        assert!(!store.account_owns_address(account_id, &address).await.unwrap());

        // A server can be saved, but sending stays off until a fetch has proven the mailbox is ours.
        let set_server = FetchAccountUpdate {
            smtp_host: Some("smtp.example.com".into()),
            smtp_port: Some(465),
            smtp_security: Some(SendSecurity::Tls),
            ..Default::default()
        };
        store.update_fetch_account(account_id, fetched.id, set_server).await.unwrap();
        let too_early = FetchAccountUpdate { send_enabled: Some(true), ..Default::default() };
        assert!(
            matches!(store.update_fetch_account(account_id, fetched.id, too_early).await, Err(StoreError::Invalid(_))),
            "send is refused before any successful fetch"
        );
        assert!(!store.account_owns_address(account_id, &address).await.unwrap());

        // One successful fetch proves control; now the owner may answer from it -- and only the owner.
        store.note_fetch_run(fetched.id, 1, None).await.unwrap();
        let turn_on = FetchAccountUpdate { send_enabled: Some(true), ..Default::default() };
        let saved = store.update_fetch_account(account_id, fetched.id, turn_on).await.unwrap();
        assert!(saved.send_enabled && saved.smtp_port == 465);
        assert!(store.account_owns_address(account_id, &address).await.unwrap());
        assert!(!store.account_owns_address(other, &address).await.unwrap(), "not somebody else's address");

        // What the delivery worker needs to send it: the provider's server and the login.
        let sender = store.fetch_sender(account_id, &address).await.unwrap().unwrap();
        assert_eq!((sender.host.as_str(), sender.port), ("smtp.example.com", 465));
        assert_eq!(sender.security, SendSecurity::Tls);
        assert_eq!(sender.username, "mini@example.com");
        assert_eq!(sender.password, "secret-at-the-provider", "the same one that opens the mailbox");
        assert_eq!(sender.account_id, account_id);

        // Another account asking for the same address gets nothing: the route is the account's own.
        assert!(store.fetch_sender(other, &address).await.unwrap().is_none());

        // Switched off again, the address stops being one to send from at once.
        let off = FetchAccountUpdate { send_enabled: Some(false), ..Default::default() };
        store.update_fetch_account(account_id, fetched.id, off).await.unwrap();
        assert!(!store.account_owns_address(account_id, &address).await.unwrap());
        assert!(store.fetch_sender(account_id, &address).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn two_accounts_fetching_the_same_address_keep_their_own_outgoing_server() {
        // Two people may legitimately fetch the same shared mailbox; neither may leave through the
        // other's provider, and neither may read the other's outbound mail. The route is keyed on
        // the account, not on the address alone (S-1).
        let (store, _dir, a) = store_with_person().await;
        let b = store
            .create_account(NewAccount {
                address: "leni@uwu.test".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;

        let shared = |account_id| NewFetchAccount { account_id, ..new_account(account_id) };
        let a_row = store.create_fetch_account(shared(a)).await.unwrap();
        let b_row = store.create_fetch_account(shared(b)).await.unwrap();
        let address = a_row.address.clone();
        assert_eq!(address, b_row.address, "the same shared address, two accounts");

        for (id, row, host) in
            [(a, a_row.id, "smtp.a.example"), (b, b_row.id, "smtp.b.example")]
        {
            store.note_fetch_run(row, 1, None).await.unwrap();
            let on = FetchAccountUpdate {
                smtp_host: Some(host.into()),
                smtp_port: Some(465),
                smtp_security: Some(SendSecurity::Tls),
                send_enabled: Some(true),
                ..Default::default()
            };
            store.update_fetch_account(id, row, on).await.unwrap();
        }

        // Each account routes through its own server, whatever the row order in the table is.
        assert_eq!(store.fetch_sender(a, &address).await.unwrap().unwrap().host, "smtp.a.example");
        assert_eq!(store.fetch_sender(b, &address).await.unwrap().unwrap().host, "smtp.b.example");
    }

    #[tokio::test]
    async fn a_fetched_address_of_a_hosted_domain_is_refused() {
        // A fetched mailbox is one somewhere else. An address of a domain hosted here would let an
        // account claim (and, with send on, impersonate) a local address it does not own (S-2).
        let (store, _dir, account_id) = store_with_person().await;
        let local = NewFetchAccount { address: "admin@uwu.test".into(), ..new_account(account_id) };
        assert!(matches!(store.create_fetch_account(local).await, Err(StoreError::Invalid(_))));
    }

    #[tokio::test]
    async fn a_private_or_loopback_host_is_refused() {
        // A fetched mailbox must live on the open internet; an IP literal in this host or the
        // sending host must not point the worker at this machine or the LAN (S-10).
        let (store, _dir, account_id) = store_with_person().await;
        for host in ["127.0.0.1", "10.0.0.5", "192.168.1.1", "169.254.0.1", "::ffff:10.0.0.1"] {
            let bad = NewFetchAccount { host: host.into(), ..new_account(account_id) };
            assert!(
                matches!(store.create_fetch_account(bad).await, Err(StoreError::Invalid(_))),
                "{host} is refused as a host"
            );
        }
        // A public IP literal and a normal name are fine.
        let ok = NewFetchAccount { host: "9.9.9.9".into(), ..new_account(account_id) };
        assert!(store.create_fetch_account(ok).await.is_ok());
    }

    #[tokio::test]
    async fn a_trashed_account_stops_fetching() {
        // Trashing a person must stop pulling their provider mail, and must not silently resume on
        // restore (S-13).
        let (store, _dir, account_id) = store_with_person().await;
        let fetched = store.create_fetch_account(new_account(account_id)).await.unwrap();
        assert_eq!(store.fetch_accounts_due().await.unwrap().len(), 1, "due before trashing");

        let login = store.account_by_id(account_id).await.unwrap().unwrap().login;
        store.trash_account(&login).await.unwrap();
        assert!(store.fetch_accounts_due().await.unwrap().is_empty(), "a trashed account is never due");
        assert!(!store.fetch_account(account_id, fetched.id).await.unwrap().unwrap().enabled, "and its fetch is off");

        // Restoring does not turn it back on by itself.
        store.restore_account(&login).await.unwrap();
        assert!(store.fetch_accounts_due().await.unwrap().is_empty(), "restore leaves the fetch off");
    }
}
