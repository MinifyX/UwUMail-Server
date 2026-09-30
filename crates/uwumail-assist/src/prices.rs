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

/// The lists as they are kept: US dollars per token, in and out, by model; euro reference rates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceTable {
    /// When the last fetch was tried (Unix time), whether or not every list came.
    #[serde(default)]
    pub fetched_at: i64,
    /// LiteLLM's models, by their name there in lower case.
    #[serde(default)]
    pub models: BTreeMap<String, (f64, f64)>,
    /// OpenRouter's, by its model id in lower case.
    #[serde(default)]
    pub openrouter: BTreeMap<String, (f64, f64)>,
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

/// What a model costs, in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub source: PriceSource,
}

impl Price {
    /// US dollars for so many tokens.
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
    loose: HashMap<String, (f64, f64)>,
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
        let mut loose: HashMap<String, (f64, f64)> = HashMap::new();
        // Names that are already loose win over dated or prefixed ones of the same model.
        for (name, price) in &table.models {
            let key = loose_name(name);
            if key == *name {
                loose.insert(key, *price);
            }
        }
        for (name, price) in &table.models {
            loose.entry(loose_name(name)).or_insert(*price);
        }
        Prices { table, loose }
    }

    /// US dollars per token, in and out, of `model` of a provider of `kind`, from the lists.
    pub fn lookup(&self, kind: &str, model: &str) -> Option<(f64, f64)> {
        let model = model.trim().to_lowercase();
        if model.is_empty() {
            return None;
        }
        if kind == "openrouter"
            && let Some(price) = self.table.openrouter.get(&model)
        {
            return Some(*price);
        }
        if let Some(price) = self.table.models.get(&model) {
            return Some(*price);
        }
        if let Some(prefix) = litellm_prefix(kind)
            && let Some(price) = self.table.models.get(&format!("{prefix}{model}"))
        {
            return Some(*price);
        }
        self.loose.get(&loose_name(&model)).copied()
    }

    /// What `model` of `record` costs: the price set by hand, nothing for a model of one's own or a
    /// subscription, or the lists' price. A price set for one direction only takes the other from
    /// the lists (or 0 when they don't know it).
    pub fn price(&self, record: &AssistProviderRecord, info: &KindInfo, model: &str) -> Option<Price> {
        let auto = self.lookup(&record.kind, model).map(|(input, output)| (input * 1e6, output * 1e6));
        if record.input_price.is_some() || record.output_price.is_some() {
            let (auto_in, auto_out) = auto.unwrap_or((0.0, 0.0));
            return Some(Price {
                input_per_million: record.input_price.unwrap_or(auto_in),
                output_per_million: record.output_price.unwrap_or(auto_out),
                source: PriceSource::Manual,
            });
        }
        if matches!(info.kind, "ollama" | "chatgpt") {
            return Some(Price { input_per_million: 0.0, output_per_million: 0.0, source: PriceSource::Free });
        }
        let (input_per_million, output_per_million) = auto?;
        Some(Price { input_per_million, output_per_million, source: PriceSource::Auto })
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

/// LiteLLM's list: `{ "gpt-4o": { "input_cost_per_token": 2.5e-6, "output_cost_per_token": 1e-5 } }`.
pub fn parse_litellm(bytes: &[u8]) -> Option<BTreeMap<String, (f64, f64)>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut out = BTreeMap::new();
    for (name, entry) in value.as_object()? {
        let cost = |key: &str| entry.get(key).and_then(Value::as_f64).filter(|v| v.is_finite() && *v >= 0.0);
        if let (Some(input), Some(output)) = (cost("input_cost_per_token"), cost("output_cost_per_token")) {
            let mode = entry.get("mode").and_then(Value::as_str).unwrap_or("chat");
            if matches!(mode, "chat" | "completion" | "responses") {
                out.insert(name.to_lowercase(), (input, output));
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// OpenRouter's `/models`: `{ "data": [{ "id": "…", "pricing": { "prompt": "0.000001", "completion": "…" } }] }`.
pub fn parse_openrouter(bytes: &[u8]) -> Option<BTreeMap<String, (f64, f64)>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let number = |value: Option<&Value>| -> Option<f64> {
        let value = value?;
        let number = value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())?;
        (number.is_finite() && number >= 0.0).then_some(number)
    };
    let mut out = BTreeMap::new();
    for entry in value.get("data")?.as_array()? {
        let Some(id) = entry.get("id").and_then(Value::as_str) else { continue };
        let pricing = entry.get("pricing");
        let input = number(pricing.and_then(|p| p.get("prompt")));
        let output = number(pricing.and_then(|p| p.get("completion")));
        if let (Some(input), Some(output)) = (input, output) {
            out.insert(id.to_lowercase(), (input, output));
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
        assert_eq!(prices.lookup("openai", "gpt-5-mini"), Some((2.5e-7, 2e-6)));
        assert_eq!(prices.lookup("openai", "GPT-5-mini-2025-08-07"), Some((9e-7, 9e-6)), "an exact name first");
        assert_eq!(prices.lookup("mistral", "mistral-small-latest"), Some((1e-7, 3e-7)), "the kind's prefix");
        assert_eq!(prices.lookup("openaiCompatible", "mistral-small-2503"), Some((1e-7, 3e-7)));
        assert_eq!(prices.lookup("anthropic", "claude-haiku-4-5"), Some((1e-6, 5e-6)), "without its date");
        assert_eq!(prices.lookup("openrouter", "openai/gpt-5-mini"), Some((3e-7, 2.5e-6)), "OpenRouter's own");
        assert_eq!(prices.lookup("openai", "gpt-5-mini-2025-08-07"), Some((9e-7, 9e-6)));
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
}
