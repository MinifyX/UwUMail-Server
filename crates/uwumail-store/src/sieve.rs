//! Sieve scripts: a person's own mail rules (docs/sieve.md). JMAP (`SieveScript`, RFC 9661) and
//! ManageSieve (RFC 5804) keep them here; delivery runs the one that is active.
//!
//! The store only knows names, sizes and which one is active. Whether a script is valid Sieve is
//! checked by whoever hands it in, with the engine that later runs it.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::db::{next_modseq, record_change};
use crate::{BlobHash, Result, Store, StoreError, now};

/// How many scripts one account may keep.
pub const SIEVE_MAX_SCRIPTS: usize = 16;
/// The largest script, in bytes.
pub const SIEVE_MAX_SCRIPT_SIZE: usize = 65_536;
/// The longest script name, in bytes (128 characters of four bytes, as RFC 5804 asks for).
pub const SIEVE_MAX_NAME_SIZE: usize = 512;

/// The change log's name for scripts, which is also their JMAP type.
const KIND: &str = "SieveScript";

/// A script without its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SieveScript {
    pub id: i64,
    pub name: String,
    /// The hash of the content; the script's JMAP blob id names it.
    pub blob: BlobHash,
    pub size: i64,
    pub is_active: bool,
}

/// Why a change to the scripts was refused.
#[derive(Debug, thiserror::Error)]
pub enum SieveError {
    /// Another script of the account has the name; its id.
    #[error("a script with this name exists already")]
    AlreadyExists(i64),
    #[error("at most {SIEVE_MAX_SCRIPTS} scripts")]
    TooMany,
    #[error("a script may have at most {SIEVE_MAX_SCRIPT_SIZE} bytes")]
    TooLarge,
    #[error("{0}")]
    InvalidName(String),
    /// Empty, or not UTF-8.
    #[error("{0}")]
    InvalidContent(String),
    #[error("no such script")]
    NotFound,
    /// The active script cannot be deleted.
    #[error("the script is active")]
    Active,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Which scripts an activation switched off and on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SieveActivation {
    pub deactivated: Option<i64>,
    pub activated: Option<i64>,
}

/// Checks a script name the way RFC 5804 (section 1.6) and RFC 9661 want it.
pub fn validate_sieve_name(name: &str) -> Result<(), SieveError> {
    if name.is_empty() {
        return Err(SieveError::InvalidName("the name is empty".into()));
    }
    if name.len() > SIEVE_MAX_NAME_SIZE {
        return Err(SieveError::InvalidName(format!("the name is longer than {SIEVE_MAX_NAME_SIZE} bytes")));
    }
    let forbidden = |c: char| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}');
    if name.chars().any(forbidden) {
        return Err(SieveError::InvalidName("the name contains control characters".into()));
    }
    Ok(())
}

/// Checks what may be stored as script content, apart from whether it is valid Sieve.
fn check_content(content: &[u8]) -> Result<String, SieveError> {
    if content.len() > SIEVE_MAX_SCRIPT_SIZE {
        return Err(SieveError::TooLarge);
    }
    if content.is_empty() {
        return Err(SieveError::InvalidContent("the script is empty".into()));
    }
    String::from_utf8(content.to_vec()).map_err(|_| SieveError::InvalidContent("the script is not UTF-8".into()))
}

const COLUMNS: &str = "id, name, blob_hash, length(CAST(content AS BLOB)), is_active";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SieveScript> {
    let hash: String = row.get(2)?;
    Ok(SieveScript {
        id: row.get(0)?,
        name: row.get(1)?,
        blob: BlobHash::parse(&hash)
            .map_err(|err| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(err)))?,
        size: row.get(3)?,
        is_active: row.get(4)?,
    })
}

fn load(conn: &Connection, account_id: i64, id: i64) -> Result<Option<SieveScript>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM sieve_scripts WHERE account_id = ?1 AND id = ?2"),
            params![account_id, id],
            from_row,
        )
        .optional()?)
}

fn id_named(conn: &Connection, account_id: i64, name: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM sieve_scripts WHERE account_id = ?1 AND name = ?2",
            params![account_id, name],
            |row| row.get(0),
        )
        .optional()?)
}

fn count(conn: &Connection, account_id: i64) -> Result<usize> {
    Ok(conn.query_row("SELECT COUNT(*) FROM sieve_scripts WHERE account_id = ?1", [account_id], |row| {
        row.get::<_, i64>(0)
    })? as usize)
}

/// The first free `script-<n>`, for scripts created without a name.
fn free_name(conn: &Connection, account_id: i64) -> Result<String> {
    for number in 1.. {
        let name = format!("script-{number}");
        if id_named(conn, account_id, &name)?.is_none() {
            return Ok(name);
        }
    }
    unreachable!("an account has only so many scripts")
}

