//! What the Bayes filter learned: per token, how often it turned up in spam and in wanted mail, for the
//! whole server and for each person, plus the queue of messages still to be learned. Splitting a
//! message into tokens and weighing them happens in the SMTP crate; this only keeps the numbers.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;

use crate::blobs::BlobHash;
use crate::{Result, Store, now};

/// The scope of knowledge that belongs to the whole server rather than one person.
const SERVER: i64 = 0;

/// Each scope needs this many learned spam and this many learned wanted messages before it counts.
pub const BAYES_MIN_LEARNED: i64 = 50;
/// When learning from existing folders, read mail counts as wanted once it is this old, so fresh mail
/// nobody judged yet stays out.
pub const BAYES_WANTED_AFTER_SECS: i64 = 14 * 24 * 3600;
/// At most this many messages of each kind per person are queued from existing folders at once.
pub const BAYES_FOLDER_LIMIT: usize = 2000;
/// Tokens seen only once and not for this long are forgotten; they would hardly ever count.
pub const BAYES_RARE_TOKEN_SECS: i64 = 90 * 24 * 3600;
/// What a message was learned as is forgotten after this long; marking it later then learns it anew.
pub const BAYES_LEARNED_SECS: i64 = 365 * 24 * 3600;

/// How many messages a scope learned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BayesTotals {
    pub spam: i64,
    pub ham: i64,
}

/// A queued message to learn: as spam or as wanted mail, for the server or one person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BayesJob {
    pub id: i64,
    pub blob: BlobHash,
    /// `None` for the whole server.
    pub account_id: Option<i64>,
    pub spam: bool,
}

fn scope(account_id: Option<i64>) -> i64 {
    account_id.unwrap_or(SERVER)
}

/// Queues a stored message to be learned, inside a transaction that already runs, e.g. the one
/// that marked it.
pub(crate) fn queue_learning(tx: &Transaction<'_>, blob_hash: &str, account_id: Option<i64>, spam: bool) -> Result<()> {
    tx.execute(
        "INSERT INTO bayes_queue (blob_hash, account_id, spam, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![blob_hash, scope(account_id), spam, now()],
    )?;
    Ok(())
}

/// A person marked an email as spam or not: it is learned for the whole server and for them.
pub(crate) fn queue_marked(tx: &Transaction<'_>, email_id: i64, account_id: i64, spam: bool) -> Result<()> {
    let blob: Option<String> =
        tx.query_row("SELECT blob_hash FROM emails WHERE id = ?1", params![email_id], |row| row.get(0)).optional()?;
    if let Some(blob) = blob {
        queue_learning(tx, &blob, None, spam)?;
        queue_learning(tx, &blob, Some(account_id), spam)?;
    }
    Ok(())
}

impl Store {
    /// Queues a stored message to be learned, e.g. one the filter judged clearly spam or clearly fine.
    pub async fn queue_bayes_learning(&self, blob: BlobHash, account_id: Option<i64>, spam: bool) -> Result<()> {
        self.write(move |tx| queue_learning(tx, blob.as_str(), account_id, spam)).await
    }

