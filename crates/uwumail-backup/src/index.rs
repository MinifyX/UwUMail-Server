//! What is in the snapshots, for a person looking for a mail they deleted (docs/backups.md,
//! "Finding a mail in the backups").
//!
//! The headers of every mail a snapshot holds -- subject, sender, recipients, date, the folder it
//! was in -- in a database of this server's own, `cache/backup-index.db`: each mail once per
//! account, with the snapshots it is in. A backup adds its snapshot from the database copy it makes
//! anyway; snapshots from before are fetched once and added. It holds nothing the backups do not,
//! is left out of them like the rest of `cache/`, and can be made again from them at any time.
//!
//! A mail belongs to the account it was in by id *and* the moment that account was made, so an
//! address given to someone new later never shows them the mail of the one who had it before.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::mailbox::{MAX_MAILS, damaged};

/// Where the index lives in the data directory.
pub const INDEX_FILE: &str = "cache/backup-index.db";
/// Where a snapshot's database is put while it is being added.
pub const FETCH_FILE: &str = "cache/backup-index-fetch.db";
/// Bumped when the tables change; an index of another version is made afresh.
const VERSION: i64 = 1;
/// The most results one search answers.
pub const MAX_RESULTS: usize = 200;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS snapshots (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mails (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL,
    account_created_at INTEGER NOT NULL,
    blob_hash          TEXT NOT NULL,
    message_id         TEXT,
    subject            TEXT NOT NULL,
    from_addr          TEXT NOT NULL,
    to_addr            TEXT NOT NULL,
    received_at        INTEGER NOT NULL,
    size               INTEGER NOT NULL,
    folder             TEXT NOT NULL,
    UNIQUE (account_id, account_created_at, blob_hash)
);
CREATE INDEX IF NOT EXISTS mails_by_account ON mails (account_id, account_created_at, received_at);
CREATE TABLE IF NOT EXISTS presence (
    mail_id     INTEGER NOT NULL REFERENCES mails (id) ON DELETE CASCADE,
    snapshot_id INTEGER NOT NULL REFERENCES snapshots (id) ON DELETE CASCADE,
    PRIMARY KEY (mail_id, snapshot_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS presence_by_snapshot ON presence (snapshot_id);
";

/// Whose mail: an account by its id and when it was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub account_id: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

/// A mail in the snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedMail {
    pub id: i64,
    pub blob_hash: String,
    pub message_id: Option<String>,
    pub subject: String,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub received_at: i64,
    pub size: i64,
    /// The name of a folder it was in.
    pub folder: String,
    /// When the snapshots it is in were made, oldest first.
    pub snapshots: Vec<i64>,
}

/// What to look for. Everything given has to match.
#[derive(Debug, Clone, Default)]
pub struct Search {
    /// In the subject, the sender or the recipients.
    pub text: Option<String>,
    /// Received at or after.
    pub after: Option<i64>,
    /// Received before.
    pub before: Option<i64>,
    pub limit: usize,
}

/// Opens the index, making it when there is none or one of another version.
fn open(path: &Path) -> Result<Connection, Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path).map_err(|err| damaged(&err))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).map_err(|err| damaged(&err))?;
    let conn = if version != VERSION && version != 0 {
        drop(conn);
        remove(path);
        if path.exists() {
            return Err(Error::Damaged("the old backup search index cannot be removed".into()));
        }
        Connection::open(path).map_err(|err| damaged(&err))?
    } else {
        conn
    };
    // The headers of deleted mail, unencrypted even when the backups are not: for the server only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    conn.execute_batch(&format!(
        "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 10000;
         {SCHEMA} PRAGMA user_version = {VERSION};"
    ))
    .map_err(|err| damaged(&err))?;
    Ok(conn)
}

/// Opens the index to search it, without writing, so a snapshot being added does not hold it up.
/// `None` when there is none yet, or one a catch-up has still to make afresh.
fn open_to_read(path: &Path) -> Result<Option<Connection>, Error> {
    if !path.exists() {
        return Ok(None);
    }
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|err| damaged(&err))?;
    conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(|err| damaged(&err))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).map_err(|err| damaged(&err))?;
    Ok((version == VERSION).then_some(conn))
}

/// Removes the index, e.g. when people may no longer search the backups.
pub fn remove(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// Adds the snapshot `name`, whose database is at `database`, to the index at `index`. A snapshot
/// that is there already is left as it is.
pub fn add(index: &Path, database: &Path, name: &str, created_at: i64) -> Result<(), Error> {
    let snapshot = crate::mailbox::open(database)?;
    let mut conn = open(index)?;
    let mut read = || -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        if tx.query_row("SELECT 1 FROM snapshots WHERE name = ?1", [name], |_| Ok(())).optional()?.is_some() {
            return Ok(());
        }
        tx.execute("INSERT INTO snapshots (name, created_at) VALUES (?1, ?2)", params![name, created_at])?;
        let snapshot_id = tx.last_insert_rowid();
        let mut rows = snapshot.prepare(
            "SELECT e.account_id, a.created_at, e.blob_hash, e.message_id, e.subject, e.from_addr, e.to_addr,
                    e.received_at, e.size,
                    (SELECT m.name FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                     WHERE em.email_id = e.id ORDER BY m.role IS NULL, m.id LIMIT 1)
             FROM emails e JOIN accounts a ON a.id = e.account_id
             LIMIT ?1",
        )?;
        let mut insert = tx.prepare(
            "INSERT INTO mails (account_id, account_created_at, blob_hash, message_id, subject, from_addr, to_addr,
                                received_at, size, folder)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (account_id, account_created_at, blob_hash) DO UPDATE SET size = mails.size
             RETURNING id",
        )?;
        let mut present = tx.prepare("INSERT OR IGNORE INTO presence (mail_id, snapshot_id) VALUES (?1, ?2)")?;
        let mut seen = 0;
        let mut found = rows.query([MAX_MAILS as i64 + 1])?;
        while let Some(row) = found.next()? {
            seen += 1;
            if seen > MAX_MAILS {
                return Err(crate::mailbox::too_many("messages"));
            }
            let mail_id: i64 = insert.query_row(
                params![
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                ],
                |row| row.get(0),
            )?;
            present.execute(params![mail_id, snapshot_id])?;
        }
        drop((found, insert, present));
        tx.commit()
    };
    read().map_err(|err| damaged(&err))
}

