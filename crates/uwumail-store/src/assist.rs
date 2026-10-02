//! What the AI assistant keeps (docs/llm.md): the providers the admin and people set up, people's
//! choices and labels, the queue of delivered mail waiting for its labels, why a label was put on,
//! and how much everyone used per day.
//!
//! A provider's key (or a ChatGPT login's tokens) is sealed with the same key as the passwords of
//! fetched mailboxes and never leaves this module except to be sent to the provider.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::{get_setting, set_setting};
use crate::fetch::{seal, unseal};
use crate::{Result, Store, StoreError, now};

/// The features, as they are spelled everywhere: settings, JMAP, the portal.
pub const ASSIST_FEATURES: [&str; 5] = ["compose", "summarize", "spamCheck", "extractEvents", "autoLabels"];
/// Providers one person may add with their own keys.
pub const ASSIST_MAX_PERSONAL_PROVIDERS: usize = 10;
/// Providers the admin may set up for the server.
pub const ASSIST_MAX_SERVER_PROVIDERS: usize = 50;
/// Labels one person may have.
pub const ASSIST_MAX_LABELS: usize = 30;
pub const ASSIST_LABEL_NAME_MAX_CHARS: usize = 40;
pub const ASSIST_LABEL_DESCRIPTION_MAX_CHARS: usize = 300;
/// Domains or logins one server provider may be limited to.
pub const ASSIST_MAX_ACCESS_ENTRIES: usize = 500;
/// Emails with a label that destroying the label takes it off, at most.
const MAX_UNLABEL: usize = 20_000;
/// Delivered emails one person may have waiting for labels; more are not queued (they keep no
/// label), so one busy mailbox can't fill the queue for everyone.
const ASSIST_MAX_QUEUED_PER_ACCOUNT: i64 = 200;

const POLICY_KEY: &str = "assist.policy";
const VERSION_KEY: &str = "assist.version";
/// The price lists and exchange rates the assistant fetched last, as it keeps them (JSON).
const PRICES_KEY: &str = "assist.prices";

/// What the admin decided for the whole server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistPolicy {
    /// Per feature (see [`ASSIST_FEATURES`]), whether anyone may use it.
    pub features: AssistFeatures,
    /// People may add providers with their own keys.
    pub allow_personal: bool,
    /// Such providers may point into the local network.
    pub allow_personal_private: bool,
    /// The assistant may be used for mail of people's other accounts, which the UwUMail app sends
    /// along (docs/jmap-assist.md, "Foreign mail"). Off by default.
    #[serde(default)]
    pub foreign_mail: bool,
}

impl Default for AssistPolicy {
    fn default() -> Self {
        AssistPolicy {
            features: AssistFeatures::all(true),
            allow_personal: false,
            allow_personal_private: false,
            foreign_mail: false,
        }
    }
}

/// The feature a server provider's `features` name when it may serve foreign mail.
pub const ASSIST_FOREIGN_MAIL: &str = "foreignMail";

/// One switch per feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistFeatures {
    pub compose: bool,
    pub summarize: bool,
    pub spam_check: bool,
    pub extract_events: bool,
    pub auto_labels: bool,
}

impl Default for AssistFeatures {
    fn default() -> Self {
        AssistFeatures::all(false)
    }
}

impl AssistFeatures {
    pub fn all(on: bool) -> AssistFeatures {
        AssistFeatures { compose: on, summarize: on, spam_check: on, extract_events: on, auto_labels: on }
    }

    pub fn get(&self, feature: &str) -> bool {
        match feature {
            "compose" => self.compose,
            "summarize" => self.summarize,
            "spamCheck" => self.spam_check,
            "extractEvents" => self.extract_events,
            "autoLabels" => self.auto_labels,
            _ => false,
        }
    }

    pub fn set(&mut self, feature: &str, on: bool) {
        match feature {
            "compose" => self.compose = on,
            "summarize" => self.summarize = on,
            "spamCheck" => self.spam_check = on,
            "extractEvents" => self.extract_events = on,
            "autoLabels" => self.auto_labels = on,
            _ => {}
        }
    }
}

/// A provider as stored, without its secret.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistProviderRecord {
    pub id: i64,
    /// `None` for the server's.
    pub account_id: Option<i64>,
    pub name: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub has_secret: bool,
    pub key_hint: Option<String>,
    pub model: Option<String>,
    pub fast_model: Option<String>,
    pub enabled: bool,
    /// `everyone`, `domains` or `people`.
    pub access: String,
    pub access_list: Vec<String>,
    pub features: Vec<String>,
    pub requests_per_day: Option<i64>,
    pub tokens_per_day: Option<i64>,
    /// US dollars per million tokens in and out, when set by hand; `None`: from the price lists.
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    /// US dollars per request, when set by hand; `None`: from the price lists.
    pub request_price: Option<f64>,
    /// Whether the people using a server provider see what it costs.
    pub show_cost: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What happens to the secret with a write.
#[derive(Clone, PartialEq, Eq)]
pub enum SecretChange {
    Keep,
    Remove,
    /// The secret, and the hint to show for it.
    Set(String, Option<String>),
}

/// Never shows the secret, only the hint.
impl std::fmt::Debug for SecretChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretChange::Keep => f.write_str("Keep"),
            SecretChange::Remove => f.write_str("Remove"),
            SecretChange::Set(_, hint) => f.debug_tuple("Set").field(&"<redacted>").field(hint).finish(),
        }
    }
}

/// A new or changed provider; every field is written.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistProviderWrite {
    pub name: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub fast_model: Option<String>,
    pub enabled: bool,
    pub access: String,
    pub access_list: Vec<String>,
    pub features: Vec<String>,
    pub requests_per_day: Option<i64>,
    pub tokens_per_day: Option<i64>,
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    pub request_price: Option<f64>,
    pub show_cost: bool,
}

impl AssistProviderWrite {
    pub fn of(record: &AssistProviderRecord) -> AssistProviderWrite {
        AssistProviderWrite {
            name: record.name.clone(),
            kind: record.kind.clone(),
            base_url: record.base_url.clone(),
            model: record.model.clone(),
            fast_model: record.fast_model.clone(),
            enabled: record.enabled,
            access: record.access.clone(),
            access_list: record.access_list.clone(),
            features: record.features.clone(),
            requests_per_day: record.requests_per_day,
            tokens_per_day: record.tokens_per_day,
            input_price: record.input_price,
            output_price: record.output_price,
            request_price: record.request_price,
            show_cost: record.show_cost,
        }
    }
}

/// A person's choices.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistPrefs {
    /// `{"default": {"providerId": 3, "model": null}, "compose": {...}, ...}`, checked by the caller.
    pub choices: serde_json::Map<String, Value>,
    /// The model judges the labels of incoming mail.
    pub auto_labels: bool,
    /// Labels are put on incoming mail without a model (docs/labels.md); on by default.
    pub non_ai_labels: bool,
    /// Changes with every write to the person's assist objects: providers, choices, labels.
    pub modseq: i64,
}

