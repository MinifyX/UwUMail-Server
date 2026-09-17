//! Bookkeeping for copying mail from another server: how far each folder got.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store, now};

/// Where copying a folder stopped: its UIDVALIDITY on the old server and the last UID taken over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportProgress {
    pub uid_validity: u32,
    pub last_uid: u32,
}

impl Store {
    pub async fn import_progress(&self, account_id: i64, source: &str, folder: &str) -> Result<Option<ImportProgress>> {
        let (source, folder) = (source.to_owned(), folder.to_owned());
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT uid_validity, last_uid FROM import_progress
                     WHERE account_id = ?1 AND source = ?2 AND folder = ?3",
                    params![account_id, source, folder],
                    |row| Ok(ImportProgress { uid_validity: row.get(0)?, last_uid: row.get(1)? }),
                )
                .optional()?)
        })
        .await
    }

    pub async fn set_import_progress(
        &self,
        account_id: i64,
        source: &str,
        folder: &str,
        progress: ImportProgress,
    ) -> Result<()> {
        let (source, folder) = (source.to_owned(), folder.to_owned());
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO import_progress (account_id, source, folder, uid_validity, last_uid, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (account_id, source, folder)
                 DO UPDATE SET uid_validity = excluded.uid_validity, last_uid = excluded.last_uid,
                               updated_at = excluded.updated_at",
                params![account_id, source, folder, progress.uid_validity, progress.last_uid, now()],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    #[tokio::test]
    async fn progress_is_kept_per_folder() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.de").await.unwrap();
        let new = NewAccount {
            address: "mini@example.de".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
        };
        let mini = store.create_account(new).await.unwrap().id;
        assert_eq!(store.import_progress(mini, "old.example.de", "INBOX").await.unwrap(), None);
        let first = ImportProgress { uid_validity: 17, last_uid: 40 };
        store.set_import_progress(mini, "old.example.de", "INBOX", first).await.unwrap();
        let later = ImportProgress { uid_validity: 17, last_uid: 55 };
        store.set_import_progress(mini, "old.example.de", "INBOX", later).await.unwrap();
        assert_eq!(store.import_progress(mini, "old.example.de", "INBOX").await.unwrap(), Some(later));
        assert_eq!(store.import_progress(mini, "old.example.de", "Sent").await.unwrap(), None);
    }
}
