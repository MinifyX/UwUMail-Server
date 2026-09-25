use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::{Result, StoreError};

const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_jmap.sql"),
    include_str!("migrations/0003_web.sql"),
    include_str!("migrations/0004_admin.sql"),
    include_str!("migrations/0005_dkim_rotation.sql"),
    include_str!("migrations/0006_security.sql"),
    include_str!("migrations/0007_self_service.sql"),
    include_str!("migrations/0008_mta_sts_reports.sql"),
    include_str!("migrations/0009_spam.sql"),
    include_str!("migrations/0010_spam_verdicts.sql"),
    include_str!("migrations/0011_bayes.sql"),
    include_str!("migrations/0012_sender_lists.sql"),
    include_str!("migrations/0013_word_lists.sql"),
    include_str!("migrations/0014_feeds.sql"),
    include_str!("migrations/0015_imap.sql"),
    include_str!("migrations/0016_dav.sql"),
    include_str!("migrations/0017_sender_patterns.sql"),
    include_str!("migrations/0018_spam_limits.sql"),
    include_str!("migrations/0019_forward_addresses.sql"),
    include_str!("migrations/0020_send_as_domains.sql"),
    include_str!("migrations/0021_imported_app_passwords.sql"),
    include_str!("migrations/0022_import_progress.sql"),
    include_str!("migrations/0023_report_detail.sql"),
    include_str!("migrations/0024_spam_log.sql"),
    include_str!("migrations/0025_service_accounts.sql"),
    include_str!("migrations/0026_greylist_hold.sql"),
    include_str!("migrations/0027_fetch_accounts.sql"),
    include_str!("migrations/0028_webmail.sql"),
    include_str!("migrations/0029_fetch_sending.sql"),
    include_str!("migrations/0030_fetch_backlog.sql"),
    include_str!("migrations/0031_user_settings.sql"),
    include_str!("migrations/0032_jmap_calendars.sql"),
    include_str!("migrations/0033_sieve.sql"),
    include_str!("migrations/0034_jmap_contacts.sql"),
    include_str!("migrations/0035_rule_stats.sql"),
    include_str!("migrations/0036_jmap_sending.sql"),
];
const MAX_IDLE_READERS: usize = 8;

pub struct Database {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Database> {
        let mut writer = Connection::open(path)?;
        writer.busy_timeout(Duration::from_secs(10))?;
        writer.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        migrate(&mut writer)?;
        Ok(Database { path: path.to_path_buf(), writer: Mutex::new(writer), readers: Mutex::new(Vec::new()) })
    }

    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = match self.readers.lock().expect("reader pool poisoned").pop() {
            Some(conn) => conn,
            None => self.open_reader()?,
        };
        let result = f(&conn);
        let mut readers = self.readers.lock().expect("reader pool poisoned");
        if readers.len() < MAX_IDLE_READERS {
            readers.push(conn);
        }
        result
    }

    pub fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.writer.lock().map_err(|_| StoreError::Internal("writer poisoned".into()))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    fn open_reader(&self) -> Result<Connection> {
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
        )?;
        conn.busy_timeout(Duration::from_secs(10))?;
        Ok(conn)
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current = conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))? as usize;
    if current > MIGRATIONS.len() {
        return Err(StoreError::Internal(format!(
            "the database was created by a newer UwUMail server (schema {current}, this build knows {})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
        tracing::info!(version = index + 1, "applied database migration");
    }
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0)).optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn delete_setting(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM settings WHERE key = ?1", [key])? > 0)
}

/// Increments and returns the account's change sequence number.
pub fn next_modseq(conn: &Connection, account_id: i64) -> Result<i64> {
    conn.query_row("UPDATE accounts SET modseq = modseq + 1 WHERE id = ?1 RETURNING modseq", [account_id], |row| {
        row.get(0)
    })
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))
}

pub fn record_change(
    conn: &Connection,
    account_id: i64,
    modseq: i64,
    kind: &str,
    object_id: i64,
    change: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO changes (account_id, modseq, kind, object_id, change) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![account_id, modseq, kind, object_id, change],
    )?;
    Ok(())
}
