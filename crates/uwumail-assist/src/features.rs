//! The features: writing, summaries, the spam check, events and labels. Each one reads what it needs
//! of the mail, asks the model through [`Assist::run`] and checks the answer before anything of it is
//! handed on: free text is only ever shown to the person, JSON answers are held to their shape and to
//! what the mail and the person's own data allow.

use std::collections::HashSet;

use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uwumail_store::{
    Account, AssistLabel, CalibrationSample, EmailRecord, KeywordsChange, MailboxRole, SenderHistory, StoreError,
};

use crate::access::{Effective, Ticket};
use crate::kinds::Shape;
use crate::llm::{self, Completion, Prompt};
use crate::mail::{MAX_MAIL_CHARS, MailText};
use crate::prices::{CostParts, Metered, Price};
use crate::prompts::{self, ComposeRequest, SUBJECT_MARK};
use crate::{Assist, AssistError, MAX_INSTRUCTION_CHARS, MAX_TEXT_CHARS, PictureRead, Result, now};

/// Mails of a conversation that are summarized, the latest ones.
pub const MAX_THREAD_MAILS: usize = 20;
/// Events one mail gives, at most.
const MAX_EVENTS: usize = 10;
const MAX_QUOTE_CHARS: usize = 300;
const MAX_REASONS: usize = 6;
const MAX_REASON_CHARS: usize = 300;
/// How much of a mail the label model reads.
const LABEL_MAIL_CHARS: usize = 4000;
/// Address book entries looked at to match people.
const MAX_CONTACTS: usize = 5000;
/// Texts of pictures that go along with a mail, and how long each may be.
const MAX_PICTURE_TEXTS: usize = 20;
const MAX_PICTURE_CHARS: usize = 4000;
/// What `Assist/estimate` counts for a picture that was never read: a short poster or ticket.
const UNREAD_PICTURE_CHARS: usize = 400;
/// Typical answers, in tokens, for `Assist/estimate` (docs/jmap-assist.md): a mail written from an
/// instruction, a summary of one mail (and what each further mail of a conversation adds), a spam
/// verdict with its reasons, and a mail's events.
const TYPICAL_WRITE_TOKENS: i64 = 400;
const TYPICAL_SUMMARY_TOKENS: i64 = 150;
const TYPICAL_SUMMARY_TOKENS_PER_MAIL: i64 = 50;
const TYPICAL_SUMMARY_MAX_TOKENS: i64 = 600;
const TYPICAL_SPAM_TOKENS: i64 = 150;
const TYPICAL_EVENTS_TOKENS: i64 = 250;
const TYPICAL_LABEL_TOKENS: i64 = 80;
/// Typical thinking, in tokens, of a model that thinks before it answers (at its default effort),
/// until its own requests tell better.
const TYPICAL_REASONING_WRITE: i64 = 700;
const TYPICAL_REASONING_SUMMARY: i64 = 500;
const TYPICAL_REASONING_SPAM: i64 = 500;
const TYPICAL_REASONING_EVENTS: i64 = 900;
const TYPICAL_REASONING_LABELS: i64 = 300;
/// Requests of the same provider, model and feature an estimate learns from, at least.
pub const MIN_CALIBRATION_SAMPLES: usize = 5;
/// How far what was learned may move an estimate.
const MIN_RATIO: f64 = 0.5;
const MAX_RATIO: f64 = 3.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Thinking, on top of `output_tokens`.
    pub reasoning_tokens: i64,
}

impl Usage {
    fn of(completion: &Completion) -> Usage {
        Usage {
            input_tokens: completion.input_tokens,
            output_tokens: completion.output_tokens,
            reasoning_tokens: completion.reasoning_tokens,
        }
    }
}

/// A piece of a streamed answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// The subject proposal of `Assist/compose`, before the text.
    Subject(String),
    /// The next piece of text.
    Delta(String),
}

#[derive(Debug, Clone, Default)]
pub struct ComposeArgs {
    pub mode: String,
    pub instruction: Option<String>,
    pub preset: Option<String>,
    pub target_language: Option<String>,
    pub text: Option<String>,
    pub subject: Option<String>,
    pub reply_to_email_id: Option<i64>,
    pub want_subject: bool,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ComposeResult {
    pub text: String,
    pub subject: Option<String>,
    pub effective: Effective,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default)]
pub struct SummarizeArgs {
    pub email_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SummaryResult {
    pub summary: String,
    pub effective: Effective,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default)]
pub struct SpamArgs {
    pub email_id: i64,
    pub language: Option<String>,
}

/// SPF, DKIM and DMARC as this server's `Authentication-Results` recorded them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationSignals {
    pub spf: Option<String>,
    pub dkim: Option<String>,
    pub dmarc: Option<String>,
    pub from_domain: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderSignals {
    pub address: Option<String>,
    pub earlier_messages: i64,
    pub earlier_in_junk: i64,
    pub written_to: i64,
    pub in_contacts: bool,
    /// UTCDate.
    pub first_seen: Option<String>,
}

/// What the server itself knows about a mail, for the spam check.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpamSignals {
    pub authentication: AuthenticationSignals,
    pub spam_score: Option<f64>,
    pub spam_threshold: Option<f64>,
    pub tests: Vec<String>,
    pub in_junk: bool,
    pub sender: SenderSignals,
}

#[derive(Debug, Clone)]
pub struct SpamResult {
    pub verdict: String,
    pub confidence: f64,
    pub reasons: Vec<String>,
    pub signals: SpamSignals,
    pub effective: Effective,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default)]
pub struct EventsArgs {
    pub email_id: i64,
    pub include_images: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub name: Option<String>,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedEvent {
    pub title: String,
    /// LocalDateTime.
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub time_zone: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub participants: Vec<Participant>,
    pub confidence: f64,
    pub quote: String,
}

#[derive(Debug, Clone)]
pub struct EventsResult {
    pub events: Vec<ExtractedEvent>,
    pub effective: Effective,
    pub usage: Usage,
}

/// The arguments of one of the calls `Assist/estimate` estimates.
#[derive(Debug, Clone)]
pub enum EstimateArgs {
    Compose(ComposeArgs),
    Summarize(SummarizeArgs),
    SpamCheck(SpamArgs),
    ExtractEvents(EventsArgs),
}

impl EstimateArgs {
    fn feature(&self) -> &'static str {
        match self {
            EstimateArgs::Compose(_) => "compose",
            EstimateArgs::Summarize(_) => "summarize",
            EstimateArgs::SpamCheck(_) => "spamCheck",
            EstimateArgs::ExtractEvents(_) => "extractEvents",
        }
    }
}

/// One call to the model a request makes, or may make.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateCall {
    /// `main`: the request itself; `retry`: asked again without the answer's JSON shape after the
    /// provider refused it.
    pub purpose: &'static str,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    /// Pictures sent to the model as pictures (never: the server reads their text itself).
    pub images: i64,
    /// How likely the call is: 1 for the request itself, the rate seen so far for a retry.
    pub weight: f64,
}

/// What an estimate costs, in US dollars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstimateCost {
    /// The expected cost: every call by its weight.
    pub usd: f64,
    /// The worst case: every answer as long as the model may write, every extra call made.
    pub max_usd: f64,
    pub parts: CostParts,
}

