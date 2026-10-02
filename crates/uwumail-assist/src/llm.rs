//! Asking a model: the three API shapes, streaming or not, with limits on time and size, and errors
//! turned into something a person can read.
//!
//! Nothing here follows a redirect, and nothing of the answer is trusted beyond being text: the
//! callers check JSON answers against what they asked for.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER, USER_AGENT};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uwumail_smtp::egress::{AssistClient, EgressError};

use crate::kinds::{ChatFlavor, Shape};

/// No answer at all (no status line, or no bytes of the body) for this long ends the request.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// A whole request, however lively, ends after this.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(180);
/// The most an answer may be.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// The most text an answer may bring.
const MAX_TEXT_CHARS: usize = 60_000;
/// The longest line of an event stream: far more than any event of an answer needs.
pub const MAX_LINE_BYTES: usize = 256 * 1024;
const AGENT: &str = "UwUMail";

/// What to ask.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub system: String,
    pub user: String,
    /// JSON output of this shape, with a name for APIs that want one.
    pub schema: Option<(&'static str, Value)>,
    pub max_tokens: u32,
}

/// Where and how to ask.
#[derive(Clone)]
pub struct Target {
    pub shape: Shape,
    pub flavor: ChatFlavor,
    /// The API's base, like `https://api.openai.com/v1`.
    pub base_url: String,
    pub key: Option<String>,
    pub model: String,
    /// The ChatGPT account, for the Codex backend.
    pub account_id: Option<String>,
    pub client: AssistClient,
    /// Counts the characters of the answer as they arrive, for a request that ends early.
    pub received: Arc<AtomicUsize>,
}

/// A finished answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completion {
    pub text: String,
    /// The whole prompt, the part read from the provider's cache included.
    pub input_tokens: i64,
    /// The answer's text, without thinking.
    pub output_tokens: i64,
    /// What the model spent thinking, on top of `output_tokens` (where the provider tells it apart;
    /// Anthropic counts thinking in `output_tokens`).
    pub reasoning_tokens: i64,
    /// Of `input_tokens`, read from the provider's cache, and written to it.
    pub cached_tokens: i64,
    pub cache_write_tokens: i64,
    /// What the provider says the request cost in US dollars (OpenRouter's `usage.cost`).
    pub cost_usd: Option<f64>,
    /// Requests made to the provider: 2 when it refused the answer's JSON shape and was asked again.
    pub calls: i64,
    /// The provider said nothing about tokens; they are estimated.
    pub estimated: bool,
}

/// Tokens as a provider reported them, in the terms of [`Completion`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Reported {
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cached: i64,
    pub cache_write: i64,
    pub cost_usd: Option<f64>,
}

impl Reported {
    fn into_completion(self, text: String) -> Completion {
        Completion {
            text,
            input_tokens: self.input,
            output_tokens: self.output,
            reasoning_tokens: self.reasoning,
            cached_tokens: self.cached,
            cache_write_tokens: self.cache_write,
            cost_usd: self.cost_usd,
            calls: 1,
            estimated: false,
        }
    }
}

/// Tokens of one kind a single request can report at most. The numbers come from the provider;
/// the largest context windows are a few million tokens, and a garbled or hostile count must not
/// overflow the sums built from it.
const MAX_REPORTED_TOKENS: i64 = 10_000_000;
/// US dollars a single request can report at most, for the same reason.
const MAX_REPORTED_COST_USD: f64 = 1000.0;

fn int(value: &Value, pointer: &str) -> i64 {
    value.pointer(pointer).and_then(Value::as_i64).unwrap_or(0).clamp(0, MAX_REPORTED_TOKENS)
}

/// The `usage` of Chat Completions, in its variants: OpenAI counts thinking inside
/// `completion_tokens` (`completion_tokens_details.reasoning_tokens`); Gemini's OpenAI-compatible
/// API leaves it out of `completion_tokens` but in `total_tokens`; Gemini's own `usageMetadata`
/// names it `thoughtsTokenCount`. Cached prompt tokens are `prompt_tokens_details.cached_tokens`
/// (DeepSeek: `prompt_cache_hit_tokens`), OpenRouter adds what it charged as `cost`.
pub fn chat_usage(usage: &Value) -> Reported {
    if usage.get("promptTokenCount").is_some() || usage.get("candidatesTokenCount").is_some() {
        return Reported {
            input: int(usage, "/promptTokenCount"),
            output: int(usage, "/candidatesTokenCount"),
            reasoning: int(usage, "/thoughtsTokenCount"),
            cached: int(usage, "/cachedContentTokenCount"),
            ..Reported::default()
        };
    }
    let prompt = int(usage, "/prompt_tokens");
    let completion = int(usage, "/completion_tokens");
    let total = int(usage, "/total_tokens");
    let inside = int(usage, "/completion_tokens_details/reasoning_tokens");
    let outside = if total > 0 { total - prompt - completion } else { 0 };
    let (output, reasoning) =
        if outside > 0 { (completion, outside) } else { (completion - inside.min(completion), inside.min(completion)) };
    let cached = match int(usage, "/prompt_tokens_details/cached_tokens") {
        0 => int(usage, "/prompt_cache_hit_tokens"),
        cached => cached,
    };
    let cost_usd = usage
        .get("cost")
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .map(|cost| cost.min(MAX_REPORTED_COST_USD));
    Reported {
        input: prompt,
        output,
        reasoning,
        cached: cached.min(prompt),
        cache_write: int(usage, "/prompt_tokens_details/cache_write_tokens"),
        cost_usd,
    }
}