    /// The oldest messages waiting to be learned. They stay queued until learned or dropped, so a
    /// restart in between does not lose them, and learning one twice changes nothing.
    pub async fn bayes_jobs(&self, limit: usize) -> Result<Vec<BayesJob>> {
        self.read(move |conn| {
            let mut stmt =
                conn.prepare("SELECT id, blob_hash, account_id, spam FROM bayes_queue ORDER BY id LIMIT ?1")?;
            let rows = stmt.query_map(params![limit as i64], |row| {
                let account: i64 = row.get(2)?;
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, account, row.get::<_, bool>(3)?))
            })?;
            let mut jobs = Vec::new();
            for row in rows {
                let (id, blob, account, spam) = row?;
                let Ok(blob) = BlobHash::parse(&blob) else { continue };
                jobs.push(BayesJob { id, blob, account_id: (account != SERVER).then_some(account), spam });
            }
            Ok(jobs)
        })
        .await
    }

    /// Takes a job off the queue without learning it, e.g. because its message is gone.
    pub async fn drop_bayes_job(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM bayes_queue WHERE id = ?1", params![id])?;
            Ok(())
        })
        .await
    }

    /// Learns a queued message's tokens and takes it off the queue. A message already learned the same
    /// way changes nothing; one learned the other way is unlearned first. Returns whether anything
    /// changed.
    pub async fn learn_bayes(&self, job: BayesJob, tokens: Vec<i64>) -> Result<bool> {
        self.write(move |tx| {
            let scope = scope(job.account_id);
            let blob = job.blob.as_str().to_owned();
            tx.execute("DELETE FROM bayes_queue WHERE id = ?1", params![job.id])?;
            let before: Option<bool> = tx
                .query_row(
                    "SELECT spam FROM bayes_learned WHERE blob_hash = ?1 AND account_id = ?2",
                    params![blob, scope],
                    |row| row.get(0),
                )
                .optional()?;
            if before == Some(job.spam) {
                return Ok(false);
            }
            let now = now();
            let (learn, unlearn) = if job.spam { ("spam", "ham") } else { ("ham", "spam") };
            {
                let mut add = tx.prepare_cached(&format!(
                    "INSERT INTO bayes_tokens (account_id, token, {learn}, updated_at) VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT (account_id, token) DO UPDATE SET {learn} = {learn} + 1, updated_at = ?3"
                ))?;
                let mut remove = tx.prepare_cached(&format!(
                    "UPDATE bayes_tokens SET {unlearn} = MAX({unlearn} - 1, 0) WHERE account_id = ?1 AND token = ?2"
                ))?;
                for token in &tokens {
                    if before.is_some() {
                        remove.execute(params![scope, token])?;
                    }
                    add.execute(params![scope, token, now])?;
                }
            }
            tx.execute(
                &format!(
                    "INSERT INTO bayes_totals (account_id, {learn}) VALUES (?1, 1)
                     ON CONFLICT (account_id) DO UPDATE SET {learn} = {learn} + 1"
                ),
                params![scope],
            )?;
            if before.is_some() {
                tx.execute(
                    &format!("UPDATE bayes_totals SET {unlearn} = MAX({unlearn} - 1, 0) WHERE account_id = ?1"),
                    params![scope],
                )?;
            }
            tx.execute(
                "INSERT INTO bayes_learned (blob_hash, account_id, spam, learned_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (blob_hash, account_id) DO UPDATE SET spam = ?3, learned_at = ?4",
                params![blob, scope, job.spam, now],
            )?;
            Ok(true)
        })
        .await
    }

    /// How many messages wait to be learned.
    pub async fn bayes_queue_length(&self) -> Result<i64> {
        self.read(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM bayes_queue", [], |row| row.get(0))?)).await
    }

    /// How many messages the whole server (`None`) or one person learned.
    pub async fn bayes_totals(&self, account_id: Option<i64>) -> Result<BayesTotals> {
        self.read(move |conn| {
            let totals = conn
                .query_row(
                    "SELECT spam, ham FROM bayes_totals WHERE account_id = ?1",
                    params![scope(account_id)],
                    |row| Ok(BayesTotals { spam: row.get(0)?, ham: row.get(1)? }),
                )
                .optional()?;
            Ok(totals.unwrap_or_default())
        })
        .await
    }

    /// How often each of these tokens turned up in spam and in wanted mail, for a scope. Tokens never
    /// seen are left out.
    pub async fn bayes_counts(&self, account_id: Option<i64>, tokens: Vec<i64>) -> Result<HashMap<i64, (i64, i64)>> {
        self.read(move |conn| {
            let scope = scope(account_id);
            let mut stmt =
                conn.prepare_cached("SELECT spam, ham FROM bayes_tokens WHERE account_id = ?1 AND token = ?2")?;
            let mut found = HashMap::new();
            for token in tokens {
                let counts = stmt.query_row(params![scope, token], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
                if let Some(counts) = counts {
                    found.insert(token, counts);
                }
            }
            Ok(found)
        })
        .await
    }

    /// Queues learning from a person's existing mail, for the whole server and for them: what lies in
    /// Junk as spam, and read mail in the inbox or the archive older than `older_than_secs` as wanted
    /// mail. Up to `limit` of each; returns how many of each were queued.
    pub async fn queue_bayes_from_folders(
        &self,
        account_id: i64,
        older_than_secs: i64,
        limit: usize,
    ) -> Result<(usize, usize)> {
        self.write(move |tx| {
            let found = |sql: &str, values: &[&dyn rusqlite::ToSql]| -> Result<Vec<String>> {
                let mut stmt = tx.prepare(sql)?;
                let rows = stmt.query_map(values, |row| row.get(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
            };
            let limit = limit as i64;
            let spam = found(
                "SELECT DISTINCT e.blob_hash FROM emails e
                 JOIN email_mailboxes em ON em.email_id = e.id JOIN mailboxes m ON m.id = em.mailbox_id
                 WHERE e.account_id = ?1 AND m.role = 'junk'
                 ORDER BY e.received_at DESC LIMIT ?2",
                &[&account_id, &limit],
            )?;
            let ham = found(
                "SELECT DISTINCT e.blob_hash FROM emails e
                 JOIN email_mailboxes em ON em.email_id = e.id JOIN mailboxes m ON m.id = em.mailbox_id
                 WHERE e.account_id = ?1 AND m.role IN ('inbox', 'archive') AND e.received_at < ?2
                   AND EXISTS (SELECT 1 FROM email_keywords k WHERE k.email_id = e.id AND k.keyword = '$seen')
                   AND NOT EXISTS (SELECT 1 FROM email_keywords k WHERE k.email_id = e.id AND k.keyword = '$junk')
                 ORDER BY e.received_at DESC LIMIT ?3",
                &[&account_id, &(now() - older_than_secs), &limit],
            )?;
            for (blobs, spam) in [(&spam, true), (&ham, false)] {
                for blob in blobs {
                    queue_learning(tx, blob, None, spam)?;
                    queue_learning(tx, blob, Some(account_id), spam)?;
                }
            }
            Ok((spam.len(), ham.len()))
        })
        .await
    }

    /// Forgets tokens seen only once long ago, what messages were learned as after a year, and the
    /// knowledge of people who are gone. Returns how many rows went.
    pub async fn prune_bayes(&self, rare_token_secs: i64, learned_secs: i64) -> Result<usize> {
        self.write(move |tx| {
            let now = now();
            let mut removed = tx.execute(
                "DELETE FROM bayes_tokens WHERE spam + ham <= 1 AND updated_at < ?1",
                params![now - rare_token_secs],
            )?;
            removed += tx.execute("DELETE FROM bayes_learned WHERE learned_at < ?1", params![now - learned_secs])?;
            for table in ["bayes_tokens", "bayes_totals", "bayes_learned", "bayes_queue"] {
                removed += tx.execute(
                    &format!(
                        "DELETE FROM {table} WHERE account_id != 0 AND account_id NOT IN (SELECT id FROM accounts)"
                    ),
                    [],
                )?;
            }
            Ok(removed)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EmailUpdate, IngestRequest, KeywordsChange, MailboxRole, MailboxTarget, MailboxesChange, NewAccount, Role,
    };

    #[tokio::test]
    async fn learning_counts_once_and_changing_ones_mind_unlearns_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let blob = BlobHash::of(b"a message");
        store.queue_bayes_learning(blob.clone(), None, true).await.unwrap();
        let queued = store.bayes_jobs(10).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert!(store.learn_bayes(queued[0].clone(), vec![1, 2]).await.unwrap());
        assert!(store.bayes_jobs(10).await.unwrap().is_empty(), "a learned job leaves the queue");
        assert_eq!(store.bayes_totals(None).await.unwrap(), BayesTotals { spam: 1, ham: 0 });

        let job = |spam| BayesJob { id: 0, blob: blob.clone(), account_id: None, spam };
        assert!(!store.learn_bayes(job(true), vec![1, 2]).await.unwrap(), "the same again changes nothing");
        assert!(store.learn_bayes(job(false), vec![1, 2]).await.unwrap());
        assert_eq!(store.bayes_totals(None).await.unwrap(), BayesTotals { spam: 0, ham: 1 });
        let counts = store.bayes_counts(None, vec![1, 2, 3]).await.unwrap();
        assert_eq!((counts.get(&1), counts.get(&3)), (Some(&(0, 1)), None));
        assert_eq!(
            store.bayes_totals(Some(7)).await.unwrap(),
            BayesTotals::default(),
            "a person's knowledge is separate"
        );
    }

    #[tokio::test]
    async fn marks_and_existing_folders_queue_learning_for_the_server_and_the_person() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("uwu.test").await.unwrap();
        let account = NewAccount {
            address: "mini@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
        };
        let account_id = store.create_account(account).await.unwrap().id;
        let month_ago = now() - 30 * 24 * 3600;
        let deliver = |raw: &[u8], role, keywords: &[&str], received_at| IngestRequest {
            account_id,
            raw: raw.to_vec(),
            mailboxes: vec![MailboxTarget::Role(role)],
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            received_at,
        };
        store.ingest(deliver(b"Subject: gratis\r\n\r\nx\r\n", MailboxRole::Junk, &[], None)).await.unwrap();
        store
            .ingest(deliver(b"Subject: Elternabend\r\n\r\nx\r\n", MailboxRole::Inbox, &["$seen"], Some(month_ago)))
            .await
            .unwrap();
        // Too new, or never read: not taken as wanted mail yet.
        let recent =
            store.ingest(deliver(b"Subject: neu\r\n\r\nx\r\n", MailboxRole::Inbox, &["$seen"], None)).await.unwrap();
        store
            .ingest(deliver(b"Subject: ungelesen\r\n\r\nx\r\n", MailboxRole::Inbox, &[], Some(month_ago)))
            .await
            .unwrap();

        assert_eq!(store.queue_bayes_from_folders(account_id, 14 * 24 * 3600, 100).await.unwrap(), (1, 1));
        let jobs = store.bayes_jobs(100).await.unwrap();
        assert_eq!(jobs.len(), 4, "each for the server and the person");
        assert_eq!(jobs.iter().filter(|job| job.account_id == Some(account_id)).count(), 2);

        // Marking a message as spam queues it the same way.
        let junk = store.mailboxes(account_id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Junk));
        let mark = EmailUpdate {
            id: recent.id,
            keywords: KeywordsChange::Keep,
            mailboxes: MailboxesChange::Replace(vec![junk.unwrap().id]),
        };
        assert!(store.update_emails(account_id, vec![mark]).await.unwrap().iter().all(Result::is_ok));
        let jobs = store.bayes_jobs(100).await.unwrap();
        assert_eq!(jobs.len(), 6);
        assert!(jobs[4..].iter().all(|job| job.spam && job.blob == recent.blob));
    }

    #[tokio::test]
    async fn rare_tokens_and_knowledge_of_people_who_left_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let job = |blob: &[u8], account_id| BayesJob { id: 0, blob: BlobHash::of(blob), account_id, spam: true };
        store.learn_bayes(job(b"one", None), vec![1, 2]).await.unwrap();
        store.learn_bayes(job(b"two", None), vec![2]).await.unwrap();
        store.learn_bayes(job(b"three", Some(999)), vec![5]).await.unwrap();

        let year = 365 * 24 * 3600;
        assert_eq!(store.prune_bayes(year, year).await.unwrap(), 3, "only the person who does not exist goes");
        // A negative age reaches everything seen only once.
        assert_eq!(store.prune_bayes(-1, year).await.unwrap(), 1);
        let counts = store.bayes_counts(None, vec![1, 2]).await.unwrap();
        assert_eq!((counts.get(&1), counts.get(&2)), (None, Some(&(2, 0))));
    }
}
