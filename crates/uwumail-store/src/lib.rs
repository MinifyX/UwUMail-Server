//! Storage for the UwUMail server.
//!
//! Everything lives under one data directory: a SQLite database (WAL mode) for
//! the directory, mailboxes, message metadata, the change log and the outbound
//! queue, and content-addressed files for raw messages (`blobs/`).
//!
//! All public methods are async and run their SQLite work on the blocking
//! thread pool. One writer connection serializes writes; reads use a small
//! pool of read-only connections.

mod acl;
mod address;
mod admin;
mod alerts;
mod assist;
mod bayes;
mod bimi;
pub mod birthday_import;
pub mod birthdays;
mod blobs;
mod calendar;
mod calendar_alerts;
mod calendar_notifications;
mod calendar_prefs;
mod calendar_subscriptions;
mod calendar_versions;
mod contact_photos;
mod contacts;
mod dav;
mod dav_import;
mod db;
mod directory;
mod external;
mod extras;
mod feeds;
mod fetch;
mod forward_addresses;
mod forwarding;
mod greylist_hold;
mod groups;
mod held;
pub mod ical;
mod identity_grants;
mod imap;
mod import;
pub mod itip;
mod limiter;
mod mail;
mod masked;
mod masked_domains;
mod microsoft;
mod migration_jobs;
pub mod mime_limits;
mod mutate;
mod oauth;
mod objects;
mod own;
mod parse;
mod password;
mod profile_pictures;
mod push;
mod query;
mod queue;
mod reports;
mod rules;
mod sasl;
mod security;
mod sender_lists;
mod shared_mailboxes;
mod sharing;
mod sieve;
mod spam;
mod spam_log;
mod stats;
mod suggestions;
mod tls_rpt;
pub mod tnef;
mod user_settings;
mod web;
mod word_lists;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Notify, broadcast};