/// The `usage` of Anthropic's Messages: `input_tokens` leaves out what was read from and written
/// to the cache; thinking is part of `output_tokens`.
pub fn anthropic_usage(usage: &Value) -> Reported {
    let cached = int(usage, "/cache_read_input_tokens");
    let cache_write = int(usage, "/cache_creation_input_tokens");
    Reported {
        input: int(usage, "/input_tokens") + cached + cache_write,
        output: int(usage, "/output_tokens"),
        cached,
        cache_write,
        ..Reported::default()
    }
}

/// The `usage` of the Responses API (ChatGPT's Codex backend): thinking is inside `output_tokens`.
pub fn responses_usage(usage: &Value) -> Reported {
    let output = int(usage, "/output_tokens");
    let reasoning = int(usage, "/output_tokens_details/reasoning_tokens").min(output);
    let input = int(usage, "/input_tokens");
    Reported {
        input,
        output: output - reasoning,
        reasoning,
        cached: int(usage, "/input_tokens_details/cached_tokens").min(input),
        ..Reported::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("the provider refused the key")]
    Unauthorized,
    #[error("the provider is busy or the key's limit is reached (HTTP 429)")]
    RateLimited { retry_after: Option<u64> },
    #[error("the provider answered HTTP {status}{}", .detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default())]
    Status { status: u16, detail: Option<String> },
    #[error("the provider could not be reached")]
    Unreachable,
    #[error("{0}")]
    NotAllowed(String),
    #[error("the provider did not answer in time")]
    Timeout,
    #[error("the provider's answer was too large")]
    TooLarge,
    #[error("the provider's answer could not be read: {0}")]
    Garbled(String),
    #[error("the model declined to answer")]
    Refused,
    #[error("the model's answer was cut off")]
    CutOff,
}

impl ProviderError {
    /// Worth trying again a little later.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Unreachable
                | ProviderError::Timeout
                | ProviderError::Status { status: 500..=599, .. }
        )
    }
}

/// Tokens of `chars` characters, roughly.
pub(crate) fn estimate(chars: usize) -> i64 {
    (chars as i64 + 3) / 4
}

/// Chinese, Japanese and Korean script: about a token per character, where other text takes about
/// four characters to a token.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF         // Hangul Jamo
        | 0x2E80..=0x2FDF       // CJK radicals
        | 0x3040..=0x30FF       // Hiragana, Katakana
        | 0x3100..=0x312F       // Bopomofo
        | 0x3130..=0x318F       // Hangul compatibility Jamo
        | 0x31F0..=0x31FF       // Katakana extensions
        | 0x3400..=0x4DBF       // CJK extension A
        | 0x4E00..=0x9FFF       // CJK unified ideographs
        | 0xAC00..=0xD7AF       // Hangul syllables
        | 0xF900..=0xFAFF       // CJK compatibility ideographs
        | 0xFF66..=0xFF9F       // half-width Katakana
        | 0x20000..=0x3134F // CJK extensions B to G
    )
}

/// Tokens of some texts together, roughly: four characters to a token, a token for each character
/// of Chinese, Japanese or Korean. The same count for what is charged before a request is made and
/// for `Assist/estimate`.
pub fn estimate_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> i64 {
    let (mut wide, mut other) = (0usize, 0usize);
    for text in texts {
        for c in text.chars() {
            if is_wide(c) { wide += 1 } else { other += 1 }
        }
    }
    wide as i64 + estimate(other)
}

/// Tokens of a prompt, roughly: its instructions, the text and the answer's JSON shape, which goes
/// along as well.
pub fn estimate_prompt(prompt: &Prompt) -> i64 {
    let schema = prompt.schema.as_ref().map(|(_, schema)| schema.to_string()).unwrap_or_default();
    estimate_texts([prompt.system.as_str(), prompt.user.as_str(), schema.as_str()])
}

/// Tokens an API adds to a prompt beyond its text: the roles and markers around each message and
/// the answer's JSON shape wrapped for the model. Small, but part of every request.
pub fn framing_tokens(shape: Shape, has_schema: bool) -> i64 {
    let messages = match shape {
        // Three a message (system, user) and three to start the answer.
        Shape::Chat => 9,
        Shape::Anthropic => 8,
        Shape::Codex => 12,
    };
    messages + if has_schema { 12 } else { 0 }
}

/// Tokens of a whole request to an API of `shape`: [`estimate_prompt`] and [`framing_tokens`].
pub fn estimate_request(prompt: &Prompt, shape: Shape) -> i64 {
    estimate_prompt(prompt) + framing_tokens(shape, prompt.schema.is_some())
}

/// Asks `target`. With `deltas`, the text is streamed and sent there piece by piece as it comes; a
/// receiver that went away ends the request. A JSON schema the provider does not understand (an
/// HTTP 400) is tried once more without, relying on the prompt.
pub async fn complete(
    target: &Target,
    prompt: &Prompt,
    deltas: Option<&mpsc::Sender<String>>,
) -> Result<Completion, ProviderError> {
    let work = async {
        match ask(target, prompt, deltas, true).await {
            Err(ProviderError::Status { status: 400, .. }) if prompt.schema.is_some() => {
                ask(target, prompt, deltas, false).await.map(|completion| Completion { calls: 2, ..completion })
            }
            other => other,
        }
    };
    let mut completion = tokio::time::timeout(TOTAL_TIMEOUT, work).await.map_err(|_| ProviderError::Timeout)??;
    if completion.input_tokens == 0 && completion.output_tokens == 0 && completion.reasoning_tokens == 0 {
        completion.input_tokens = estimate_request(prompt, target.shape);
        completion.output_tokens = estimate_texts([completion.text.as_str()]);
        completion.estimated = true;
    }
    Ok(completion)
}

