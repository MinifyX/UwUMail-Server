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
pub mod foreign;
pub mod kinds;
mod labeling;
pub mod llm;
pub mod mail;
pub mod prices;
pub mod prompts;
pub mod spam;
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
    AuthenticationSignals, Calibration, ComposeArgs, ComposeResult, Estimate, EstimateArgs, EstimateCall, EstimateCost,
    EstimatePlan, EventsArgs, EventsResult, ExtractedEvent, LabelPick, LabelVerdict, MIN_CALIBRATION_SAMPLES, NewLabel,
    Participant, SenderSignals, SpamAnswer, SpamArgs, SpamResult, SpamSignals, StreamEvent, SuggestArgs, SuggestResult,
    SummarizeArgs, SummaryResult, Usage, parse_spam, plan_estimate, rule_meaning, thinks,
};
pub use foreign::{ForeignLabel, ForeignMail, foreign_labels, foreign_mails};
pub use kinds::{KINDS, KindInfo};
pub use prices::{
    Cost, CostParts, Metered, Price, PriceSource, PriceSources, PriceTable, PriceTier, Prices, Rates, Tier,
};

/// Longest instruction a person may give.
pub const MAX_INSTRUCTION_CHARS: usize = 2000;
/// Longest draft text that may be sent along.
pub const MAX_TEXT_CHARS: usize = 20_000;
/// Requests to models running at once, for everyone together.
const MAX_CONCURRENT: usize = 16;
/// … and for one person.
const MAX_CONCURRENT_PER_ACCOUNT: usize = 3;
/// `Assist/estimate`s running at once for one person: they read mail, but ask no one.
const MAX_ESTIMATES_PER_ACCOUNT: usize = 4;
/// `AssistLabel/apply` calls running at once for one person: each labels up to 20 mails, reading
/// them and comparing them with thousands of examples (security review 0.22 LABELS22-L4).
const MAX_LABEL_APPLIES_PER_ACCOUNT: usize = 1;

/// How the text in pictures is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureRead {
    /// Reads every picture that was not read before.
    Read,
    /// Only what was read before; pictures never read are counted in [`PictureTexts::unread`]. For
    /// `Assist/estimate`, which must stay cheap.
    KnownOnly,
}

/// The text in a mail's pictures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PictureTexts {
    /// The texts found, one per picture that has any.
    pub texts: Vec<String>,
    /// Pictures that were not read yet ([`PictureRead::KnownOnly`] only).
    pub unread: usize,
}

impl PictureTexts {
    pub fn read(texts: Vec<String>) -> PictureTexts {
        PictureTexts { texts, unread: 0 }
    }
}

/// Reads the text in a mail's pictures (`Email/imageText`, docs/jmap-image-text.md): `(account,
/// email, how)` to the texts, or `None` when the server can't read pictures. Remote pictures are not
/// read this way.
pub type ImageText = Arc<
    dyn Fn(i64, i64, PictureRead) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<PictureTexts>> + Send>>
        + Send
        + Sync,
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
    /// ChatGPT device logins that were started, by owner and provider: a provider id taken again by
    /// someone else after the account changed hands never finds the old login (security audit 0.21.0).
    logins: Mutex<HashMap<(i64, i64), chatgpt::DeviceCode>>,
    /// Held while a ChatGPT sign-in is renewed: its refresh token works once, so requests side by
    /// side must not each renew it (AI-05 of the 0.18.0 audit).
    renewing: tokio::sync::Mutex<()>,
    chatgpt: chatgpt::Endpoints,
    /// For tests: providers set up by people may reach anything, like the admin's.
    reach_anything: bool,
    permits: Semaphore,
    running: Mutex<HashMap<i64, usize>>,
    /// `Assist/estimate`s running, per person.
    estimating: Mutex<HashMap<i64, usize>>,
    /// `AssistLabel/apply` calls running, per person.
    applying: Mutex<HashMap<i64, usize>>,
    /// The text in a mail's pictures, for `Assist/extractEvents` with `includeImages`.
    image_text: Option<ImageText>,
    /// What models cost, once loaded from the database.
    prices: std::sync::RwLock<Option<Arc<prices::Prices>>>,
    price_sources: prices::PriceSources,
}

impl Assist {
    pub fn new(store: Store, egress: Egress, hostname: &str) -> Assist {
        Assist {
            inner: Arc::new(Inner {
                store,
                egress,
                hostname: hostname.to_ascii_lowercase(),
                logins: Mutex::new(HashMap::new()),
                renewing: tokio::sync::Mutex::new(()),
                chatgpt: chatgpt::Endpoints::default(),
                reach_anything: false,
                permits: Semaphore::new(MAX_CONCURRENT),
                running: Mutex::new(HashMap::new()),
                estimating: Mutex::new(HashMap::new()),
                applying: Mutex::new(HashMap::new()),
                image_text: None,
                prices: std::sync::RwLock::new(None),
                price_sources: prices::PriceSources::default(),
            }),
        }
    }

    /// Talks to fake ChatGPT servers, and lets personal providers reach this machine: for tests.
    pub fn for_tests(store: Store, hostname: &str, chatgpt: chatgpt::Endpoints) -> Assist {
        let assist = Assist::new(store, Egress::direct(), hostname);
        let inner = Arc::into_inner(assist.inner).expect("not shared yet");
        Assist { inner: Arc::new(Inner { chatgpt, reach_anything: true, ..inner }) }
    }

    /// Fetches the price lists from `sources`: for tests.
    pub fn with_price_sources(self, sources: prices::PriceSources) -> Assist {
        let inner = Arc::into_inner(self.inner).expect("set before anything else holds the assistant");
        Assist { inner: Arc::new(Inner { price_sources: sources, ..inner }) }
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

    /// One more estimate for `account`, if not too many run already.
    fn begin_estimate(&self, account_id: i64) -> Result<Slot<'_>> {
        Slot::take(&self.inner.estimating, account_id, MAX_ESTIMATES_PER_ACCOUNT)
    }

    /// One more `AssistLabel/apply` for `account`, if none runs for them already; hold the answer
    /// while it runs. [`AssistError::Busy`] otherwise.
    pub fn begin_label_apply(&self, account_id: i64) -> Result<Slot<'_>> {
        Slot::take(&self.inner.applying, account_id, MAX_LABEL_APPLIES_PER_ACCOUNT)
    }
}

/// Something running for one person, among at most so many; counted down again when it ends.
pub struct Slot<'a> {
    counts: &'a Mutex<HashMap<i64, usize>>,
    account_id: i64,
}

impl<'a> Slot<'a> {
    fn take(counts: &'a Mutex<HashMap<i64, usize>>, account_id: i64, max: usize) -> Result<Slot<'a>> {
        let mut map = counts.lock().unwrap_or_else(|e| e.into_inner());
        let count = map.entry(account_id).or_default();
        if *count >= max {
            return Err(AssistError::Busy);
        }
        *count += 1;
        Ok(Slot { counts, account_id })
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut map = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = map.get_mut(&self.account_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.account_id);
            }
        }
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
