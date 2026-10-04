//! The messages a move left out, so nobody has to guess which: for an admin's move per mailbox, for
//! a person's own move per job. The copy (in the server crate) counts them in the progress and
//! hands over what it knows of each one: the folder, the UID, why, and what the headers said.
//!
//! The list keeps at most [`MAX_SKIPPED_LISTED`] per mailbox or job, the first ones; the counts go
//! on counting. A message is listed once for a reason, however often a round or a retry meets it.

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{Result, Store, now};

/// Messages listed per mailbox or job, at most.
pub const MAX_SKIPPED_LISTED: i64 = 1000;
/// The longest sender and subject kept.
const MAX_TEXT: usize = 300;

/// Why a message was left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SkipReason {
    /// The folder's mailbox here holds it already.
    Known,
    /// Larger than this server takes.
    TooLarge,
    /// It could not be read safely (nested too deep, made of too many parts).
    Unreadable,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::TooLarge => "tooLarge",
            Self::Unreadable => "unreadable",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "tooLarge" => Self::TooLarge,
            "unreadable" => Self::Unreadable,
            _ => Self::Known,
        }
    }
}

/// One message a move left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedMessage {
    /// The folder at the old provider, as its path reads.
    pub folder: String,
    pub uid: u32,
    pub reason: SkipReason,
    /// From and Subject as the headers said (empty when they could not be read), and the Date.
    pub from: String,
    pub subject: String,
    pub date: Option<i64>,
    /// Its size at the old provider, in bytes.
    pub size: i64,
    /// When it was written down; set by the store.
    pub recorded_at: i64,
}

/// Whose list a message goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkippedOf {
    /// A mailbox of an admin's move (`move_mailboxes`).
    MoveMailbox(i64),
    /// A person's own move (`migration_jobs`).
    MigrationJob(i64),
}

impl SkippedOf {
    /// The list's table, its owner column and the owner's table.
    fn tables(self) -> (&'static str, &'static str, &'static str, i64) {
        match self {
            Self::MoveMailbox(id) => ("move_mailbox_skipped", "move_mailbox_id", "move_mailboxes", id),
            Self::MigrationJob(id) => ("migration_job_skipped", "job_id", "migration_jobs", id),
        }
    }
}

fn shorten(value: &str) -> String {
    let value = value.trim();
    match value.char_indices().nth(MAX_TEXT) {
        Some((cut, _)) => format!("{}...", &value[..cut]),
        None => value.to_owned(),
    }
}

/// Empties a list, when its counts start from zero again.
pub(crate) fn clear_skipped(tx: &Connection, of: SkippedOf) -> Result<()> {
    let (table, owner, _, id) = of.tables();
    tx.execute(&format!("DELETE FROM {table} WHERE {owner} = ?1"), [id])?;
    Ok(())
}