async fn ask(
    target: &Target,
    prompt: &Prompt,
    deltas: Option<&mpsc::Sender<String>>,
    with_schema: bool,
) -> Result<Completion, ProviderError> {
    // The Codex backend only answers streams.
    let stream = deltas.is_some() || target.shape == Shape::Codex;
    let (url, body) = request_body(target, prompt, stream, with_schema);
    let mut request = Request::post(url.as_str())
        .header(CONTENT_TYPE, "application/json")
        .header(USER_AGENT, AGENT)
        .header(ACCEPT, if stream { "text/event-stream" } else { "application/json" });
    for (name, value) in auth_headers(target) {
        request = request.header(name, value);
    }
    let request = request
        .body(Full::new(Bytes::from(ordered_json(&body))))
        .map_err(|_| ProviderError::NotAllowed("the provider's address is not usable".into()))?;
    let response = tokio::time::timeout(IDLE_TIMEOUT, target.client.send(request))
        .await
        .map_err(|_| ProviderError::Timeout)?
        .map_err(egress_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = read_all(response.into_body(), 64 * 1024).await.unwrap_or_default();
        return Err(status_error(status, retry_after, &body));
    }
    let body = response.into_body();
    if stream {
        let mut reader = SseReader::new(body);
        let collector = Collector { text: String::new(), deltas, received: &target.received };
        match target.shape {
            Shape::Chat => stream_chat(&mut reader, collector).await,
            Shape::Anthropic => stream_anthropic(&mut reader, collector).await,
            Shape::Codex => stream_codex(&mut reader, collector).await,
        }
    } else {
        let body = read_all(body, MAX_RESPONSE_BYTES).await?;
        target.received.fetch_add(body.len(), Ordering::Relaxed);
        let value: Value = serde_json::from_slice(&body).map_err(|_| ProviderError::Garbled("not JSON".into()))?;
        match target.shape {
            Shape::Chat => parse_chat(&value),
            Shape::Anthropic => parse_anthropic(&value),
            Shape::Codex => Err(ProviderError::Garbled("expected a stream".into())),
        }
    }
}

/// Texts sent in one embeddings request, at most.
pub const MAX_EMBED_TEXTS: usize = 16;

/// The embeddings of `texts` from an OpenAI-compatible `/embeddings` endpoint (OpenAI, Ollama,
/// llama.cpp …), in their order, and the tokens the provider counted (0 when it does not say).
pub async fn embed(target: &Target, texts: &[String]) -> Result<(Vec<Vec<f32>>, i64), ProviderError> {
    if texts.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let texts = &texts[..texts.len().min(MAX_EMBED_TEXTS)];
    let url = format!("{}/embeddings", target.base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "model": target.model, "input": texts });
    let mut request = Request::post(url.as_str())
        .header(CONTENT_TYPE, "application/json")
        .header(USER_AGENT, AGENT)
        .header(ACCEPT, "application/json");
    if let Some(key) = target.key.as_deref().filter(|key| !key.is_empty()) {
        request = request.header(AUTHORIZATION, format!("Bearer {key}"));
    }
    let request = request
        .body(Full::new(Bytes::from(body.to_string())))
        .map_err(|_| ProviderError::NotAllowed("the provider's address is not usable".into()))?;
    let work = async {
        let response = tokio::time::timeout(IDLE_TIMEOUT, target.client.send(request))
            .await
            .map_err(|_| ProviderError::Timeout)?
            .map_err(egress_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            let body = read_all(response.into_body(), 64 * 1024).await.unwrap_or_default();
            return Err(status_error(status, retry_after, &body));
        }
        // 16 texts of 4,096 numbers as JSON fit well into this.
        let body = read_all(response.into_body(), MAX_RESPONSE_BYTES * 8).await?;
        let value: Value = serde_json::from_slice(&body).map_err(|_| ProviderError::Garbled("not JSON".into()))?;
        parse_embeddings(&value, texts.len())
    };
    tokio::time::timeout(TOTAL_TIMEOUT, work).await.map_err(|_| ProviderError::Timeout)?
}

/// `{"data": [{"embedding": [...], "index": 0}], "usage": {"prompt_tokens": 9}}`.
pub fn parse_embeddings(value: &Value, expected: usize) -> Result<(Vec<Vec<f32>>, i64), ProviderError> {
    let garbled = |why: &str| ProviderError::Garbled(why.into());
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| garbled("no data"))?;
    if data.len() != expected {
        return Err(garbled("not one embedding per text"));
    }
    let mut out: Vec<Option<Vec<f32>>> = vec![None; expected];
    for (position, item) in data.iter().enumerate() {
        let index = item.get("index").and_then(Value::as_u64).map_or(position, |i| i as usize);
        let numbers = item.get("embedding").and_then(Value::as_array).ok_or_else(|| garbled("no embedding"))?;
        if numbers.is_empty() || numbers.len() > uwumail_labels::similar::MAX_DIMENSIONS {
            return Err(garbled("an embedding of an unusable size"));
        }
        let vector: Vec<f32> = numbers.iter().map(|n| n.as_f64().unwrap_or(f64::NAN) as f32).collect();
        if vector.iter().any(|x| !x.is_finite()) {
            return Err(garbled("an embedding with something else than numbers"));
        }
        let slot = out.get_mut(index).ok_or_else(|| garbled("an embedding for no text"))?;
        *slot = Some(vector);
    }
    let vectors: Option<Vec<Vec<f32>>> = out.into_iter().collect();
    let vectors = vectors.ok_or_else(|| garbled("an embedding is missing"))?;
    let tokens = value
        .get("usage")
        .and_then(|usage| usage.get("prompt_tokens").or_else(|| usage.get("total_tokens")))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0);
    Ok((vectors, tokens))
}