/// What a call would take, without making it.
#[derive(Debug, Clone)]
pub struct Estimate {
    /// Everything the calls would send, each call by its weight: the prompt with the API's frame,
    /// the text of pictures included.
    pub input_tokens: i64,
    /// A typical answer, at most what the call allows the model.
    pub output_tokens: i64,
    /// Typical thinking of a model that thinks first, on top of `output_tokens`.
    pub reasoning_tokens: i64,
    /// Pictures whose text goes along, and its tokens (part of `input_tokens`).
    pub image_count: i64,
    pub image_tokens: i64,
    pub calls: Vec<EstimateCall>,
    /// Learned from this provider's and model's last requests for the feature.
    pub calibrated: bool,
    pub effective: Effective,
    /// What is left of the day's limits of that provider; `None` without a limit.
    pub requests_left_today: Option<i64>,
    pub tokens_left_today: Option<i64>,
    /// What it would cost; `None` when the price is not known or the admin does not show this
    /// provider's costs.
    pub cost: Option<EstimateCost>,
}

impl Estimate {
    /// In, out and thinking, of every call by its weight.
    pub fn total_tokens(&self) -> i64 {
        self.input_tokens + self.output_tokens + self.reasoning_tokens
    }
}

/// What a provider's last requests of a model for a feature taught.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Calibration {
    /// Median of real to estimated prompt tokens.
    pub input_ratio: Option<f64>,
    /// Median of real to typical answer tokens.
    pub output_ratio: Option<f64>,
    /// Median thinking tokens.
    pub reasoning: Option<i64>,
    /// Share of requests that had to be asked again.
    pub retry_rate: Option<f64>,
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) { (values[middle - 1] + values[middle]) / 2.0 } else { values[middle] })
}

impl Calibration {
    /// Learns from `samples` once there are [`MIN_CALIBRATION_SAMPLES`] of them.
    pub fn of(samples: &[CalibrationSample]) -> Calibration {
        if samples.len() < MIN_CALIBRATION_SAMPLES {
            return Calibration::default();
        }
        let ratio = |pairs: Vec<(i64, i64)>| -> Option<f64> {
            let ratios: Vec<f64> = pairs
                .into_iter()
                .filter(|(expected, _)| *expected > 0)
                .map(|(e, real)| real as f64 / e as f64)
                .collect();
            if ratios.len() < MIN_CALIBRATION_SAMPLES {
                return None;
            }
            median(ratios).map(|r| r.clamp(MIN_RATIO, MAX_RATIO))
        };
        Calibration {
            input_ratio: ratio(samples.iter().map(|s| (s.estimated_input, s.input_tokens)).collect()),
            output_ratio: ratio(samples.iter().map(|s| (s.estimated_output, s.output_tokens)).collect()),
            reasoning: median(samples.iter().map(|s| s.reasoning_tokens as f64).collect()).map(|m| m.round() as i64),
            retry_rate: Some(samples.iter().filter(|s| s.calls > 1).count() as f64 / samples.len() as f64),
        }
    }
}

/// Whether a model thinks before it answers without being asked to: by the price lists, or by its
/// name where they don't know it. Anthropic's models think only when asked, which the server never
/// does.
pub fn thinks(shape: Shape, price: Option<&Price>, model: &str) -> bool {
    if shape == Shape::Anthropic {
        return false;
    }
    if price.is_some_and(|price| price.supports_reasoning) {
        return true;
    }
    let model = model.trim().to_lowercase();
    let name = model.rsplit('/').next().unwrap_or(&model);
    let starts = |prefix: &str| name.starts_with(prefix);
    (["o1", "o3", "o4"].iter().any(|p| starts(p)) && !name.contains("audio"))
        || (starts("gpt-5") && !name.contains("chat"))
        || starts("gpt-oss")
        || (starts("gemini-2.5") && !name.contains("lite"))
        || starts("gemini-3")
        || [
            "deepseek-r1",
            "deepseek-reasoner",
            "qwq",
            "qwen3",
            "magistral",
            "thinking",
            "grok-4",
            "grok-3-mini",
            "codex",
        ]
        .iter()
        .any(|part| name.contains(part))
}

/// What goes into an estimate besides the prompt itself.
#[derive(Debug, Clone)]
pub struct EstimatePlan<'a> {
    pub prompt: &'a Prompt,
    pub shape: Shape,
    /// A typical answer by the feature's heuristic.
    pub typical: i64,
    /// Typical thinking when the model thinks, `None` when it does not.
    pub reasoning: Option<i64>,
    /// Pictures whose text goes along, and its tokens (part of the prompt).
    pub image_count: i64,
    pub image_tokens: i64,
    /// What the model costs, and what the lists say about it.
    pub price: Option<&'a Price>,
    /// Whether the person sees the cost.
    pub show_cost: bool,
}

/// The calls, tokens and cost of a request, with what the provider's last requests taught.
pub fn plan_estimate(plan: &EstimatePlan<'_>, calibration: &Calibration) -> (Vec<EstimateCall>, Option<EstimateCost>) {
    let heuristic_input = llm::estimate_request(plan.prompt, plan.shape);
    let input_ratio = calibration.input_ratio.unwrap_or(1.0);
    let input = (heuristic_input as f64 * input_ratio).round() as i64;
    let budget = i64::from(plan.prompt.max_tokens)
        .min(plan.price.and_then(|price| price.max_output_tokens).unwrap_or(i64::MAX))
        .max(1);
    let output = ((plan.typical as f64 * calibration.output_ratio.unwrap_or(1.0)).round() as i64).clamp(1, budget);
    // A median of real thinking wins over the guess, also when it is none.
    let reasoning = match (calibration.reasoning, plan.reasoning) {
        (Some(learned), _) => learned,
        (None, Some(typical)) => typical,
        (None, None) => 0,
    }
    .clamp(0, budget - output);
    let mut calls = vec![EstimateCall {
        purpose: "main",
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reasoning,
        images: 0,
        weight: 1.0,
    }];
    let retry_rate = calibration.retry_rate.unwrap_or(0.0);
    if plan.prompt.schema.is_some() && retry_rate > 0.0 {
        // The first request, refused for its JSON shape, is asked again: its prompt counts once
        // more (big providers don't bill a refused request, some servers do).
        calls.push(EstimateCall {
            purpose: "retry",
            input_tokens: input,
            output_tokens: 0,
            reasoning_tokens: 0,
            images: 0,
            weight: retry_rate,
        });
    }
    let cost = plan.price.filter(|_| plan.show_cost).map(|price| {
        let main = Metered {
            input: input as f64,
            output: output as f64,
            reasoning: reasoning as f64,
            requests: 1.0,
            prompt: input,
            ..Metered::default()
        };
        let mut parts = price.cost_of(&main);
        // The pictures' text is part of the prompt: its share shows on its own.
        let image_tokens = plan.image_tokens as f64 * input_ratio;
        let images = (image_tokens * price.level(input).0 / 1e6).min(parts.input);
        parts.input -= images;
        parts.images += images;
        let refused = Metered { input: input as f64, requests: 1.0, prompt: input, ..Metered::default() };
        for call in calls.iter().filter(|call| call.purpose != "main") {
            parts.other += price.cost_of(&refused).total() * call.weight;
        }
        // The worst case: the whole answer budget spent at the dearer of answer and thinking, the
        // prompt at least as the heuristic counts it, and a retry.
        let (_, out_rate, reasoning_rate, _) = price.level(input.max(heuristic_input));
        let thinking_dearer = (reasoning > 0 || plan.reasoning.is_some()) && reasoning_rate > out_rate;
        let worst_answer = if thinking_dearer {
            Metered { reasoning: budget as f64, ..Metered::default() }
        } else {
            Metered { output: budget as f64, ..Metered::default() }
        };
        let worst_input = input.max(heuristic_input);
        let worst = Metered { input: worst_input as f64, requests: 1.0, prompt: worst_input, ..worst_answer };
        let mut max_usd = price.cost_of(&worst).total();
        if plan.prompt.schema.is_some() {
            max_usd += price
                .cost_of(&Metered {
                    input: worst_input as f64,
                    requests: 1.0,
                    prompt: worst_input,
                    ..Metered::default()
                })
                .total();
        }
        let usd = parts.total();
        EstimateCost { usd, max_usd: max_usd.max(usd), parts }
    });
    (calls, cost)
}

