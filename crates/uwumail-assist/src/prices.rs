//! What a model costs (docs/llm.md, "Costs"): the price lists the server fetches once a day, the
//! exchange rates to show costs in, and what a provider's model costs per token.
//!
//! - LiteLLM's price list of most providers' models (US dollars per token),
//! - OpenRouter's own prices, when a provider is OpenRouter,
//! - the ECB's euro reference rates, to show costs in euros, yen, yuan and the others it lists.
//!
//! They are fetched through the egress like the requests to the providers, kept in the database,
//! and a list that can't be fetched keeps its last good copy. Ollama and a ChatGPT subscription
//! cost nothing per request; a price the admin or the person set comes before the lists.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use http_body_util::BodyExt;
use hyper::Request;
use hyper::header::{ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;
use uwumail_smtp::egress::Reach;
use uwumail_store::AssistProviderRecord;

use crate::kinds::KindInfo;
use crate::{Assist, AssistError, Result, now};

/// How often the lists are fetched, and how soon a failed fetch is tried again.
const REFRESH_SECS: i64 = 86_400;
const RETRY_SECS: i64 = 3600;
/// The largest list taken (LiteLLM's is a few megabytes).
const MAX_LIST_BYTES: usize = 32 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// The highest price per million tokens that may be set by hand, in US dollars.
pub const MAX_PRICE_PER_MILLION: f64 = 100_000.0;
/// The highest price per request that may be set by hand, in US dollars.
pub const MAX_PRICE_PER_REQUEST: f64 = 100.0;
/// Rough euro rates of the currencies the apps show costs in, for as long as the server never got
/// the ECB's (no way out to the internet yet): a price set by hand still shows in euros, yen or
/// yuan instead of not at all. Replaced by the real rates with the first list that arrives.
const FALLBACK_RATES: [(&str, f64); 3] = [("USD", 1.17), ("JPY", 170.0), ("CNY", 8.3)];

/// Where the lists come from; other addresses in tests.
#[derive(Debug, Clone)]
pub struct PriceSources {
    pub litellm: String,
    pub ecb: String,
    pub openrouter: String,
}

impl Default for PriceSources {
    fn default() -> PriceSources {
        PriceSources {
            litellm: "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
                .into(),
            ecb: "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml".into(),
            openrouter: "https://openrouter.ai/api/v1/models".into(),
        }
    }
}

/// A price level above a prompt size (LiteLLM's `*_above_128k_tokens`): once a request's prompt is
/// larger than `above` tokens, the whole request is charged at these rates (US dollars per token).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tier {
    pub above: i64,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub reasoning: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
}

/// What a model costs by a price list: US dollars per token, per request, per picture and per web
/// search, and what the list says about the model itself.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    /// Thinking tokens; `None`: as `output`.
    pub reasoning: Option<f64>,
    /// Prompt tokens read from the provider's cache; `None`: as `input`.
    pub cache_read: Option<f64>,
    /// Prompt tokens written to the provider's cache; `None`: as `input`.
    pub cache_write: Option<f64>,
    pub per_request: f64,
    pub per_image: f64,
    pub web_search: f64,
    /// Higher prices for large prompts, by `above`, smallest first.
    pub tiers: Vec<Tier>,
    /// The model thinks before it answers (LiteLLM's `supports_reasoning`).
    pub reasoning_model: bool,
    /// The most the model writes in one answer, thinking included.
    pub max_output_tokens: Option<i64>,
}

impl Rates {
    /// Only the price in and out, as lists kept before 0.20.0 have it.
    pub fn plain(input: f64, output: f64) -> Rates {
        Rates { input, output, ..Rates::default() }
    }
}

/// A map of rates as kept: also the `[input, output]` pairs of lists kept before 0.20.0.
fn rates_map<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Rates>, D::Error> {
    let raw: BTreeMap<String, Value> = BTreeMap::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|(name, value)| {
            let rates = match &value {
                Value::Array(pair) => Rates::plain(pair.first()?.as_f64()?, pair.get(1)?.as_f64()?),
                _ => serde_json::from_value(value).ok()?,
            };
            Some((name, rates))
        })
        .collect())
}

/// The lists as they are kept: what models cost by LiteLLM and OpenRouter; euro reference rates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceTable {
    /// When the last fetch was tried (Unix time), whether or not every list came.
    #[serde(default)]
    pub fetched_at: i64,
    /// LiteLLM's models, by their name there in lower case.
    #[serde(default, deserialize_with = "rates_map")]
    pub models: BTreeMap<String, Rates>,
    /// OpenRouter's, by its model id in lower case.
    #[serde(default, deserialize_with = "rates_map")]
    pub openrouter: BTreeMap<String, Rates>,
    /// Units of a currency per euro (`USD: 1.08`); the euro itself is 1.
    #[serde(default)]
    pub rates: BTreeMap<String, f64>,
    /// The day of the rates.
    #[serde(default)]
    pub rates_day: Option<String>,
}

/// Where a price comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PriceSource {
    /// The price lists.
    Auto,
    /// Set by the admin or the person.
    Manual,
    /// Nothing per request: a model of one's own (Ollama), a subscription (ChatGPT).
    Free,
}

/// A higher price for large prompts, in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceTier {
    /// Prompts larger than this many tokens.
    pub above_tokens: i64,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub reasoning_per_million: f64,
    pub cache_read_per_million: f64,
}