fn auth_headers(target: &Target) -> Vec<(&'static str, String)> {
    let mut headers = Vec::new();
    match target.shape {
        Shape::Chat => {
            if let Some(key) = target.key.as_deref().filter(|key| !key.is_empty()) {
                headers.push((AUTHORIZATION.as_str(), format!("Bearer {key}")));
            }
        }
        Shape::Anthropic => {
            if let Some(key) = &target.key {
                headers.push(("x-api-key", key.clone()));
            }
            headers.push(("anthropic-version", "2023-06-01".into()));
        }
        Shape::Codex => {
            if let Some(token) = &target.key {
                headers.push((AUTHORIZATION.as_str(), format!("Bearer {token}")));
            }
            if let Some(account) = &target.account_id {
                headers.push(("chatgpt-account-id", account.clone()));
            }
            headers.push(("originator", "codex_cli_rs".into()));
        }
    }
    headers
}

/// The request as JSON text, with every schema's `properties` in the order of its `required` list.
///
/// A provider that holds the model to a schema (OpenAI, llama.cpp, Ollama …) makes it write the
/// keys in the order the schema lists them, and `serde_json` keeps an object's keys sorted. So
/// `{"fits", "name", "reason"}` would make the model decide before it gives its reason, and propose
/// `newLabels` before it judged the labels. The schemas list `required` in the order meant.
fn ordered_json(value: &Value) -> String {
    let mut out = String::new();
    write_ordered(value, &mut out);
    out
}

fn write_ordered(value: &Value, out: &mut String) {
    match value {
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_ordered(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let order: Vec<&str> = match (map.get("properties"), map.get("required")) {
                (Some(Value::Object(_)), Some(Value::Array(required))) => {
                    required.iter().filter_map(Value::as_str).collect()
                }
                _ => Vec::new(),
            };
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                match item {
                    Value::Object(properties) if key == "properties" && !order.is_empty() => {
                        let mut keys: Vec<&String> = properties.keys().collect();
                        keys.sort_by_key(|name| order.iter().position(|first| first == name).unwrap_or(usize::MAX));
                        out.push('{');
                        for (index, name) in keys.into_iter().enumerate() {
                            if index > 0 {
                                out.push(',');
                            }
                            out.push_str(&Value::String(name.clone()).to_string());
                            out.push(':');
                            write_ordered(&properties[name.as_str()], out);
                        }
                        out.push('}');
                    }
                    _ => write_ordered(item, out),
                }
            }
            out.push('}');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

fn request_body(target: &Target, prompt: &Prompt, stream: bool, with_schema: bool) -> (String, Value) {
    let base = target.base_url.trim_end_matches('/');
    let schema = prompt.schema.as_ref().filter(|_| with_schema);
    match target.shape {
        Shape::Chat => {
            let mut body = json!({
                "model": target.model,
                "messages": [
                    { "role": "system", "content": prompt.system },
                    { "role": "user", "content": prompt.user }
                ],
                "stream": stream,
            });
            let tokens = if target.flavor.max_completion_tokens { "max_completion_tokens" } else { "max_tokens" };
            body[tokens] = json!(prompt.max_tokens);
            if stream && target.flavor.stream_usage {
                body["stream_options"] = json!({ "include_usage": true });
            }
            if let Some((name, schema)) = schema {
                body["response_format"] =
                    json!({ "type": "json_schema", "json_schema": { "name": name, "strict": true, "schema": schema } });
            }
            (format!("{base}/chat/completions"), body)
        }
        Shape::Anthropic => {
            let mut body = json!({
                "model": target.model,
                "max_tokens": prompt.max_tokens,
                "system": prompt.system,
                "messages": [{ "role": "user", "content": prompt.user }],
                "stream": stream,
            });
            if let Some((_, schema)) = schema {
                body["output_config"] = json!({ "format": { "type": "json_schema", "schema": schema } });
            }
            (format!("{base}/messages"), body)
        }
        Shape::Codex => {
            let mut body = json!({
                "model": target.model,
                "instructions": prompt.system,
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": prompt.user }]
                }],
                "store": false,
                "stream": true,
                "parallel_tool_calls": false,
                "include": [],
            });
            if let Some((name, schema)) = schema {
                body["text"] =
                    json!({ "format": { "type": "json_schema", "name": name, "strict": true, "schema": schema } });
            }
            (format!("{base}/responses"), body)
        }
    }
}

impl ProviderError {
    pub(crate) fn from_egress(err: EgressError) -> ProviderError {
        egress_error(err)
    }
}

fn egress_error(err: EgressError) -> ProviderError {
    match err {
        EgressError::NotAllowed(_) => {
            ProviderError::NotAllowed("the provider's address is not one this server may connect to".into())
        }
        EgressError::Timeout => ProviderError::Timeout,
        EgressError::TooLarge => ProviderError::TooLarge,
        _ => ProviderError::Unreachable,
    }
}