fn insert(tx: &Transaction<'_>, account_id: i64, name: &str, content: &str) -> Result<(SieveScript, i64)> {
    let modseq = next_modseq(tx, account_id)?;
    let hash = BlobHash::of(content.as_bytes());
    let at = now();
    tx.execute(
        "INSERT INTO sieve_scripts (account_id, name, content, blob_hash, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![account_id, name, content, hash.as_str(), at],
    )?;
    let id = tx.last_insert_rowid();
    record_change(tx, account_id, modseq, KIND, id, "created")?;
    let script = load(tx, account_id, id)?.ok_or_else(|| StoreError::Internal("the new script is gone".into()))?;
    Ok((script, modseq))
}

/// Writes name and/or content of an existing script.
fn change(tx: &Transaction<'_>, account_id: i64, id: i64, name: Option<&str>, content: Option<&str>) -> Result<i64> {
    let modseq = next_modseq(tx, account_id)?;
    if let Some(name) = name {
        tx.execute("UPDATE sieve_scripts SET name = ?1 WHERE id = ?2", params![name, id])?;
    }
    if let Some(content) = content {
        let hash = BlobHash::of(content.as_bytes());
        tx.execute(
            "UPDATE sieve_scripts SET content = ?1, blob_hash = ?2 WHERE id = ?3",
            params![content, hash.as_str(), id],
        )?;
    }
    tx.execute("UPDATE sieve_scripts SET updated_at = ?1 WHERE id = ?2", params![now(), id])?;
    record_change(tx, account_id, modseq, KIND, id, "updated")?;
    Ok(modseq)
}

fn activate(tx: &Transaction<'_>, account_id: i64, id: Option<i64>) -> Result<(SieveActivation, Option<i64>)> {
    let current: Option<i64> = tx
        .query_row("SELECT id FROM sieve_scripts WHERE account_id = ?1 AND is_active", [account_id], |row| row.get(0))
        .optional()?;
    if current == id {
        return Ok((SieveActivation::default(), None));
    }
    let modseq = next_modseq(tx, account_id)?;
    let mut activation = SieveActivation::default();
    if let Some(current) = current {
        tx.execute("UPDATE sieve_scripts SET is_active = 0 WHERE id = ?1", [current])?;
        record_change(tx, account_id, modseq, KIND, current, "updated")?;
        activation.deactivated = Some(current);
    }
    if let Some(id) = id {
        tx.execute("UPDATE sieve_scripts SET is_active = 1 WHERE id = ?1", [id])?;
        record_change(tx, account_id, modseq, KIND, id, "updated")?;
        activation.activated = Some(id);
    }
    Ok((activation, Some(modseq)))
}

impl Store {
    /// Writes with a refusal that leaves the database as it was, and tells listeners afterwards.
    async fn write_sieve<T, F>(&self, account_id: i64, f: F) -> Result<T, SieveError>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<Result<(T, Option<i64>), SieveError>> + Send + 'static,
    {
        let (value, modseq) = self.write(f).await??;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        Ok(value)
    }

