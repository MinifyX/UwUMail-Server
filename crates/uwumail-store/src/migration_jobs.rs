//! Moving from another provider: a person's old mailbox, copied over IMAP in the background.
//!
//! A job holds where the old mailbox is, the password for it (sealed, like the passwords of
//! fetched mailboxes) and how far the current round got. The copying itself is in the server
//! crate; it takes a queued job, works on it for a while and hands it back -- queued again when its
//! time was up, done when everything is here, or paused with a reason when the person has to do
//! something first (a full mailbox, a password the old provider refused).
//!
//! The person decides when it is over: "done" removes the job, and the password with it.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::normalize_address;
use crate::fetch::{check_host, seal, unseal};
use crate::{Result, Store, StoreError, now};

/// How many moves one person may have at once.
pub const MAX_MIGRATION_JOBS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MigrationState {
    /// Waiting for its turn.
    Queued,
    /// Being copied right now.
    Running,
    /// Stopped by something the person has to look at; `error` says what.
    Paused,
    /// Everything the old mailbox had is here.
    Done,
}

impl MigrationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Done => "done",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "done" => Self::Done,
            _ => Self::Queued,
        }
    }
}

/// A move, as the portal shows it. The password never leaves the store this way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationJob {
    pub id: i64,
    pub account_id: i64,
    pub address: String,
    pub host: String,
    pub port: u16,
    pub login: String,
    pub state: MigrationState,
    /// A code for why it paused (`quotaExceeded`, `loginRefused`, `unreachable`, `notPublic`,
    /// `noMailbox`, `stopped`, `failed`); empty otherwise.
    pub error: String,
    pub error_detail: String,
    #[serde(flatten)]
    pub progress: MigrationProgress,
    pub created_at: i64,
    /// When the current round began and ended.
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub last_run_at: Option<i64>,
}

/// How far the current round got.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationProgress {
    pub folders_done: i64,
    pub folders_total: i64,
    /// Copied and skipped together, out of `messages_total`.
    pub messages_done: i64,
    pub messages_total: i64,
    /// Messages that were already here and were left out.
    pub messages_skipped: i64,
    pub bytes_done: i64,
}

#[derive(Clone)]
pub struct NewMigrationJob {
    pub account_id: i64,
    pub address: String,
    pub host: String,
    pub port: u16,
    pub login: String,
    pub password: String,
}

impl std::fmt::Debug for NewMigrationJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The password never into a log line.
        f.debug_struct("NewMigrationJob")
            .field("account_id", &self.account_id)
            .field("address", &self.address)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("login", &self.login)
            .finish_non_exhaustive()
    }
}

/// How a run of the worker ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationRun {
    /// Its time was up; it goes back in the queue and continues where it stopped.
    Continue,
    /// Everything is here.
    Done,
    /// Stopped until the person does something: `code` for the portal, `detail` for the curious.
    Paused { code: String, detail: String },
}

const COLUMNS: &str = "id, account_id, address, host, port, login, state, error, error_detail, folders_done, \
                       folders_total, messages_done, messages_total, messages_skipped, bytes_done, created_at, \
                       started_at, finished_at, last_run_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<MigrationJob> {
    let state: String = row.get(6)?;
    Ok(MigrationJob {
        id: row.get(0)?,
        account_id: row.get(1)?,
        address: row.get(2)?,
        host: row.get(3)?,
        port: row.get::<_, i64>(4)? as u16,
        login: row.get(5)?,
        state: MigrationState::parse(&state),
        error: row.get(7)?,
        error_detail: row.get(8)?,
        progress: MigrationProgress {
            folders_done: row.get(9)?,
            folders_total: row.get(10)?,
            messages_done: row.get(11)?,
            messages_total: row.get(12)?,
            messages_skipped: row.get(13)?,
            bytes_done: row.get(14)?,
        },
        created_at: row.get(15)?,
        started_at: row.get(16)?,
        finished_at: row.get(17)?,
        last_run_at: row.get(18)?,
    })
}

/// What another server said, short enough for a table and a page.
fn shorten(value: &str) -> String {
    let value = value.trim();
    match value.char_indices().nth(300) {
        Some((cut, _)) => format!("{}...", &value[..cut]),
        None => value.to_owned(),
    }
}