impl Store {
    /// Writes down messages a move left out. Ones listed already for the same reason, ones past
    /// [`MAX_SKIPPED_LISTED`] and ones of a mailbox or job that is gone meanwhile are passed over.
    pub async fn note_skipped_messages(&self, of: SkippedOf, messages: Vec<SkippedMessage>) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let at = now();
        self.write(move |tx| {
            let (table, owner, owners, id) = of.tables();
            let mut stmt = tx.prepare(&format!(
                "INSERT OR IGNORE INTO {table} ({owner}, folder, uid, reason, sender, subject, sent_at, size, recorded_at)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
                 WHERE EXISTS (SELECT 1 FROM {owners} WHERE id = ?1)
                   AND (SELECT count(*) FROM {table} WHERE {owner} = ?1) < ?10"
            ))?;
            for message in messages {
                stmt.execute(params![
                    id,
                    shorten(&message.folder),
                    i64::from(message.uid),
                    message.reason.as_str(),
                    shorten(&message.from),
                    shorten(&message.subject),
                    message.date,
                    message.size.max(0),
                    at,
                    MAX_SKIPPED_LISTED,
                ])?;
            }
            Ok(())
        })
        .await
    }

    /// The messages a move left out, in the order they were met.
    pub async fn skipped_messages(&self, of: SkippedOf) -> Result<Vec<SkippedMessage>> {
        self.read(move |conn| {
            let (table, owner, _, id) = of.tables();
            let mut stmt = conn.prepare(&format!(
                "SELECT folder, uid, reason, sender, subject, sent_at, size, recorded_at FROM {table}
                 WHERE {owner} = ?1 ORDER BY recorded_at, rowid"
            ))?;
            let rows = stmt.query_map([id], |row| {
                Ok(SkippedMessage {
                    folder: row.get(0)?,
                    uid: row.get::<_, i64>(1)?.clamp(0, i64::from(u32::MAX)) as u32,
                    reason: SkipReason::parse(&row.get::<_, String>(2)?),
                    from: row.get(3)?,
                    subject: row.get(4)?,
                    date: row.get(5)?,
                    size: row.get(6)?,
                    recorded_at: row.get(7)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MigrationRun, NewAccount, NewMigrationJob, Role};

    fn skipped(folder: &str, uid: u32, reason: SkipReason) -> SkippedMessage {
        SkippedMessage {
            folder: folder.into(),
            uid,
            reason,
            from: "Nyu <nyu@example.net>".into(),
            subject: "Hallo".into(),
            date: Some(1_700_000_000),
            size: 1234,
            recorded_at: 0,
        }
    }

    #[tokio::test]
    async fn a_list_takes_each_message_once_and_is_capped() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let new = NewAccount {
            address: "mini@example.org".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let mini = store.create_account(new).await.unwrap().id;
        let job = store
            .create_migration_job(NewMigrationJob {
                account_id: mini,
                address: "mini@example.net".into(),
                host: "imap.example.net".into(),
                port: 993,
                login: "mini@example.net".into(),
                password: "altes-passwort".into(),
            })
            .await
            .unwrap();
        let of = SkippedOf::MigrationJob(job.id);
        let first = vec![skipped("INBOX", 1, SkipReason::Known), skipped("INBOX", 2, SkipReason::TooLarge)];
        store.note_skipped_messages(of, first.clone()).await.unwrap();
        // A retry meets them again: still listed once.
        store.note_skipped_messages(of, first).await.unwrap();
        let listed = store.skipped_messages(of).await.unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert_eq!((listed[1].uid, listed[1].reason, listed[1].size), (2, SkipReason::TooLarge, 1234));
        assert!(listed[0].recorded_at > 0);
        let json = serde_json::to_string(&listed[1]).unwrap();
        assert!(json.contains(r#""reason":"tooLarge""#) && json.contains(r#""recordedAt""#), "{json}");

        // Never more than the cap.
        let many: Vec<_> =
            (10..10 + MAX_SKIPPED_LISTED as u32).map(|uid| skipped("Archiv", uid, SkipReason::Unreadable)).collect();
        store.note_skipped_messages(of, many).await.unwrap();
        assert_eq!(store.skipped_messages(of).await.unwrap().len() as i64, MAX_SKIPPED_LISTED);

        // A new round after a finished one starts an empty list; a job that is gone takes its list along.
        store.take_migration_job().await.unwrap().unwrap();
        store.finish_migration_run(job.id, MigrationRun::Done).await.unwrap();
        store.sync_migration_job(mini, job.id, None).await.unwrap();
        assert!(store.skipped_messages(of).await.unwrap().is_empty());
        store.note_skipped_messages(of, vec![skipped("INBOX", 3, SkipReason::Known)]).await.unwrap();
        store.delete_migration_job(mini, job.id).await.unwrap();
        assert!(store.skipped_messages(of).await.unwrap().is_empty());
        store.note_skipped_messages(of, vec![skipped("INBOX", 4, SkipReason::Known)]).await.unwrap();
        assert!(store.skipped_messages(of).await.unwrap().is_empty(), "nothing for a job that is gone");
    }
}