impl Default for AssistPrefs {
    fn default() -> Self {
        AssistPrefs { choices: serde_json::Map::new(), auto_labels: false, non_ai_labels: true, modseq: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistLabel {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub keyword: String,
    pub color: Option<String>,
    pub created_at: i64,
    /// `{"match", "conditions"}` as the caller checked it (`uwumail_labels::Rules`), or `None`.
    pub rules: Option<Value>,
    /// A detector's name (`uwumail_labels::Detector`).
    pub detector: Option<String>,
    pub learn_senders: bool,
    pub classifier: bool,
    /// The base label it is (`uwumail_labels::Base`), if any.
    pub base: Option<String>,
    /// Put on by itself; off, only by hand.
    pub auto: bool,
}

/// What is written of a label; the caller checked `rules` and `detector`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistLabelWrite {
    pub name: String,
    pub description: String,
    pub color: Option<String>,
    pub rules: Option<Value>,
    pub detector: Option<String>,
    pub learn_senders: bool,
    pub classifier: bool,
    pub auto: bool,
}

impl AssistLabelWrite {
    /// A label with only a name, a description and a color; learning on, no rules, no detector.
    pub fn simple(name: String, description: String, color: Option<String>) -> AssistLabelWrite {
        AssistLabelWrite {
            name,
            description,
            color,
            rules: None,
            detector: None,
            learn_senders: true,
            classifier: true,
            auto: true,
        }
    }

    pub fn of(label: &AssistLabel) -> AssistLabelWrite {
        AssistLabelWrite {
            name: label.name.clone(),
            description: label.description.clone(),
            color: label.color.clone(),
            rules: label.rules.clone(),
            detector: label.detector.clone(),
            learn_senders: label.learn_senders,
            classifier: label.classifier,
            auto: label.auto,
        }
    }
}

/// How many emails carry a label, and how many of them are unread; mail only in Junk or the Trash
/// does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelCounts {
    pub total: i64,
    pub unread: i64,
    /// Examples the classifier learned with the label.
    pub examples: i64,
}

/// A delivered email waiting for its labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelJob {
    pub id: i64,
    pub account_id: i64,
    pub email_id: i64,
    pub queued_at: i64,
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelLogEntry {
    pub id: i64,
    pub email_id: i64,
    pub label_id: i64,
    pub name: String,
    pub keyword: String,
    /// `ai`, `rule`, `sender`, `detector` or `classifier`.
    pub source: String,
    pub reason: String,
    /// What the reason says, for clients to put in their words, and its details (JSON).
    pub code: String,
    pub params: Value,
    /// Empty when not the model's.
    pub provider: String,
    pub model: String,
    pub created_at: i64,
    /// Taken off with undo, or the keyword is no longer on the email.
    pub undone: bool,
}

/// A new log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelLogWrite {
    pub email_id: i64,
    pub label_id: i64,
    pub source: String,
    pub code: String,
    pub params: Value,
    pub reason: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    pub day: String,
    pub account_id: i64,
    pub login: String,
    pub provider_id: i64,
    /// `None` once the provider is gone.
    pub provider_name: Option<String>,
    pub feature: String,
    pub requests: i64,
    pub input_tokens: i64,
    /// The answer's text, without what the model spent thinking.
    pub output_tokens: i64,
    /// Thinking (reasoning) tokens, on top of `output_tokens`.
    pub reasoning_tokens: i64,
    /// The part of `input_tokens` read from the provider's cache.
    pub cached_tokens: i64,
    /// Calls to the model the requests took.
    pub calls: i64,
    /// US dollars, at the prices of the time; `None` where they were not known.
    pub cost_usd: Option<f64>,
}

/// Tokens (and their cost) added to a counted request by [`Store::add_assist_tokens`]. Negative
/// numbers take back part of an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TokenCount {
    pub input: i64,
    /// The answer's text.
    pub output: i64,
    /// Thinking, on top of `output`.
    pub reasoning: i64,
    /// The part of `input` read from the provider's cache.
    pub cached: i64,
    /// Calls to the model.
    pub calls: i64,
    /// US dollars, when the price is known.
    pub cost_usd: Option<f64>,
}

/// Tokens of each kind one [`Store::add_assist_tokens`] adds or takes back at most. The numbers
/// come from the provider; a garbled or hostile count must neither overflow the sums (SQLite turns
/// an overflowing integer into a REAL, which no longer reads as one) nor wipe a day's limit.
const MAX_TOKENS_PER_ADD: i64 = 10_000_000;
/// US dollars one [`Store::add_assist_tokens`] adds or takes back at most.
const MAX_COST_PER_ADD: f64 = 1000.0;
/// A usage column counts no higher, so sums over many rows stay far from overflowing.
const MAX_USAGE_COUNT: i64 = 1_000_000_000_000;
/// Nor does a day's cost.
const MAX_USAGE_COST: f64 = 1_000_000_000.0;

/// One request as the provider reported it, next to what the server expected, for
/// `Assist/estimate` to learn from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CalibrationSample {
    pub estimated_input: i64,
    pub estimated_output: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub calls: i64,
}

/// Samples kept per provider, model and feature.
pub const ASSIST_CALIBRATION_SAMPLES: usize = 50;

/// What a person used today with one provider.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UsedToday {
    pub requests: i64,
    /// In, out and thinking.
    pub tokens: i64,
    /// US dollars; `None` when no price was known.
    pub cost_usd: Option<f64>,
}

/// This account's history with an address, for the spam check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderHistory {
    /// Mails from the address before the one asked about.
    pub earlier_messages: i64,
    /// Of those, how many are in Junk.
    pub earlier_in_junk: i64,
    /// Mails the person sent to the address.
    pub written_to: i64,
    /// When the first mail from it came.
    pub first_seen: Option<i64>,
}

/// The UTC day of a Unix time, `YYYY-MM-DD`.
pub fn utc_day(secs: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The keyword for a label named `name`: lower-case ASCII letters, digits and dashes, umlauts
/// written out. Empty when nothing of the name is left.
/// Keywords other mail programs and servers read as more than a word (Thunderbird's and some
/// servers' junk marks without `$`, and the names of system flags without their `\` or `$`): a
/// label named like one gets `label-<keyword>` instead, so labelling a mail never marks it junk,
/// read or deleted anywhere (security audit 0.21.0; the same list as the UwUMail app's).
const RESERVED_KEYWORDS: [&str; 14] = [
    "junk",
    "nonjunk",
    "notjunk",
    "phishing",
    "seen",
    "answered",
    "flagged",
    "deleted",
    "draft",
    "recent",
    "forwarded",
    "mdnsent",
    "submitpending",
    "submitted",
];

pub fn label_keyword(name: &str) -> String {
    let keyword = keyword_of_name(name);
    if RESERVED_KEYWORDS.contains(&keyword.as_str()) { format!("label-{keyword}") } else { keyword }
}

fn keyword_of_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let piece: &str = match c {
            'ä' => "ae",
            'ö' => "oe",
            'ü' => "ue",
            'ß' => "ss",
            'à' | 'á' | 'â' | 'ã' | 'å' => "a",
            'ç' => "c",
            'è' | 'é' | 'ê' | 'ë' => "e",
            'ì' | 'í' | 'î' | 'ï' => "i",
            'ñ' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ø' => "o",
            'ù' | 'ú' | 'û' => "u",
            'ý' | 'ÿ' => "y",
            c if c.is_ascii_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => "-",
        };
        out.push_str(piece);
    }
    let mut keyword = String::new();
    for part in out.split('-').filter(|part| !part.is_empty()) {
        if keyword.len() + part.len() + 1 > 40 {
            break;
        }
        if !keyword.is_empty() {
            keyword.push('-');
        }
        keyword.push_str(part);
    }
    keyword
}

const PROVIDER_COLUMNS: &str = "id, account_id, name, kind, base_url, secret IS NOT NULL, key_hint, model, fast_model,
     enabled, access, access_list, features, requests_per_day, tokens_per_day, created_at, updated_at, input_price,
     output_price, show_cost, request_price";