/// The provider's own words about an error, shortened: `{"error": {"message": …}}` in its variants.
fn error_detail(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let value = match &value {
        Value::Array(items) => items.first()?.clone(),
        _ => value,
    };
    let message = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error").filter(|error| error.is_string()))
        .or_else(|| value.get("detail"))?
        .as_str()?;
    Some(shorten(message, 300))
}

/// At most `max` characters of `text`, on one line.
pub fn shorten(text: &str, max: usize) -> String {
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.trim();
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat.to_owned(),
    }
}

pub(crate) fn status_error(status: u16, retry_after: Option<u64>, body: &[u8]) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Unauthorized,
        429 => ProviderError::RateLimited { retry_after },
        _ => ProviderError::Status { status, detail: error_detail(body) },
    }
}

async fn read_all(body: hyper::body::Incoming, max: usize) -> Result<Vec<u8>, ProviderError> {
    let mut body = body;
    let mut out = Vec::new();
    loop {
        let frame = tokio::time::timeout(IDLE_TIMEOUT, body.frame()).await.map_err(|_| ProviderError::Timeout)?;
        let Some(frame) = frame else { return Ok(out) };
        let frame = frame.map_err(|_| ProviderError::Unreachable)?;
        if let Ok(data) = frame.into_data() {
            if out.len() + data.len() > max {
                return Err(ProviderError::TooLarge);
            }
            out.extend_from_slice(&data);
        }
    }
}

fn parse_chat(value: &Value) -> Result<Completion, ProviderError> {
    let choice = value.pointer("/choices/0").ok_or_else(|| ProviderError::Garbled("no choices".into()))?;
    let text = choice.pointer("/message/content").and_then(Value::as_str).unwrap_or_default();
    if choice.get("finish_reason").and_then(Value::as_str) == Some("content_filter") {
        return Err(ProviderError::Refused);
    }
    if text.is_empty() && choice.pointer("/message/refusal").and_then(Value::as_str).is_some() {
        return Err(ProviderError::Refused);
    }
    if text.is_empty() && choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        return Err(ProviderError::CutOff);
    }
    let reported = value
        .get("usage")
        .filter(|u| u.is_object())
        .or_else(|| value.get("usageMetadata"))
        .map(chat_usage)
        .unwrap_or_default();
    Ok(reported.into_completion(cap(text)))
}

fn parse_anthropic(value: &Value) -> Result<Completion, ProviderError> {
    if value.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return Err(ProviderError::Refused);
    }
    let text: String = value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if text.is_empty() && value.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
        return Err(ProviderError::CutOff);
    }
    let reported = value.get("usage").map(anthropic_usage).unwrap_or_default();
    Ok(reported.into_completion(cap(&text)))
}

fn cap(text: &str) -> String {
    match text.char_indices().nth(MAX_TEXT_CHARS) {
        Some((cut, _)) => text[..cut].to_owned(),
        None => text.to_owned(),
    }
}

/// Collects streamed text and passes it on.
struct Collector<'a> {
    text: String,
    deltas: Option<&'a mpsc::Sender<String>>,
    received: &'a AtomicUsize,
}

impl Collector<'_> {
    async fn push(&mut self, piece: &str) -> Result<(), ProviderError> {
        if piece.is_empty() {
            return Ok(());
        }
        if self.text.len() + piece.len() > MAX_RESPONSE_BYTES {
            return Err(ProviderError::TooLarge);
        }
        self.text.push_str(piece);
        self.received.fetch_add(piece.chars().count(), Ordering::Relaxed);
        if let Some(deltas) = self.deltas {
            // Nobody listens any more: stop asking the provider.
            deltas.send(piece.to_owned()).await.map_err(|_| ProviderError::Unreachable)?;
        }
        Ok(())
    }
}

async fn stream_chat(reader: &mut SseReader, mut collector: Collector<'_>) -> Result<Completion, ProviderError> {
    let mut reported = Reported::default();
    let mut finish = None;
    while let Some((_, data)) = reader.next().await? {
        if data.trim() == "[DONE]" {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else { continue };
        if let Some(error) = value.get("error") {
            let detail = error.get("message").and_then(Value::as_str).map(|m| shorten(m, 300));
            return Err(ProviderError::Status { status: 502, detail });
        }
        if let Some(piece) = value.pointer("/choices/0/delta/content").and_then(Value::as_str) {
            collector.push(piece).await?;
        }
        if let Some(reason) = value.pointer("/choices/0/finish_reason").and_then(Value::as_str) {
            finish = Some(reason.to_owned());
        }
        if let Some(u) = value.get("usage").filter(|u| u.is_object()).or_else(|| value.get("usageMetadata")) {
            reported = chat_usage(u);
        }
    }
    match finish.as_deref() {
        Some("content_filter") => return Err(ProviderError::Refused),
        Some("length") if collector.text.is_empty() => return Err(ProviderError::CutOff),
        _ => {}
    }
    Ok(reported.into_completion(cap(&collector.text)))
}

async fn stream_anthropic(reader: &mut SseReader, mut collector: Collector<'_>) -> Result<Completion, ProviderError> {
    let mut reported = Reported::default();
    let mut stop = None;
    while let Some((event, data)) = reader.next().await? {
        let Ok(value) = serde_json::from_str::<Value>(&data) else { continue };
        match value.get("type").and_then(Value::as_str).unwrap_or(event.as_str()) {
            "message_start" => {
                if let Some(u) = value.pointer("/message/usage") {
                    reported = Reported { output: 0, ..anthropic_usage(u) };
                }
            }
            "content_block_delta" => {
                if value.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta")
                    && let Some(piece) = value.pointer("/delta/text").and_then(Value::as_str)
                {
                    collector.push(piece).await?;
                }
            }
            "message_delta" => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    stop = Some(reason.to_owned());
                }
                if let Some(u) = value.get("usage") {
                    // The final counts; newer versions repeat the prompt's here as well.
                    if u.get("input_tokens").and_then(Value::as_i64).is_some() {
                        let again = anthropic_usage(u);
                        (reported.input, reported.cached, reported.cache_write) =
                            (again.input, again.cached, again.cache_write);
                    }
                    if let Some(tokens) = u.get("output_tokens").and_then(Value::as_i64) {
                        reported.output = tokens.clamp(0, MAX_REPORTED_TOKENS);
                    }
                }
            }
            "error" => {
                let kind = value.pointer("/error/type").and_then(Value::as_str).unwrap_or_default();
                if kind == "overloaded_error" || kind == "rate_limit_error" {
                    return Err(ProviderError::RateLimited { retry_after: None });
                }
                let detail = value.pointer("/error/message").and_then(Value::as_str).map(|m| shorten(m, 300));
                return Err(ProviderError::Status { status: 502, detail });
            }
            "message_stop" => break,
            _ => {}
        }
    }
    match stop.as_deref() {
        Some("refusal") => return Err(ProviderError::Refused),
        Some("max_tokens") if collector.text.is_empty() => return Err(ProviderError::CutOff),
        _ => {}
    }
    Ok(reported.into_completion(cap(&collector.text)))
}

