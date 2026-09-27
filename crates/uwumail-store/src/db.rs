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
    include_str!("migrations/0039_calendar_subscriptions.sql"),
    include_str!("migrations/0040_migration_jobs.sql"),
    include_str!("migrations/0041_groups_shared_mailboxes.sql"),
    include_str!("migrations/0042_masked_addresses.sql"),
    include_str!("migrations/0043_tls_rpt.sql"),
    include_str!("migrations/0044_oauth.sql"),
    include_str!("migrations/0045_stats_alerts.sql"),
    include_str!("migrations/0046_push_subscriptions.sql"),
    include_str!("migrations/0047_masked_domains.sql"),
    include_str!("migrations/0048_logins_and_ids.sql"),
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

/// The id for a new row of `accounts`, `app_passwords` or `oauth_grants`: one above the highest
/// that table ever had. Those ids stand for a person or a credential, so they are never handed out
/// twice, and the table refuses any other (migration 0048). Call it in the transaction that inserts.
pub(crate) fn next_id(conn: &Connection, table: &str) -> Result<i64> {
    Ok(conn.query_row("SELECT value + 1 FROM id_high_water WHERE name = ?1", [table], |row| row.get(0))?)
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
    /// UwUMail 0.15.0 shipped with the first 46 migrations.
    const RELEASED_0_15: usize = 46;

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
        assert!(table_exists(&conn, "groups") && table_exists(&conn, "group_members"));
        assert!(table_exists(&conn, "shared_mailbox_members") && table_exists(&conn, "masked_addresses"));
        assert!(table_exists(&conn, "tls_rpt_sessions") && table_exists(&conn, "tls_rpt_sent"));
        assert!(table_exists(&conn, "push_subscriptions"));
        assert!(table_exists(&conn, "migration_jobs"));
        assert!(table_exists(&conn, "stats_daily") && table_exists(&conn, "alerts"));
        assert!(table_exists(&conn, "oauth_grants") && table_exists(&conn, "external_identities"));
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

    #[test]
    fn a_domain_open_for_masked_addresses_keeps_them_for_its_own_people() {
        let mut conn = connection();
        for (index, sql) in MIGRATIONS[..RELEASED_0_15].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", (index + 1) as i64).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO domains (id, name, created_at, masked_addresses) VALUES (1, 'example.org', 0, 1),
                 (2, 'example.net', 0, 0);
             INSERT INTO accounts (id, login, created_at) VALUES (1, 'leni@example.net', 0);
             INSERT INTO masked_addresses (account_id, local_part, domain_id, created_at)
                 VALUES (1, 'maple.otter482', 1, 0);",
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len());
        let domains: Vec<(String, String, String)> = conn
            .prepare("SELECT name, kind, masked_mode FROM domains ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            domains,
            vec![
                ("example.org".to_owned(), "mail".to_owned(), "own".to_owned()),
                ("example.net".to_owned(), "mail".to_owned(), "off".to_owned())
            ]
        );
        // The old switch is gone, and the masked address Leni made there stays hers.
        let old: i64 = conn
            .query_row("SELECT count(*) FROM pragma_table_info('domains') WHERE name = 'masked_addresses'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(old, 0);
        let owner: i64 = conn.query_row("SELECT account_id FROM masked_addresses", [], |row| row.get(0)).unwrap();
        assert_eq!(owner, 1);
        let custom: Option<String> =
            conn.query_row("SELECT masked_mode FROM accounts WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(custom, None);
        assert!(table_exists(&conn, "domain_masked_domains") && table_exists(&conn, "account_masked_domains"));
    }

    #[test]
    fn ids_in_use_or_still_named_are_not_handed_out_after_upgrading_0_15() {
        let mut conn = connection();
        for (index, sql) in MIGRATIONS[..RELEASED_0_15].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", (index + 1) as i64).unwrap();
        }
        // Account 7 was purged: its learned words stayed behind. App password 5 was revoked, and a
        // push subscription made with it was still waiting for the clean-up.
        conn.execute_batch(
            "INSERT INTO accounts (id, login, created_at) VALUES (1, 'mini@example.org', 0), (3, 'nyu@example.org', 0);
             INSERT INTO bayes_totals (account_id, spam, ham) VALUES (0, 1, 1), (1, 1, 1), (7, 1, 1);
             INSERT INTO bayes_tokens (account_id, token, spam, updated_at) VALUES (7, 1, 1, 0), (1, 1, 1, 0);
             INSERT INTO app_passwords (id, account_id, name, secret_hash, scopes, created_at)
                 VALUES (2, 1, 'phone', x'01', 'mail', 0);
             INSERT INTO push_subscriptions (account_id, credential, device_client_id, url, url_digest, url_shown,
                     verification_code, expires, created_at)
                 VALUES (1, 'app:5', 'phone', x'00', 'a', 'push.example.net', 'code', 0, 0),
                        (1, 'app:2', 'phone', x'00', 'b', 'push.example.net', 'code', 0, 0);
             INSERT INTO blobs (hash, size, created_at) VALUES ('ab', 1, 0);
             INSERT INTO threads (id, account_id) VALUES (1, 1);
             INSERT INTO emails (id, account_id, thread_id, blob_hash, size, received_at, created_modseq, updated_modseq)
                 VALUES (1, 1, 1, 'ab', 1, 0, 0, 0);
             INSERT INTO email_keywords (email_id, keyword) VALUES (1, '$seen'), (1, 'project-x'),
                 (1, 'a' || char(13, 10) || '* BYE x'), (1, 'two words'), (1, 'x)'), (1, 'x]'), (1, 'x\\'),
                 (1, 'ümlaut');",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let high = |name: &str| -> i64 {
            conn.query_row("SELECT value FROM id_high_water WHERE name = ?1", [name], |row| row.get(0)).unwrap()
        };
        assert_eq!(high("accounts"), 7);
        assert_eq!(high("app_passwords"), 5);
        assert_eq!(high("oauth_grants"), 0);
        assert_eq!(next_id(&conn, "accounts").unwrap(), 8);
        let orphans: i64 = conn
            .query_row(
                "SELECT (SELECT count(*) FROM bayes_totals WHERE account_id = 7)
                      + (SELECT count(*) FROM bayes_tokens WHERE account_id = 7)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
        let kept: i64 = conn
            .query_row("SELECT count(*) FROM bayes_totals WHERE account_id IN (0, 1)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept, 2, "the server's and living people's learned words stay");
        let credentials: Vec<String> = conn
            .prepare("SELECT credential FROM push_subscriptions")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(credentials, vec!["app:2".to_owned()]);
        // Keywords that are no IMAP atom go.
        let keywords: Vec<String> = conn
            .prepare("SELECT keyword FROM email_keywords ORDER BY keyword")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(keywords, vec!["$seen".to_owned(), "project-x".to_owned()]);
    }
}