/// What a model costs: US dollars per million tokens, per request, per picture and per web search.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input_per_million: f64,
    pub output_per_million: f64,
    /// Thinking tokens (the same as `output_per_million` where the list names none).
    pub reasoning_per_million: f64,
    /// Prompt tokens read from or written to the provider's cache.
    pub cache_read_per_million: f64,
    pub cache_write_per_million: f64,
    pub per_request: f64,
    pub per_image: f64,
    pub web_search_per_query: f64,
    pub tiers: Vec<PriceTier>,
    /// The model thinks before it answers, by the lists.
    pub supports_reasoning: bool,
    /// The most the model writes in one answer, thinking included, by the lists.
    pub max_output_tokens: Option<i64>,
    pub source: PriceSource,
}

/// Tokens and other things one or more calls to a model are billed for.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Metered {
    /// The whole prompt, cached parts included.
    pub input: f64,
    /// Of `input`, read from the provider's cache.
    pub cache_read: f64,
    /// Of `input`, written to the provider's cache.
    pub cache_write: f64,
    /// The answer's text.
    pub output: f64,
    /// Thinking, on top of `output`.
    pub reasoning: f64,
    pub images: f64,
    /// Requests billed (per-request fees).
    pub requests: f64,
    pub web_searches: f64,
    /// The prompt of one call, which decides the price level (tiers).
    pub prompt: i64,
}

/// What something costs, by what it is for, in US dollars (or, converted, another currency).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct CostParts {
    /// The prompt, with what was read from or written to the cache.
    pub input: f64,
    pub output: f64,
    pub reasoning: f64,
    pub images: f64,
    /// Per-request fees.
    pub requests: f64,
    /// Everything else: web searches, and for an estimate the extra calls a request may take.
    pub other: f64,
}

impl CostParts {
    pub fn total(&self) -> f64 {
        self.input + self.output + self.reasoning + self.images + self.requests + self.other
    }

    pub fn add(&mut self, other: &CostParts) {
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.images += other.images;
        self.requests += other.requests;
        self.other += other.other;
    }

    /// Every part times `factor`.
    pub fn scaled(&self, factor: f64) -> CostParts {
        CostParts {
            input: self.input * factor,
            output: self.output * factor,
            reasoning: self.reasoning * factor,
            images: self.images * factor,
            requests: self.requests * factor,
            other: self.other * factor,
        }
    }
}

impl Price {
    /// Nothing per request.
    pub fn free() -> Price {
        Price::from_rates(&Rates::default(), PriceSource::Free)
    }

    fn from_rates(rates: &Rates, source: PriceSource) -> Price {
        let million = |per_token: f64| per_token * 1e6;
        Price {
            input_per_million: million(rates.input),
            output_per_million: million(rates.output),
            reasoning_per_million: million(rates.reasoning.unwrap_or(rates.output)),
            cache_read_per_million: million(rates.cache_read.unwrap_or(rates.input)),
            cache_write_per_million: million(rates.cache_write.unwrap_or(rates.input)),
            per_request: rates.per_request,
            per_image: rates.per_image,
            web_search_per_query: rates.web_search,
            tiers: rates
                .tiers
                .iter()
                .map(|tier| PriceTier {
                    above_tokens: tier.above,
                    input_per_million: million(tier.input),
                    output_per_million: million(tier.output),
                    reasoning_per_million: million(tier.reasoning.unwrap_or(tier.output)),
                    cache_read_per_million: million(tier.cache_read.or(rates.cache_read).unwrap_or(tier.input)),
                })
                .collect(),
            supports_reasoning: rates.reasoning_model,
            max_output_tokens: rates.max_output_tokens,
            source,
        }
    }

    /// Input, output, thinking and cache-read price per million tokens for a prompt of `prompt`
    /// tokens: the highest tier it is above, or the base price.
    pub fn level(&self, prompt: i64) -> (f64, f64, f64, f64) {
        match self.tiers.iter().rev().find(|tier| prompt > tier.above_tokens) {
            Some(tier) => (
                tier.input_per_million,
                tier.output_per_million,
                tier.reasoning_per_million,
                tier.cache_read_per_million,
            ),
            None => (
                self.input_per_million,
                self.output_per_million,
                self.reasoning_per_million,
                self.cache_read_per_million,
            ),
        }
    }

    /// What `metered` costs, in US dollars.
    pub fn cost_of(&self, metered: &Metered) -> CostParts {
        let (input, output, reasoning, cache_read) = self.level(metered.prompt);
        let cache_read_tokens = metered.cache_read.clamp(0.0, metered.input.max(0.0));
        let cache_write_tokens = metered.cache_write.clamp(0.0, (metered.input - cache_read_tokens).max(0.0));
        let uncached = (metered.input - cache_read_tokens - cache_write_tokens).max(0.0);
        CostParts {
            input: (uncached * input
                + cache_read_tokens * cache_read
                + cache_write_tokens * self.cache_write_per_million)
                / 1e6,
            output: metered.output.max(0.0) * output / 1e6,
            reasoning: metered.reasoning.max(0.0) * reasoning / 1e6,
            images: metered.images.max(0.0) * self.per_image,
            requests: metered.requests.max(0.0) * self.per_request,
            other: metered.web_searches.max(0.0) * self.web_search_per_query,
        }
    }

    /// US dollars for so many tokens in and out at the base price, nothing else.
    pub fn cost(&self, input_tokens: i64, output_tokens: i64) -> f64 {
        (input_tokens.max(0) as f64 * self.input_per_million + output_tokens.max(0) as f64 * self.output_per_million)
            / 1_000_000.0
    }
}

/// A cost as shown: in the currency asked for, and in US dollars.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
    pub usd: f64,
}

