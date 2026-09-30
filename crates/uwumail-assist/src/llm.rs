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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// The provider said nothing about tokens; they are estimated.
    pub estimated: bool,
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
                ask(target, prompt, deltas, false).await
            }
            other => other,
        }
    };
    let mut completion = tokio::time::timeout(TOTAL_TIMEOUT, work).await.map_err(|_| ProviderError::Timeout)??;
    if completion.input_tokens == 0 && completion.output_tokens == 0 {
        completion.input_tokens = estimate_prompt(prompt);
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
        .body(Full::new(Bytes::from(body.to_string())))
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

fn usage(value: &Value, input: &str, output: &str) -> (i64, i64) {
    let number = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
    (number(input), number(output))
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
    let (input_tokens, output_tokens) =
        value.get("usage").map(|u| usage(u, "prompt_tokens", "completion_tokens")).unwrap_or_default();
    Ok(Completion { text: cap(text), input_tokens, output_tokens, estimated: false })
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
    let (input_tokens, output_tokens) =
        value.get("usage").map(|u| usage(u, "input_tokens", "output_tokens")).unwrap_or_default();
    Ok(Completion { text: cap(&text), input_tokens, output_tokens, estimated: false })
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
    let (mut input_tokens, mut output_tokens) = (0, 0);
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
        if let Some(u) = value.get("usage").filter(|u| u.is_object()) {
            (input_tokens, output_tokens) = usage(u, "prompt_tokens", "completion_tokens");
        }
    }
    match finish.as_deref() {
        Some("content_filter") => return Err(ProviderError::Refused),
        Some("length") if collector.text.is_empty() => return Err(ProviderError::CutOff),
        _ => {}
    }
    Ok(Completion { text: cap(&collector.text), input_tokens, output_tokens, estimated: false })
}

async fn stream_anthropic(reader: &mut SseReader, mut collector: Collector<'_>) -> Result<Completion, ProviderError> {
    let (mut input_tokens, mut output_tokens) = (0, 0);
    let mut stop = None;
    while let Some((event, data)) = reader.next().await? {
        let Ok(value) = serde_json::from_str::<Value>(&data) else { continue };
        match value.get("type").and_then(Value::as_str).unwrap_or(event.as_str()) {
            "message_start" => {
                if let Some(u) = value.pointer("/message/usage") {
                    input_tokens = u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0)
                        + u.get("cache_read_input_tokens").and_then(Value::as_i64).unwrap_or(0)
                        + u.get("cache_creation_input_tokens").and_then(Value::as_i64).unwrap_or(0);
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
                if let Some(tokens) = value.pointer("/usage/output_tokens").and_then(Value::as_i64) {
                    output_tokens = tokens;
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
    Ok(Completion { text: cap(&collector.text), input_tokens, output_tokens, estimated: false })
}

async fn stream_codex(reader: &mut SseReader, mut collector: Collector<'_>) -> Result<Completion, ProviderError> {
    let (mut input_tokens, mut output_tokens) = (0, 0);
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
                    (input_tokens, output_tokens) = usage(u, "input_tokens", "output_tokens");
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
    Ok(Completion { text: cap(&collector.text), input_tokens, output_tokens, estimated: false })
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
}