/// A label the model put on a mail, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelPick {
    pub label: AssistLabel,
    pub reason: String,
}

fn invalid(property: &'static str, description: impl Into<String>) -> AssistError {
    AssistError::Invalid { code: "invalidArguments", property, description: description.into() }
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// At most `max` characters of `text`, trimmed, without control characters but line breaks.
fn clean(text: &str, max: usize) -> String {
    let cleaned: String = text.chars().filter(|c| !c.is_control() || *c == '\n').take(max).collect::<String>();
    cleaned.trim().to_owned()
}

/// One optional line of text out of a JSON answer: trimmed, capped, `None` when empty.
fn optional_text(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    let text = clean(&text.replace('\n', " "), max);
    (!text.is_empty()).then_some(text)
}

/// Splits a `SUBJECT: …` first line off a written mail.
pub fn split_subject(text: &str) -> (Option<String>, String) {
    let trimmed = text.trim_start();
    let starts = trimmed.get(..SUBJECT_MARK.len()).is_some_and(|head| head.eq_ignore_ascii_case(SUBJECT_MARK));
    if !starts {
        return (None, text.trim().to_owned());
    }
    let rest = &trimmed[SUBJECT_MARK.len()..];
    let (line, body) = rest.split_once('\n').unwrap_or((rest, ""));
    let subject = clean(line, 200);
    ((!subject.is_empty()).then_some(subject), body.trim().to_owned())
}

/// Passes streamed text on as [`StreamEvent`]s, taking a `SUBJECT:` line off the start when one is
/// asked for. Ends when the model's stream ends; a listener that went away ends it early, which
/// in turn stops the request to the provider.
async fn relay(mut from: mpsc::Receiver<String>, to: &mpsc::Sender<StreamEvent>, want_subject: bool) {
    let mut head = String::new();
    let mut in_head = want_subject;
    let mut at_start = true;
    let mut pieces: Vec<StreamEvent> = Vec::new();
    loop {
        let piece = from.recv().await;
        let ended = piece.is_none();
        if in_head {
            if let Some(piece) = &piece {
                head.push_str(piece);
            }
            let trimmed = head.trim_start();
            let upper: String = trimmed.chars().take(SUBJECT_MARK.len()).collect::<String>().to_ascii_uppercase();
            let could_be = SUBJECT_MARK.starts_with(&upper);
            let line_done = trimmed.contains('\n');
            if !ended && could_be && (upper.len() < SUBJECT_MARK.len() || !line_done) && chars(&head) < 500 {
                continue;
            }
            in_head = false;
            let (subject, body) = if upper == SUBJECT_MARK {
                let rest = &trimmed[SUBJECT_MARK.len()..];
                let (line, body) = rest.split_once('\n').unwrap_or((rest, ""));
                let subject = clean(line, 200);
                ((!subject.is_empty()).then_some(subject), body.to_owned())
            } else {
                (None, head.clone())
            };
            if let Some(subject) = subject {
                pieces.push(StreamEvent::Subject(subject));
            }
            head.clear();
            if !body.is_empty() {
                pieces.push(StreamEvent::Delta(body));
            }
        } else if let Some(piece) = piece {
            pieces.push(StreamEvent::Delta(piece));
        }
        for event in pieces.drain(..) {
            let event = match event {
                StreamEvent::Delta(text) if at_start => {
                    let text = text.trim_start_matches(['\n', '\r', ' ']).to_owned();
                    if text.is_empty() {
                        continue;
                    }
                    at_start = false;
                    StreamEvent::Delta(text)
                }
                other => other,
            };
            if to.send(event).await.is_err() {
                return;
            }
        }
        if ended {
            return;
        }
    }
}

impl Assist {
    /// One of the account's mails, without reading it: cheap, done before a request is taken.
    async fn record(&self, account: &Account, email_id: i64) -> Result<EmailRecord> {
        match self.store().email(account.id, email_id).await {
            Ok(record) => Ok(record),
            Err(StoreError::NotFound(_)) => Err(AssistError::NotFound(format!("email {email_id}"))),
            Err(err) => Err(err.into()),
        }
    }

    /// Reads a mail for a prompt: only after [`Assist::prepare`] said the request may be made.
    async fn text(&self, record: &EmailRecord, max_chars: usize) -> Result<MailText> {
        let raw = self.store().blob(&record.blob).await?;
        Ok(MailText::read(record, &raw, max_chars))
    }

    /// Asks the model, streaming when `events` listens.
    async fn ask(
        &self,
        ticket: Ticket<'_>,
        prompt: &Prompt,
        events: Option<&mpsc::Sender<StreamEvent>>,
        want_subject: bool,
    ) -> Result<(Completion, Effective)> {
        let Some(events) = events else {
            return self.send(ticket, prompt, None).await;
        };
        let (tx, rx) = mpsc::channel::<String>(64);
        let work = async move {
            let result = self.send(ticket, prompt, Some(&tx)).await;
            drop(tx);
            result
        };
        let (result, ()) = tokio::join!(work, relay(rx, events, want_subject));
        result
    }

    /// `Assist/compose`.
    pub async fn compose(
        &self,
        account: &Account,
        args: ComposeArgs,
        events: Option<&mpsc::Sender<StreamEvent>>,
    ) -> Result<ComposeResult> {
        check_compose(&args)?;
        let reply_to = match args.reply_to_email_id {
            Some(id) => Some(self.record(account, id).await?),
            None => None,
        };
        let ticket = self.prepare(account, "compose").await?.expecting(typical_compose(&args));
        let (prompt, want_subject) = self.compose_prompt(account, &args, reply_to.as_ref()).await?;
        let (completion, effective) = self.ask(ticket, &prompt, events, want_subject).await?;
        let (subject, text) =
            if want_subject { split_subject(&completion.text) } else { (None, completion.text.trim().to_owned()) };
        if text.is_empty() {
            return Err(AssistError::ProviderFailed {
                description: "the model gave an empty answer".into(),
                retry_after: None,
                transient: false,
            });
        }
        Ok(ComposeResult { text, subject, effective, usage: Usage::of(&completion) })
    }

    /// The prompt of `Assist/compose`, and whether it asks for a subject. Reads the mail replied to:
    /// only once the request may be made.
    async fn compose_prompt(
        &self,
        account: &Account,
        args: &ComposeArgs,
        reply_to: Option<&EmailRecord>,
    ) -> Result<(Prompt, bool)> {
        let instruction = args.instruction.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let text = args.text.as_deref().filter(|s| !s.trim().is_empty());
        let reply_to = match reply_to {
            Some(record) => Some(self.text(record, 8000).await?),
            None => None,
        };
        let sender = if account.display_name.trim().is_empty() {
            account.login.clone()
        } else {
            format!("{} <{}>", account.display_name.trim(), account.login)
        };
        let today = chrono::Utc::now().format("%A, %Y-%m-%d").to_string();
        let want_subject = args.mode == "write" && args.want_subject;
        let prompt = prompts::compose(&ComposeRequest {
            mode: &args.mode,
            instruction,
            preset: args.preset.as_deref(),
            target_language: args.target_language.as_deref(),
            text,
            subject: args.subject.as_deref(),
            reply_to: reply_to.as_ref(),
            want_subject,
            language: args.language.as_deref(),
            sender: &sender,
            today: &today,
        });
        Ok((prompt, want_subject))
    }

    /// `Assist/summarize`.
    pub async fn summarize(
        &self,
        account: &Account,
        args: SummarizeArgs,
        events: Option<&mpsc::Sender<StreamEvent>>,
    ) -> Result<SummaryResult> {
        let records = self.summary_records(account, &args).await?;
        let ticket = self.prepare(account, "summarize").await?.expecting(typical_summary(records.len()));
        let prompt = self.summary_prompt(&records, args.language.as_deref()).await?;
        let (completion, effective) = self.ask(ticket, &prompt, events, false).await?;
        let summary = completion.text.trim().to_owned();
        if summary.is_empty() {
            return Err(AssistError::ProviderFailed {
                description: "the model gave an empty answer".into(),
                retry_after: None,
                transient: false,
            });
        }
        Ok(SummaryResult { summary, effective, usage: Usage::of(&completion) })
    }

    /// The mails a summary is about, oldest first, without reading them.
    async fn summary_records(&self, account: &Account, args: &SummarizeArgs) -> Result<Vec<EmailRecord>> {
        let ids = match (args.email_id, args.thread_id) {
            (Some(email), None) => vec![email],
            (None, Some(thread)) => {
                let mut threads = self.store().thread_emails(account.id, vec![thread]).await?;
                let mut ids = threads.remove(&thread).unwrap_or_default();
                if ids.is_empty() {
                    return Err(AssistError::NotFound(format!("thread {thread}")));
                }
                let skip = ids.len().saturating_sub(MAX_THREAD_MAILS);
                ids.drain(..skip);
                ids
            }
            _ => return Err(invalid("emailId", "give either emailId or threadId")),
        };
        let mut records = Vec::new();
        for id in ids {
            records.push(self.record(account, id).await?);
        }
        records.sort_by_key(|record| (record.sent_at.unwrap_or(record.received_at), record.id));
        Ok(records)
    }

    /// The prompt of `Assist/summarize`: each mail cut to its share.
    async fn summary_prompt(&self, records: &[EmailRecord], language: Option<&str>) -> Result<Prompt> {
        let per_mail = (MAX_MAIL_CHARS / records.len().max(1)).max(2000);
        let mut texts: Vec<MailText> = Vec::new();
        for record in records {
            texts.push(self.text(record, per_mail).await?);
        }
        Ok(prompts::summarize(&texts, language))
    }

    /// What the server knows about a mail by itself.
    async fn spam_signals(&self, account: &Account, record: &EmailRecord, mail: &MailText) -> Result<SpamSignals> {
        let authentication = authentication(&mail.headers, self.hostname(), &record.from);
        let (spam_score, spam_threshold, tests) = spam_status(&mail.headers);
        let mailboxes = self.store().mailboxes(account.id).await?;
        let in_junk = mailboxes
            .iter()
            .any(|mailbox| mailbox.role == Some(MailboxRole::Junk) && record.mailbox_ids.contains(&mailbox.id));
        let address = record.from.first().map(|from| from.email.trim().to_lowercase()).filter(|a| !a.is_empty());
        let mut sender = SenderSignals { address: address.clone(), ..SenderSignals::default() };
        if let Some(address) = address {
            let SenderHistory { earlier_messages, earlier_in_junk, written_to, first_seen } =
                self.store().sender_history(account.id, address.clone(), record.id).await?;
            sender.earlier_messages = earlier_messages;
            sender.earlier_in_junk = earlier_in_junk;
            sender.written_to = written_to;
            sender.first_seen = first_seen.map(utc_date);
            sender.in_contacts = self
                .store()
                .contact_addresses(account.id, MAX_CONTACTS)
                .await?
                .iter()
                .any(|(_, email)| *email == address);
        }
        Ok(SpamSignals { authentication, spam_score, spam_threshold, tests, in_junk, sender })
    }

    /// `Assist/spamCheck`.
    pub async fn spam_check(&self, account: &Account, args: SpamArgs) -> Result<SpamResult> {
        let record = self.record(account, args.email_id).await?;
        let ticket = self.prepare(account, "spamCheck").await?.expecting(TYPICAL_SPAM_TOKENS);
        let (prompt, signals) = self.spam_prompt(account, &record, args.language.as_deref()).await?;
        let (completion, effective) = self.send(ticket, &prompt, None).await?;
        let (verdict, confidence, reasons) =
            parse_spam(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
                description: "the model's answer was not a verdict".into(),
                retry_after: None,
                transient: false,
            })?;
        Ok(SpamResult { verdict, confidence, reasons, signals, effective, usage: Usage::of(&completion) })
    }

    /// The prompt of `Assist/spamCheck`, with the facts it gives the model.
    async fn spam_prompt(
        &self,
        account: &Account,
        record: &EmailRecord,
        language: Option<&str>,
    ) -> Result<(Prompt, SpamSignals)> {
        let mail = self.text(record, MAX_MAIL_CHARS).await?;
        let signals = self.spam_signals(account, record, &mail).await?;
        let prompt = prompts::spam_check(&mail, &findings(&signals), language);
        Ok((prompt, signals))
    }

    /// The text of a mail's pictures for a prompt. With [`PictureRead::KnownOnly`], a picture that
    /// was never read stands in with [`UNREAD_PICTURE_CHARS`] characters.
    async fn picture_texts(&self, account: &Account, record: &EmailRecord, how: PictureRead) -> Vec<String> {
        let Some(read) = &self.inner.image_text else { return Vec::new() };
        let found = read(account.id, record.id, how).await.unwrap_or_default();
        let unread = if how == PictureRead::KnownOnly { found.unread } else { 0 };
        found
            .texts
            .iter()
            .map(|text| clean(text, MAX_PICTURE_CHARS))
            .chain(std::iter::repeat_n("x".repeat(UNREAD_PICTURE_CHARS), unread))
            .take(MAX_PICTURE_TEXTS)
            .collect()
    }

    /// `Assist/extractEvents`.
    pub async fn extract_events(&self, account: &Account, args: EventsArgs) -> Result<EventsResult> {
        let record = self.record(account, args.email_id).await?;
        let ticket = self.prepare(account, "extractEvents").await?.expecting(TYPICAL_EVENTS_TOKENS);
        let mail = self.text(&record, MAX_MAIL_CHARS).await?;
        let image_text = if args.include_images {
            self.picture_texts(account, &record, PictureRead::Read).await
        } else {
            Vec::new()
        };
        let prompt = prompts::extract_events(&mail, &image_text);
        let (completion, effective) = self.send(ticket, &prompt, None).await?;
        let answer = llm::json_answer(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
            description: "the model's answer was not a list of events".into(),
            retry_after: None,
            transient: false,
        })?;
        let mut people: Vec<(String, String)> = Vec::new();
        for address in record.from.iter().chain(&record.to).chain(&record.cc).take(100) {
            people.push((address.name.clone().unwrap_or_default(), address.email.trim().to_lowercase()));
        }
        people.extend(self.store().contact_addresses(account.id, MAX_CONTACTS).await?);
        let mut mine = HashSet::new();
        mine.insert(account.login.to_lowercase());
        for (_, email) in &people {
            if !mine.contains(email) && self.store().account_owns_address(account.id, email).await.unwrap_or(false) {
                mine.insert(email.clone());
            }
            if mine.len() > 50 {
                break;
            }
        }
        let mut source = mail.searchable();
        for text in &image_text {
            source.push('\n');
            source.push_str(text);
        }
        let context = EventContext { source: &source, links: &mail.links, people: &people, mine: &mine };
        let events = parse_events(&answer, &context);
        Ok(EventsResult { events, effective, usage: Usage::of(&completion) })
    }

    /// `Assist/estimate`: builds the prompt the call would send, from the same arguments, checks and
    /// provider choice, but sends nothing and counts nothing against the day's limits. Pictures are
    /// not read for it: their text is taken when it was read before, and estimated when not. Every
    /// call the request makes is counted (see [`plan_estimate`]), with what this provider's model
    /// took for the feature lately.
    pub async fn estimate(&self, account: &Account, args: EstimateArgs) -> Result<Estimate> {
        let _estimating = self.begin_estimate(account.id)?;
        let feature = args.feature();
        let (prompt, typical, pictures, (provider, model, _)) = match &args {
            EstimateArgs::Compose(args) => {
                check_compose(args)?;
                let reply_to = match args.reply_to_email_id {
                    Some(id) => Some(self.record(account, id).await?),
                    None => None,
                };
                let chosen = self.resolve(account, feature).await?;
                let (prompt, _) = self.compose_prompt(account, args, reply_to.as_ref()).await?;
                (prompt, typical_compose(args), Vec::new(), chosen)
            }
            EstimateArgs::Summarize(args) => {
                let records = self.summary_records(account, args).await?;
                let chosen = self.resolve(account, feature).await?;
                let prompt = self.summary_prompt(&records, args.language.as_deref()).await?;
                (prompt, typical_summary(records.len()), Vec::new(), chosen)
            }
            EstimateArgs::SpamCheck(args) => {
                let record = self.record(account, args.email_id).await?;
                let chosen = self.resolve(account, feature).await?;
                let (prompt, _) = self.spam_prompt(account, &record, args.language.as_deref()).await?;
                (prompt, TYPICAL_SPAM_TOKENS, Vec::new(), chosen)
            }
            EstimateArgs::ExtractEvents(args) => {
                let record = self.record(account, args.email_id).await?;
                let chosen = self.resolve(account, feature).await?;
                let mail = self.text(&record, MAX_MAIL_CHARS).await?;
                let image_text = if args.include_images {
                    self.picture_texts(account, &record, PictureRead::KnownOnly).await
                } else {
                    Vec::new()
                };
                (prompts::extract_events(&mail, &image_text), TYPICAL_EVENTS_TOKENS, image_text, chosen)
            }
        };
        let (requests_left_today, tokens_left_today) = self.left_today(account, &provider).await?;
        let price = self.price_of(&provider.record, provider.info, &model).await;
        let samples = self.store().assist_calibration(provider.record.id, &model, feature).await?;
        let calibration = Calibration::of(&samples);
        let plan = EstimatePlan {
            prompt: &prompt,
            shape: provider.info.shape,
            typical,
            reasoning: thinks(provider.info.shape, price.as_ref(), &model).then(|| typical_reasoning(feature)),
            image_count: pictures.len() as i64,
            image_tokens: llm::estimate_texts(pictures.iter().map(String::as_str)),
            price: price.as_ref(),
            show_cost: provider.shows_cost(),
        };
        let (calls, cost) = plan_estimate(&plan, &calibration);
        let weighted = |tokens: fn(&EstimateCall) -> i64| -> i64 {
            calls.iter().map(|call| tokens(call) as f64 * call.weight).sum::<f64>().round() as i64
        };
        Ok(Estimate {
            input_tokens: weighted(|call| call.input_tokens),
            output_tokens: weighted(|call| call.output_tokens),
            reasoning_tokens: weighted(|call| call.reasoning_tokens),
            image_count: plan.image_count,
            image_tokens: plan.image_tokens,
            calibrated: calibration.input_ratio.is_some(),
            calls,
            cost,
            effective: provider.effective(model),
            requests_left_today,
            tokens_left_today,
        })
    }

    /// Asks which of the person's labels fit one of their mails and puts them on. Labels already on
    /// the mail are left alone and not logged again.
    pub async fn label_email(&self, account: &Account, email_id: i64) -> Result<Vec<LabelPick>> {
        let labels = self.store().assist_labels(account.id).await?;
        if labels.is_empty() {
            return Ok(Vec::new());
        }
        let record = self.record(account, email_id).await?;
        let ticket = self.prepare(account, "autoLabels").await?.expecting(TYPICAL_LABEL_TOKENS);
        let mail = self.text(&record, LABEL_MAIL_CHARS).await?;
        let list: Vec<(String, String)> = labels.iter().map(|l| (l.name.clone(), l.description.clone())).collect();
        let prompt = prompts::labels(&mail, &list);
        let (completion, effective) = self.send(ticket, &prompt, None).await?;
        let answer = llm::json_answer(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
            description: "the model's answer was not a list of labels".into(),
            retry_after: None,
            transient: false,
        })?;
        let picks: Vec<LabelPick> = parse_labels(&answer, &labels)
            .into_iter()
            .filter(|pick| !record.keywords.contains(&pick.label.keyword))
            .collect();
        if picks.is_empty() {
            return Ok(picks);
        }
        let change = KeywordsChange::Patch(picks.iter().map(|pick| (pick.label.keyword.clone(), true)).collect());
        let update = uwumail_store::EmailUpdate { id: email_id, keywords: change, ..Default::default() };
        if let Some(Err(err)) = self.store().update_emails(account.id, vec![update]).await?.pop() {
            return Err(err.into());
        }
        for pick in &picks {
            self.store()
                .add_label_log(
                    account.id,
                    email_id,
                    pick.label.id,
                    pick.reason.clone(),
                    effective.provider_name.clone(),
                    effective.model.clone(),
                )
                .await?;
        }
        Ok(picks)
    }

    /// Takes one label's keyword off emails, in batches.
    pub(crate) async fn remove_keyword(&self, account_id: i64, keyword: &str, emails: &[i64]) -> Result<()> {
        for chunk in emails.chunks(500) {
            let updates = chunk
                .iter()
                .map(|id| uwumail_store::EmailUpdate {
                    id: *id,
                    keywords: KeywordsChange::Patch(vec![(keyword.to_owned(), false)]),
                    ..Default::default()
                })
                .collect();
            self.store().update_emails(account_id, updates).await?;
        }
        Ok(())
    }

    /// Removes a label and takes it off every email.
    pub async fn delete_label(&self, account: &Account, label_id: i64) -> Result<()> {
        let (keyword, emails) = match self.store().delete_assist_label(account.id, label_id).await {
            Ok(found) => found,
            Err(StoreError::NotFound(_)) => return Err(AssistError::NotFound(format!("label {label_id}"))),
            Err(err) => return Err(err.into()),
        };
        self.remove_keyword(account.id, &keyword, &emails).await
    }

    /// Takes a label the model set off its email again. `false` when there is no such entry.
    pub async fn undo_label(&self, account: &Account, log_id: i64) -> Result<bool> {
        let Some((email_id, keyword)) = self.store().undo_label_log(account.id, log_id).await? else {
            return Ok(false);
        };
        self.remove_keyword(account.id, &keyword, &[email_id]).await?;
        Ok(true)
    }

    /// `Assist/usage`: the person's rows (with the costs they may see) since `days` days ago (today included), and today.
    pub async fn usage(
        &self,
        account: &Account,
        days: u32,
    ) -> Result<(Vec<uwumail_store::UsageRow>, Vec<crate::TodayUsage>)> {
        let since = uwumail_store::utc_day(now() - i64::from(days.clamp(1, 400) - 1) * 86_400);
        let mut rows = self.store().assist_usage(Some(account.id), since).await?;
        // Costs only of the providers whose costs the person sees; a provider that is gone keeps
        // them to itself.
        let policy = self.store().assist_policy().await?;
        let shown: HashSet<i64> =
            self.available(account, &policy).await?.iter().filter(|a| a.shows_cost()).map(|a| a.record.id).collect();
        for row in &mut rows {
            if !shown.contains(&row.provider_id) {
                row.cost_usd = None;
            }
        }
        Ok((rows, self.today(account).await?))
    }
}