async fn stream_codex(reader: &mut SseReader, mut collector: Collector<'_>) -> Result<Completion, ProviderError> {
    let mut reported = Reported::default();
    while let Some((event, data)) = reader.next().await? {
        let Ok(value) = serde_json::from_str::<Value>(&data) else { continue };
        match value.get("type").and_then(Value::as_str).unwrap_or(event.as_str()) {
            "response.output_text.delta" => {
                if let Some(piece) = value.get("delta").and_then(Value::as_str) {
                    collector.push(piece).await?;
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(u) = value.pointer("/response/usage") {
                    reported = responses_usage(u);
                }
                break;
            }
            "response.failed" | "error" => {
                let detail = value
                    .pointer("/response/error/message")
                    .or_else(|| value.pointer("/error/message"))
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .map(|m| shorten(m, 300));
                return Err(ProviderError::Status { status: 502, detail });
            }
            _ => {}
        }
    }
    Ok(reported.into_completion(cap(&collector.text)))
}

/// Reads server-sent events off a body, event by event, within the limits.
pub struct SseReader {
    body: Option<hyper::body::Incoming>,
    parser: SseParser,
    read: usize,
}

impl SseReader {
    fn new(body: hyper::body::Incoming) -> SseReader {
        SseReader { body: Some(body), parser: SseParser::default(), read: 0 }
    }

    /// The next `(event, data)`, or `None` at the end of the stream.
    async fn next(&mut self) -> Result<Option<(String, String)>, ProviderError> {
        loop {
            if let Some(event) = self.parser.next_event() {
                return Ok(Some(event));
            }
            let Some(body) = self.body.as_mut() else { return Ok(self.parser.finish()) };
            let frame = tokio::time::timeout(IDLE_TIMEOUT, body.frame()).await.map_err(|_| ProviderError::Timeout)?;
            match frame {
                None => self.body = None,
                Some(frame) => {
                    let frame = frame.map_err(|_| ProviderError::Unreachable)?;
                    if let Ok(data) = frame.into_data() {
                        self.read += data.len();
                        if self.read > MAX_RESPONSE_BYTES * 4 {
                            return Err(ProviderError::TooLarge);
                        }
                        self.parser.feed(&data)?;
                    }
                }
            }
        }
    }
}

/// Server-sent events (the parts every provider uses): `event:` and `data:` lines, a blank line ends
/// an event, `:` starts a comment. Bytes are kept until a line is whole, so a character split
/// between two chunks is never cut.
#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    /// The start of `buffer` up to here holds no line break: a chunk only looks at what is new.
    scanned: usize,
    event: String,
    data: Vec<String>,
    ready: std::collections::VecDeque<(String, String)>,
    /// Bytes looked at for line breaks, for the test that this stays linear.
    #[cfg(test)]
    looked_at: usize,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        self.buffer.extend_from_slice(bytes);
        let mut start = 0;
        let mut from = self.scanned;
        while let Some(found) = self.buffer[from..].iter().position(|&b| b == b'\n') {
            #[cfg(test)]
            {
                self.looked_at += found + 1;
            }
            let end = from + found;
            let line = String::from_utf8_lossy(&self.buffer[start..end]).into_owned();
            self.line(line.trim_end_matches('\r'));
            start = end + 1;
            from = start;
        }
        #[cfg(test)]
        {
            self.looked_at += self.buffer.len() - from;
        }
        if start > 0 {
            self.buffer.drain(..start);
        }
        self.scanned = self.buffer.len();
        // One line longer than any event needs is not an event stream.
        if self.buffer.len() > MAX_LINE_BYTES {
            return Err(ProviderError::TooLarge);
        }
        Ok(())
    }

    fn line(&mut self, line: &str) {
        if line.is_empty() {
            if !self.data.is_empty() {
                let data = std::mem::take(&mut self.data).join("\n");
                self.ready.push_back((std::mem::take(&mut self.event), data));
            }
            self.event.clear();
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
    }

    pub fn next_event(&mut self) -> Option<(String, String)> {
        self.ready.pop_front()
    }

    /// What is left when the stream ended without a last blank line.
    pub fn finish(&mut self) -> Option<(String, String)> {
        if !self.buffer.is_empty() {
            self.scanned = 0;
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned();
            self.line(rest.trim_end_matches('\r'));
        }
        self.line("");
        self.ready.pop_front()
    }
}