/// Forgets the snapshots that are not in `names` any more, and the mail only they held. Answers the
/// snapshots that remain in the index.
pub fn retain(index: &Path, names: &[String]) -> Result<HashSet<String>, Error> {
    let mut conn = open(index)?;
    let mut work = || -> rusqlite::Result<HashSet<String>> {
        let tx = conn.transaction()?;
        let indexed: Vec<String> =
            tx.prepare("SELECT name FROM snapshots")?.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?;
        let keep: HashSet<&String> = names.iter().collect();
        let mut remaining = HashSet::new();
        for name in indexed {
            if keep.contains(&name) {
                remaining.insert(name);
            } else {
                tx.execute("DELETE FROM snapshots WHERE name = ?1", [&name])?;
            }
        }
        tx.execute(
            "DELETE FROM mails WHERE NOT EXISTS (SELECT 1 FROM presence WHERE presence.mail_id = mails.id)",
            [],
        )?;
        tx.commit()?;
        Ok(remaining)
    };
    work().map_err(|err| damaged(&err))
}

/// `%`, `_` and `\` stand for themselves in a LIKE pattern.
fn like_pattern(text: &str) -> String {
    let mut pattern = String::from("%");
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

fn addresses(raw: &str) -> Vec<Address> {
    serde_json::from_str(raw).unwrap_or_default()
}

const COLUMNS: &str = "m.id, m.blob_hash, m.message_id, m.subject, m.from_addr, m.to_addr, m.received_at, m.size,
     m.folder, (SELECT group_concat(s.created_at) FROM (SELECT s.created_at FROM presence p
                JOIN snapshots s ON s.id = p.snapshot_id WHERE p.mail_id = m.id ORDER BY s.created_at) s)";

fn mail_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedMail> {
    let snapshots: Option<String> = row.get(9)?;
    Ok(IndexedMail {
        id: row.get(0)?,
        blob_hash: row.get(1)?,
        message_id: row.get(2)?,
        subject: row.get(3)?,
        from: addresses(&row.get::<_, String>(4)?),
        to: addresses(&row.get::<_, String>(5)?),
        received_at: row.get(6)?,
        size: row.get(7)?,
        folder: row.get(8)?,
        snapshots: snapshots.unwrap_or_default().split(',').filter_map(|at| at.parse().ok()).collect(),
    })
}

/// The mail of `owner` in the snapshots that matches, newest first.
pub fn search(index: &Path, owner: Owner, search: &Search) -> Result<Vec<IndexedMail>, Error> {
    let Some(conn) = open_to_read(index)? else { return Ok(Vec::new()) };
    // shortcut: LIKE ignores case for ASCII letters only (not for "Ä" and "ä"); a real full-text
    // index if people miss mail because of it.
    let pattern = search.text.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(like_pattern);
    let limit = search.limit.clamp(1, MAX_RESULTS) as i64;
    let query = || -> rusqlite::Result<Vec<IndexedMail>> {
        conn.prepare(&format!(
            "SELECT {COLUMNS} FROM mails m
             WHERE m.account_id = ?1 AND m.account_created_at = ?2
               AND (?3 IS NULL OR m.subject LIKE ?3 ESCAPE '\\' OR m.from_addr LIKE ?3 ESCAPE '\\'
                    OR m.to_addr LIKE ?3 ESCAPE '\\')
               AND (?4 IS NULL OR m.received_at >= ?4) AND (?5 IS NULL OR m.received_at < ?5)
             ORDER BY m.received_at DESC, m.id DESC LIMIT ?6"
        ))?
        .query_map(params![owner.account_id, owner.created_at, pattern, search.after, search.before, limit], mail_of)?
        .collect()
    };
    query().map_err(|err| damaged(&err))
}

/// One mail of `owner` in the snapshots, by its id in the index.
pub fn get(index: &Path, owner: Owner, id: i64) -> Result<Option<IndexedMail>, Error> {
    let Some(conn) = open_to_read(index)? else { return Ok(None) };
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM mails m WHERE m.id = ?1 AND m.account_id = ?2 AND m.account_created_at = ?3"),
        params![id, owner.account_id, owner.created_at],
        mail_of,
    )
    .optional()
    .map_err(|err| damaged(&err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_are_searched_for_as_they_are() {
        assert_eq!(like_pattern("50%_a\\b"), "%50\\%\\_a\\\\b%");
    }
}