/// The table with a lookup built for it.
#[derive(Debug, Default)]
pub struct Prices {
    pub table: PriceTable,
    /// LiteLLM's models by their name without a provider prefix or a date.
    loose: HashMap<String, Rates>,
}

/// A currency code as ISO 4217 writes it.
pub fn is_currency(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase())
}

/// The name without a provider prefix (`openai/`, `models/`), a date or version stamp at its end
/// (`-2024-08-06`, `-20241022`, `@20240620`, `-0613`, `-latest`), in lower case.
fn loose_name(model: &str) -> String {
    let model = model.trim().to_lowercase();
    let mut name = model.rsplit('/').next().unwrap_or(&model).to_owned();
    loop {
        let before = name.clone();
        for suffix in ["-latest", ":latest", "-preview"] {
            if let Some(stripped) = name.strip_suffix(suffix) {
                name = stripped.to_owned();
            }
        }
        if let Some((head, tail)) = name.rsplit_once(['-', '@', ':'])
            && !head.is_empty()
            && (tail.len() == 8 || tail.len() == 4 || tail.len() == 6)
            && tail.bytes().all(|b| b.is_ascii_digit())
        {
            name = head.to_owned();
        }
        // `-2024-08-06`: three pieces of digits.
        let parts: Vec<&str> = name.rsplitn(4, '-').collect();
        if parts.len() == 4
            && parts[0].len() == 2
            && parts[1].len() == 2
            && parts[2].len() == 4
            && parts[..3].iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()))
        {
            name = parts[3].to_owned();
        }
        if name == before {
            return name;
        }
    }
}

/// How LiteLLM prefixes a kind's models, where it does.
fn litellm_prefix(kind: &str) -> Option<&'static str> {
    match kind {
        "gemini" => Some("gemini/"),
        "mistral" => Some("mistral/"),
        "anthropic" => Some("anthropic/"),
        "openrouter" => Some("openrouter/"),
        _ => None,
    }
}

impl Prices {
    pub fn new(table: PriceTable) -> Prices {
        let mut loose: HashMap<String, Rates> = HashMap::new();
        // Names that are already loose win over dated or prefixed ones of the same model.
        for (name, rates) in &table.models {
            let key = loose_name(name);
            if key == *name {
                loose.insert(key, rates.clone());
            }
        }
        for (name, rates) in &table.models {
            loose.entry(loose_name(name)).or_insert_with(|| rates.clone());
        }
        Prices { table, loose }
    }

    /// What `model` of a provider of `kind` costs by the lists.
    pub fn lookup(&self, kind: &str, model: &str) -> Option<&Rates> {
        let model = model.trim().to_lowercase();
        if model.is_empty() {
            return None;
        }
        if kind == "openrouter"
            && let Some(rates) = self.table.openrouter.get(&model)
        {
            return Some(rates);
        }
        if let Some(rates) = self.table.models.get(&model) {
            return Some(rates);
        }
        if let Some(prefix) = litellm_prefix(kind)
            && let Some(rates) = self.table.models.get(&format!("{prefix}{model}"))
        {
            return Some(rates);
        }
        self.loose.get(&loose_name(&model))
    }

    /// What `model` of `record` costs: the price set by hand, nothing for a model of one's own or a
    /// subscription, or the lists' price. A price set for one direction only takes the other from
    /// the lists (or 0 when they don't know it); with a price set by hand, thinking costs what the
    /// answer costs, the cache what the prompt costs, and the lists' tiers no longer apply. What the
    /// lists say about the model itself (whether it thinks, how long it may answer) stays.
    pub fn price(&self, record: &AssistProviderRecord, info: &KindInfo, model: &str) -> Option<Price> {
        let auto = self.lookup(&record.kind, model);
        if record.input_price.is_some() || record.output_price.is_some() || record.request_price.is_some() {
            let mut rates = auto.cloned().unwrap_or_default();
            if let Some(input) = record.input_price {
                rates.input = input / 1e6;
                rates.cache_read = None;
                rates.cache_write = None;
            }
            if let Some(output) = record.output_price {
                rates.output = output / 1e6;
                rates.reasoning = None;
            }
            if record.input_price.is_some() || record.output_price.is_some() {
                rates.tiers.clear();
            }
            if let Some(per_request) = record.request_price {
                rates.per_request = per_request;
            }
            return Some(Price::from_rates(&rates, PriceSource::Manual));
        }
        if matches!(info.kind, "ollama" | "chatgpt") {
            let mut price = Price::free();
            if let Some(rates) = auto {
                price.supports_reasoning = rates.reasoning_model;
                price.max_output_tokens = rates.max_output_tokens;
            }
            return Some(price);
        }
        Some(Price::from_rates(auto?, PriceSource::Auto))
    }

    /// `usd` in `currency` by the euro reference rates (rough ones before the first list came, see
    /// [`FALLBACK_RATES`]); `None` when there is no rate for it.
    pub fn convert(&self, usd: f64, currency: &str) -> Option<Cost> {
        let amount = if currency == "USD" {
            usd
        } else {
            let per_euro = |code: &str| match code {
                "EUR" => Some(1.0),
                _ if self.table.rates.is_empty() => {
                    FALLBACK_RATES.iter().find(|(known, _)| *known == code).map(|(_, rate)| *rate)
                }
                _ => self.table.rates.get(code).copied(),
            };
            let usd_rate = per_euro("USD").filter(|rate| *rate > 0.0)?;
            usd / usd_rate * per_euro(currency)?
        };
        Some(Cost { amount, currency: currency.to_owned(), usd })
    }
}