impl Store {
    /// The moves of one person, newest first.
    pub async fn migration_jobs(&self, account_id: i64) -> Result<Vec<MigrationJob>> {
        self.read(move |conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT {COLUMNS} FROM migration_jobs WHERE account_id = ?1 ORDER BY id DESC"))?;
            let rows = stmt.query_map(params![account_id], from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// One move of this person. Like the fetched mailboxes, every call takes whose it is, so an id
    /// from a URL cannot reach anybody else's.
    pub async fn migration_job(&self, account_id: i64, id: i64) -> Result<Option<MigrationJob>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM migration_jobs WHERE id = ?1 AND account_id = ?2"),
                    params![id, account_id],
                    from_row,
                )
                .optional()?)
        })
        .await
    }

    /// Starts a move. It is queued at once; the worker picks it up within seconds.
    pub async fn create_migration_job(&self, new: NewMigrationJob) -> Result<MigrationJob> {
        let (local, domain) = normalize_address(&new.address)
            .map_err(|_| StoreError::Invalid(format!("'{}' is not a valid email address", new.address)))?;
        let address = format!("{local}@{domain}");
        if self.is_local_domain(&domain).await? {
            return Err(StoreError::Rule {
                code: "moveFromHere",
                message: "this address is on this server already; there is nothing to move".into(),
            });
        }
        let host = check_host(&new.host)?;
        let login = new.login.trim().to_owned();
        if login.is_empty() {
            return Err(StoreError::Invalid("the old provider needs a user name".into()));
        }
        if new.password.is_empty() {
            return Err(StoreError::Invalid("the old provider needs a password".into()));
        }
        if new.port == 0 {
            return Err(StoreError::Invalid("the port of the old provider is missing".into()));
        }
        let at = now();
        self.write(move |tx| {
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM migration_jobs WHERE account_id = ?1",
                params![new.account_id],
                |row| row.get(0),
            )?;
            if count as usize >= MAX_MIGRATION_JOBS {
                return Err(StoreError::Rule {
                    code: "moveLimit",
                    message: format!("at most {MAX_MIGRATION_JOBS} moves at once"),
                });
            }
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM migration_jobs WHERE account_id = ?1 AND address = ?2)",
                params![new.account_id, address],
                |row| row.get(0),
            )?;
            if taken {
                return Err(StoreError::Rule {
                    code: "moveExists",
                    message: format!("{address} is being moved already"),
                });
            }
            let sealed = seal(tx, &new.password)?;
            tx.execute(
                "INSERT INTO migration_jobs (account_id, address, host, port, login, password_sealed, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![new.account_id, address, host, i64::from(new.port), login, sealed, at],
            )?;
            let id = tx.last_insert_rowid();
            Ok(tx.query_row(&format!("SELECT {COLUMNS} FROM migration_jobs WHERE id = ?1"), params![id], from_row)?)
        })
        .await
    }

    /// The password for the old mailbox, for the worker.
    pub async fn migration_password(&self, account_id: i64, id: i64) -> Result<Option<String>> {
        self.read(move |conn| {
            let sealed: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT password_sealed FROM migration_jobs WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                    |row| row.get(0),
                )
                .optional()?;
            sealed.map(|sealed| unseal(conn, &sealed)).transpose()
        })
        .await
    }

    /// Takes the next queued move, the one that waited longest, and marks it running. Moves of
    /// accounts that are being deleted wait.
    pub async fn take_migration_job(&self) -> Result<Option<MigrationJob>> {
        let at = now();
        self.write(move |tx| {
            let id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM migration_jobs
                     WHERE state = 'queued'
                       AND account_id IN (SELECT id FROM accounts WHERE deleted_at IS NULL)
                     ORDER BY last_run_at IS NOT NULL, last_run_at, id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = id else { return Ok(None) };
            tx.execute(
                "UPDATE migration_jobs SET state = 'running', last_run_at = ?2, started_at = coalesce(started_at, ?2)
                 WHERE id = ?1",
                params![id, at],
            )?;
            Ok(Some(tx.query_row(
                &format!("SELECT {COLUMNS} FROM migration_jobs WHERE id = ?1"),
                params![id],
                from_row,
            )?))
        })
        .await
    }

    /// Writes down how far a running move got. `false` when it is not wanted any more -- the
    /// person paused it or said they are done -- and the worker should stop.
    pub async fn note_migration_progress(&self, id: i64, progress: MigrationProgress) -> Result<bool> {
        self.write(move |tx| {
            let changed = tx.execute(
                "UPDATE migration_jobs SET folders_done = ?2, folders_total = ?3, messages_done = ?4,
                     messages_total = ?5, messages_skipped = ?6, bytes_done = ?7
                 WHERE id = ?1 AND state = 'running'",
                params![
                    id,
                    progress.folders_done,
                    progress.folders_total,
                    progress.messages_done,
                    progress.messages_total,
                    progress.messages_skipped,
                    progress.bytes_done,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    /// Hands a move back after a run. Only a move that is still running changes: one the person
    /// paused or removed meanwhile stays as they left it.
    pub async fn finish_migration_run(&self, id: i64, run: MigrationRun) -> Result<()> {
        let at = now();
        self.write(move |tx| {
            match run {
                MigrationRun::Continue => tx.execute(
                    "UPDATE migration_jobs SET state = 'queued' WHERE id = ?1 AND state = 'running'",
                    params![id],
                )?,
                MigrationRun::Done => tx.execute(
                    "UPDATE migration_jobs SET state = 'done', error = '', error_detail = '', finished_at = ?2,
                         folders_done = folders_total, messages_total = messages_done
                     WHERE id = ?1 AND state = 'running'",
                    params![id, at],
                )?,
                MigrationRun::Paused { code, detail } => tx.execute(
                    "UPDATE migration_jobs SET state = 'paused', error = ?2, error_detail = ?3
                     WHERE id = ?1 AND state = 'running'",
                    params![id, code, shorten(&detail)],
                )?,
            };
            Ok(())
        })
        .await
    }

    /// Queues a move again. After it was done, a new round starts that only fetches what arrived
    /// at the old provider since; after a pause, the round goes on where it stopped. A new
    /// password replaces the stored one, for a pause over a refused password.
    pub async fn sync_migration_job(&self, account_id: i64, id: i64, password: Option<String>) -> Result<MigrationJob> {
        self.write(move |tx| {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM migration_jobs WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                    |row| row.get(0),
                )
                .optional()?;
            let state = state.ok_or_else(|| StoreError::NotFound(format!("move {id}")))?;
            match MigrationState::parse(&state) {
                MigrationState::Queued | MigrationState::Running => {
                    return Err(StoreError::Rule { code: "moveRunning", message: "this move is running".into() });
                }
                MigrationState::Done => {
                    tx.execute(
                        "UPDATE migration_jobs SET folders_done = 0, folders_total = 0, messages_done = 0,
                             messages_total = 0, messages_skipped = 0, bytes_done = 0, started_at = NULL,
                             finished_at = NULL
                         WHERE id = ?1 AND account_id = ?2",
                        params![id, account_id],
                    )?;
                }
                MigrationState::Paused => {}
            }
            if let Some(password) = password.filter(|password| !password.is_empty()) {
                let sealed = seal(tx, &password)?;
                tx.execute(
                    "UPDATE migration_jobs SET password_sealed = ?3 WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id, sealed],
                )?;
            }
            tx.execute(
                "UPDATE migration_jobs SET state = 'queued', error = '', error_detail = ''
                 WHERE id = ?1 AND account_id = ?2",
                params![id, account_id],
            )?;
            Ok(tx.query_row(&format!("SELECT {COLUMNS} FROM migration_jobs WHERE id = ?1"), params![id], from_row)?)
        })
        .await
    }

    /// Stops a queued or running move until the person goes on with it.
    pub async fn pause_migration_job(&self, account_id: i64, id: i64) -> Result<MigrationJob> {
        self.write(move |tx| {
            let changed = tx.execute(
                "UPDATE migration_jobs SET state = 'paused', error = 'stopped', error_detail = ''
                 WHERE id = ?1 AND account_id = ?2 AND state IN ('queued', 'running')",
                params![id, account_id],
            )?;
            let job = tx
                .query_row(
                    &format!("SELECT {COLUMNS} FROM migration_jobs WHERE id = ?1 AND account_id = ?2"),
                    params![id, account_id],
                    from_row,
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("move {id}")))?;
            if changed == 0 && job.state != MigrationState::Paused {
                return Err(StoreError::Rule { code: "moveNotRunning", message: "this move is not running".into() });
            }
            Ok(job)
        })
        .await
    }

    /// Ends a move for good and forgets the password. What was copied stays.
    pub async fn delete_migration_job(&self, account_id: i64, id: i64) -> Result<()> {
        self.write(move |tx| {
            let changed =
                tx.execute("DELETE FROM migration_jobs WHERE id = ?1 AND account_id = ?2", params![id, account_id])?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("move {id}")));
            }
            Ok(())
        })
        .await
    }

    /// Moves that were running when the server stopped go back in the queue; they continue where
    /// their folders stood.
    pub async fn requeue_running_migrations(&self) -> Result<usize> {
        self.write(|tx| Ok(tx.execute("UPDATE migration_jobs SET state = 'queued' WHERE state = 'running'", [])?)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    #[test]
    fn debug_leaves_the_password_out() {
        let job = NewMigrationJob {
            account_id: 1,
            address: "leni@example.org".into(),
            host: "imap.example.net".into(),
            port: 993,
            login: "leni".into(),
            password: "hunter2-plaintext".into(),
        };
        let shown = format!("{job:?}");
        assert!(shown.contains("imap.example.net"), "{shown}");
        assert!(!shown.contains("hunter2-plaintext"), "{shown}");
    }

    async fn person(store: &Store, address: &str) -> i64 {
        let new = NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        store.create_account(new).await.unwrap().id
    }

    fn new_job(account_id: i64, address: &str) -> NewMigrationJob {
        NewMigrationJob {
            account_id,
            address: address.into(),
            host: "imap.example.net".into(),
            port: 993,
            login: address.into(),
            password: "altes-passwort".into(),
        }
    }

    fn rule(err: StoreError) -> &'static str {
        match err {
            StoreError::Rule { code, .. } => code,
            other => panic!("not a rule: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_move_goes_through_its_states() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let nyu = person(&store, "nyu@example.org").await;

        let job = store.create_migration_job(new_job(mini, "Mini@Example.NET")).await.unwrap();
        assert_eq!((job.state, job.address.as_str()), (MigrationState::Queued, "mini@example.net"));
        assert_eq!(store.migration_password(mini, job.id).await.unwrap().as_deref(), Some("altes-passwort"));
        assert_eq!(store.migration_password(nyu, job.id).await.unwrap(), None, "only the owner's");
        let json = serde_json::to_string(&job).unwrap();
        assert!(!json.contains("altes-passwort") && json.contains(r#""messagesDone":0"#), "{json}");

        // Refused: the same address twice, an address of this server, a private host.
        assert_eq!(
            rule(store.create_migration_job(new_job(mini, "mini@example.net")).await.unwrap_err()),
            "moveExists"
        );
        assert_eq!(
            rule(store.create_migration_job(new_job(mini, "nyu@example.org")).await.unwrap_err()),
            "moveFromHere"
        );
        let private = NewMigrationJob { host: "192.168.1.1".into(), ..new_job(mini, "alt@example.net") };
        assert!(matches!(store.create_migration_job(private).await, Err(StoreError::Invalid(_))));

        // The worker takes it, notes progress, runs out of time, takes it again, finishes.
        let taken = store.take_migration_job().await.unwrap().unwrap();
        assert_eq!((taken.id, taken.state), (job.id, MigrationState::Running));
        assert!(store.take_migration_job().await.unwrap().is_none(), "nothing else is queued");
        assert!(sync_refused(&store, mini, job.id).await, "a running move cannot be queued again");
        let halfway = MigrationProgress {
            folders_total: 3,
            folders_done: 1,
            messages_total: 10,
            messages_done: 4,
            ..Default::default()
        };
        assert!(store.note_migration_progress(job.id, halfway).await.unwrap());
        store.finish_migration_run(job.id, MigrationRun::Continue).await.unwrap();
        let queued = store.migration_job(mini, job.id).await.unwrap().unwrap();
        assert_eq!((queued.state, queued.progress), (MigrationState::Queued, halfway));
        store.take_migration_job().await.unwrap().unwrap();
        let most = MigrationProgress { messages_done: 9, ..halfway };
        store.note_migration_progress(job.id, most).await.unwrap();
        store.finish_migration_run(job.id, MigrationRun::Done).await.unwrap();
        let done = store.migration_job(mini, job.id).await.unwrap().unwrap();
        assert_eq!(done.state, MigrationState::Done);
        assert_eq!((done.progress.folders_done, done.progress.messages_total), (3, 9));
        assert!(done.finished_at.is_some());

        // Sync again: a new round from zero.
        let again = store.sync_migration_job(mini, job.id, None).await.unwrap();
        assert_eq!(
            (again.state, again.progress, again.finished_at),
            (MigrationState::Queued, Default::default(), None)
        );

        // A full mailbox pauses it; going on keeps the round and takes a new password.
        store.take_migration_job().await.unwrap().unwrap();
        store.note_migration_progress(job.id, halfway).await.unwrap();
        let full = MigrationRun::Paused { code: "quotaExceeded".into(), detail: "mailbox is full".into() };
        store.finish_migration_run(job.id, full).await.unwrap();
        let paused = store.migration_job(mini, job.id).await.unwrap().unwrap();
        assert_eq!((paused.state, paused.error.as_str()), (MigrationState::Paused, "quotaExceeded"));
        assert!(store.take_migration_job().await.unwrap().is_none(), "a paused move waits");
        let resumed = store.sync_migration_job(mini, job.id, Some("neues-passwort".into())).await.unwrap();
        assert_eq!((resumed.state, resumed.error.as_str(), resumed.progress), (MigrationState::Queued, "", halfway));
        assert_eq!(store.migration_password(mini, job.id).await.unwrap().as_deref(), Some("neues-passwort"));

        // Paused by the person while it runs: the worker's next note says stop, its end changes nothing.
        store.take_migration_job().await.unwrap().unwrap();
        assert_eq!(store.pause_migration_job(mini, job.id).await.unwrap().error, "stopped");
        assert!(!store.note_migration_progress(job.id, most).await.unwrap());
        store.finish_migration_run(job.id, MigrationRun::Done).await.unwrap();
        assert_eq!(store.migration_job(mini, job.id).await.unwrap().unwrap().state, MigrationState::Paused);

        // A restart puts running moves back in the queue.
        store.sync_migration_job(mini, job.id, None).await.unwrap();
        store.take_migration_job().await.unwrap().unwrap();
        assert_eq!(store.requeue_running_migrations().await.unwrap(), 1);
        assert_eq!(store.migration_job(mini, job.id).await.unwrap().unwrap().state, MigrationState::Queued);

        // Done for good: the job and its password go; nobody else can remove it.
        assert!(matches!(store.delete_migration_job(nyu, job.id).await, Err(StoreError::NotFound(_))));
        store.delete_migration_job(mini, job.id).await.unwrap();
        assert!(store.migration_jobs(mini).await.unwrap().is_empty());
        assert_eq!(store.migration_password(mini, job.id).await.unwrap(), None);
    }

    async fn sync_refused(store: &Store, account_id: i64, id: i64) -> bool {
        matches!(
            store.sync_migration_job(account_id, id, None).await,
            Err(StoreError::Rule { code: "moveRunning", .. })
        )
    }

    #[tokio::test]
    async fn one_person_has_a_few_moves_at_most() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        for number in 0..MAX_MIGRATION_JOBS {
            store.create_migration_job(new_job(mini, &format!("alt{number}@example.net"))).await.unwrap();
        }
        let refused = store.create_migration_job(new_job(mini, "noch-eine@example.net")).await.unwrap_err();
        assert_eq!(rule(refused), "moveLimit");
        // Oldest first: the first one created is taken first.
        let first = store.take_migration_job().await.unwrap().unwrap();
        assert_eq!(first.address, "alt0@example.net");
    }
}