fn provider_row(row: &Row<'_>) -> rusqlite::Result<AssistProviderRecord> {
    let list = |index: usize| -> rusqlite::Result<Vec<String>> {
        let text: String = row.get(index)?;
        Ok(serde_json::from_str(&text).unwrap_or_default())
    };
    Ok(AssistProviderRecord {
        id: row.get(0)?,
        account_id: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        base_url: row.get(4)?,
        has_secret: row.get(5)?,
        key_hint: row.get(6)?,
        model: row.get(7)?,
        fast_model: row.get(8)?,
        enabled: row.get(9)?,
        access: row.get(10)?,
        access_list: list(11)?,
        features: list(12)?,
        requests_per_day: row.get(13)?,
        tokens_per_day: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        input_price: row.get(17)?,
        output_price: row.get(18)?,
        show_cost: row.get(19)?,
        request_price: row.get(20)?,
    })
}

fn load_provider(conn: &Connection, id: i64) -> Result<Option<AssistProviderRecord>> {
    Ok(conn
        .query_row(&format!("SELECT {PROVIDER_COLUMNS} FROM assist_providers WHERE id = ?1"), [id], provider_row)
        .optional()?)
}

fn bump_version(tx: &Connection) -> Result<()> {
    let version: i64 = get_setting(tx, VERSION_KEY)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    set_setting(tx, VERSION_KEY, &(version + 1).to_string())
}

pub(crate) fn bump_prefs(tx: &Connection, account_id: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO assist_prefs (account_id, modseq) VALUES (?1, 1)
         ON CONFLICT (account_id) DO UPDATE SET modseq = modseq + 1",
        [account_id],
    )?;
    Ok(())
}

/// The person's own AI setup ends when the account stops being theirs (it becomes a service or a
/// shared mailbox): their own providers go with the keys and sign-ins in them, the model no longer
/// labels incoming mail, and mail waiting for it is dropped. Otherwise the mailbox's mail went on to
/// the former person's own provider. The labels and what they learned stay: they are the mailbox's,
/// and labels without a model send nothing anywhere.
pub(crate) fn stop_personal_assist(tx: &Connection, account_id: i64) -> Result<()> {
    let providers = tx.execute("DELETE FROM assist_providers WHERE account_id = ?1", [account_id])?;
    tx.execute("DELETE FROM assist_label_queue WHERE account_id = ?1", [account_id])?;
    let prefs = tx.execute(
        "UPDATE assist_prefs SET choices = '{}', auto_labels = 0 WHERE account_id = ?1 AND (choices <> '{}' OR auto_labels)",
        [account_id],
    )?;
    if prefs > 0 || providers > 0 {
        bump_prefs(tx, account_id)?;
        bump_version(tx)?;
    }
    Ok(())
}

pub(crate) const LABEL_COLUMNS: &str =
    "id, name, description, keyword, color, created_at, rules, detector, learn_senders, classifier, base, auto";

pub(crate) fn label_row(row: &Row<'_>) -> rusqlite::Result<AssistLabel> {
    let rules: Option<String> = row.get(6)?;
    Ok(AssistLabel {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        keyword: row.get(3)?,
        color: row.get(4)?,
        created_at: row.get(5)?,
        rules: rules.and_then(|text| serde_json::from_str(&text).ok()),
        detector: row.get(7)?,
        learn_senders: row.get(8)?,
        classifier: row.get(9)?,
        base: row.get(10)?,
        auto: row.get(11)?,
    })
}

fn load_label(conn: &Connection, id: i64) -> Result<AssistLabel> {
    Ok(conn.query_row(&format!("SELECT {LABEL_COLUMNS} FROM assist_labels WHERE id = ?1"), [id], label_row)?)
}

/// Records a change of a label for push and the label state (`AssistLabel` is a push type).
pub(crate) fn label_changed(tx: &Transaction<'_>, account_id: i64, label_id: i64, change: &str) -> Result<i64> {
    let modseq = crate::db::next_modseq(tx, account_id)?;
    crate::db::record_change(tx, account_id, modseq, "AssistLabel", label_id, change)?;
    Ok(modseq)
}

fn check_label(name: &str, description: &str, color: Option<&str>) -> Result<()> {
    let chars = name.trim().chars().count();
    if chars == 0 || chars > ASSIST_LABEL_NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(StoreError::Rule {
            code: "invalidProperties",
            message: format!("a label's name has 1 to {ASSIST_LABEL_NAME_MAX_CHARS} characters"),
        });
    }
    if description.chars().count() > ASSIST_LABEL_DESCRIPTION_MAX_CHARS {
        return Err(StoreError::Rule {
            code: "invalidProperties",
            message: format!("a label's description has at most {ASSIST_LABEL_DESCRIPTION_MAX_CHARS} characters"),
        });
    }
    if let Some(color) = color {
        let hex = color.strip_prefix('#').unwrap_or("");
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(StoreError::Rule { code: "invalidProperties", message: "a color is #rrggbb".into() });
        }
    }
    Ok(())
}

pub(crate) fn name_taken(tx: &Transaction<'_>, account_id: i64, name: &str, except: i64) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM assist_labels WHERE account_id = ?1 AND lower(name) = lower(?2) AND id != ?3)",
        params![account_id, name.trim(), except],
        |row| row.get(0),
    )?)
}