    /// The account's scripts, by name.
    pub async fn sieve_scripts(&self, account_id: i64) -> Result<Vec<SieveScript>> {
        self.read(move |conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT {COLUMNS} FROM sieve_scripts WHERE account_id = ?1 ORDER BY name, id"))?;
            let rows = stmt.query_map([account_id], from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// A script and its content.
    pub async fn sieve_script(&self, account_id: i64, id: i64) -> Result<Option<(SieveScript, String)>> {
        self.read(move |conn| {
            let Some(script) = load(conn, account_id, id)? else {
                return Ok(None);
            };
            let content: String =
                conn.query_row("SELECT content FROM sieve_scripts WHERE id = ?1", [id], |r| r.get(0))?;
            Ok(Some((script, content)))
        })
        .await
    }

    /// A script by its name, with its content.
    pub async fn sieve_script_named(&self, account_id: i64, name: &str) -> Result<Option<(SieveScript, String)>> {
        let name = name.to_owned();
        let id = self.read(move |conn| id_named(conn, account_id, &name)).await?;
        match id {
            Some(id) => self.sieve_script(account_id, id).await,
            None => Ok(None),
        }
    }

    /// The script that filters the account's incoming mail, if one does.
    pub async fn active_sieve_script(&self, account_id: i64) -> Result<Option<(SieveScript, String)>> {
        let id: Option<i64> = self
            .read(move |conn| {
                Ok(conn
                    .query_row("SELECT id FROM sieve_scripts WHERE account_id = ?1 AND is_active", [account_id], |r| {
                        r.get(0)
                    })
                    .optional()?)
            })
            .await?;
        match id {
            Some(id) => self.sieve_script(account_id, id).await,
            None => Ok(None),
        }
    }

    /// The content behind a script's blob id, for downloads and for using it as the source of
    /// another script. Only the account's own scripts are found.
    pub async fn sieve_script_blob(&self, account_id: i64, hash: &BlobHash) -> Result<Option<String>> {
        let key = hash.as_str().to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT content FROM sieve_scripts WHERE account_id = ?1 AND blob_hash = ?2 LIMIT 1",
                    params![account_id, key],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// Stores a new script. Without a name it gets the first free `script-<n>`.
    pub async fn create_sieve_script(
        &self,
        account_id: i64,
        name: Option<&str>,
        content: &[u8],
    ) -> Result<SieveScript, SieveError> {
        if let Some(name) = name {
            validate_sieve_name(name)?;
        }
        let content = check_content(content)?;
        let name = name.map(str::to_owned);
        self.write_sieve(account_id, move |tx| {
            if count(tx, account_id)? >= SIEVE_MAX_SCRIPTS {
                return Ok(Err(SieveError::TooMany));
            }
            let name = match name {
                Some(name) => {
                    if let Some(existing) = id_named(tx, account_id, &name)? {
                        return Ok(Err(SieveError::AlreadyExists(existing)));
                    }
                    name
                }
                None => free_name(tx, account_id)?,
            };
            let (script, modseq) = insert(tx, account_id, &name, &content)?;
            Ok(Ok((script, Some(modseq))))
        })
        .await
    }

    /// Renames a script and/or replaces its content. An active script stays active.
    pub async fn update_sieve_script(
        &self,
        account_id: i64,
        id: i64,
        name: Option<&str>,
        content: Option<&[u8]>,
    ) -> Result<SieveScript, SieveError> {
        if let Some(name) = name {
            validate_sieve_name(name)?;
        }
        let content = content.map(check_content).transpose()?;
        let name = name.map(str::to_owned);
        self.write_sieve(account_id, move |tx| {
            let Some(before) = load(tx, account_id, id)? else {
                return Ok(Err(SieveError::NotFound));
            };
            let name = name.filter(|name| *name != before.name);
            if let Some(name) = &name
                && let Some(existing) = id_named(tx, account_id, name)?
            {
                return Ok(Err(SieveError::AlreadyExists(existing)));
            }
            if name.is_none() && content.is_none() {
                return Ok(Ok((before, None)));
            }
            let modseq = change(tx, account_id, id, name.as_deref(), content.as_deref())?;
            let script = load(tx, account_id, id)?.ok_or(StoreError::NotFound(format!("sieve script {id}")))?;
            Ok(Ok((script, Some(modseq))))
        })
        .await
    }

    /// Stores a script under a name: a new one, or new content for the one with that name
    /// (ManageSieve's PUTSCRIPT). Returns the script and whether it is new.
    pub async fn put_sieve_script(
        &self,
        account_id: i64,
        name: &str,
        content: &[u8],
    ) -> Result<(SieveScript, bool), SieveError> {
        validate_sieve_name(name)?;
        let content = check_content(content)?;
        let name = name.to_owned();
        self.write_sieve(account_id, move |tx| match id_named(tx, account_id, &name)? {
            Some(id) => {
                let modseq = change(tx, account_id, id, None, Some(&content))?;
                let script = load(tx, account_id, id)?.ok_or(StoreError::NotFound(format!("sieve script {id}")))?;
                Ok(Ok(((script, false), Some(modseq))))
            }
            None if count(tx, account_id)? >= SIEVE_MAX_SCRIPTS => Ok(Err(SieveError::TooMany)),
            None => {
                let (script, modseq) = insert(tx, account_id, &name, &content)?;
                Ok(Ok(((script, true), Some(modseq))))
            }
        })
        .await
    }

    /// Deletes a script; the active one only after it was switched off.
    pub async fn destroy_sieve_script(&self, account_id: i64, id: i64) -> Result<(), SieveError> {
        self.write_sieve(account_id, move |tx| {
            let Some(script) = load(tx, account_id, id)? else {
                return Ok(Err(SieveError::NotFound));
            };
            if script.is_active {
                return Ok(Err(SieveError::Active));
            }
            let modseq = next_modseq(tx, account_id)?;
            tx.execute("DELETE FROM sieve_scripts WHERE id = ?1", [id])?;
            record_change(tx, account_id, modseq, KIND, id, "destroyed")?;
            Ok(Ok(((), Some(modseq))))
        })
        .await
    }

    /// Makes a script the active one, or switches filtering off with `None`. Activating the script
    /// that is already active changes nothing.
    pub async fn activate_sieve_script(&self, account_id: i64, id: Option<i64>) -> Result<SieveActivation, SieveError> {
        self.write_sieve(account_id, move |tx| {
            if let Some(id) = id
                && load(tx, account_id, id)?.is_none()
            {
                return Ok(Err(SieveError::NotFound));
            }
            Ok(Ok(activate(tx, account_id, id)?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    async fn account(store: &Store, address: &str) -> i64 {
        let (_, domain) = address.split_once('@').unwrap();
        let _ = store.create_domain(domain).await;
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn scripts_are_named_counted_and_one_of_them_is_active() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.com").await;
        let nyu = account(&store, "nyu@example.com").await;

        let first = store.create_sieve_script(mini, Some("rules"), b"keep;").await.unwrap();
        assert_eq!(first.size, 5);
        assert!(!first.is_active);
        let unnamed = store.create_sieve_script(mini, None, b"discard;").await.unwrap();
        assert_eq!(unnamed.name, "script-1");
        assert!(matches!(
            store.create_sieve_script(mini, Some("rules"), b"stop;").await,
            Err(SieveError::AlreadyExists(id)) if id == first.id
        ));
        // Names are per account.
        store.create_sieve_script(nyu, Some("rules"), b"keep;").await.unwrap();

        let on = store.activate_sieve_script(mini, Some(first.id)).await.unwrap();
        assert_eq!(on, SieveActivation { deactivated: None, activated: Some(first.id) });
        let switched = store.activate_sieve_script(mini, Some(unnamed.id)).await.unwrap();
        assert_eq!(switched, SieveActivation { deactivated: Some(first.id), activated: Some(unnamed.id) });
        assert_eq!(store.active_sieve_script(mini).await.unwrap().unwrap().1, "discard;");
        assert!(store.active_sieve_script(nyu).await.unwrap().is_none(), "nyu has nothing active");
        assert!(matches!(store.destroy_sieve_script(mini, unnamed.id).await, Err(SieveError::Active)));
        // Someone else's script is not there.
        assert!(matches!(store.activate_sieve_script(nyu, Some(first.id)).await, Err(SieveError::NotFound)));

        let renamed = store.update_sieve_script(mini, unnamed.id, Some("Ablage"), Some(b"keep;")).await.unwrap();
        assert!(renamed.is_active, "an active script stays active");
        assert_eq!(renamed.blob, first.blob, "the same content has the same blob");
        assert_eq!(store.sieve_script_blob(mini, &first.blob).await.unwrap().as_deref(), Some("keep;"));
        assert!(store.sieve_script_blob(nyu, &BlobHash::of(b"discard;")).await.unwrap().is_none());

        store.activate_sieve_script(mini, None).await.unwrap();
        store.destroy_sieve_script(mini, unnamed.id).await.unwrap();
        let names: Vec<String> = store.sieve_scripts(mini).await.unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["rules"]);

        let changes = store.changes(mini, KIND, 0, 0).await.unwrap();
        assert_eq!(changes.created, [first.id]);
        assert_eq!(changes.destroyed, Vec::<i64>::new(), "created and destroyed in between is not reported");
    }

    #[tokio::test]
    async fn limits_hold() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.com").await;
        let big = vec![b'#'; SIEVE_MAX_SCRIPT_SIZE + 1];
        assert!(matches!(store.create_sieve_script(mini, Some("big"), &big).await, Err(SieveError::TooLarge)));
        assert!(matches!(
            store.create_sieve_script(mini, Some("empty"), b"").await,
            Err(SieveError::InvalidContent(_))
        ));
        assert!(matches!(
            store.create_sieve_script(mini, Some("bad\nname"), b"keep;").await,
            Err(SieveError::InvalidName(_))
        ));
        let long = "x".repeat(SIEVE_MAX_NAME_SIZE + 1);
        assert!(matches!(
            store.create_sieve_script(mini, Some(&long), b"keep;").await,
            Err(SieveError::InvalidName(_))
        ));
        for number in 0..SIEVE_MAX_SCRIPTS {
            store.put_sieve_script(mini, &format!("s{number}"), b"keep;").await.unwrap();
        }
        assert!(matches!(store.put_sieve_script(mini, "one more", b"keep;").await, Err(SieveError::TooMany)));
        // Replacing one is still fine.
        let (replaced, new) = store.put_sieve_script(mini, "s0", b"discard;").await.unwrap();
        assert!(!new);
        assert_eq!(replaced.size, 8);
    }
}