/// A typical answer of `Assist/compose`: a rewrite is about as long as the draft.
fn typical_compose(args: &ComposeArgs) -> i64 {
    match args.text.as_deref().filter(|_| args.mode != "write") {
        Some(text) => (llm::estimate_texts([text]) * 5 / 4).max(100),
        None => TYPICAL_WRITE_TOKENS,
    }
}

/// A typical summary of `mails` mails.
fn typical_summary(mails: usize) -> i64 {
    let more = mails.saturating_sub(1) as i64;
    (TYPICAL_SUMMARY_TOKENS + more * TYPICAL_SUMMARY_TOKENS_PER_MAIL).min(TYPICAL_SUMMARY_MAX_TOKENS)
}

/// Typical thinking for `feature` of a model that thinks.
fn typical_reasoning(feature: &str) -> i64 {
    match feature {
        "compose" => TYPICAL_REASONING_WRITE,
        "summarize" => TYPICAL_REASONING_SUMMARY,
        "spamCheck" => TYPICAL_REASONING_SPAM,
        "extractEvents" => TYPICAL_REASONING_EVENTS,
        _ => TYPICAL_REASONING_LABELS,
    }
}

/// The checks of `Assist/compose` that need nothing but its arguments.
fn check_compose(args: &ComposeArgs) -> Result<()> {
    let instruction = args.instruction.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = args.text.as_deref().filter(|s| !s.trim().is_empty());
    if instruction.is_some_and(|i| chars(i) > MAX_INSTRUCTION_CHARS) {
        return Err(invalid("instruction", format!("an instruction has at most {MAX_INSTRUCTION_CHARS} characters")));
    }
    if text.is_some_and(|t| chars(t) > MAX_TEXT_CHARS) {
        return Err(invalid("text", format!("the text has at most {MAX_TEXT_CHARS} characters")));
    }
    if args.subject.as_deref().is_some_and(|s| chars(s) > 998) {
        return Err(invalid("subject", "the subject is too long"));
    }
    if args.target_language.as_deref().is_some_and(|l| chars(l) > 60) {
        return Err(invalid("targetLanguage", "the language is too long"));
    }
    match args.mode.as_str() {
        "write" if instruction.is_none() => return Err(invalid("instruction", "write needs an instruction")),
        "write" => {}
        "rewrite" => {
            if text.is_none() {
                return Err(invalid("text", "rewrite needs the text"));
            }
            let preset = args.preset.as_deref().unwrap_or("");
            if prompts::preset_instruction(preset, None).is_none() {
                return Err(invalid("preset", format!("{preset:?} is not a preset")));
            }
        }
        "adjust" => {
            if text.is_none() || instruction.is_none() {
                return Err(invalid("instruction", "adjust needs the text and an instruction"));
            }
        }
        other => return Err(invalid("mode", format!("{other:?} is not a mode"))),
    }
    Ok(())
}