impl Store {
    /// The admin's decisions, or the defaults: every feature allowed, no own providers.
    pub async fn assist_policy(&self) -> Result<AssistPolicy> {
        self.read(|conn| {
            Ok(get_setting(conn, POLICY_KEY)?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
        })
        .await
    }

    pub async fn set_assist_policy(&self, policy: AssistPolicy) -> Result<()> {
        self.write(move |tx| {
            let text = serde_json::to_string(&policy).map_err(|err| StoreError::Internal(err.to_string()))?;
            set_setting(tx, POLICY_KEY, &text)?;
            bump_version(tx)
        })
        .await
    }

    /// The price lists and exchange rates kept last, as the assistant wrote them.
    pub async fn assist_prices(&self) -> Result<Option<String>> {
        self.read(|conn| get_setting(conn, PRICES_KEY)).await
    }

    pub async fn set_assist_prices(&self, prices: String) -> Result<()> {
        self.write(move |tx| set_setting(tx, PRICES_KEY, &prices)).await
    }

    /// Counts up with every change to the policy or to a server provider, for session states.
    pub async fn assist_version(&self) -> Result<i64> {
        self.read(|conn| Ok(get_setting(conn, VERSION_KEY)?.and_then(|v| v.parse().ok()).unwrap_or(0))).await
    }

    /// The server's providers (`None`) or one person's own, oldest first.
    pub async fn assist_providers(&self, owner: Option<i64>) -> Result<Vec<AssistProviderRecord>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {PROVIDER_COLUMNS} FROM assist_providers WHERE account_id IS ?1 ORDER BY id"
            ))?;
            let rows = stmt.query_map([owner], provider_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// How many providers of `kind` there are, the server's and everyone's own.
    pub async fn assist_providers_of_kind(&self, kind: &str) -> Result<usize> {
        let kind = kind.to_owned();
        self.read(move |conn| {
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM assist_providers WHERE kind = ?1", [kind], |row| row.get(0))?;
            Ok(count as usize)
        })
        .await
    }

    pub async fn assist_provider(&self, id: i64) -> Result<Option<AssistProviderRecord>> {
        self.read(move |conn| load_provider(conn, id)).await
    }

    /// The provider's key or login tokens, unsealed.
    pub async fn assist_provider_secret(&self, id: i64) -> Result<Option<String>> {
        self.read(move |conn| {
            let sealed: Option<Vec<u8>> = conn
                .query_row("SELECT secret FROM assist_providers WHERE id = ?1", [id], |row| row.get(0))
                .optional()?
                .flatten();
            sealed.map(|sealed| unseal(conn, &sealed)).transpose()
        })
        .await
    }

    /// Adds a provider for the server (`owner` `None`) or a person. The caller checked the values;
    /// the limits on how many are checked here.
    pub async fn create_assist_provider(
        &self,
        owner: Option<i64>,
        write: AssistProviderWrite,
        secret: SecretChange,
    ) -> Result<AssistProviderRecord> {
        self.write(move |tx| {
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM assist_providers WHERE account_id IS ?1", [owner], |row| {
                    row.get(0)
                })?;
            let max = if owner.is_some() { ASSIST_MAX_PERSONAL_PROVIDERS } else { ASSIST_MAX_SERVER_PROVIDERS };
            if count as usize >= max {
                return Err(StoreError::Rule { code: "overQuota", message: format!("at most {max} providers") });
            }
            let now = now();
            tx.execute(
                "INSERT INTO assist_providers (account_id, name, kind, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![owner, write.name, write.kind, now],
            )?;
            let id = tx.last_insert_rowid();
            write_provider(tx, id, &write, &secret)?;
            match owner {
                Some(account) => bump_prefs(tx, account)?,
                None => bump_version(tx)?,
            }
            load_provider(tx, id)?.ok_or_else(|| StoreError::NotFound(format!("provider {id}")))
        })
        .await
    }

    pub async fn update_assist_provider(
        &self,
        id: i64,
        write: AssistProviderWrite,
        secret: SecretChange,
    ) -> Result<AssistProviderRecord> {
        self.write(move |tx| {
            let Some(before) = load_provider(tx, id)? else {
                return Err(StoreError::NotFound(format!("provider {id}")));
            };
            write_provider(tx, id, &write, &secret)?;
            match before.account_id {
                Some(account) => bump_prefs(tx, account)?,
                None => bump_version(tx)?,
            }
            load_provider(tx, id)?.ok_or_else(|| StoreError::NotFound(format!("provider {id}")))
        })
        .await
    }

    /// Replaces only the secret: a ChatGPT login's tokens after they were refreshed.
    pub async fn set_assist_provider_secret(&self, id: i64, secret: SecretChange) -> Result<()> {
        self.write(move |tx| {
            let Some(before) = load_provider(tx, id)? else {
                return Err(StoreError::NotFound(format!("provider {id}")));
            };
            write_secret(tx, id, &secret)?;
            if let Some(account) = before.account_id {
                bump_prefs(tx, account)?;
            }
            Ok(())
        })
        .await
    }

    pub async fn delete_assist_provider(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            let Some(before) = load_provider(tx, id)? else {
                return Err(StoreError::NotFound(format!("provider {id}")));
            };
            tx.execute("DELETE FROM assist_providers WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM assist_calibration WHERE provider_id = ?1", [id])?;
            match before.account_id {
                Some(account) => bump_prefs(tx, account)?,
                None => bump_version(tx)?,
            }
            Ok(())
        })
        .await
    }

    pub async fn assist_prefs(&self, account_id: i64) -> Result<AssistPrefs> {
        self.read(move |conn| {
            let found = conn
                .query_row(
                    "SELECT choices, auto_labels, modseq, non_ai_labels FROM assist_prefs WHERE account_id = ?1",
                    [account_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, bool>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    },
                )
                .optional()?;
            Ok(match found {
                Some((choices, auto_labels, modseq, non_ai_labels)) => AssistPrefs {
                    choices: serde_json::from_str(&choices).unwrap_or_default(),
                    auto_labels,
                    non_ai_labels,
                    modseq,
                },
                None => AssistPrefs::default(),
            })
        })
        .await
    }

    /// Replaces a person's choices; answers the new modseq.
    pub async fn set_assist_prefs(
        &self,
        account_id: i64,
        choices: serde_json::Map<String, Value>,
        auto_labels: bool,
    ) -> Result<i64> {
        let text = Value::Object(choices).to_string();
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO assist_prefs (account_id, choices, auto_labels, modseq) VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT (account_id) DO UPDATE SET choices = ?2, auto_labels = ?3, modseq = modseq + 1",
                params![account_id, text, auto_labels],
            )?;
            if !auto_labels {
                tx.execute("DELETE FROM assist_label_queue WHERE account_id = ?1", [account_id])?;
            }
            Ok(tx.query_row("SELECT modseq FROM assist_prefs WHERE account_id = ?1", [account_id], |row| row.get(0))?)
        })
        .await
    }

    /// Switches labels without a model on or off for a person.
    pub async fn set_non_ai_labels(&self, account_id: i64, on: bool) -> Result<i64> {
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO assist_prefs (account_id, non_ai_labels, modseq) VALUES (?1, ?2, 1)
                 ON CONFLICT (account_id) DO UPDATE SET non_ai_labels = ?2, modseq = modseq + 1",
                params![account_id, on],
            )?;
            Ok(tx.query_row("SELECT modseq FROM assist_prefs WHERE account_id = ?1", [account_id], |row| row.get(0))?)
        })
        .await
    }

    pub async fn assist_labels(&self, account_id: i64) -> Result<Vec<AssistLabel>> {
        self.read(move |conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT {LABEL_COLUMNS} FROM assist_labels WHERE account_id = ?1 ORDER BY id"))?;
            let rows = stmt.query_map([account_id], label_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Adds a label with only a name, a description and a color.
    pub async fn create_assist_label(
        &self,
        account_id: i64,
        name: String,
        description: String,
        color: Option<String>,
    ) -> Result<AssistLabel> {
        self.create_assist_label_with(account_id, AssistLabelWrite::simple(name, description, color)).await
    }

    /// Adds a label; its keyword is made from the name and never changes afterwards.
    pub async fn create_assist_label_with(&self, account_id: i64, write: AssistLabelWrite) -> Result<AssistLabel> {
        check_label(&write.name, &write.description, write.color.as_deref())?;
        let (label, modseq) = self
            .write(move |tx| {
                // The base labels come on top of the person's own.
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM assist_labels WHERE account_id = ?1 AND base IS NULL",
                    [account_id],
                    |row| row.get(0),
                )?;
                if count as usize >= ASSIST_MAX_LABELS {
                    return Err(StoreError::Rule {
                        code: "overQuota",
                        message: format!("at most {ASSIST_MAX_LABELS} labels"),
                    });
                }
                if name_taken(tx, account_id, &write.name, 0)? {
                    return Err(StoreError::Rule {
                        code: "invalidProperties",
                        message: format!("there is a label called {} already", write.name.trim()),
                    });
                }
                let id = insert_label(tx, account_id, &write, None)?;
                bump_prefs(tx, account_id)?;
                let modseq = label_changed(tx, account_id, id, "created")?;
                Ok((load_label(tx, id)?, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(label)
    }

    /// Changes a label's name, description and color; the rest stays.
    pub async fn update_assist_label(
        &self,
        account_id: i64,
        id: i64,
        name: String,
        description: String,
        color: Option<String>,
    ) -> Result<AssistLabel> {
        let before = self
            .assist_labels(account_id)
            .await?
            .into_iter()
            .find(|label| label.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("label {id}")))?;
        let write = AssistLabelWrite { name, description, color, ..AssistLabelWrite::of(&before) };
        self.update_assist_label_with(account_id, id, write).await
    }

    pub async fn update_assist_label_with(
        &self,
        account_id: i64,
        id: i64,
        write: AssistLabelWrite,
    ) -> Result<AssistLabel> {
        check_label(&write.name, "", write.color.as_deref())?;
        let (label, modseq) = self
            .write(move |tx| {
                let base: Option<Option<String>> = tx
                    .query_row(
                        "SELECT base FROM assist_labels WHERE id = ?1 AND account_id = ?2",
                        params![id, account_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(base) = base else {
                    return Err(StoreError::NotFound(format!("label {id}")));
                };
                // A base label's description is its definition: it stays as it is.
                let description = match base {
                    Some(_) => tx.query_row("SELECT description FROM assist_labels WHERE id = ?1", [id], |row| {
                        row.get::<_, String>(0)
                    })?,
                    None => {
                        check_label(&write.name, &write.description, None)?;
                        write.description.trim().to_owned()
                    }
                };
                if name_taken(tx, account_id, &write.name, id)? {
                    return Err(StoreError::Rule {
                        code: "invalidProperties",
                        message: format!("there is a label called {} already", write.name.trim()),
                    });
                }
                tx.execute(
                    "UPDATE assist_labels SET name = ?2, description = ?3, color = ?4 WHERE id = ?1",
                    params![id, write.name.trim(), description, write.color],
                )?;
                write_label_extras(tx, id, &write)?;
                bump_prefs(tx, account_id)?;
                let modseq = label_changed(tx, account_id, id, "updated")?;
                Ok((load_label(tx, id)?, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(label)
    }

    /// Removes a label and answers its keyword and the emails that carry it, for the caller to take
    /// it off them.
    pub async fn delete_assist_label(&self, account_id: i64, id: i64) -> Result<(String, Vec<i64>)> {
        self.write(move |tx| {
            let keyword: Option<String> = tx
                .query_row(
                    "SELECT keyword FROM assist_labels WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(keyword) = keyword else {
                return Err(StoreError::NotFound(format!("label {id}")));
            };
            let mut stmt = tx.prepare(
                "SELECT k.email_id FROM email_keywords k JOIN emails e ON e.id = k.email_id
                 WHERE e.account_id = ?1 AND k.keyword = ?2 LIMIT ?3",
            )?;
            let emails: Vec<i64> = stmt
                .query_map(params![account_id, keyword, MAX_UNLABEL as i64], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            drop(stmt);
            tx.execute("DELETE FROM assist_labels WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM label_training WHERE label_id = ?1", [id])?;
            bump_prefs(tx, account_id)?;
            let modseq = label_changed(tx, account_id, id, "destroyed")?;
            Ok((keyword, emails, modseq))
        })
        .await
        .map(|(keyword, emails, modseq)| {
            self.notify_change(account_id, modseq);
            (keyword, emails)
        })
    }

    /// Queues a delivered email for its labels when the person switched auto-labels on and has
    /// labels. Cheap when not; never fails the delivery (errors are the caller's to log).
    pub async fn enqueue_auto_label(&self, account_id: i64, email_id: i64) -> Result<bool> {
        let queued = self
            .write(move |tx| {
                let now = now();
                Ok(tx.execute(
                    "INSERT INTO assist_label_queue (account_id, email_id, queued_at, next_at)
                     SELECT ?1, ?2, ?3, ?3
                     WHERE EXISTS (SELECT 1 FROM assist_prefs WHERE account_id = ?1 AND auto_labels = 1)
                       AND EXISTS (SELECT 1 FROM assist_labels WHERE account_id = ?1)
                       AND (SELECT COUNT(*) FROM assist_label_queue WHERE account_id = ?1) < ?4
                     ON CONFLICT (account_id, email_id) DO NOTHING",
                    params![account_id, email_id, now, ASSIST_MAX_QUEUED_PER_ACCOUNT],
                )? == 1)
            })
            .await?;
        if queued {
            self.inner.assist_wakeup.notify_one();
        }
        Ok(queued)
    }

    /// Whether the account wants labels on its mail at all, so delivery can skip the write.
    pub async fn wants_auto_labels(&self, account_id: i64) -> Result<bool> {
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM assist_prefs WHERE account_id = ?1 AND auto_labels = 1)",
                [account_id],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// Wakes the label worker when mail was queued.
    pub fn assist_wakeup(&self) -> &tokio::sync::Notify {
        &self.inner.assist_wakeup
    }

    /// Jobs that are due, oldest first, at most one per person: one person's full queue never
    /// holds up everyone else's labels.
    pub async fn due_label_jobs(&self, limit: usize) -> Result<Vec<LabelJob>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_id, email_id, queued_at, attempts FROM (
                     SELECT id, account_id, email_id, queued_at, attempts, next_at,
                            ROW_NUMBER() OVER (PARTITION BY account_id ORDER BY next_at, id) AS nth
                     FROM assist_label_queue WHERE next_at <= ?1)
                 WHERE nth = 1 ORDER BY next_at, id LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now(), limit as i64], |row| {
                Ok(LabelJob {
                    id: row.get(0)?,
                    account_id: row.get(1)?,
                    email_id: row.get(2)?,
                    queued_at: row.get(3)?,
                    attempts: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// When the next job is due, if any.
    pub async fn next_label_job_at(&self) -> Result<Option<i64>> {
        self.read(|conn| Ok(conn.query_row("SELECT MIN(next_at) FROM assist_label_queue", [], |row| row.get(0))?)).await
    }

    pub async fn finish_label_job(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM assist_label_queue WHERE id = ?1", [id])?;
            Ok(())
        })
        .await
    }

    pub async fn retry_label_job(&self, id: i64, next_at: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "UPDATE assist_label_queue SET attempts = attempts + 1, next_at = ?2 WHERE id = ?1",
                params![id, next_at],
            )?;
            Ok(())
        })
        .await
    }

    /// Notes which label the model put on an email and why.
    pub async fn add_label_log(
        &self,
        account_id: i64,
        email_id: i64,
        label_id: i64,
        reason: String,
        provider: String,
        model: String,
    ) -> Result<i64> {
        let entry = LabelLogWrite {
            email_id,
            label_id,
            source: "ai".into(),
            code: "ai".into(),
            params: Value::Object(serde_json::Map::new()),
            reason,
            provider,
            model,
        };
        Ok(self.add_label_log_entries(account_id, vec![entry]).await?.pop().unwrap_or_default())
    }

    /// Notes labels that were put on emails, by whom and why; answers the entries' ids.
    pub async fn add_label_log_entries(&self, account_id: i64, entries: Vec<LabelLogWrite>) -> Result<Vec<i64>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        self.write(move |tx| {
            let mut ids = Vec::new();
            for entry in &entries {
                tx.execute(
                    "INSERT INTO assist_label_log (account_id, email_id, label_id, reason, provider, model, created_at,
                        source, code, params)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        account_id,
                        entry.email_id,
                        entry.label_id,
                        entry.reason,
                        entry.provider,
                        entry.model,
                        now(),
                        entry.source,
                        entry.code,
                        entry.params.to_string()
                    ],
                )?;
                ids.push(tx.last_insert_rowid());
            }
            // Old entries go; a person does not look back further than this.
            tx.execute(
                "DELETE FROM assist_label_log WHERE account_id = ?1 AND id NOT IN
                 (SELECT id FROM assist_label_log WHERE account_id = ?1 ORDER BY id DESC LIMIT 5000)",
                [account_id],
            )?;
            Ok(ids)
        })
        .await
    }

    /// The latest log entries, of some emails or of all; newest first.
    pub async fn label_log(
        &self,
        account_id: i64,
        email_ids: Option<Vec<i64>>,
        limit: usize,
    ) -> Result<Vec<LabelLogEntry>> {
        let ids = email_ids.map(|ids| serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()));
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT g.id, g.email_id, g.label_id, l.name, l.keyword, g.reason, g.provider, g.model, g.created_at,
                        g.undone_at IS NOT NULL OR NOT EXISTS
                            (SELECT 1 FROM email_keywords k WHERE k.email_id = g.email_id AND k.keyword = l.keyword),
                        g.source, g.code, g.params
                 FROM assist_label_log g JOIN assist_labels l ON l.id = g.label_id
                 WHERE g.account_id = ?1 AND (?2 IS NULL OR g.email_id IN (SELECT value FROM json_each(?2)))
                 ORDER BY g.id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![account_id, ids, limit as i64], |row| {
                Ok(LabelLogEntry {
                    id: row.get(0)?,
                    email_id: row.get(1)?,
                    label_id: row.get(2)?,
                    name: row.get(3)?,
                    keyword: row.get(4)?,
                    reason: row.get(5)?,
                    provider: row.get(6)?,
                    model: row.get(7)?,
                    created_at: row.get(8)?,
                    undone: row.get(9)?,
                    source: row.get(10)?,
                    code: row.get(11)?,
                    params: serde_json::from_str(&row.get::<_, String>(12)?).unwrap_or_default(),
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Marks a log entry undone and answers its email and keyword, for the caller to take the keyword
    /// off. `None` when there is no such entry.
    pub async fn undo_label_log(&self, account_id: i64, id: i64) -> Result<Option<(i64, String)>> {
        self.write(move |tx| {
            let found: Option<(i64, String)> = tx
                .query_row(
                    "SELECT g.email_id, l.keyword FROM assist_label_log g JOIN assist_labels l ON l.id = g.label_id
                     WHERE g.id = ?1 AND g.account_id = ?2",
                    params![id, account_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if found.is_some() {
                tx.execute(
                    "UPDATE assist_label_log SET undone_at = ?2 WHERE id = ?1 AND undone_at IS NULL",
                    params![id, now()],
                )?;
            }
            Ok(found)
        })
        .await
    }

    /// Counts a request before it is made, within the daily limits (`None`: no limit): checking and
    /// counting are one write, so requests running side by side can't share the last one left.
    /// Answers the day it was counted on, for [`Store::add_assist_tokens`]; a limit that is used up
    /// is `Rule { code: "overQuota" }` with `requests` or `tokens` as the message.
    pub async fn reserve_assist_usage(
        &self,
        account_id: i64,
        provider_id: i64,
        feature: &str,
        requests_per_day: Option<i64>,
        tokens_per_day: Option<i64>,
    ) -> Result<String> {
        let feature = feature.to_owned();
        self.write(move |tx| {
            let day = utc_day(now());
            if requests_per_day.is_some() || tokens_per_day.is_some() {
                let (requests, tokens): (i64, i64) = tx.query_row(
                    "SELECT COALESCE(SUM(requests), 0), CAST(TOTAL(input_tokens + output_tokens + reasoning_tokens) AS INTEGER)
                     FROM assist_usage WHERE account_id = ?1 AND provider_id = ?2 AND day = ?3",
                    params![account_id, provider_id, day],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                if requests_per_day.is_some_and(|limit| requests >= limit) {
                    return Err(StoreError::Rule { code: "overQuota", message: "requests".into() });
                }
                if tokens_per_day.is_some_and(|limit| tokens >= limit) {
                    return Err(StoreError::Rule { code: "overQuota", message: "tokens".into() });
                }
            }
            tx.execute(
                "INSERT INTO assist_usage (account_id, provider_id, day, feature, requests) VALUES (?1, ?2, ?3, ?4, 1)
                 ON CONFLICT (account_id, provider_id, day, feature) DO UPDATE SET requests = requests + 1",
                params![account_id, provider_id, day, feature],
            )?;
            Ok(day)
        })
        .await
    }

    /// Adds tokens, and what they cost in US dollars when that is known, to a request counted with
    /// [`Store::reserve_assist_usage`] on `day`. Negative numbers take back part of an estimate; a
    /// count never goes below zero.
    pub async fn add_assist_tokens(
        &self,
        account_id: i64,
        provider_id: i64,
        day: String,
        feature: &str,
        count: TokenCount,
    ) -> Result<()> {
        let feature = feature.to_owned();
        let tokens = |n: i64| n.clamp(-MAX_TOKENS_PER_ADD, MAX_TOKENS_PER_ADD);
        let cost_usd =
            count.cost_usd.filter(|cost| cost.is_finite()).map(|cost| cost.clamp(-MAX_COST_PER_ADD, MAX_COST_PER_ADD));
        self.write(move |tx| {
            // Each column stays between 0 and its cap; the cap also brings back a column an older
            // overflow left as a REAL.
            tx.execute(
                "INSERT INTO assist_usage (account_id, provider_id, day, feature, input_tokens, output_tokens, cost_usd,
                    reasoning_tokens, cached_tokens, calls)
                 VALUES (?1, ?2, ?3, ?4, MAX(?5, 0), MAX(?6, 0), MAX(?7, 0), MAX(?8, 0), MAX(?9, 0), MAX(?10, 0))
                 ON CONFLICT (account_id, provider_id, day, feature) DO UPDATE SET
                    input_tokens = MIN(MAX(input_tokens + ?5, 0), ?11),
                    output_tokens = MIN(MAX(output_tokens + ?6, 0), ?11),
                    cost_usd = CASE WHEN ?7 IS NULL THEN cost_usd ELSE MIN(MAX(COALESCE(cost_usd, 0) + ?7, 0), ?12) END,
                    reasoning_tokens = MIN(MAX(reasoning_tokens + ?8, 0), ?11),
                    cached_tokens = MIN(MAX(cached_tokens + ?9, 0), ?11),
                    calls = MIN(MAX(calls + ?10, 0), ?11)",
                params![
                    account_id,
                    provider_id,
                    day,
                    feature,
                    tokens(count.input),
                    tokens(count.output),
                    cost_usd,
                    tokens(count.reasoning),
                    tokens(count.cached),
                    tokens(count.calls),
                    MAX_USAGE_COUNT,
                    MAX_USAGE_COST
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Keeps what one request really took next to what was expected, and forgets the oldest beyond
    /// [`ASSIST_CALIBRATION_SAMPLES`] of that provider, model and feature.
    pub async fn add_assist_calibration(
        &self,
        provider_id: i64,
        model: &str,
        feature: &str,
        sample: CalibrationSample,
    ) -> Result<()> {
        let (model, feature) = (model.to_owned(), feature.to_owned());
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO assist_calibration (provider_id, model, feature, estimated_input, estimated_output,
                    input_tokens, output_tokens, reasoning_tokens, calls, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    provider_id,
                    model,
                    feature,
                    sample.estimated_input,
                    sample.estimated_output,
                    sample.input_tokens,
                    sample.output_tokens,
                    sample.reasoning_tokens,
                    sample.calls,
                    now()
                ],
            )?;
            tx.execute(
                "DELETE FROM assist_calibration WHERE provider_id = ?1 AND model = ?2 AND feature = ?3 AND id NOT IN (
                    SELECT id FROM assist_calibration WHERE provider_id = ?1 AND model = ?2 AND feature = ?3
                    ORDER BY id DESC LIMIT ?4)",
                params![provider_id, model, feature, ASSIST_CALIBRATION_SAMPLES as i64],
            )?;
            Ok(())
        })
        .await
    }

    /// The last requests of a provider's model for a feature, newest first.
    pub async fn assist_calibration(
        &self,
        provider_id: i64,
        model: &str,
        feature: &str,
    ) -> Result<Vec<CalibrationSample>> {
        let (model, feature) = (model.to_owned(), feature.to_owned());
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT estimated_input, estimated_output, input_tokens, output_tokens, reasoning_tokens, calls
                 FROM assist_calibration WHERE provider_id = ?1 AND model = ?2 AND feature = ?3
                 ORDER BY id DESC LIMIT ?4",
            )?;
            let rows =
                stmt.query_map(params![provider_id, model, feature, ASSIST_CALIBRATION_SAMPLES as i64], |row| {
                    Ok(CalibrationSample {
                        estimated_input: row.get(0)?,
                        estimated_output: row.get(1)?,
                        input_tokens: row.get(2)?,
                        output_tokens: row.get(3)?,
                        reasoning_tokens: row.get(4)?,
                        calls: row.get(5)?,
                    })
                })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Requests and tokens (in, out and thinking) a person used today with one provider.
    pub async fn assist_used_today(&self, account_id: i64, provider_id: i64) -> Result<UsedToday> {
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT COALESCE(SUM(requests), 0), CAST(TOTAL(input_tokens + output_tokens + reasoning_tokens) AS INTEGER), SUM(cost_usd)
                 FROM assist_usage WHERE account_id = ?1 AND provider_id = ?2 AND day = ?3",
                params![account_id, provider_id, utc_day(now())],
                |row| Ok(UsedToday { requests: row.get(0)?, tokens: row.get(1)?, cost_usd: row.get(2)? }),
            )?)
        })
        .await
    }

    /// Usage from `since_day` on, of one person or of everyone; newest day first.
    pub async fn assist_usage(&self, account_id: Option<i64>, since_day: String) -> Result<Vec<UsageRow>> {
        self.read(move |conn| {
            // Counts read as integers within their cap whatever an older overflow left in them, so
            // one bad row can't break the whole page.
            let count = |column: &str| format!("MIN(MAX(CAST(u.{column} AS INTEGER), 0), {MAX_USAGE_COUNT})");
            let mut stmt = conn.prepare(&format!(
                "SELECT u.day, u.account_id, a.login, u.provider_id, p.name, u.feature, {}, {}, {},
                        MIN(MAX(CAST(u.cost_usd AS REAL), 0), {MAX_USAGE_COST:?}), {}, {}, {}
                 FROM assist_usage u JOIN accounts a ON a.id = u.account_id
                 LEFT JOIN assist_providers p ON p.id = u.provider_id
                 WHERE u.day >= ?1 AND (?2 IS NULL OR u.account_id = ?2)
                 ORDER BY u.day DESC, a.login, u.provider_id, u.feature LIMIT 20000",
                count("requests"),
                count("input_tokens"),
                count("output_tokens"),
                count("reasoning_tokens"),
                count("cached_tokens"),
                count("calls"),
            ))?;
            let rows = stmt.query_map(params![since_day, account_id], |row| {
                Ok(UsageRow {
                    day: row.get(0)?,
                    account_id: row.get(1)?,
                    login: row.get(2)?,
                    provider_id: row.get(3)?,
                    provider_name: row.get(4)?,
                    feature: row.get(5)?,
                    requests: row.get(6)?,
                    input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    cost_usd: row.get(9)?,
                    reasoning_tokens: row.get(10)?,
                    cached_tokens: row.get(11)?,
                    calls: row.get(12)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Forgets usage older than `before_day`, and label jobs and hand-labelings still to learn
    /// queued before `queued_before`.
    pub async fn prune_assist(&self, before_day: String, queued_before: i64) -> Result<usize> {
        self.write(move |tx| {
            let mut removed = tx.execute("DELETE FROM assist_usage WHERE day < ?1", [before_day])?;
            removed += tx.execute("DELETE FROM assist_label_queue WHERE queued_at < ?1", [queued_before])?;
            removed += tx.execute("DELETE FROM label_training WHERE queued_at < ?1", [queued_before])?;
            Ok(removed)
        })
        .await
    }

    /// This account's history with `address` (lower case), counting mail received before
    /// `before_email` only.
    pub async fn sender_history(&self, account_id: i64, address: String, before_email: i64) -> Result<SenderHistory> {
        let address = address.trim().to_lowercase();
        if address.is_empty() {
            return Ok(SenderHistory::default());
        }
        self.read(move |conn| {
            // The addresses are stored as JSON; the quotes keep "leni@x" from matching "a.leni@x".
            let needle = format!("\"{}\"", address.replace('"', ""));
            let (earlier, in_junk, first): (i64, i64, Option<i64>) = conn.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                                             WHERE em.email_id = e.id AND m.role = 'junk')), 0),
                        MIN(e.received_at)
                 FROM emails e
                 WHERE e.account_id = ?1 AND e.id != ?3 AND e.received_at <=
                       COALESCE((SELECT received_at FROM emails WHERE id = ?3), 9223372036854775807)
                   AND instr(lower(e.from_addr), ?2) > 0
                   AND NOT EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                                   WHERE em.email_id = e.id AND m.role = 'sent')",
                params![account_id, needle, before_email],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let written: i64 = conn.query_row(
                "SELECT COUNT(*) FROM emails e
                 WHERE e.account_id = ?1
                   AND EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                               WHERE em.email_id = e.id AND m.role = 'sent')
                   AND instr(lower(e.to_addr || e.cc_addr || e.bcc_addr), ?2) > 0",
                params![account_id, needle],
                |row| row.get(0),
            )?;
            Ok(SenderHistory {
                earlier_messages: earlier,
                earlier_in_junk: in_junk,
                written_to: written,
                first_seen: first,
            })
        })
        .await
    }

    /// Names and addresses (lower case) from the account's own address books, at most `max`
    /// pairs: for matching the people a mail names, and for the spam check's "in the address book".
    pub async fn contact_addresses(&self, account_id: i64, max: usize) -> Result<Vec<(String, String)>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT r.content FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                 WHERE c.account_id = ?1 AND c.kind = 'addressbook' AND r.component = 'VCARD'
                 ORDER BY r.id LIMIT ?2",
            )?;
            let cards: Vec<String> = stmt
                .query_map(params![account_id, MAX_CONTACT_CARDS as i64], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let mut out = Vec::new();
            for content in cards {
                for pair in card_addresses(&content) {
                    if out.len() >= max {
                        return Ok(out);
                    }
                    out.push(pair);
                }
            }
            Ok(out)
        })
        .await
    }
}

/// Cards read for [`Store::contact_addresses`], at most.
const MAX_CONTACT_CARDS: usize = 5000;

/// The name (FN) and addresses of one vCard.
fn card_addresses(content: &str) -> Vec<(String, String)> {
    use calcard::vcard::{VCard, VCardProperty, VCardValue};
    let Ok(card) = VCard::parse(content) else { return Vec::new() };
    let text = |value: &VCardValue| match value {
        VCardValue::Text(text) => Some(text.trim().to_owned()),
        _ => None,
    };
    let name = card
        .entries
        .iter()
        .filter(|entry| entry.name == VCardProperty::Fn)
        .find_map(|entry| entry.values.iter().find_map(text))
        .unwrap_or_default();
    let name: String = name.chars().filter(|c| !c.is_control()).take(100).collect();
    card.entries
        .iter()
        .filter(|entry| entry.name == VCardProperty::Email)
        .flat_map(|entry| entry.values.iter())
        .filter_map(text)
        .map(|email| email.strip_prefix("mailto:").unwrap_or(&email).trim().to_lowercase())
        .filter(|email| email.len() <= 320 && email.contains('@') && !email.contains(char::is_whitespace))
        .take(16)
        .map(|email| (name.clone(), email))
        .collect()
}

/// The columns of a label besides name, description and color.
/// Writes a new label with a keyword made from its name (unique for the account, never `$…`), and
/// answers its id.
pub(crate) fn insert_label(
    tx: &Transaction<'_>,
    account_id: i64,
    write: &AssistLabelWrite,
    base: Option<&str>,
) -> Result<i64> {
    let stem = label_keyword(&write.name);
    let taken = |keyword: &str| -> Result<bool> {
        Ok(tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM assist_labels WHERE account_id = ?1 AND keyword = ?2)",
            params![account_id, keyword],
            |row| row.get(0),
        )?)
    };
    let mut keyword = if stem.is_empty() { "label-1".to_owned() } else { stem.clone() };
    let mut n = 1;
    while taken(&keyword)? || keyword.starts_with('$') {
        n += 1;
        keyword = if stem.is_empty() { format!("label-{n}") } else { format!("{stem}-{n}") };
    }
    tx.execute(
        "INSERT INTO assist_labels (account_id, name, description, keyword, color, created_at, base)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![account_id, write.name.trim(), write.description.trim(), keyword, write.color, now(), base],
    )?;
    let id = tx.last_insert_rowid();
    write_label_extras(tx, id, write)?;
    Ok(id)
}

fn write_label_extras(tx: &Transaction<'_>, id: i64, write: &AssistLabelWrite) -> Result<()> {
    tx.execute(
        "UPDATE assist_labels SET rules = ?2, detector = ?3, learn_senders = ?4, classifier = ?5, auto = ?6 WHERE id = ?1",
        params![
            id,
            write.rules.as_ref().map(Value::to_string),
            write.detector,
            write.learn_senders,
            write.classifier,
            write.auto
        ],
    )?;
    Ok(())
}

fn write_provider(tx: &Transaction<'_>, id: i64, write: &AssistProviderWrite, secret: &SecretChange) -> Result<()> {
    let list = |items: &Vec<String>| serde_json::to_string(items).unwrap_or_else(|_| "[]".into());
    tx.execute(
        "UPDATE assist_providers SET name = ?2, kind = ?3, base_url = ?4, model = ?5, fast_model = ?6, enabled = ?7,
            access = ?8, access_list = ?9, features = ?10, requests_per_day = ?11, tokens_per_day = ?12,
            updated_at = ?13, input_price = ?14, output_price = ?15, show_cost = ?16, request_price = ?17
         WHERE id = ?1",
        params![
            id,
            write.name,
            write.kind,
            write.base_url,
            write.model,
            write.fast_model,
            write.enabled,
            write.access,
            list(&write.access_list),
            list(&write.features),
            write.requests_per_day,
            write.tokens_per_day,
            now(),
            write.input_price,
            write.output_price,
            write.show_cost,
            write.request_price
        ],
    )?;
    write_secret(tx, id, secret)
}

fn write_secret(tx: &Transaction<'_>, id: i64, secret: &SecretChange) -> Result<()> {
    match secret {
        SecretChange::Keep => {}
        SecretChange::Remove => {
            tx.execute("UPDATE assist_providers SET secret = NULL, key_hint = NULL WHERE id = ?1", [id])?;
        }
        SecretChange::Set(plain, hint) => {
            let sealed = seal(tx, plain)?;
            tx.execute(
                "UPDATE assist_providers SET secret = ?2, key_hint = ?3 WHERE id = ?1",
                params![id, sealed, hint],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(1_790_553_600), "2026-09-28");
        assert_eq!(utc_day(1_790_639_999), "2026-09-28");
        assert_eq!(utc_day(951_782_400), "2000-02-29");
        assert_eq!(utc_day(-1), "1969-12-31");
    }

    #[test]
    fn keywords_are_ascii_words() {
        assert_eq!(label_keyword("Rechnungen"), "rechnungen");
        assert_eq!(label_keyword("Bestellungen & Versand"), "bestellungen-versand");
        assert_eq!(label_keyword("Persönlich"), "persoenlich");
        assert_eq!(label_keyword("  Größe!! "), "groesse");
        assert_eq!(label_keyword("旅行"), "");
        assert_eq!(label_keyword("Junk"), "label-junk", "no mark other programs act on");
        assert_eq!(label_keyword("Non Junk"), "non-junk");
        assert_eq!(label_keyword("NonJunk"), "label-nonjunk");
        assert_eq!(label_keyword("Seen"), "label-seen");
        assert!(label_keyword(&"sehr-lang ".repeat(20)).len() <= 40);
        assert!(crate::mutate::valid_keyword(&label_keyword("Reisen (privat)")));
    }

    #[tokio::test]
    async fn huge_token_counts_are_capped_and_usage_stays_readable() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let account = store
            .create_account(crate::NewAccount {
                address: "nyu@example.org".into(),
                display_name: "Nyu".into(),
                password: None,
                role: crate::Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let day = store.reserve_assist_usage(account, 1, "summarize", None, None).await.unwrap();
        let max = TokenCount {
            input: i64::MAX,
            output: i64::MAX,
            reasoning: i64::MAX,
            cached: i64::MAX,
            calls: i64::MAX,
            cost_usd: Some(f64::MAX),
        };
        store.add_assist_tokens(account, 1, day.clone(), "summarize", max).await.unwrap();
        store.add_assist_tokens(account, 1, day.clone(), "summarize", max).await.unwrap();
        let rows = store.assist_usage(None, day.clone()).await.unwrap();
        assert_eq!((rows[0].input_tokens, rows[0].calls), (2 * MAX_TOKENS_PER_ADD, 2 * MAX_TOKENS_PER_ADD));
        assert_eq!(rows[0].cost_usd, Some(2.0 * MAX_COST_PER_ADD));

        // A huge take-back only takes back one request's worth, never the whole day.
        let back = TokenCount { input: i64::MIN, cost_usd: Some(f64::MIN), ..TokenCount::default() };
        store.add_assist_tokens(account, 1, day.clone(), "summarize", back).await.unwrap();
        let rows = store.assist_usage(None, day.clone()).await.unwrap();
        assert_eq!((rows[0].input_tokens, rows[0].cost_usd), (MAX_TOKENS_PER_ADD, Some(MAX_COST_PER_ADD)));

        // A row an older overflow turned into REALs still reads, and the next count repairs it.
        store
            .write(|tx| {
                tx.execute("UPDATE assist_usage SET input_tokens = 1e300, output_tokens = 9.3e18", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let rows = store.assist_usage(None, day.clone()).await.unwrap();
        assert_eq!((rows[0].input_tokens, rows[0].output_tokens), (MAX_USAGE_COUNT, MAX_USAGE_COUNT));
        assert!(store.assist_used_today(account, 1).await.unwrap().tokens > 0);
        // The limit check reads it too: used up, not a broken column.
        let limited = store.reserve_assist_usage(account, 1, "summarize", None, Some(i64::MAX)).await;
        assert!(matches!(limited, Err(StoreError::Rule { code: "overQuota", .. })), "{limited:?}");
        store.add_assist_tokens(account, 1, day.clone(), "summarize", TokenCount::default()).await.unwrap();
        let rows = store.assist_usage(None, day).await.unwrap();
        assert_eq!(rows[0].input_tokens, MAX_USAGE_COUNT);
    }
}