/// The JSON object in a model's answer: the whole text, or what stands between the first `{` and the
/// last `}` (models like to wrap JSON in a code block or a sentence when no schema holds them).
pub fn json_answer(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && value.is_object()
    {
        return Some(value);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end]).ok().filter(Value::is_object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_go_out_in_the_order_of_their_required_list() {
        let schema = crate::prompts::suggest_schema(&["Rechnungen".into()], 2);
        let body = serde_json::json!({ "model": "m", "response_format": { "json_schema": { "schema": schema } } });
        let text = ordered_json(&body);
        let at = |key: &str| text.find(&format!("\"{key}\":")).unwrap();
        assert!(at("verdicts") < at("newLabels"), "{text}");
        assert!(at("name") < at("reason") && at("reason") < at("fits"), "{text}");
        assert_eq!(schema["properties"]["verdicts"]["minItems"], 1);
        assert_eq!(schema["properties"]["verdicts"]["maxItems"], 1);
        // The same JSON, only in another order.
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), body);
        let plain = serde_json::json!({ "b": [1, "x\"y", null, { "a": true }], "a": 1.5 });
        assert_eq!(ordered_json(&plain), plain.to_string());
    }

    #[test]
    fn tokens_are_counted_by_script() {
        assert_eq!(estimate_texts(["Hallo Nyu!"]), 3);
        assert_eq!(estimate_texts(["東京で会議", "ab"]), 6, "a token per character, and one for the rest");
        assert_eq!(estimate_texts(["회의 내일"]), 5);
        let prompt = Prompt { system: "abcd".into(), user: "efgh".into(), schema: None, max_tokens: 10 };
        assert_eq!(estimate_prompt(&prompt), 2);
        let with_schema = Prompt { schema: Some(("x", serde_json::json!({ "type": "object" }))), ..prompt };
        assert_eq!(estimate_prompt(&with_schema), 7);
    }

    #[test]
    fn events_are_split_on_blank_lines_and_across_chunks() {
        let mut parser = SseParser::default();
        let text = "event: message_start\r\ndata: {\"a\":1}\r\n\r\n: ping\n\ndata: first\ndata: second\n\ndata: Grü";
        let bytes = text.as_bytes();
        // Cut in the middle of the ü.
        let cut = bytes.len() - 1;
        parser.feed(&bytes[..cut]).unwrap();
        assert_eq!(parser.next_event(), Some(("message_start".into(), "{\"a\":1}".into())));
        assert_eq!(parser.next_event(), Some((String::new(), "first\nsecond".into())));
        assert_eq!(parser.next_event(), None);
        parser.feed(&bytes[cut..]).unwrap();
        parser.feed(b"\n\n").unwrap();
        assert_eq!(parser.next_event(), Some((String::new(), "Grü".into())));
        parser.feed(b"data: last").unwrap();
        assert_eq!(parser.finish(), Some((String::new(), "last".into())));
    }

    #[test]
    fn a_long_line_is_read_once_and_has_a_limit() {
        let mut parser = SseParser::default();
        let piece = [b'x'; 100];
        let mut fed = 0;
        while fed + piece.len() <= MAX_LINE_BYTES {
            parser.feed(&piece).unwrap();
            fed += piece.len();
        }
        // Every byte was looked at once, not again with every chunk.
        assert_eq!(parser.looked_at, fed);
        parser.feed(b"\n\n").unwrap();
        assert_eq!(parser.next_event(), None, "no data field, no event");
        assert_eq!(parser.looked_at, fed + 2);

        let mut parser = SseParser::default();
        parser.feed(b"data: ").unwrap();
        assert_eq!(parser.feed(&vec![b'x'; MAX_LINE_BYTES]), Err(ProviderError::TooLarge));
        // Many events in one chunk are all read.
        let mut parser = SseParser::default();
        parser.feed(b"data: a\n\ndata: b\n\ndata: c\n\n").unwrap();
        let events: Vec<_> = std::iter::from_fn(|| parser.next_event()).map(|(_, data)| data).collect();
        assert_eq!(events, ["a", "b", "c"]);
    }

    #[test]
    fn json_is_found_in_chatter() {
        assert_eq!(json_answer("{\"a\": 1}").unwrap()["a"], 1);
        assert_eq!(json_answer("Sure! ```json\n{\"labels\": []}\n```").unwrap()["labels"], json!([]));
        assert!(json_answer("no json here").is_none());
        assert!(json_answer("[1, 2]").is_none());
    }

    #[test]
    fn errors_say_what_the_provider_said() {
        let body = br#"{"error": {"message": "Incorrect API key provided: sk-...", "type": "invalid_request_error"}}"#;
        assert_eq!(status_error(401, None, body), ProviderError::Unauthorized);
        assert_eq!(status_error(429, Some(7), b"{}"), ProviderError::RateLimited { retry_after: Some(7) });
        let gemini = br#"[{"error": {"code": 400, "message": "model not found"}}]"#;
        assert_eq!(
            status_error(400, None, gemini),
            ProviderError::Status { status: 400, detail: Some("model not found".into()) }
        );
        assert_eq!(shorten("a\nb", 10), "a b");
        assert_eq!(shorten("äöüäöü", 3), "äöü…");
    }

    #[test]
    fn usage_is_read_per_kind_with_thinking_and_the_cache() {
        // OpenAI: thinking inside completion_tokens, a cached prompt.
        let openai = json!({ "prompt_tokens": 1200, "completion_tokens": 900, "total_tokens": 2100,
            "prompt_tokens_details": { "cached_tokens": 1024 },
            "completion_tokens_details": { "reasoning_tokens": 640 } });
        let r = chat_usage(&openai);
        assert_eq!((r.input, r.output, r.reasoning, r.cached, r.cost_usd), (1200, 260, 640, 1024, None));
        // Gemini's OpenAI-compatible API: thinking only in total_tokens.
        let gemini = json!({ "prompt_tokens": 500, "completion_tokens": 120, "total_tokens": 1020 });
        let r = chat_usage(&gemini);
        assert_eq!((r.input, r.output, r.reasoning), (500, 120, 400));
        // Gemini's own usageMetadata.
        let native = json!({ "promptTokenCount": 300, "candidatesTokenCount": 50, "thoughtsTokenCount": 700,
            "cachedContentTokenCount": 100, "totalTokenCount": 1050 });
        let r = chat_usage(&native);
        assert_eq!((r.input, r.output, r.reasoning, r.cached), (300, 50, 700, 100));
        // OpenRouter: what it charged, and cache writes.
        let openrouter = json!({ "prompt_tokens": 800, "completion_tokens": 200, "total_tokens": 1000, "cost": 0.00042,
            "prompt_tokens_details": { "cached_tokens": 0, "cache_write_tokens": 600 },
            "completion_tokens_details": { "reasoning_tokens": 150 } });
        let r = chat_usage(&openrouter);
        assert_eq!((r.output, r.reasoning, r.cache_write, r.cost_usd), (50, 150, 600, Some(0.00042)));
        // DeepSeek's cache hits; a plain server without details.
        assert_eq!(
            chat_usage(&json!({ "prompt_tokens": 90, "completion_tokens": 10, "prompt_cache_hit_tokens": 64 })).cached,
            64
        );
        let plain = chat_usage(&json!({ "prompt_tokens": 90, "completion_tokens": 10 }));
        assert_eq!((plain.input, plain.output, plain.reasoning), (90, 10, 0));
        // Anthropic: the cache outside input_tokens, thinking inside output_tokens.
        let anthropic = json!({ "input_tokens": 20, "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 300,
            "output_tokens": 500 });
        let r = anthropic_usage(&anthropic);
        assert_eq!((r.input, r.output, r.reasoning, r.cached, r.cache_write), (1320, 500, 0, 1000, 300));
        // The Responses API (ChatGPT).
        let codex = json!({ "input_tokens": 400, "input_tokens_details": { "cached_tokens": 128 },
            "output_tokens": 300, "output_tokens_details": { "reasoning_tokens": 256 } });
        let r = responses_usage(&codex);
        assert_eq!((r.input, r.output, r.reasoning, r.cached), (400, 44, 256, 128));
        // Answers as a whole.
        let answer =
            json!({ "choices": [{ "message": { "content": "Hi" }, "finish_reason": "stop" }], "usage": openai });
        let completion = parse_chat(&answer).unwrap();
        assert_eq!((completion.output_tokens, completion.reasoning_tokens, completion.calls), (260, 640, 1));
        let message =
            json!({ "content": [{ "type": "text", "text": "Hi" }], "stop_reason": "end_turn", "usage": anthropic });
        assert_eq!(parse_anthropic(&message).unwrap().input_tokens, 1320, "the cache counts as input");
    }

    #[test]
    fn huge_reported_usage_is_capped() {
        let max = i64::MAX;
        let chat = json!({ "prompt_tokens": max, "completion_tokens": max, "total_tokens": max,
            "prompt_tokens_details": { "cached_tokens": max, "cache_write_tokens": max },
            "completion_tokens_details": { "reasoning_tokens": max }, "cost": 1e308 });
        let r = chat_usage(&chat);
        let cap = MAX_REPORTED_TOKENS;
        assert_eq!((r.input, r.output, r.reasoning, r.cached, r.cache_write), (cap, 0, cap, cap, cap));
        assert_eq!(r.cost_usd, Some(MAX_REPORTED_COST_USD));
        // Thinking only in a huge total_tokens.
        let r = chat_usage(&json!({ "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": max }));
        assert_eq!((r.output, r.reasoning), (1, cap - 2));
        let anthropic = json!({ "input_tokens": max, "cache_read_input_tokens": max,
            "cache_creation_input_tokens": max, "output_tokens": max });
        let r = anthropic_usage(&anthropic);
        assert_eq!((r.input, r.output, r.cached, r.cache_write), (3 * cap, cap, cap, cap));
        let r = responses_usage(&json!({ "input_tokens": max, "output_tokens": max }));
        assert_eq!((r.input, r.output), (cap, cap));
        assert_eq!(chat_usage(&json!({ "prompt_tokens": -5, "cost": -1.0 })), Reported::default());
    }

    #[test]
    fn a_request_is_its_text_and_the_frame_around_it() {
        let prompt = Prompt { system: "abcd".into(), user: "efgh".into(), schema: None, max_tokens: 10 };
        assert_eq!(estimate_request(&prompt, Shape::Chat), 2 + 9);
        let with_schema = Prompt { schema: Some(("x", json!({ "type": "object" }))), ..prompt };
        assert_eq!(estimate_request(&with_schema, Shape::Anthropic), 7 + 8 + 12);
    }
}