fn utc_date(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|date| date.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

/// SPF, DKIM and DMARC from the topmost `Authentication-Results` this server wrote.
pub fn authentication(
    headers: &[(String, String)],
    hostname: &str,
    from: &[uwumail_store::EmailAddress],
) -> AuthenticationSignals {
    let from_domain = from
        .first()
        .and_then(|address| address.email.rsplit_once('@'))
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
        .filter(|domain| !domain.is_empty());
    let mut signals = AuthenticationSignals { from_domain, ..AuthenticationSignals::default() };
    let ours = headers.iter().find(|(name, value)| {
        name.eq_ignore_ascii_case("Authentication-Results")
            && value.split(';').next().is_some_and(|id| id.trim().eq_ignore_ascii_case(hostname))
    });
    let Some((_, value)) = ours else { return signals };
    let mut dkim: Vec<String> = Vec::new();
    for part in value.split(';').skip(1) {
        let Some(first) = part.split_whitespace().next() else { continue };
        let Some((method, result)) = first.split_once('=') else { continue };
        let result: String =
            result.chars().filter(|c| c.is_ascii_alphanumeric()).take(20).collect::<String>().to_ascii_lowercase();
        if result.is_empty() {
            continue;
        }
        match method.to_ascii_lowercase().as_str() {
            "spf" if signals.spf.is_none() => signals.spf = Some(result),
            "dmarc" if signals.dmarc.is_none() => signals.dmarc = Some(result),
            "dkim" => dkim.push(result),
            _ => {}
        }
    }
    signals.dkim = if dkim.iter().any(|r| r == "pass") { Some("pass".into()) } else { dkim.into_iter().next() };
    signals
}

/// The server's spam filter verdict: `X-Spam-Status: Yes, score=6.0 required=5.0 tests=A,B`.
pub fn spam_status(headers: &[(String, String)]) -> (Option<f64>, Option<f64>, Vec<String>) {
    let Some((_, value)) = headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("X-Spam-Status")) else {
        return (None, None, Vec::new());
    };
    let (mut score, mut required, mut tests) = (None, None, Vec::new());
    for token in value.split([' ', ',', '\t']).filter(|t| !t.is_empty()) {
        if let Some(value) = token.strip_prefix("score=") {
            score = value.parse::<f64>().ok().filter(|v| v.is_finite());
        } else if let Some(value) = token.strip_prefix("required=") {
            required = value.parse::<f64>().ok().filter(|v| v.is_finite());
        } else if let Some(value) = token.strip_prefix("tests=") {
            if !value.eq_ignore_ascii_case("none") {
                tests.push(value.to_owned());
            }
        } else if !tests.is_empty() && !token.contains('=') {
            // `tests=A,B`, split at the comma above.
            tests.push(token.to_owned());
        }
    }
    tests.retain(|test| test.len() <= 60 && test.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    tests.truncate(30);
    (score, required, tests)
}

/// The signals as facts for the prompt.
fn findings(signals: &SpamSignals) -> String {
    let auth = &signals.authentication;
    let or_none = |value: &Option<String>| value.clone().unwrap_or_else(|| "not checked".into());
    let mut out = format!(
        "- SPF: {}\n- DKIM: {}\n- DMARC: {}\n- Domain of the From address: {}\n",
        or_none(&auth.spf),
        or_none(&auth.dkim),
        or_none(&auth.dmarc),
        auth.from_domain.clone().unwrap_or_else(|| "none".into())
    );
    match (signals.spam_score, signals.spam_threshold) {
        (Some(score), Some(threshold)) => {
            out.push_str(&format!("- Spam filter: {score:.1} points, Junk from {threshold:.1}\n"));
        }
        (Some(score), None) => out.push_str(&format!("- Spam filter: {score:.1} points\n")),
        _ => out.push_str("- Spam filter: did not look at this mail\n"),
    }
    if !signals.tests.is_empty() {
        out.push_str(&format!("- Spam filter rules that counted: {}\n", signals.tests.join(", ")));
    }
    out.push_str(&format!("- In the Junk folder now: {}\n", if signals.in_junk { "yes" } else { "no" }));
    let sender = &signals.sender;
    out.push_str(&format!(
        "- Earlier mails from this address: {} ({} of them in Junk); mails the reader sent to it: {}; in the reader's \
address book: {}",
        sender.earlier_messages,
        sender.earlier_in_junk,
        sender.written_to,
        if sender.in_contacts { "yes" } else { "no" }
    ));
    out
}

/// Verdict, confidence and reasons out of the model's answer.
pub fn parse_spam(text: &str) -> Option<(String, f64, Vec<String>)> {
    let answer = llm::json_answer(text)?;
    let verdict = answer.get("verdict")?.as_str()?.trim().to_ascii_lowercase();
    if !matches!(verdict.as_str(), "legitimate" | "suspicious" | "spam" | "phishing") {
        return None;
    }
    let confidence = answer.get("confidence").and_then(Value::as_f64).filter(|c| c.is_finite()).unwrap_or(0.5);
    let reasons = answer
        .get("reasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|reason| optional_text(Some(reason), MAX_REASON_CHARS))
        .take(MAX_REASONS)
        .collect();
    Some((verdict, confidence.clamp(0.0, 1.0), reasons))
}

/// Which of the person's labels the model chose, with its reasons. Names that are not labels are
/// dropped, each label counts once.
pub fn parse_labels(answer: &Value, labels: &[AssistLabel]) -> Vec<LabelPick> {
    let mut picks: Vec<LabelPick> = Vec::new();
    for entry in answer.get("labels").and_then(Value::as_array).into_iter().flatten().take(50) {
        let (name, reason) = match entry {
            Value::String(name) => (name.as_str(), ""),
            Value::Object(object) => (
                object.get("name").and_then(Value::as_str).unwrap_or_default(),
                object.get("reason").and_then(Value::as_str).unwrap_or_default(),
            ),
            _ => continue,
        };
        let name = name.trim();
        let Some(label) = labels.iter().find(|label| label.name.trim().to_lowercase() == name.to_lowercase()) else {
            continue;
        };
        if picks.iter().any(|pick| pick.label.id == label.id) {
            continue;
        }
        picks.push(LabelPick { label: label.clone(), reason: clean(&reason.replace('\n', " "), MAX_REASON_CHARS) });
    }
    picks
}

/// What an extracted event is checked against.
pub struct EventContext<'a> {
    /// All the text the event may be read from: subject, body, links, text in pictures.
    pub source: &'a str,
    pub links: &'a [String],
    /// Names and addresses (lower case) of the mail's From, To and Cc and of the address book.
    pub people: &'a [(String, String)],
    /// The person's own addresses, never suggested.
    pub mine: &'a HashSet<String>,
}

/// Lower case, one space between words, without quotation marks: to find a quote in the mail even
/// when the model changed its spacing.
fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '"' | '\'' | '„' | '“' | '”' | '‚' | '‘' | '’' | '«' | '»' | '*'))
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

