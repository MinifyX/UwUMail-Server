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
    include_str!("migrations/0037_mailbox_acl.sql"),
    include_str!("migrations/0038_calendar_sharing_itip.sql"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// UwUMail 0.11.0 shipped with the first 35 migrations.
    const RELEASED_0_11: usize = 35;

    fn connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn
    }

    fn version(conn: &Connection) -> usize {
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0)).unwrap() as usize
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
            == 1
    }

    #[test]
    fn a_fresh_database_gets_every_migration() {
        let mut conn = connection();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len());
        assert!(table_exists(&conn, "mailbox_acl") && table_exists(&conn, "dav_shares"));
        // Running again changes nothing.
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len());
    }

    #[test]
    fn a_database_of_0_11_is_upgraded_with_its_data() {
        let mut conn = connection();
        for (index, sql) in MIGRATIONS[..RELEASED_0_11].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", (index + 1) as i64).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO accounts (id, login, created_at) VALUES (1, 'mini@example.org', 0), (2, 'nyu@example.org', 0);
             INSERT INTO user_settings (account_id, key, value) VALUES (1, 'undoSendSeconds', '10'),
                 (1, 'theme', '\"dark\"'), (2, 'undoSendSeconds', '99');
             INSERT INTO dav_collections (id, account_id, kind, slug, created_at) VALUES (1, 1, 'calendar', 'personal', 0);
             INSERT INTO dav_resources (collection_id, name, uid, etag, content, size, modified_at, change)
                 VALUES (1, 'a.ics', 'a', '\"e1\"', 'BEGIN:VCALENDAR', 15, 0, 1);",
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len());
        // The undo window moved into the preferences, where it was a value the portal offers.
        let undo: Vec<Option<String>> = conn
            .prepare("SELECT json_extract(preferences, '$.mailUndoSend') FROM accounts ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(undo, vec![Some("10".to_owned()), None]);
        let left: i64 = conn
            .query_row("SELECT count(*) FROM user_settings WHERE key = 'undoSendSeconds'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 0);
        let theme: String =
            conn.query_row("SELECT value FROM user_settings WHERE key = 'theme'", [], |row| row.get(0)).unwrap();
        assert_eq!(theme, "\"dark\"");
        // Existing events get a Schedule-Tag, and sharing starts empty.
        let tag: String = conn.query_row("SELECT schedule_tag FROM dav_resources", [], |row| row.get(0)).unwrap();
        assert_eq!(tag, "\"e1\"");
        let shares: i64 = conn.query_row("SELECT count(*) FROM mailbox_acl", [], |row| row.get(0)).unwrap();
        assert_eq!(shares, 0);
    }
}
