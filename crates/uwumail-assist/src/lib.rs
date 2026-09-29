//! The AI assistant of the UwUMail server (docs/llm.md, docs/jmap-assist.md).
//!
//! [`Assist`] holds what every feature needs: which providers a person may use for what, the
//! person's choices, the daily quotas, the way out to the providers (the egress), and the ChatGPT
//! logins in progress. JMAP and the portal call into it; the label worker runs beside the server.
//!
//! Every request to a model is made here, on the server: the keys never reach a browser, labels can
//! be set while nobody looks, and the admin can count and limit what is used.

mod access;
pub mod chatgpt;
mod features;
pub mod kinds;
pub mod llm;
pub mod mail;
pub mod prompts;
mod worker;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;
use uwumail_smtp::egress::Egress;
use uwumail_store::{Store, StoreError};

pub use access::{
    AdminProviderView, Capability, Choice, Effective, ProviderInput, ProviderView, Quota, SettingsPatch, SettingsView,
    TodayUsage,
};
pub use features::{
    AuthenticationSignals, ComposeArgs, ComposeResult, EventsArgs, EventsResult, ExtractedEvent, LabelPick,
    Participant, SenderSignals, SpamArgs, SpamResult, SpamSignals, StreamEvent, SummarizeArgs, SummaryResult, Usage,
};
pub use kinds::{KINDS, KindInfo};

/// Longest instruction a person may give.
pub const MAX_INSTRUCTION_CHARS: usize = 2000;
/// Longest draft text that may be sent along.
pub const MAX_TEXT_CHARS: usize = 20_000;
/// Requests to models running at once, for everyone together.
const MAX_CONCURRENT: usize = 16;
/// … and for one person.
const MAX_CONCURRENT_PER_ACCOUNT: usize = 3;

/// Reads the text in a mail's pictures (the images agent's `Email/imageText`, docs/jmap-imagetext.md):
/// `(account, email)` to the texts, or `None` when the server can't read pictures. Remote pictures are
/// not read this way.
pub type ImageText = Arc<
    dyn Fn(i64, i64) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<String>>> + Send>> + Send + Sync,
>;

/// The five features, spelled as everywhere else.
pub const FEATURES: [&str; 5] = uwumail_store::ASSIST_FEATURES;

#[derive(Debug, thiserror::Error)]
pub enum AssistError {
    /// The feature is off, or no provider the person may use is allowed it (`assistUnavailable`).
    #[error("{0}")]
    Unavailable(String),
    /// A daily limit is reached (`overQuota`).
    #[error("{0}")]
    OverQuota(String),
    /// The provider gave no usable answer (`providerFailed`).
    #[error("{description}")]
    ProviderFailed { description: String, retry_after: Option<u64>, transient: bool },
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Forbidden(String),
    /// A value that is not allowed. `code` is the portal's error code, `property` the field.
    #[error("{description}")]
    Invalid { code: &'static str, property: &'static str, description: String },
    /// Too busy right now.
    #[error("too many requests to AI providers are running; try again in a moment")]
    Busy,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl AssistError {
    fn invalid(code: &'static str, property: &'static str, description: impl Into<String>) -> AssistError {
        AssistError::Invalid { code, property, description: description.into() }
    }
}

pub type Result<T, E = AssistError> = std::result::Result<T, E>;

#[derive(Clone)]
pub struct Assist {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    egress: Egress,
    /// This server's name: its own `Authentication-Results` carry it.
    hostname: String,
    /// ChatGPT device logins that were started, by provider.
    logins: Mutex<HashMap<i64, chatgpt::DeviceCode>>,
    chatgpt: chatgpt::Endpoints,
    /// For tests: providers set up by people may reach anything, like the admin's.
    reach_anything: bool,
    permits: Semaphore,
    running: Mutex<HashMap<i64, usize>>,
    /// The text in a mail's pictures, for `Assist/extractEvents` with `includeImages`.
    image_text: Option<ImageText>,
}

impl Assist {
    pub fn new(store: Store, egress: Egress, hostname: &str) -> Assist {
        Assist {
            inner: Arc::new(Inner {
                store,
                egress,
                hostname: hostname.to_ascii_lowercase(),
                logins: Mutex::new(HashMap::new()),
                chatgpt: chatgpt::Endpoints::default(),
                reach_anything: false,
                permits: Semaphore::new(MAX_CONCURRENT),
                running: Mutex::new(HashMap::new()),
                image_text: None,
            }),
        }
    }

    /// Talks to fake ChatGPT servers, and lets personal providers reach this machine: for tests.
    pub fn for_tests(store: Store, hostname: &str, chatgpt: chatgpt::Endpoints) -> Assist {
        let assist = Assist::new(store, Egress::direct(), hostname);
        let inner = Arc::into_inner(assist.inner).expect("not shared yet");
        Assist { inner: Arc::new(Inner { chatgpt, reach_anything: true, ..inner }) }
    }

    /// Reads the text in pictures with `read`. Called before anything else holds the assistant.
    pub fn with_image_text(self, read: ImageText) -> Assist {
        let inner = Arc::into_inner(self.inner).expect("set before anything else holds the assistant");
        Assist { inner: Arc::new(Inner { image_text: Some(read), ..inner }) }
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn hostname(&self) -> &str {
        &self.inner.hostname
    }

    /// One more request for `account`, if the limits allow it now.
    fn begin(&self, account_id: i64) -> Result<Running<'_>> {
        let permit = self.inner.permits.try_acquire().map_err(|_| AssistError::Busy)?;
        let mut running = self.inner.running.lock().unwrap_or_else(|e| e.into_inner());
        let count = running.entry(account_id).or_default();
        if *count >= MAX_CONCURRENT_PER_ACCOUNT {
            return Err(AssistError::Busy);
        }
        *count += 1;
        Ok(Running { assist: self, account_id, _permit: permit })
    }
}

/// A request that is running; counted down again when it ends.
struct Running<'a> {
    assist: &'a Assist,
    account_id: i64,
    _permit: tokio::sync::SemaphorePermit<'a>,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut running = self.assist.inner.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = running.get_mut(&self.account_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                running.remove(&self.account_id);
            }
        }
    }
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}