enum When {
    At(NaiveDateTime),
    Day(NaiveDate),
}

fn parse_when(text: &str) -> Option<When> {
    let text = text.trim();
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(at) = NaiveDateTime::parse_from_str(text, format) {
            return Some(When::At(at));
        }
    }
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(When::At(at.naive_local()));
    }
    if let Some(stripped) = text.strip_suffix('Z')
        && let Ok(at) = NaiveDateTime::parse_from_str(stripped, "%Y-%m-%dT%H:%M:%S")
    {
        return Some(When::At(at));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().map(When::Day)
}

fn local(at: NaiveDateTime) -> String {
    at.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn sane(at: NaiveDateTime) -> bool {
    (1970..=2200).contains(&chrono::Datelike::year(&at))
}

/// The people the model named, as addresses the person knows: from the mail's From, To and Cc or
/// the address book. Others, and the person themselves, are left out.
fn participants(named: &[Value], context: &EventContext<'_>) -> Vec<Participant> {
    let mut out: Vec<Participant> = Vec::new();
    for entry in named.iter().take(40) {
        let text = match entry {
            Value::String(text) => text.clone(),
            Value::Object(object) => object
                .get("email")
                .and_then(Value::as_str)
                .filter(|e| !e.trim().is_empty())
                .or_else(|| object.get("name").and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned(),
            _ => continue,
        };
        let text = text.trim();
        if text.is_empty() || chars(text) > 320 {
            continue;
        }
        let found = if let Some(address) = text
            .split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | ',' | ';'))
            .find(|word| word.contains('@'))
        {
            let address = address.to_lowercase();
            context.people.iter().find(|(_, email)| *email == address).cloned()
        } else {
            let wanted = normalized(text);
            let exact: Vec<&(String, String)> =
                context.people.iter().filter(|(name, _)| !name.is_empty() && normalized(name) == wanted).collect();
            match exact.first() {
                Some(first) if exact.iter().all(|p| p.1 == first.1) => Some((*first).clone()),
                _ => {
                    // "Leni" for "Leni Beispiel", when only one person is meant.
                    let first_names: Vec<&(String, String)> = context
                        .people
                        .iter()
                        .filter(|(name, _)| normalized(name).split(' ').next().is_some_and(|first| first == wanted))
                        .collect();
                    match first_names.first() {
                        Some(first) if !wanted.contains(' ') && first_names.iter().all(|p| p.1 == first.1) => {
                            Some((*first).clone())
                        }
                        _ => None,
                    }
                }
            }
        };
        let Some((name, email)) = found else { continue };
        if context.mine.contains(&email) || out.iter().any(|p| p.email == email) {
            continue;
        }
        let name = clean(&name, 100);
        out.push(Participant { name: (!name.is_empty()).then_some(name), email });
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// The events out of the model's answer, each one checked: a date that is one, an end after the
/// start, a quote that stands in the mail, a link that is in it, people the person knows.
pub fn parse_events(answer: &Value, context: &EventContext<'_>) -> Vec<ExtractedEvent> {
    let source = normalized(context.source);
    let mut events = Vec::new();
    for entry in answer.get("events").and_then(Value::as_array).into_iter().flatten().take(MAX_EVENTS * 3) {
        let Some(title) = optional_text(entry.get("title"), 200) else { continue };
        let Some(start) = entry.get("start").and_then(Value::as_str).and_then(parse_when) else { continue };
        let mut all_day = entry.get("allDay").and_then(Value::as_bool).unwrap_or(false);
        let start = match start {
            When::At(at) => at,
            When::Day(day) => {
                all_day = true;
                day.and_hms_opt(0, 0, 0).unwrap_or_default()
            }
        };
        let start = if all_day { start.date().and_hms_opt(0, 0, 0).unwrap_or(start) } else { start };
        if !sane(start) {
            continue;
        }
        let default_end = if all_day { start + TimeDelta::days(1) } else { start + TimeDelta::hours(1) };
        let end = match entry.get("end").and_then(Value::as_str).and_then(parse_when) {
            Some(When::At(at)) if all_day => at.date().and_hms_opt(0, 0, 0).unwrap_or(at),
            Some(When::At(at)) => at,
            Some(When::Day(day)) => {
                let day = day.and_hms_opt(0, 0, 0).unwrap_or_default();
                // The last day, as people write it: the end is the day after.
                if all_day { day + TimeDelta::days(1) } else { day }
            }
            None => default_end,
        };
        let end = if end <= start || !sane(end) || end - start > TimeDelta::days(366) { default_end } else { end };
        let Some(quote) = optional_text(entry.get("quote"), MAX_QUOTE_CHARS) else { continue };
        let wanted = normalized(quote.trim_matches(['…', '.']));
        if wanted.is_empty() || !source.contains(&wanted) {
            continue;
        }
        let time_zone = optional_text(entry.get("timeZone"), 64).filter(|zone| zone.parse::<chrono_tz::Tz>().is_ok());
        let url = optional_text(entry.get("url"), 300).filter(|url| {
            url.starts_with("https://") && (context.links.contains(url) || context.source.contains(url.as_str()))
        });
        let named = entry.get("participants").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let confidence = entry.get("confidence").and_then(Value::as_f64).filter(|c| c.is_finite()).unwrap_or(0.5);
        events.push(ExtractedEvent {
            title,
            start: local(start),
            end: local(end),
            all_day,
            time_zone,
            location: optional_text(entry.get("location"), 300),
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .map(|text| clean(text, 1000))
                .filter(|text| !text.is_empty()),
            url,
            participants: participants(named, context),
            confidence: confidence.clamp(0.0, 1.0),
            quote,
        });
        if events.len() >= MAX_EVENTS {
            break;
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn subjects_come_off_the_first_line() {
        assert_eq!(split_subject("SUBJECT: Freitag\n\nHallo Mia"), (Some("Freitag".into()), "Hallo Mia".into()));
        assert_eq!(split_subject("subject:Hi\nText"), (Some("Hi".into()), "Text".into()));
        assert_eq!(split_subject("Hallo Mia"), (None, "Hallo Mia".into()));
        assert_eq!(split_subject("Süß"), (None, "Süß".into()));
    }

    #[tokio::test]
    async fn the_stream_hands_on_the_subject_first() {
        let (tx, rx) = mpsc::channel(8);
        let (out, mut events) = mpsc::channel(8);
        let feed = async move {
            for piece in ["SUBJ", "ECT: Frei", "tag\n", "\nHallo", " Mia"] {
                tx.send(piece.to_owned()).await.unwrap();
            }
        };
        let (_, ()) = tokio::join!(feed, relay(rx, &out, true));
        drop(out);
        let mut got = Vec::new();
        while let Some(event) = events.recv().await {
            got.push(event);
        }
        assert_eq!(
            got,
            [
                StreamEvent::Subject("Freitag".into()),
                StreamEvent::Delta("Hallo".into()),
                StreamEvent::Delta(" Mia".into())
            ]
        );

        // Without a subject line, the text goes on as it came.
        let (tx, rx) = mpsc::channel(8);
        let (out, mut events) = mpsc::channel(8);
        let feed = async move {
            for piece in ["Hä", "llo"] {
                tx.send(piece.to_owned()).await.unwrap();
            }
        };
        let (_, ()) = tokio::join!(feed, relay(rx, &out, true));
        drop(out);
        assert_eq!(events.recv().await, Some(StreamEvent::Delta("Hä".into())));
        assert_eq!(events.recv().await, Some(StreamEvent::Delta("llo".into())));
    }

    #[test]
    fn authentication_is_read_from_our_own_header_only() {
        let headers = vec![
            ("Authentication-Results".to_owned(), "mx.example.org; spf=fail smtp.mailfrom=x@bank.example; dkim=none; dkim=pass header.d=bank.example; dmarc=fail header.from=bank.example".to_owned()),
            ("Authentication-Results".to_owned(), "evil.example; spf=pass; dmarc=pass".to_owned()),
            ("X-Spam-Status".to_owned(), "Yes, score=6.0 required=5.0 tests=SPF_FAIL,SPAMHAUS_ZEN".to_owned()),
        ];
        let from = [uwumail_store::EmailAddress { name: None, email: "service@Bank.example".into() }];
        let auth = authentication(&headers, "MX.example.org", &from);
        assert_eq!(auth.spf.as_deref(), Some("fail"));
        assert_eq!(auth.dkim.as_deref(), Some("pass"));
        assert_eq!(auth.dmarc.as_deref(), Some("fail"));
        assert_eq!(auth.from_domain.as_deref(), Some("bank.example"));
        assert_eq!(authentication(&headers[1..], "mx.example.org", &from).spf, None, "a stranger's claim");
        assert_eq!(spam_status(&headers), (Some(6.0), Some(5.0), vec!["SPF_FAIL".into(), "SPAMHAUS_ZEN".into()]));
        let none = [("X-Spam-Status".to_owned(), "No, score=0.0 required=5.0 tests=none".to_owned())];
        assert_eq!(spam_status(&none), (Some(0.0), Some(5.0), vec![]));
    }

    #[test]
    fn spam_answers_are_held_to_their_shape() {
        let (verdict, confidence, reasons) =
            parse_spam(r#"{"verdict": "Phishing", "confidence": 7, "reasons": ["a", "", "b\nc"]}"#).unwrap();
        assert_eq!((verdict.as_str(), confidence), ("phishing", 1.0));
        assert_eq!(reasons, ["a", "b c"]);
        assert!(parse_spam(r#"{"verdict": "delete all mail"}"#).is_none());
    }

    fn label(id: i64, name: &str) -> AssistLabel {
        AssistLabel {
            id,
            name: name.into(),
            description: String::new(),
            keyword: uwumail_store::label_keyword(name),
            color: None,
            created_at: 0,
        }
    }

    #[test]
    fn only_the_persons_labels_are_picked() {
        let labels = [label(1, "Rechnungen"), label(2, "Reisen")];
        let answer = json!({ "labels": [
            { "name": "rechnungen", "reason": "Eine Rechnung" },
            { "name": "Rechnungen", "reason": "again" },
            { "name": "Delete everything", "reason": "x" },
            "Reisen"
        ]});
        let picks = parse_labels(&answer, &labels);
        assert_eq!(picks.iter().map(|p| p.label.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(picks[0].reason, "Eine Rechnung");
    }

    #[test]
    fn events_are_checked_against_the_mail() {
        let people = vec![
            ("Leni Beispiel".to_owned(), "leni@example.org".to_owned()),
            ("Mia".to_owned(), "mia@example.org".to_owned()),
        ];
        let mine: HashSet<String> = ["mia@example.org".to_owned()].into();
        let links = vec!["https://praxis.example/termin".to_owned()];
        let source = "Ihr Termin am Dienstag, 6. Oktober um 9:30 Uhr in der Praxis.\nhttps://praxis.example/termin";
        let context = EventContext { source, links: &links, people: &people, mine: &mine };
        let answer = json!({ "events": [
            {
                "title": "Zahnarzt", "start": "2026-10-06T09:30:00", "end": null, "allDay": false,
                "timeZone": "Europe/Berlin", "location": "Praxis", "description": null,
                "url": "https://praxis.example/termin", "participants": ["Leni", "Mia", "Unbekannt"],
                "confidence": 0.9, "quote": "Ihr Termin am  Dienstag, 6. Oktober um 9:30 Uhr"
            },
            { "title": "Made up", "start": "2026-10-07", "quote": "not in the mail", "participants": [] },
            { "title": "Urlaub", "start": "2026-10-10", "end": "2026-10-12", "allDay": true, "timeZone": "Mars/Base",
              "url": "https://evil.example/", "quote": "in der Praxis", "participants": [], "confidence": "high" },
            { "title": "Bad", "start": "next Tuesday", "quote": "Praxis", "participants": [] }
        ]});
        let events = parse_events(&answer, &context);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].end, "2026-10-06T10:30:00", "an hour without an end");
        assert_eq!(events[0].time_zone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(events[0].url.as_deref(), Some("https://praxis.example/termin"));
        assert_eq!(
            events[0].participants,
            [Participant { name: Some("Leni Beispiel".into()), email: "leni@example.org".into() }]
        );
        assert_eq!((events[1].start.as_str(), events[1].end.as_str()), ("2026-10-10T00:00:00", "2026-10-13T00:00:00"));
        assert!(events[1].all_day && events[1].time_zone.is_none() && events[1].url.is_none());
        assert_eq!(events[1].confidence, 0.5);
    }
}
