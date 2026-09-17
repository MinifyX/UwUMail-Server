//! Storage for the UwUMail server.
//!
//! Everything lives under one data directory: a SQLite database (WAL mode) for
//! the directory, mailboxes, message metadata, the change log and the outbound
//! queue, and content-addressed files for raw messages (`blobs/`).
//!
//! All public methods are async and run their SQLite work on the blocking
//! thread pool. One writer connection serializes writes; reads use a small
//! pool of read-only connections.

mod address;
mod admin;
mod bayes;
mod blobs;
mod db;
mod directory;
mod extras;
mod forwarding;
mod mail;
mod mutate;
mod objects;
mod own;
mod parse;
mod password;
mod query;
mod queue;
mod reports;
mod security;
mod sender_lists;
mod spam;
mod web;
mod word_lists;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Notify, broadcast};

pub use address::{EmailAddress, normalize_address, normalize_domain};
pub use admin::{
    AccountUpdate, AddressInfo, AuditEntry, AuditRecord, PasswordLink, PasswordLinkPurpose, Person,
    TRASH_RETENTION_SECS,
};
pub use bayes::{
    BAYES_FOLDER_LIMIT, BAYES_LEARNED_SECS, BAYES_MIN_LEARNED, BAYES_RARE_TOKEN_SECS, BAYES_WANTED_AFTER_SECS,
    BayesJob, BayesTotals,
};
pub use blobs::BlobHash;
pub use directory::{Account, DkimKey, DkimKeyAlgorithm, DkimKeyState, Domain, NewAccount, Role};
pub use extras::{Identity, IdentityUpdate, SubmissionRecord, UPLOAD_LIFETIME_SECS, VacationResponse};
pub use forwarding::{ActiveForwarding, FORWARD_LINK_LIFETIME_SECS, ForwardTarget, Forwarding, MAX_FORWARD_TARGETS};
pub use mail::{EmailSummary, IngestRequest, IngestedEmail, Mailbox, MailboxRole, MailboxTarget, TestMessageStatus};
pub use mutate::{EmailUpdate, KeywordsChange, MailboxUpdate, MailboxesChange};
pub use objects::{Changes, EmailRecord};
pub use own::{MailboxUsage, OwnAddress, OwnAddresses, RELEASED_ADDRESS_SECS, ReleasedAddress};
pub use query::{EmailFilter, EmailSort, EmailSortProperty};
pub use queue::{NewQueueRecipient, QueueEntry, QueueRecipient, QueueRecipientStatus, QueuedMessage};
pub use reports::{
    CachedStsPolicy, DMARC_REPORT_ADDRESS, DmarcRow, DmarcSource, DmarcSummary, MtaStsMode, MtaStsSettings,
    NewDmarcReport, NewTlsReport, REPORT_RETENTION_SECS, ReportKind, ReportStored, ReportSummary, Reporter,
    TLS_REPORT_ADDRESS, TlsFailure, TlsFailureSummary, TlsSummary,
};
pub use security::{
    AppPassword, AppScope, CodeCheck, CreatedAppPassword, MailAuth, MailAuthDenied, NewAppPassword, Passkey,
    SecurityEvent, SecurityEventRecord, SecurityOverview, TotpSetup, WebSessionInfo,
};
pub use sender_lists::{
    ListOwner, ListScope, NewSenderListEntry, SENDER_LIST_ADMIN_LIMIT, SENDER_LIST_PERSONAL_LIMIT, SenderKind,
    SenderList, SenderListEntry, guess_sender_kind, normalize_sender,
};
pub use spam::{GREYLIST_PASSED_SECS, GREYLIST_WAITING_SECS, Greylist, REPUTATION_RETENTION_SECS, Reputation};
pub use web::{NewWebSession, ServerCounts, WebSession};
pub use word_lists::{
    CompiledWord, PATTERN_SIZE_LIMIT, RefusedWord, WORD_LIST_ADMIN_LIMIT, WORD_LIST_PERSONAL_LIMIT, WORD_POINTS,
    WORD_POINTS_MAX, WORD_SOURCE_ENTRY_LIMIT, WORD_SOURCE_MAX_BYTES, WORD_SOURCES_ADMIN_LIMIT,
    WORD_SOURCES_PERSONAL_LIMIT, WordEntry, WordImport, WordSource, normalize_word, parse_word_lines, word_regex,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    Conflict(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("mailbox is full")]
    QuotaExceeded,
    /// A rule of the data model was broken; `code` is a stable machine-readable name.
    #[error("{message}")]
    Rule { code: &'static str, message: String },
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Sent after every committed write that changed an account's mail data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateChange {
    pub account_id: i64,
    pub modseq: i64,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    db: db::Database,
    blobs: blobs::BlobStore,
    blob_lock: tokio::sync::RwLock<()>,
    changes: broadcast::Sender<StateChange>,
    queue_wakeup: Notify,
    data_dir: PathBuf,
}

impl Store {
    /// Opens (or creates) the store in `data_dir` and runs pending migrations.
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<Store> {
        let data_dir = data_dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&data_dir).await?;
        let db_path = data_dir.join("uwumail.db");
        let db = tokio::task::spawn_blocking(move || db::Database::open(&db_path))
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))??;
        let blobs = blobs::BlobStore::open(data_dir.join("blobs")).await?;
        let (changes, _) = broadcast::channel(1024);
        Ok(Store {
            inner: Arc::new(Inner {
                db,
                blobs,
                blob_lock: Default::default(),
                changes,
                queue_wakeup: Notify::new(),
                data_dir,
            }),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    /// Subscribes to account state changes (new mail, flag changes, ...).
    pub fn subscribe_changes(&self) -> broadcast::Receiver<StateChange> {
        self.inner.changes.subscribe()
    }

    /// Resolves when something was added to the outbound queue.
    pub async fn queue_wakeup(&self) {
        self.inner.queue_wakeup.notified().await
    }

    pub async fn setting(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_owned();
        self.read(move |conn| db::get_setting(conn, &key)).await
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.write(move |tx| db::set_setting(tx, &key, &value)).await
    }

    /// Returns whether the setting existed.
    pub async fn delete_setting(&self, key: &str) -> Result<bool> {
        let key = key.to_owned();
        self.write(move |tx| db::delete_setting(tx, &key)).await
    }

    async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.db.read(f))
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))?
    }

    async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.db.write(f))
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))?
    }

    fn notify_change(&self, account_id: i64, modseq: i64) {
        // Nobody listening is fine.
        let _ = self.inner.changes.send(StateChange { account_id, modseq });
    }
}

pub(crate) fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system RNG failed");
    bytes
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub async fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        (store, dir)
    }
}