/// A price in a list: a number or a string of one, finite and not negative.
fn list_number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let number = value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())?;
    (number.is_finite() && number >= 0.0).then_some(number)
}

/// LiteLLM's `search_context_cost_per_query`: a number, or one per search context size (the
/// medium one is taken).
fn search_cost(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Object(sizes)) => list_number(sizes.get("search_context_size_medium"))
            .or_else(|| sizes.values().filter_map(|v| list_number(Some(v))).reduce(f64::max))
            .unwrap_or(0.0),
        other => list_number(other).unwrap_or(0.0),
    }
}

/// `_above_128k_tokens`, `_above_200000_tokens` at the end of a key: the threshold in tokens.
fn tier_threshold(key: &str, base: &str) -> Option<i64> {
    let rest = key.strip_prefix(base)?.strip_prefix("_above_")?.strip_suffix("_tokens")?;
    let (digits, factor) = match rest.strip_suffix(['k', 'K']) {
        Some(digits) => (digits, 1000),
        None => match rest.strip_suffix(['m', 'M']) {
            Some(digits) => (digits, 1_000_000),
            None => (rest, 1),
        },
    };
    let number: i64 = digits.parse().ok()?;
    (number > 0).then_some(number * factor)
}

/// One model of LiteLLM's list, when it has a price in and out.
pub fn litellm_rates(entry: &Value) -> Option<Rates> {
    let cost = |key: &str| list_number(entry.get(key));
    let positive = |key: &str| cost(key).filter(|v| *v > 0.0);
    let input = cost("input_cost_per_token")?;
    let output = cost("output_cost_per_token")?;
    let mut tiers: BTreeMap<i64, Tier> = BTreeMap::new();
    if let Some(object) = entry.as_object() {
        for (key, value) in object {
            let Some(value) = list_number(Some(value)) else { continue };
            for base in [
                "input_cost_per_token",
                "output_cost_per_token",
                "output_cost_per_reasoning_token",
                "cache_read_input_token_cost",
            ] {
                let Some(above) = tier_threshold(key, base) else { continue };
                let tier =
                    tiers.entry(above).or_insert(Tier { above, input, output, reasoning: None, cache_read: None });
                match base {
                    "input_cost_per_token" => tier.input = value,
                    "output_cost_per_token" => tier.output = value,
                    "output_cost_per_reasoning_token" => tier.reasoning = Some(value).filter(|v| *v > 0.0),
                    _ => tier.cache_read = Some(value),
                }
            }
        }
    }
    let max_output_tokens = entry
        .get("max_output_tokens")
        .or_else(|| entry.get("max_tokens"))
        .and_then(Value::as_i64)
        .filter(|tokens| *tokens > 0);
    Some(Rates {
        input,
        output,
        reasoning: positive("output_cost_per_reasoning_token"),
        cache_read: cost("cache_read_input_token_cost"),
        cache_write: cost("cache_creation_input_token_cost"),
        per_request: cost("input_cost_per_request").or_else(|| cost("input_cost_per_query")).unwrap_or(0.0),
        per_image: cost("input_cost_per_image").unwrap_or(0.0),
        web_search: search_cost(entry.get("search_context_cost_per_query")),
        tiers: tiers.into_values().collect(),
        reasoning_model: entry.get("supports_reasoning").and_then(Value::as_bool).unwrap_or(false),
        max_output_tokens,
    })
}

