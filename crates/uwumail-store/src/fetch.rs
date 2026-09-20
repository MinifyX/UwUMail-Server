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
                       interval_secs, enabled, auth_serv_id, created_at, last_run_at, last_ok_at, last_error, \
                       last_fetched, total_fetched";

fn from_row(row: &Row<'_>) -> rusqlite::Result<FetchAccount> {
    let security: String = row.get(5)?;
    let after: String = row.get(7)?;
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
        created_at: row.get(12)?,
        last_run_at: row.get(13)?,
        last_ok_at: row.get(14)?,
        last_error: row.get(15)?,
        last_fetched: row.get(16)?,
        total_fetched: row.get(17)?,
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
    String::from_utf8(plain.to_vec())
        .map_err(|_| StoreError::Internal("a stored provider password is not text".into()))
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

fn check_host(host: &str) -> Result<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || host.contains(char::is_whitespace) {
        return Err(StoreError::Invalid(format!("'{host}' is not a server name")));
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

    pub async fn fetch_account(&self, id: i64) -> Result<Option<FetchAccount>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(&format!("SELECT {COLUMNS} FROM fetch_accounts WHERE id = ?1"), params![id], from_row)
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
                 WHERE enabled = 1 AND (last_run_at IS NULL OR last_run_at + interval_secs <= ?1)
                 ORDER BY last_run_at IS NOT NULL, last_run_at"
            ))?;
            let rows = stmt.query_map(params![at], from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// The password to send to the provider.
    pub async fn fetch_password(&self, id: i64) -> Result<Option<String>> {
        self.read(move |conn| {
            let sealed: Option<Vec<u8>> = conn
                .query_row("SELECT password FROM fetch_accounts WHERE id = ?1", params![id], |row| row.get(0))
                .optional()?;
            sealed.map(|sealed| unseal(conn, &sealed)).transpose()
        })
        .await
    }

    pub async fn create_fetch_account(&self, new: NewFetchAccount) -> Result<FetchAccount> {
        let (local, domain) = normalize_address(&new.address)
            .map_err(|_| StoreError::Invalid(format!("'{}' is not a valid email address", new.address)))?;
        let address = format!("{local}@{domain}");
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
    pub async fn update_fetch_account(&self, id: i64, update: FetchAccountUpdate) -> Result<FetchAccount> {
        let host = update.host.as_deref().map(check_host).transpose()?;
        let interval = update.interval_secs.map(check_interval).transpose()?;
        self.write(move |tx| {
            let exists: bool =
                tx.query_row("SELECT EXISTS (SELECT 1 FROM fetch_accounts WHERE id = ?1)", params![id], |row| {
                    row.get(0)
                })?;
            if !exists {
                return Err(StoreError::NotFound(format!("fetched mailbox {id}")));
            }
            let set = |column: &str, value: &dyn rusqlite::ToSql| -> Result<()> {
                tx.execute(&format!("UPDATE fetch_accounts SET {column} = ?1 WHERE id = ?2"), params![value, id])?;
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
                    tx.execute("UPDATE fetch_accounts SET last_run_at = NULL WHERE id = ?1", params![id])?;
                }
            }
            if let Some(name) = &update.auth_serv_id {
                set("auth_serv_id", &name.trim().to_ascii_lowercase())?;
            }
            Ok(tx.query_row(&format!("SELECT {COLUMNS} FROM fetch_accounts WHERE id = ?1"), params![id], from_row)?)
        })
        .await
    }

    pub async fn delete_fetch_account(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            let gone = tx.execute("DELETE FROM fetch_accounts WHERE id = ?1", params![id])?;
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

    /// Lets the next run start at once, whatever the interval says.
    pub async fn fetch_account_due_now(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("UPDATE fetch_accounts SET last_run_at = NULL WHERE id = ?1", params![id])?;
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

        assert_eq!(store.fetch_password(fetched.id).await.unwrap().as_deref(), Some("secret-at-the-provider"));
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
        assert_eq!(store.fetch_account(fetched.id).await.unwrap().unwrap().total_fetched, 3);

        store.fetch_account_due_now(fetched.id).await.unwrap();
        assert_eq!(store.fetch_accounts_due().await.unwrap().len(), 1, "asking for it now works");

        store
            .update_fetch_account(fetched.id, FetchAccountUpdate { enabled: Some(false), ..Default::default() })
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

        store.delete_fetch_account(fetched.id).await.unwrap();
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
}