pub use acl::{ALL_RIGHTS, AclEntry, ShareLevel, SharePerson, SharedMailbox, has_rights, normalize_rights};
pub use address::{EmailAddress, normalize_address, normalize_domain};
pub use admin::{
    AccountUpdate, AddressInfo, AuditEntry, AuditRecord, PasswordLink, PasswordLinkPurpose, Person,
    TRASH_RETENTION_SECS,
};
pub use alerts::{
    ALERT_HISTORY_SECS, ALERT_REMINDER_SECS, ALERT_RESOLVE_AFTER_SECS, Alert, AlertEvent, AlertLevel, AlertNotice,
    AlertObservation, CertificateOrders,
};
pub use assist::{
    ASSIST_FEATURES, ASSIST_LABEL_DESCRIPTION_MAX_CHARS, ASSIST_LABEL_NAME_MAX_CHARS, ASSIST_MAX_ACCESS_ENTRIES,
    ASSIST_MAX_LABELS, ASSIST_MAX_PERSONAL_PROVIDERS, ASSIST_MAX_SERVER_PROVIDERS, AssistFeatures, AssistLabel,
    AssistPolicy, AssistPrefs, AssistProviderRecord, AssistProviderWrite, LabelJob, LabelLogEntry, SecretChange,
    SenderHistory, UsageRow, UsedToday, label_keyword, utc_day,
};
pub use bayes::{
    BAYES_FOLDER_LIMIT, BAYES_LEARNED_SECS, BAYES_MIN_LEARNED, BAYES_RARE_TOKEN_SECS, BAYES_WANTED_AFTER_SECS,
    BayesJob, BayesTotals,
};
pub use bimi::{BimiUpdate, DomainBimi};
pub use blobs::{BlobCleanupPause, BlobHash};
pub use calendar::{CalendarEventRecord, CalendarEventWrite};
pub use calendar_alerts::{CalendarAlertFired, DueAlert, PlannedAlert};
pub use calendar_notifications::{
    Author, CalendarNotification, EventAuthor, KEPT_SECS as CALENDAR_NOTIFICATIONS_KEPT_SECS,
    MAX_NOTIFICATIONS as MAX_CALENDAR_NOTIFICATIONS,
};
pub use calendar_prefs::{
    CALENDAR_DEFAULT_ALERTS_MAX_BYTES, CALENDAR_EVENT_PREFS_MAX_BYTES, CalendarEventPrefs, CalendarPrefs,
    CalendarPrefsUpdate,
};
pub use calendar_subscriptions::{
    CalendarSubscription, CalendarSubscriptionUpdate, DEFAULT_SUBSCRIPTION_INTERVAL_SECS, MAX_CALENDAR_SUBSCRIPTIONS,
    MAX_SUBSCRIPTION_INTERVAL_SECS, MIN_SUBSCRIPTION_INTERVAL_SECS, NewCalendarSubscription,
    SUBSCRIPTION_REFRESH_PAUSE_SECS, SubscriptionRun, shown_url,
};
pub use contact_photos::{ContactPhoto, contact_photo};
pub use contacts::{ContactCardRecord, ContactCardWrite};
pub use dav::{
    DAV_COLLECTIONS_PER_ACCOUNT, DAV_RESOURCE_MAX_BYTES, DAV_RESOURCES_PER_COLLECTION, DavChanges, DavCollection,
    DavCollectionUpdate, DavKind, DavPrecondition, DavResource, DavResourceInfo, DavWrite, DavWriteOutcome,
    NewDavCollection, dav_etag,
};
pub use dav_import::{
    DavImportMode, DavImportReport, DavMirrorReport, IMPORT_MAX_OBJECTS, IMPORT_MAX_PROBLEMS, IcsMeta, ImportProblem,
    NewImportCollection, Split, SplitObject, dav_color, decode_text, split_ics, split_vcf,
};
pub use directory::{Account, DkimKey, DkimKeyAlgorithm, DkimKeyState, Domain, NewAccount, Protocols, Role};
pub use external::{BoxFuture as ExternalFuture, ExternalPasswords};
pub use extras::{
    IDENTITY_SIGNATURE_MAX_BYTES, Identity, IdentityUpdate, SubmissionRecord, UPLOAD_LIFETIME_SECS, VacationResponse,
};
pub use feeds::FeedState;
pub use fetch::{
    AfterFetch, DEFAULT_FETCH_INTERVAL_SECS, FETCH_HOLD_LIMIT_SECS, FETCH_SEEN_SECS, FetchAccount, FetchAccountUpdate,
    FetchAuth, FetchFolder, FetchGrant, FetchOAuth, FetchSecurity, FetchSender, FetchTokens, MAX_FETCH_ACCOUNTS,
    MAX_FETCH_INTERVAL_SECS, MIN_FETCH_INTERVAL_SECS, NewFetchAccount, SendSecurity, is_public_ip,
};
pub use forward_addresses::{FORWARD_ADDRESS_MAX_TARGETS, ForwardAddress};
pub use forwarding::{ActiveForwarding, FORWARD_LINK_LIFETIME_SECS, ForwardTarget, Forwarding, MAX_FORWARD_TARGETS};
pub use greylist_hold::{GreylistHold, GreylistHoldMessage, MAX_HELD_SIZE, NewGreylistHold, Returning, Settled};
pub use groups::{GROUP_MAX_MEMBERS, Group, GroupDelivery, GroupMember, GroupUpdate, NewGroup, WhoMaySend};
pub use held::{HeldSubmission, NewHeldSubmission};
pub use imap::{DELETED_KEYWORD, FlagChange, ImapEmail, ImapMailbox, ImapMessage, ImapMessages, ImapStatus};
pub use import::ImportProgress;
pub use limiter::{Attempt, AuthLimiter, Reporter as BlockReporter};
pub use mail::{EmailSummary, IngestRequest, IngestedEmail, Mailbox, MailboxRole, MailboxTarget, TestMessageStatus};
pub use masked::{MASKED_PENDING_SECS, MaskedAddress, MaskedDelivery, MaskedState, MaskedUpdate, NewMaskedAddress};
pub use masked_domains::{
    AccountMaskedPolicy, DomainKind, DomainMaskedPolicy, EffectiveMaskedPolicy, KindBlockers, KindChange, MaskedMode,
};
pub use microsoft::{MICROSOFT_RESOLVE_AFTER_SECS, MicrosoftIssue, MicrosoftRefusal};
pub use migration_jobs::{
    MAX_MIGRATION_JOBS, MigrationJob, MigrationProgress, MigrationRun, MigrationState, NewMigrationJob,
};
pub use mutate::{EmailUpdate, KeywordsChange, MailboxUpdate, MailboxesChange, valid_keyword};
pub use oauth::{
    MASKED_EMAIL_SCOPE, NewOAuthCode, OAUTH_ACCESS_TOKEN_SECS, OAUTH_CODE_SECS, OAUTH_REFRESH_TOKEN_SECS, OAUTH_SCOPES,
    OAuthClient, OAuthGrant, OAuthRefusal, OAuthTokens, is_oauth_access_token, oauth_scopes, oauth_scopes_usable,
    pkce_matches, redirect_uri_registered, valid_pkce_challenge, valid_redirect_uri,
};
pub use objects::{Changes, EmailRecord};
pub use own::{MailboxUsage, OwnAddress, OwnAddresses, RELEASED_ADDRESS_SECS, ReleasedAddress};
/// Checks a password hash from another server (bcrypt or Argon2) and returns how it would be stored.
pub use password::import_hash as normalize_imported_password_hash;
pub use profile_pictures::{
    AddressPicture, GroupPicture, MAX_RECEIVED_FACES, NewPicture, PUBLIC_PICTURES_SETTING, PictureMeta, PictureOwner,
    PictureVisibility, ProfileSettings, ProfileUpdate, StoredPicture,
};
pub use push::{
    MAX_PUSH_SUBSCRIPTIONS, NewPushSubscription, PUSH_CREDENTIAL_PASSWORD, PUSH_MAX_FAILURES, PUSH_MAX_VERIFY_ATTEMPTS,
    PUSH_SUBSCRIPTION_MAX_SECS, PushKeys, PushSubscription, PushSubscriptionUpdate, PushTarget,
    push_credential_for_app_password, push_credential_for_oauth_grant, push_credential_for_session,
};
pub use query::{EmailFilter, EmailSort, EmailSortProperty};
pub use queue::{NewQueueRecipient, QueueEntry, QueueRecipient, QueueRecipientStatus, QueuedMessage};
pub use reports::{
    CachedStsPolicy, DMARC_REPORT_ADDRESS, DmarcRow, DmarcSource, DmarcSummary, MtaStsMode, MtaStsSettings,
    NewDmarcReport, NewTlsReport, REPORT_RETENTION_SECS, ReportEntry, ReportKind, ReportStored, ReportSummary,
    Reporter, TLS_REPORT_ADDRESS, TlsFailure, TlsFailureSummary, TlsSummary,
};
pub use rules::{
    BulkAction, BulkReport, ImportReport, RULES_BULK_MAX, RULES_IMPORT_MAX, RULES_PAGE_MAX, Rule, RuleChange,
    RuleImport, RuleList, RulePage, RuleQuery, RuleScope, RuleSort, RuleState, RuleType, ScopeFilter,
};
pub use sasl::{SaslBearer, parse_oauthbearer, parse_xoauth2, sasl_bearer_error, sasl_user_matches};
pub use security::{
    ALL_SCOPES, AppPassword, AppScope, CodeCheck, CreatedAppPassword, LiveLogin, MailAuth, MailAuthDenied,
    NewAppPassword, Passkey, SecurityEvent, SecurityEventRecord, SecurityOverview, TotpSetup, WebSessionInfo,
    scopes_for,
};
pub use sender_lists::{
    ListOwner, ListScope, NewSenderListEntry, SENDER_LIST_ADMIN_LIMIT, SENDER_LIST_PERSONAL_LIMIT, SenderKind,
    SenderList, SenderListEntry, guess_sender_kind, normalize_sender, pattern_matches,
};
pub use shared_mailboxes::{NewSharedMailbox, SharedMailboxInfo, SharedMailboxMember, SharedMembership};
pub use sharing::{DAV_SHARES_PER_COLLECTION, DavAccess, DavShare, ShareRights, SharedDavCollection};
pub use sieve::{
    SIEVE_MAX_NAME_SIZE, SIEVE_MAX_SCRIPT_SIZE, SIEVE_MAX_SCRIPTS, SieveActivation, SieveError, SieveScript,
    validate_sieve_name,
};
pub use spam::{
    GREYLIST_PASSED_SECS, GREYLIST_WAITING_SECS, Greylist, REPUTATION_RETENTION_SECS, Reputation, SPAM_LIMIT_RANGE,
    SpamLimits,
};
pub use spam_log::{
    FetchedVerdicts, NewSpamLogEntry, SPAM_LOG_MAX_ROWS, SpamAction, SpamLogEntry, SpamLogFilter, SpamLogHit,
    SpamLogRecipient,
};
pub use stats::{STATS_RETENTION_DAYS, Stat, Stats, StatsDay};
pub use suggestions::AddressUse;
pub use tls_rpt::{
    TLS_RPT_MAX_AGE_DAYS, TlsRptDue, TlsRptOutcome, TlsRptSent, TlsSession, TlsSessionCount, tls_rpt_day,
};
pub use user_settings::{
    DEFAULT_UNDO_SEND_SECONDS, SettingProblem, SettingsChange, USER_SETTINGS_MAX_KEYS, USER_SETTINGS_MAX_SIZE,
    USER_SETTINGS_MAX_VALUE_SIZE, UserSettings, validate_setting,
};
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
    /// Too much of something slow is running at once (password checks); the same request may
    /// work in a moment. Protocols answer with their "try again later".
    #[error("the server is busy, try again in a moment")]
    Busy,
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
    /// Backups running; cleaning up blobs waits while one reads them.
    blob_cleanup_paused: Arc<std::sync::atomic::AtomicUsize>,
    changes: broadcast::Sender<StateChange>,
    /// Calendar alerts that went off, for push (calendar_alerts.rs).
    calendar_alerts: broadcast::Sender<CalendarAlertFired>,
    queue_wakeup: Notify,
    /// Wakes the AI assistant's label worker when delivered mail was queued for it.
    assist_wakeup: Notify,
    data_dir: PathBuf,
    /// What happened since the server started, for the statistics and the metrics.
    stats: stats::Stats,
    /// Where passwords of directory (LDAP) accounts are checked, once the server plugged it in.
    external: std::sync::RwLock<Option<Arc<dyn ExternalPasswords>>>,
    /// The failed-login counts every protocol checks and adds to.
    auth_limiter: Arc<AuthLimiter>,
    /// How many password hashes are checked at once.
    hashing: password::Gate,
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
        let (calendar_alerts, _) = broadcast::channel(256);
        Ok(Store {
            inner: Arc::new(Inner {
                db,
                blobs,
                blob_lock: Default::default(),
                blob_cleanup_paused: Default::default(),
                changes,
                calendar_alerts,
                queue_wakeup: Notify::new(),
                assist_wakeup: Notify::new(),
                data_dir,
                stats: stats::Stats::default(),
                external: std::sync::RwLock::new(None),
                auth_limiter: Arc::default(),
                hashing: password::Gate::new(),
            }),
        })
    }

    /// The failed-login counts of the whole server. Every login path checks it before a password
    /// check ([`AuthLimiter::begin`]) and records how the check ended.
    pub fn auth_limiter(&self) -> &Arc<AuthLimiter> {
        &self.inner.auth_limiter
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