/// LiteLLM's list: `{ "gpt-4o": { "input_cost_per_token": 2.5e-6, "output_cost_per_token": 1e-5, … } }`.
pub fn parse_litellm(bytes: &[u8]) -> Option<BTreeMap<String, Rates>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut out = BTreeMap::new();
    for (name, entry) in value.as_object()? {
        let mode = entry.get("mode").and_then(Value::as_str).unwrap_or("chat");
        if !matches!(mode, "chat" | "completion" | "responses") {
            continue;
        }
        if let Some(rates) = litellm_rates(entry) {
            out.insert(name.to_lowercase(), rates);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// One model of OpenRouter's `/models`: `{ "id": "…", "pricing": { "prompt": "0.000001", "completion":
/// "…", "request": "…", "image": "…", "internal_reasoning": "…", "input_cache_read": "…",
/// "input_cache_write": "…", "web_search": "…" }, "supported_parameters": ["reasoning", …],
/// "top_provider": { "max_completion_tokens": … } }`.
pub fn openrouter_rates(entry: &Value) -> Option<Rates> {
    let pricing = entry.get("pricing")?;
    let cost = |key: &str| list_number(pricing.get(key));
    let reasoning_model = entry
        .get("supported_parameters")
        .and_then(Value::as_array)
        .is_some_and(|params| params.iter().any(|p| matches!(p.as_str(), Some("reasoning" | "include_reasoning"))));
    Some(Rates {
        input: cost("prompt")?,
        output: cost("completion")?,
        reasoning: cost("internal_reasoning").filter(|v| *v > 0.0),
        cache_read: cost("input_cache_read"),
        cache_write: cost("input_cache_write"),
        per_request: cost("request").unwrap_or(0.0),
        per_image: cost("image").unwrap_or(0.0),
        web_search: cost("web_search").unwrap_or(0.0),
        tiers: Vec::new(),
        reasoning_model,
        max_output_tokens: entry
            .pointer("/top_provider/max_completion_tokens")
            .and_then(Value::as_i64)
            .filter(|tokens| *tokens > 0),
    })
}

/// OpenRouter's `/models`: `{ "data": [ … ] }`, see [`openrouter_rates`].
pub fn parse_openrouter(bytes: &[u8]) -> Option<BTreeMap<String, Rates>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut out = BTreeMap::new();
    for entry in value.get("data")?.as_array()? {
        let Some(id) = entry.get("id").and_then(Value::as_str) else { continue };
        if let Some(rates) = openrouter_rates(entry) {
            out.insert(id.to_lowercase(), rates);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The ECB's daily rates: `<Cube time='2026-09-29'>` and `<Cube currency='USD' rate='1.0862'/>`.
pub fn parse_ecb(text: &str) -> Option<(BTreeMap<String, f64>, Option<String>)> {
    let attribute = |tag: &str, name: &str| -> Option<String> {
        let at = tag.find(&format!("{name}="))? + name.len() + 1;
        let quote = tag[at..].chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let rest = &tag[at + 1..];
        Some(rest[..rest.find(quote)?].to_owned())
    };
    let mut rates = BTreeMap::new();
    let mut day = None;
    for tag in text.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or_default();
        if !tag.starts_with("Cube") {
            continue;
        }
        if let Some(time) = attribute(tag, "time") {
            day = Some(time);
        }
        if let (Some(currency), Some(rate)) = (attribute(tag, "currency"), attribute(tag, "rate"))
            && is_currency(&currency)
            && let Ok(rate) = rate.trim().parse::<f64>()
            && rate.is_finite()
            && rate > 0.0
        {
            rates.insert(currency, rate);
        }
    }
    (!rates.is_empty()).then_some((rates, day))
}

impl Assist {
    /// The prices, loaded from the database the first time.
    pub async fn prices(&self) -> Arc<Prices> {
        if let Some(prices) = self.inner.prices.read().unwrap_or_else(|e| e.into_inner()).clone() {
            return prices;
        }
        let table = match self.store().assist_prices().await {
            Ok(Some(text)) => serde_json::from_str(&text).unwrap_or_default(),
            _ => PriceTable::default(),
        };
        let prices = Arc::new(Prices::new(table));
        let mut slot = self.inner.prices.write().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(prices).clone()
    }

    /// Takes `table` as the prices from now on, and keeps it.
    pub async fn set_prices(&self, table: PriceTable) -> Result<()> {
        let text = serde_json::to_string(&table).map_err(|err| uwumail_store::StoreError::Internal(err.to_string()))?;
        self.store().set_assist_prices(text).await?;
        *self.inner.prices.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Prices::new(table)));
        Ok(())
    }

    /// What `model` of `record` costs, as [`Prices::price`] says.
    pub(crate) async fn price_of(&self, record: &AssistProviderRecord, info: &KindInfo, model: &str) -> Option<Price> {
        self.prices().await.price(record, info, model)
    }

    async fn fetch_list(&self, url: &str, accept: &str) -> Result<Vec<u8>, String> {
        let reach = if self.inner.reach_anything { Reach::Any } else { Reach::Public };
        let client = self.inner.egress.assist_client(reach);
        let request = Request::get(url)
            .header(ACCEPT, accept)
            .header(USER_AGENT, "UwUMail")
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .map_err(|_| "not a usable address".to_owned())?;
        let work = async {
            let response = client.send(request).await.map_err(|err| err.to_string())?;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(format!("the answer was {status}"));
            }
            let body = http_body_util::Limited::new(response.into_body(), MAX_LIST_BYTES)
                .collect()
                .await
                .map_err(|_| "too big or broken off".to_owned())?;
            Ok(body.to_bytes().to_vec())
        };
        tokio::time::timeout(FETCH_TIMEOUT, work).await.map_err(|_| "no answer in time".to_owned())?
    }

    /// Fetches the lists anew. Each one that can't be fetched or read keeps its last good copy;
    /// OpenRouter's only while there is an OpenRouter provider. Answers whether all came.
    pub async fn refresh_prices(&self) -> Result<bool> {
        let sources = self.inner.price_sources.clone();
        let mut table = self.prices().await.table.clone();
        let mut complete = true;
        match self.fetch_list(&sources.litellm, "application/json").await.map(|b| parse_litellm(&b)) {
            Ok(Some(models)) => table.models = models,
            Ok(None) => {
                complete = false;
                tracing::warn!("LiteLLM's price list could not be read; the last one is kept");
            }
            Err(err) => {
                complete = false;
                tracing::warn!(%err, "LiteLLM's price list could not be fetched; the last one is kept");
            }
        }
        match self.fetch_list(&sources.ecb, "application/xml").await.map(|b| parse_ecb(&String::from_utf8_lossy(&b))) {
            Ok(Some((rates, day))) => {
                table.rates = rates;
                table.rates_day = day;
            }
            Ok(None) => {
                complete = false;
                tracing::warn!("the ECB's exchange rates could not be read; the last ones are kept");
            }
            Err(err) => {
                complete = false;
                tracing::warn!(%err, "the ECB's exchange rates could not be fetched; the last ones are kept");
            }
        }
        let uses_openrouter =
            self.store().assist_providers_of_kind("openrouter").await.map(|n| n > 0).map_err(AssistError::from)?;
        if uses_openrouter {
            match self.fetch_list(&sources.openrouter, "application/json").await.map(|b| parse_openrouter(&b)) {
                Ok(Some(models)) => table.openrouter = models,
                Ok(None) | Err(_) => {
                    complete = false;
                    tracing::warn!("OpenRouter's prices could not be fetched; the last ones are kept");
                }
            }
        }
        // Only a complete fetch counts as one: a server that could not reach the lists tries again
        // soon after a restart too, and the portal does not claim lists it never got.
        if complete {
            table.fetched_at = now();
        }
        self.set_prices(table).await?;
        Ok(complete)
    }

    /// Fetches the price lists once a day, sooner again after a failure, until `shutdown`.
    pub async fn run_price_updater(self, mut shutdown: watch::Receiver<bool>) {
        let mut next = self.prices().await.table.fetched_at + REFRESH_SECS;
        loop {
            if *shutdown.borrow() {
                return;
            }
            if now() >= next {
                next = match self.refresh_prices().await {
                    Ok(true) => now() + REFRESH_SECS,
                    Ok(false) => now() + RETRY_SECS,
                    Err(err) => {
                        tracing::warn!(%err, "keeping the AI price lists failed");
                        now() + RETRY_SECS
                    }
                };
            }
            let wait = Duration::from_secs(next.saturating_sub(now()).clamp(1, REFRESH_SECS) as u64);
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.changed() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_found_without_prefixes_and_dates() {
        assert_eq!(loose_name("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(loose_name("claude-3-5-sonnet-20241022"), "claude-3-5-sonnet");
        assert_eq!(loose_name("mistral/mistral-small-latest"), "mistral-small");
        assert_eq!(loose_name("vertex_ai/claude-3-opus@20240229"), "claude-3-opus");
        assert_eq!(loose_name("gpt-4-0613"), "gpt-4");
        assert_eq!(loose_name("models/Gemini-2.5-Flash"), "gemini-2.5-flash");
        assert_eq!(loose_name("gpt-5-mini"), "gpt-5-mini");
    }

    fn table() -> PriceTable {
        let litellm = br#"{
            "sample_spec": { "input_cost_per_token": 0, "output_cost_per_token": 0, "mode": "one of chat, embedding" },
            "gpt-5-mini": { "input_cost_per_token": 2.5e-7, "output_cost_per_token": 2e-6, "mode": "chat" },
            "gpt-5-mini-2025-08-07": { "input_cost_per_token": 9e-7, "output_cost_per_token": 9e-6, "mode": "chat" },
            "mistral/mistral-small-latest": { "input_cost_per_token": 1e-7, "output_cost_per_token": 3e-7 },
            "claude-haiku-4-5-20251001": { "input_cost_per_token": 1e-6, "output_cost_per_token": 5e-6 },
            "text-embedding-3-small": { "input_cost_per_token": 2e-8, "output_cost_per_token": 0, "mode": "embedding" }
        }"#;
        let openrouter = br#"{ "data": [
            { "id": "openai/gpt-5-mini", "pricing": { "prompt": "0.0000003", "completion": "0.0000025" } },
            { "id": "openrouter/auto", "pricing": { "prompt": "-1", "completion": "-1" } }
        ] }"#;
        let ecb = "<gesmes:Envelope><Cube><Cube time='2026-09-29'><Cube currency='USD' rate='1.25'/>\
                   <Cube currency=\"JPY\" rate=\"160\"/><Cube currency='CNY' rate='8'/></Cube></Cube></gesmes:Envelope>";
        let (rates, rates_day) = parse_ecb(ecb).unwrap();
        PriceTable {
            fetched_at: 1,
            models: parse_litellm(litellm).unwrap(),
            openrouter: parse_openrouter(openrouter).unwrap(),
            rates,
            rates_day,
        }
    }

    #[test]
    fn lists_are_read_and_looked_up_tolerantly() {
        let prices = Prices::new(table());
        assert!(!prices.table.models.contains_key("text-embedding-3-small"));
        assert_eq!(prices.table.openrouter.len(), 1, "a price of -1 is none");
        assert_eq!(prices.table.rates_day.as_deref(), Some("2026-09-29"));
        let found = |kind: &str, model: &str| prices.lookup(kind, model).map(|r| (r.input, r.output));
        assert_eq!(found("openai", "gpt-5-mini"), Some((2.5e-7, 2e-6)));
        assert_eq!(found("openai", "GPT-5-mini-2025-08-07"), Some((9e-7, 9e-6)), "an exact name first");
        assert_eq!(found("mistral", "mistral-small-latest"), Some((1e-7, 3e-7)), "the kind's prefix");
        assert_eq!(found("openaiCompatible", "mistral-small-2503"), Some((1e-7, 3e-7)));
        assert_eq!(found("anthropic", "claude-haiku-4-5"), Some((1e-6, 5e-6)), "without its date");
        assert_eq!(found("openrouter", "openai/gpt-5-mini"), Some((3e-7, 2.5e-6)), "OpenRouter's own");
        assert_eq!(found("openai", "gpt-5-mini-2025-08-07"), Some((9e-7, 9e-6)));
        assert_eq!(prices.lookup("openai", "unheard-of"), None);
    }

    #[test]
    fn costs_are_converted_by_the_euro_rates() {
        let prices = Prices::new(table());
        let euro = prices.convert(1.25, "EUR").unwrap();
        assert!((euro.amount - 1.0).abs() < 1e-9 && euro.usd == 1.25);
        assert!((prices.convert(1.25, "JPY").unwrap().amount - 160.0).abs() < 1e-9);
        assert!((prices.convert(2.5, "CNY").unwrap().amount - 16.0).abs() < 1e-9);
        assert_eq!(prices.convert(2.0, "USD").unwrap().amount, 2.0);
        assert_eq!(prices.convert(2.0, "XXX"), None);
        assert_eq!(Prices::default().convert(2.0, "USD").unwrap().amount, 2.0);
    }

    #[test]
    fn before_the_first_rates_came_rough_ones_stand_in() {
        let none = Prices::default();
        let euro = none.convert(1.17, "EUR").unwrap();
        assert!((euro.amount - 1.0).abs() < 1e-9 && euro.usd == 1.17, "{euro:?}");
        assert!(none.convert(1.17, "JPY").unwrap().amount > 100.0);
        assert!(none.convert(1.17, "CNY").unwrap().amount > 5.0);
        assert_eq!(none.convert(1.0, "GBP"), None, "only the currencies the apps show");
        let real = Prices::new(table());
        assert!((real.convert(1.25, "EUR").unwrap().amount - 1.0).abs() < 1e-9, "the real rates win");
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn litellm_entries_give_the_whole_price_sheet() {
        let list = br#"{
            "gemini/gemini-2.5-pro": {
                "input_cost_per_token": 1.25e-6, "output_cost_per_token": 1e-5,
                "input_cost_per_token_above_200k_tokens": 2.5e-6, "output_cost_per_token_above_200k_tokens": 1.5e-5,
                "cache_read_input_token_cost": 3.1e-7, "cache_read_input_token_cost_above_200k_tokens": 6.25e-7,
                "output_cost_per_reasoning_token": 1e-5, "input_cost_per_image": 0.00131,
                "search_context_cost_per_query": { "search_context_size_low": 0.035, "search_context_size_medium": 0.035, "search_context_size_high": 0.05 },
                "supports_reasoning": true, "max_output_tokens": 65535, "mode": "chat"
            },
            "o3": {
                "input_cost_per_token": 2e-6, "output_cost_per_token": 8e-6, "cache_read_input_token_cost": 5e-7,
                "supports_reasoning": true, "max_tokens": 100000, "mode": "responses"
            },
            "claude-sonnet-4-5": {
                "input_cost_per_token": 3e-6, "output_cost_per_token": 1.5e-5,
                "input_cost_per_token_above_200k_tokens": 6e-6, "output_cost_per_token_above_200k_tokens": 2.25e-5,
                "cache_creation_input_token_cost": 3.75e-6, "cache_read_input_token_cost": 3e-7,
                "input_cost_per_query": 0.0, "max_output_tokens": 64000
            },
            "perplexity/sonar": {
                "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6, "input_cost_per_request": 0.005,
                "search_context_cost_per_query": 0.008
            },
            "qwen-plus": { "input_cost_per_token": 4e-7, "output_cost_per_token": 1.2e-6,
                "input_cost_per_token_above_128k_tokens": 1.2e-6, "output_cost_per_token_above_128k_tokens": 3.6e-6 },
            "whisper-1": { "input_cost_per_second": 0.0001, "output_cost_per_second": 0.0001, "mode": "audio_transcription" }
        }"#;
        let models = parse_litellm(list).unwrap();
        assert_eq!(models.len(), 5, "only chat models with a price in and out");
        let pro = &models["gemini/gemini-2.5-pro"];
        assert_eq!((pro.cache_read, pro.cache_write), (Some(3.1e-7), None));
        assert_eq!((pro.per_image, pro.web_search, pro.reasoning), (0.00131, 0.035, Some(1e-5)));
        assert!(pro.reasoning_model);
        assert_eq!(pro.max_output_tokens, Some(65535));
        assert_eq!(
            pro.tiers,
            [Tier { above: 200_000, input: 2.5e-6, output: 1.5e-5, reasoning: None, cache_read: Some(6.25e-7) }]
        );
        let o3 = &models["o3"];
        assert_eq!((o3.reasoning_model, o3.max_output_tokens, o3.reasoning), (true, Some(100_000), None));
        let sonnet = &models["claude-sonnet-4-5"];
        assert_eq!((sonnet.cache_write, sonnet.per_request, sonnet.reasoning_model), (Some(3.75e-6), 0.0, false));
        let sonar = &models["perplexity/sonar"];
        assert_eq!((sonar.per_request, sonar.web_search), (0.005, 0.008));
        assert_eq!(models["qwen-plus"].tiers[0].above, 128_000);

        // The whole sheet in US dollars per million, thinking as the answer where the list names none.
        let price = Price::from_rates(o3, PriceSource::Auto);
        assert!(near(price.reasoning_per_million, 8.0) && near(price.cache_read_per_million, 0.5));
        assert!(near(price.cache_write_per_million, 2.0), "a cache write costs the prompt's price");
        assert!(price.supports_reasoning);
    }

    #[test]
    fn openrouter_entries_give_the_whole_price_sheet() {
        let list = br#"{ "data": [
            { "id": "anthropic/claude-sonnet-4.5", "pricing": { "prompt": "0.000003", "completion": "0.000015",
                "request": "0", "image": "0.0048", "web_search": "0.01", "internal_reasoning": "0",
                "input_cache_read": "0.0000003", "input_cache_write": "0.00000375" },
              "supported_parameters": ["max_tokens", "reasoning", "include_reasoning"],
              "top_provider": { "max_completion_tokens": 64000 } },
            { "id": "perplexity/sonar", "pricing": { "prompt": "0.000001", "completion": "0.000001", "request": "0.005" } },
            { "id": "deepseek/deepseek-r1", "pricing": { "prompt": 4e-7, "completion": 2e-6, "internal_reasoning": "0.000003" } },
            { "id": "openrouter/auto", "pricing": { "prompt": "-1", "completion": "-1" } }
        ] }"#;
        let models = parse_openrouter(list).unwrap();
        assert_eq!(models.len(), 3);
        let sonnet = &models["anthropic/claude-sonnet-4.5"];
        assert_eq!((sonnet.per_image, sonnet.web_search, sonnet.per_request), (0.0048, 0.01, 0.0));
        assert_eq!((sonnet.cache_read, sonnet.cache_write, sonnet.reasoning), (Some(3e-7), Some(3.75e-6), None));
        assert!(sonnet.reasoning_model);
        assert_eq!(sonnet.max_output_tokens, Some(64000));
        assert_eq!(models["perplexity/sonar"].per_request, 0.005);
        let r1 = &models["deepseek/deepseek-r1"];
        assert_eq!((r1.input, r1.reasoning, r1.reasoning_model), (4e-7, Some(3e-6), false));
    }

    #[test]
    fn a_table_kept_before_the_price_sheet_still_reads() {
        let old = r#"{ "fetchedAt": 5, "models": { "gpt-5-mini": [2.5e-7, 2e-6] },
                       "openrouter": { "openai/gpt-5-mini": [3e-7, 2.5e-6] }, "rates": { "USD": 1.2 } }"#;
        let table: PriceTable = serde_json::from_str(old).unwrap();
        assert_eq!(table.models["gpt-5-mini"], Rates::plain(2.5e-7, 2e-6));
        assert_eq!(table.openrouter["openai/gpt-5-mini"].output, 2.5e-6);
        let again: PriceTable = serde_json::from_str(&serde_json::to_string(&table).unwrap()).unwrap();
        assert_eq!(again, table, "and is kept in the new form");
    }

    #[test]
    fn large_prompts_cost_the_tier_and_everything_is_counted() {
        let rates = Rates {
            input: 1e-6,
            output: 4e-6,
            reasoning: Some(8e-6),
            cache_read: Some(1e-7),
            cache_write: Some(2e-6),
            per_request: 0.001,
            per_image: 0.002,
            web_search: 0.01,
            tiers: vec![Tier { above: 128_000, input: 2e-6, output: 8e-6, reasoning: None, cache_read: None }],
            reasoning_model: true,
            max_output_tokens: None,
        };
        let price = Price::from_rates(&rates, PriceSource::Auto);
        let small = Metered {
            input: 1000.0,
            cache_read: 200.0,
            cache_write: 100.0,
            output: 100.0,
            reasoning: 50.0,
            images: 2.0,
            requests: 1.0,
            web_searches: 1.0,
            prompt: 1000,
        };
        let parts = price.cost_of(&small);
        assert!(near(parts.input, (700.0 * 1.0 + 200.0 * 0.1 + 100.0 * 2.0) / 1e6), "{parts:?}");
        assert!(near(parts.output, 100.0 * 4.0 / 1e6) && near(parts.reasoning, 50.0 * 8.0 / 1e6));
        assert!(near(parts.images, 0.004) && near(parts.requests, 0.001) && near(parts.other, 0.01));
        assert!(near(parts.total(), parts.input + parts.output + parts.reasoning + 0.015));

        let large = Metered { input: 130_000.0, output: 100.0, prompt: 130_000, ..Metered::default() };
        let parts = price.cost_of(&large);
        assert!(near(parts.input, 130_000.0 * 2.0 / 1e6) && near(parts.output, 100.0 * 8.0 / 1e6), "the tier");
        assert_eq!(price.level(128_000).0, 1.0, "at the threshold still the base price");
        assert!(near(price.level(128_001).2, 8.0), "thinking in a tier without its own price: the tier's answer");
    }

    fn record(kind: &str) -> AssistProviderRecord {
        AssistProviderRecord {
            id: 1,
            account_id: None,
            name: "P".into(),
            kind: kind.into(),
            base_url: None,
            has_secret: true,
            key_hint: None,
            model: None,
            fast_model: None,
            enabled: true,
            access: "everyone".into(),
            access_list: Vec::new(),
            features: Vec::new(),
            requests_per_day: None,
            tokens_per_day: None,
            input_price: None,
            output_price: None,
            request_price: None,
            show_cost: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn a_price_set_by_hand_keeps_the_lists_facts_but_not_their_prices() {
        let mut table = table();
        let mut rates = Rates::plain(1e-6, 4e-6);
        rates.reasoning = Some(9e-6);
        rates.per_request = 0.002;
        rates.reasoning_model = true;
        rates.tiers = vec![Tier { above: 1000, input: 5e-6, output: 5e-6, reasoning: None, cache_read: None }];
        table.models.insert("thinker".into(), rates);
        let prices = Prices::new(table);
        let openai = crate::kinds::kind("openai").unwrap();
        let mut manual = record("openai");
        manual.output_price = Some(3.0);
        let price = prices.price(&manual, openai, "thinker").unwrap();
        assert_eq!(price.source, PriceSource::Manual);
        assert!(near(price.input_per_million, 1.0) && near(price.output_per_million, 3.0));
        assert!(near(price.reasoning_per_million, 3.0), "thinking as the answer set by hand");
        assert!(price.tiers.is_empty() && price.supports_reasoning && near(price.per_request, 0.002));
        manual.request_price = Some(0.01);
        assert!(near(prices.price(&manual, openai, "thinker").unwrap().per_request, 0.01));
        let only_fee = AssistProviderRecord { request_price: Some(0.5), ..record("openai") };
        let price = prices.price(&only_fee, openai, "thinker").unwrap();
        assert_eq!((price.source, price.tiers.len()), (PriceSource::Manual, 1), "token prices from the lists");
        let ollama = prices.price(&record("ollama"), crate::kinds::kind("ollama").unwrap(), "thinker").unwrap();
        assert_eq!(
            (ollama.source, ollama.output_per_million, ollama.supports_reasoning),
            (PriceSource::Free, 0.0, true)
        );
    }
}
