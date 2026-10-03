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
use crate::foreign::{ForeignLabel, ForeignMail, MAX_FOREIGN_MAILS};
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
/// Contact domains the phishing checks compare a sender with, at most.
const MAX_CONTACT_DOMAINS: usize = 2000;
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
pub(crate) const TYPICAL_LABEL_TOKENS_PER_LABEL: i64 = 40;
/// … and what proposing new labels adds.
const TYPICAL_NEW_LABEL_TOKENS: i64 = 120;
/// New labels `AssistLabel/suggest` proposes, at most.
const MAX_NEW_LABELS: usize = 2;
const MAX_LABEL_NAME_CHARS: usize = 40;
const MAX_LABEL_DESCRIPTION_CHARS: usize = 300;
/// Colors for a proposed label whose own color is no `#rrggbb`.
const LABEL_COLORS: [&str; 8] =
    ["#e5484d", "#f76b15", "#ffc53d", "#30a46c", "#12a594", "#0090ff", "#8e4ec6", "#d6409f"];
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
    /// The mail answered, of another account, instead of `reply_to_email_id`: exactly one.
    pub foreign_mails: Vec<ForeignMail>,
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
    /// Mails of another account instead of the ids, oldest first.
    pub foreign_mails: Vec<ForeignMail>,
}

#[derive(Debug, Clone)]
pub struct SummaryResult {
    pub summary: String,
    pub effective: Effective,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default)]
pub struct SpamArgs {
    /// Not looked at with `foreign_mails`.
    pub email_id: i64,
    pub language: Option<String>,
    /// A mail of another account instead of `email_id`: exactly one.
    pub foreign_mails: Vec<ForeignMail>,
}

/// SPF, DKIM and DMARC as this server's `Authentication-Results` recorded them (for a foreign
/// mail: the topmost ones, of the other provider).
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
    /// `None` for a mail of another account: the server knows nothing of its history.
    pub sender: Option<SenderSignals>,
}

#[derive(Debug, Clone)]
pub struct SpamResult {
    pub verdict: String,
    pub confidence: f64,
    /// The reasons that cite something real, as text.
    pub reasons: Vec<String>,
    /// The same reasons with what each one cites.
    pub reason_details: Vec<crate::spam::Reason>,
    /// Reasons of the model that cited nothing real or contradicted the facts, and were dropped.
    pub dropped_reasons: usize,
    /// The model's own verdict, only when it was outside what the facts allow and was moved.
    pub model_verdict: Option<String>,
    /// What the facts say: score, band and evidence.
    pub assessment: crate::spam::Assessment,
    pub signals: SpamSignals,
    pub effective: Effective,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default)]
pub struct EventsArgs {
    /// Not looked at with `foreign_mails`.
    pub email_id: i64,
    pub include_images: bool,
    /// A mail of another account instead of `email_id`: exactly one.
    pub foreign_mails: Vec<ForeignMail>,
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

#[derive(Debug, Clone)]
pub struct SuggestArgs {
    /// Not looked at with `foreign_mails`.
    pub email_id: i64,
    pub suggest_new: bool,
    pub language: Option<String>,
    /// A mail of another account instead of `email_id`: exactly one, judged by `foreign_labels`.
    pub foreign_mails: Vec<ForeignMail>,
    pub foreign_labels: Vec<ForeignLabel>,
}

impl Default for SuggestArgs {
    fn default() -> Self {
        SuggestArgs {
            email_id: 0,
            suggest_new: true,
            language: None,
            foreign_mails: Vec::new(),
            foreign_labels: Vec::new(),
        }
    }
}

/// What the model says of one label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelVerdict {
    /// `None` for a label of another account.
    pub label_id: Option<i64>,
    pub name: String,
    pub reason: String,
    pub fits: bool,
    pub is_set: bool,
}

/// A label the model proposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewLabel {
    pub name: String,
    pub description: String,
    pub color: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct SuggestResult {
    pub verdicts: Vec<LabelVerdict>,
    pub new_labels: Vec<NewLabel>,
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
    Suggest(SuggestArgs),
}

impl EstimateArgs {
    fn feature(&self) -> &'static str {
        match self {
            EstimateArgs::Compose(_) => "compose",
            EstimateArgs::Summarize(_) => "summarize",
            EstimateArgs::SpamCheck(_) => "spamCheck",
            EstimateArgs::ExtractEvents(_) => "extractEvents",
            EstimateArgs::Suggest(_) => "autoLabels",
        }
    }

    /// Whether the call is about mail of another account.
    fn foreign(&self) -> bool {
        !match self {
            EstimateArgs::Compose(args) => &args.foreign_mails,
            EstimateArgs::Summarize(args) => &args.foreign_mails,
            EstimateArgs::SpamCheck(args) => &args.foreign_mails,
            EstimateArgs::ExtractEvents(args) => &args.foreign_mails,
            EstimateArgs::Suggest(args) => &args.foreign_mails,
        }
        .is_empty()
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

/// A label put on a mail, why, by which way (`rule`, `detector`, `sender`, `similar`,
/// `classifier`, `ai`) and how sure.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelPick {
    pub label: AssistLabel,
    pub reason: String,
    pub source: &'static str,
    pub confidence: f64,
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
        let foreign = !args.foreign_mails.is_empty();
        let ticket = self.prepare_for(account, "compose", foreign).await?.expecting(typical_compose(&args));
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
        let reply_to = match (reply_to, args.foreign_mails.first()) {
            (Some(record), _) => Some(self.text(record, 8000).await?),
            (None, Some(foreign)) => Some(foreign.mail_text(8000)),
            (None, None) => None,
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
        let foreign = !args.foreign_mails.is_empty();
        let mails = records.len() + args.foreign_mails.len();
        let ticket = self.prepare_for(account, "summarize", foreign).await?.expecting(typical_summary(mails));
        let prompt = self.summary_prompt(&records, &args.foreign_mails, args.language.as_deref()).await?;
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
        if !args.foreign_mails.is_empty() {
            if args.email_id.is_some() || args.thread_id.is_some() {
                return Err(invalid("foreignMails", "give either foreignMails or emailId/threadId"));
            }
            if args.foreign_mails.len() > MAX_FOREIGN_MAILS {
                return Err(invalid("foreignMails", format!("at most {MAX_FOREIGN_MAILS} foreign mails")));
            }
            return Ok(Vec::new());
        }
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
    async fn summary_prompt(
        &self,
        records: &[EmailRecord],
        foreign: &[ForeignMail],
        language: Option<&str>,
    ) -> Result<Prompt> {
        let per_mail = (MAX_MAIL_CHARS / (records.len() + foreign.len()).max(1)).max(2000);
        let mut texts: Vec<MailText> = Vec::new();
        for record in records {
            texts.push(self.text(record, per_mail).await?);
        }
        texts.extend(foreign.iter().map(|mail| mail.mail_text(per_mail)));
        Ok(prompts::summarize(&texts, language))
    }

    /// What the server knows about a mail by itself.
    async fn spam_signals(
        &self,
        account: &Account,
        record: &EmailRecord,
        mail: &MailText,
        contacts: &[(String, String)],
    ) -> Result<SpamSignals> {
        let authentication = authentication(&mail.headers, Some(self.hostname()), &record.from);
        let (spam_score, spam_threshold, tests) = spam_status(trusted_headers(&mail.headers, Some(self.hostname())));
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
            sender.in_contacts = contacts.iter().any(|(_, email)| email.trim().eq_ignore_ascii_case(&address));
        }
        Ok(SpamSignals { authentication, spam_score, spam_threshold, tests, in_junk, sender: Some(sender) })
    }

    /// What the headers of a mail of another account say: its provider's findings.
    fn foreign_spam_signals(mail: &ForeignMail) -> SpamSignals {
        let authentication = authentication(&mail.headers, None, &mail.from);
        let (spam_score, spam_threshold, tests) = spam_status(trusted_headers(&mail.headers, None));
        SpamSignals { authentication, spam_score, spam_threshold, tests, in_junk: mail.in_junk, sender: None }
    }

    /// `Assist/spamCheck`: the facts decide the range of verdicts, the model chooses within it and
    /// explains, and reasons that cite nothing real are dropped (see [`crate::spam`]).
    pub async fn spam_check(&self, account: &Account, args: SpamArgs) -> Result<SpamResult> {
        let (ticket, check) = if let Some(foreign) = one_foreign(&args.foreign_mails)? {
            let ticket = self.prepare_for(account, "spamCheck", true).await?.expecting(TYPICAL_SPAM_TOKENS);
            (ticket, self.foreign_spam_prompt(account, foreign, args.language.as_deref()).await?)
        } else {
            let record = self.record(account, args.email_id).await?;
            let ticket = self.prepare(account, "spamCheck").await?.expecting(TYPICAL_SPAM_TOKENS);
            (ticket, self.spam_prompt(account, &record, args.language.as_deref()).await?)
        };
        let (completion, effective) = self.send(ticket, &check.prompt, None).await?;
        let (verdict, confidence, reasons) =
            parse_spam(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
                description: "the model's answer was not a verdict".into(),
                retry_after: None,
                transient: false,
            })?;
        let (verdict, confidence, model_verdict) = crate::spam::settle(&check.assessment, &verdict, confidence);
        let (reason_details, dropped_reasons) =
            crate::spam::verify(reasons, &check.mail, &check.shape, &check.facts, &check.signals, MAX_REASONS);
        Ok(SpamResult {
            verdict,
            confidence,
            reasons: reason_details.iter().map(|reason| reason.text.clone()).collect(),
            reason_details,
            dropped_reasons,
            model_verdict,
            assessment: check.assessment,
            signals: check.signals,
            effective,
            usage: Usage::of(&completion),
        })
    }

    /// The domains of the person's contacts, for lookalikes of a partner's domain.
    async fn contact_domains(&self, account: &Account) -> Result<(Vec<String>, Vec<(String, String)>)> {
        let contacts = self.store().contact_addresses(account.id, MAX_CONTACTS).await?;
        let mut domains: Vec<String> =
            contacts.iter().filter_map(|(_, email)| uwumail_smtp::phishing::domain_of(email)).collect();
        domains.sort();
        domains.dedup();
        domains.truncate(MAX_CONTACT_DOMAINS);
        Ok((domains, contacts))
    }

    /// The prompt of `Assist/spamCheck`, with the facts it gives the model.
    async fn spam_prompt(
        &self,
        account: &Account,
        record: &EmailRecord,
        language: Option<&str>,
    ) -> Result<SpamCheckPrompt> {
        let raw = self.store().blob(&record.blob).await?;
        let (domains, contacts) = self.contact_domains(account).await?;
        // Parsing and the phishing checks read a whole message the sender wrote: off the async
        // runtime, so one large or crafted mail does not hold up others (security review SPAM-4).
        let owned = record.clone();
        let hostname = self.hostname().to_owned();
        let (mail, phishing, attachments) = tokio::task::spawn_blocking(move || {
            let mail = MailText::read(&owned, &raw, MAX_MAIL_CHARS);
            let auth = authentication(&mail.headers, Some(&hostname), &owned.from);
            let phishing = uwumail_smtp::phishing::check_message(&raw, &domains, crate::spam::authentic(&auth));
            (mail, phishing, record_attachments(&raw))
        })
        .await
        .map_err(|err| AssistError::Store(uwumail_store::StoreError::Internal(err.to_string())))?;
        let signals = self.spam_signals(account, record, &mail, &contacts).await?;
        let shape = crate::spam::MailShape { has_links: !mail.links.is_empty(), attachments: Some(attachments) };
        Ok(spam_check_prompt(mail, signals, &phishing, shape, language))
    }

    /// The prompt of `Assist/spamCheck` for a mail of another account, with its provider's findings.
    async fn foreign_spam_prompt(
        &self,
        account: &Account,
        foreign: &ForeignMail,
        language: Option<&str>,
    ) -> Result<SpamCheckPrompt> {
        let (domains, _) = self.contact_domains(account).await?;
        let foreign = foreign.clone();
        let language = language.map(str::to_owned);
        tokio::task::spawn_blocking(move || foreign_spam_check_prompt(&foreign, &domains, language.as_deref()))
            .await
            .map_err(|err| AssistError::Store(uwumail_store::StoreError::Internal(err.to_string())))
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
        let (ticket, mail, image_text) = if let Some(foreign) = one_foreign(&args.foreign_mails)? {
            if args.include_images {
                return Err(invalid("includeImages", "pictures of a foreign mail can't be read"));
            }
            let ticket = self.prepare_for(account, "extractEvents", true).await?.expecting(TYPICAL_EVENTS_TOKENS);
            (ticket, foreign.mail_text(MAX_MAIL_CHARS), Vec::new())
        } else {
            let record = self.record(account, args.email_id).await?;
            let ticket = self.prepare(account, "extractEvents").await?.expecting(TYPICAL_EVENTS_TOKENS);
            let mail = self.text(&record, MAX_MAIL_CHARS).await?;
            let image_text = if args.include_images {
                self.picture_texts(account, &record, PictureRead::Read).await
            } else {
                Vec::new()
            };
            (ticket, mail, image_text)
        };
        let prompt = prompts::extract_events(&mail, &image_text);
        let (completion, effective) = self.send(ticket, &prompt, None).await?;
        let answer = llm::json_answer(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
            description: "the model's answer was not a list of events".into(),
            retry_after: None,
            transient: false,
        })?;
        let mut people: Vec<(String, String)> = Vec::new();
        for address in mail.from.iter().chain(&mail.to).chain(&mail.cc).take(100) {
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
        let foreign = args.foreign();
        // Whether the feature is on and which provider would answer comes first: without them
        // nothing would be sent, so no mail is read for the estimate either.
        let (provider, model, _) = self.resolve_for(account, feature, foreign).await?;
        let (prompt, typical, pictures) = match &args {
            EstimateArgs::Compose(args) => {
                check_compose(args)?;
                let reply_to = match args.reply_to_email_id {
                    Some(id) => Some(self.record(account, id).await?),
                    None => None,
                };
                let (prompt, _) = self.compose_prompt(account, args, reply_to.as_ref()).await?;
                (prompt, typical_compose(args), Vec::new())
            }
            EstimateArgs::Summarize(args) => {
                let records = self.summary_records(account, args).await?;
                let prompt = self.summary_prompt(&records, &args.foreign_mails, args.language.as_deref()).await?;
                (prompt, typical_summary(records.len() + args.foreign_mails.len()), Vec::new())
            }
            EstimateArgs::SpamCheck(args) => {
                let prompt = if let Some(foreign) = one_foreign(&args.foreign_mails)? {
                    self.foreign_spam_prompt(account, foreign, args.language.as_deref()).await?.prompt
                } else {
                    let record = self.record(account, args.email_id).await?;
                    self.spam_prompt(account, &record, args.language.as_deref()).await?.prompt
                };
                (prompt, TYPICAL_SPAM_TOKENS, Vec::new())
            }
            EstimateArgs::ExtractEvents(args) => {
                let (mail, image_text) = if let Some(foreign) = one_foreign(&args.foreign_mails)? {
                    if args.include_images {
                        return Err(invalid("includeImages", "pictures of a foreign mail can't be read"));
                    }
                    (foreign.mail_text(MAX_MAIL_CHARS), Vec::new())
                } else {
                    let record = self.record(account, args.email_id).await?;
                    let mail = self.text(&record, MAX_MAIL_CHARS).await?;
                    let image_text = if args.include_images {
                        self.picture_texts(account, &record, PictureRead::KnownOnly).await
                    } else {
                        Vec::new()
                    };
                    (mail, image_text)
                };
                (prompts::extract_events(&mail, &image_text), TYPICAL_EVENTS_TOKENS, image_text)
            }
            EstimateArgs::Suggest(args) => {
                let request = self.suggest_request(account, args).await?;
                let typical = typical_suggest(request.labels.len(), request.room);
                (request.prompt(args.language.as_deref()), typical, Vec::new())
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

    /// Takes one label's keyword off emails, in batches: as the person (`by_hand`, which teaches the
    /// label's learning) or as the server.
    pub(crate) async fn remove_keyword(
        &self,
        account_id: i64,
        keyword: &str,
        emails: &[i64],
        by_hand: bool,
    ) -> Result<()> {
        for chunk in emails.chunks(500) {
            let updates = chunk
                .iter()
                .map(|id| uwumail_store::EmailUpdate {
                    id: *id,
                    keywords: KeywordsChange::Patch(vec![(keyword.to_owned(), false)]),
                    ..Default::default()
                })
                .collect();
            if by_hand {
                self.store().update_emails(account_id, updates).await?;
            } else {
                self.store().update_emails_by_server(account_id, updates).await?;
            }
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
        self.remove_keyword(account.id, &keyword, &emails, false).await
    }

    /// Takes a label that was put on by itself off its email again, as the person would. `false`
    /// when there is no such entry.
    pub async fn undo_label(&self, account: &Account, log_id: i64) -> Result<bool> {
        let Some((email_id, keyword)) = self.store().undo_label_log(account.id, log_id).await? else {
            return Ok(false);
        };
        self.remove_keyword(account.id, &keyword, &[email_id], true).await?;
        Ok(true)
    }

    /// What `AssistLabel/suggest` asks about, without reading the mail but a foreign one.
    async fn suggest_request(&self, account: &Account, args: &SuggestArgs) -> Result<SuggestRequest> {
        if let Some(foreign) = one_foreign(&args.foreign_mails)? {
            let labels = args
                .foreign_labels
                .iter()
                .map(|label| SuggestLabel {
                    id: None,
                    name: label.name.clone(),
                    description: label.description.clone(),
                    is_set: label.is_set,
                })
                .collect();
            let room = if args.suggest_new { MAX_NEW_LABELS } else { 0 };
            return Ok(SuggestRequest { labels, room, mail: foreign.mail_text(LABEL_MAIL_CHARS) });
        }
        if !args.foreign_labels.is_empty() {
            return Err(invalid("foreignLabels", "foreignLabels go with foreignMails"));
        }
        let record = self.record(account, args.email_id).await?;
        let labels: Vec<SuggestLabel> = self
            .store()
            .assist_labels(account.id)
            .await?
            .into_iter()
            .map(|label| SuggestLabel {
                id: Some(label.id),
                is_set: record.keywords.contains(&label.keyword),
                name: label.name,
                description: label.description,
            })
            .collect();
        let room = if args.suggest_new {
            MAX_NEW_LABELS.min(uwumail_store::ASSIST_MAX_LABELS.saturating_sub(labels.len()))
        } else {
            0
        };
        let mail = self.text(&record, LABEL_MAIL_CHARS).await?;
        Ok(SuggestRequest { labels, room, mail })
    }

    /// `AssistLabel/suggest`: what the model says of every label for one mail, and new labels when
    /// none fits. Changes nothing.
    pub async fn suggest_labels(&self, account: &Account, args: SuggestArgs) -> Result<SuggestResult> {
        let foreign = !args.foreign_mails.is_empty();
        if foreign {
            one_foreign(&args.foreign_mails)?;
        } else {
            if !args.foreign_labels.is_empty() {
                return Err(invalid("foreignLabels", "foreignLabels go with foreignMails"));
            }
            self.record(account, args.email_id).await?;
        }
        let ticket = self.prepare_for(account, "autoLabels", foreign).await?;
        let request = self.suggest_request(account, &args).await?;
        let ticket = ticket.expecting(typical_suggest(request.labels.len(), request.room));
        let prompt = request.prompt(args.language.as_deref());
        let (completion, effective) = self.send(ticket, &prompt, None).await?;
        let answer = llm::json_answer(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
            description: "the model's answer was not a list of labels".into(),
            retry_after: None,
            transient: false,
        })?;
        let verdicts = parse_verdicts(&answer, &request.labels);
        let new_labels = if verdicts.iter().any(|verdict| verdict.fits) {
            Vec::new()
        } else {
            parse_new_labels(&answer, &request.labels, request.room)
        };
        Ok(SuggestResult { verdicts, new_labels, effective, usage: Usage::of(&completion) })
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

/// A typical answer of `AssistLabel/suggest`.
fn typical_suggest(labels: usize, room: usize) -> i64 {
    let new = if room > 0 { TYPICAL_NEW_LABEL_TOKENS } else { 0 };
    TYPICAL_LABEL_TOKENS_PER_LABEL * labels as i64 + new
}

/// The one foreign mail of a call that takes one, if the call is about a foreign mail.
fn one_foreign(mails: &[ForeignMail]) -> Result<Option<&ForeignMail>> {
    match mails {
        [] => Ok(None),
        [mail] => Ok(Some(mail)),
        _ => Err(invalid("foreignMails", "this call takes exactly 1 foreign mail")),
    }
}

/// Everything a spam check needs between asking the model and reading its answer.
struct SpamCheckPrompt {
    prompt: Prompt,
    signals: SpamSignals,
    assessment: crate::spam::Assessment,
    facts: Vec<crate::spam::Fact>,
    mail: MailText,
    shape: crate::spam::MailShape,
}

fn spam_check_prompt(
    mail: MailText,
    signals: SpamSignals,
    phishing: &[uwumail_smtp::phishing::Finding],
    shape: crate::spam::MailShape,
    language: Option<&str>,
) -> SpamCheckPrompt {
    let assessment = crate::spam::assess(&signals, phishing, &format!("{}\n{}", mail.subject, mail.text));
    let facts = crate::spam::facts(&signals, &assessment, rule_meaning);
    let prompt = prompts::spam_check(&mail, &facts, &assessment.allowed, language);
    SpamCheckPrompt { prompt, signals, assessment, facts, mail, shape }
}

/// The spam check of a mail of another account: its provider's findings, the phishing checks on
/// what the app sent along (no HTML, so links only as written out in the text).
fn foreign_spam_check_prompt(
    foreign: &ForeignMail,
    contact_domains: &[String],
    language: Option<&str>,
) -> SpamCheckPrompt {
    let signals = Assist::foreign_spam_signals(foreign);
    let mail = foreign.mail_text(MAX_MAIL_CHARS);
    let header = |name: &str| {
        foreign.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    };
    let reply_to = header("Reply-To")
        .and_then(|value| value.split(['<', '>', ',', ' ']).find(|part| part.contains('@')).map(str::to_owned));
    let from = foreign.from.first();
    let input = uwumail_smtp::phishing::Input {
        from_name: from.and_then(|from| from.name.as_deref()),
        from_address: from.map(|from| from.email.as_str()),
        reply_to: reply_to.as_deref(),
        subject: &foreign.subject,
        text: &mail.text,
        links: uwumail_smtp::phishing::links_in_text(&mail.text),
        contact_domains,
        mailing_list: header("List-Id").is_some(),
        from_authenticated: crate::spam::authentic(&signals.authentication),
    };
    let phishing = uwumail_smtp::phishing::check(&input);
    let shape = crate::spam::MailShape { has_links: mail.text.contains("http"), attachments: None };
    spam_check_prompt(mail, signals, &phishing, shape, language)
}

/// How many attachments a stored message has.
fn record_attachments(raw: &[u8]) -> usize {
    let raw = &raw[..raw.len().min(MAX_PARSE_BYTES)];
    uwumail_store::mime_limits::parse_message(raw).map_or(0, |message| message.attachments().count())
}

/// A message is parsed for its attachments up to this size, like the phishing checks.
const MAX_PARSE_BYTES: usize = 25 * 1024 * 1024;

/// A label `AssistLabel/suggest` asks about: one of the person's, or of another account.
struct SuggestLabel {
    id: Option<i64>,
    name: String,
    description: String,
    is_set: bool,
}

struct SuggestRequest {
    labels: Vec<SuggestLabel>,
    /// New labels that may be proposed.
    room: usize,
    mail: MailText,
}

impl SuggestRequest {
    fn prompt(&self, language: Option<&str>) -> Prompt {
        let list: Vec<(String, String)> =
            self.labels.iter().map(|label| (label.name.clone(), label.description.clone())).collect();
        prompts::suggest(&self.mail, &list, self.room, language)
    }
}

/// The model's verdicts, in the order of the labels; a label it did not answer for is left out, a
/// name that is no label is dropped, each label counts by its first verdict.
fn parse_verdicts(answer: &Value, labels: &[SuggestLabel]) -> Vec<LabelVerdict> {
    let mut found: Vec<Option<LabelVerdict>> = labels.iter().map(|_| None).collect();
    for entry in answer.get("verdicts").and_then(Value::as_array).into_iter().flatten().take(100) {
        let Some(name) = entry.get("name").and_then(Value::as_str) else { continue };
        let Some(fits) = entry.get("fits").and_then(Value::as_bool) else { continue };
        let name = name.trim().to_lowercase();
        let Some(index) = labels.iter().position(|label| label.name.trim().to_lowercase() == name) else {
            continue;
        };
        if found[index].is_some() {
            continue;
        }
        let label = &labels[index];
        found[index] = Some(LabelVerdict {
            label_id: label.id,
            name: label.name.clone(),
            reason: optional_text(entry.get("reason"), MAX_REASON_CHARS).unwrap_or_default(),
            fits,
            is_set: label.is_set,
        });
    }
    found.into_iter().flatten().collect()
}

/// The proposed labels that hold: a name of 1 to 40 characters that is no label's yet, a
/// description of at most 300; a color that is no `#rrggbb` is replaced.
fn parse_new_labels(answer: &Value, labels: &[SuggestLabel], room: usize) -> Vec<NewLabel> {
    let mut taken: HashSet<String> = labels.iter().map(|label| label.name.trim().to_lowercase()).collect();
    let mut out = Vec::new();
    for entry in answer.get("newLabels").and_then(Value::as_array).into_iter().flatten().take(10) {
        if out.len() >= room {
            break;
        }
        let text = |field: &str| entry.get(field).and_then(Value::as_str).map(|t| t.replace('\n', " "));
        let name = text("name").map(|name| clean(&name, usize::MAX)).unwrap_or_default();
        let description = text("description").map(|d| clean(&d, usize::MAX)).unwrap_or_default();
        if name.is_empty() || chars(&name) > MAX_LABEL_NAME_CHARS || chars(&description) > MAX_LABEL_DESCRIPTION_CHARS {
            continue;
        }
        if !taken.insert(name.to_lowercase()) {
            continue;
        }
        let color = text("color")
            .map(|color| color.trim().to_ascii_lowercase())
            .filter(|color| {
                color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit())
            })
            .unwrap_or_else(|| {
                let at = name.chars().map(|c| c as usize).sum::<usize>() % LABEL_COLORS.len();
                LABEL_COLORS[at].to_owned()
            });
        let reason = optional_text(entry.get("reason"), MAX_REASON_CHARS).unwrap_or_default();
        out.push(NewLabel { name, description, color, reason });
    }
    out
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

/// SPF, DKIM and DMARC from the topmost `Authentication-Results` this server (`hostname`) wrote, or
/// the topmost of any server without one (for mail of another account).
pub fn authentication(
    headers: &[(String, String)],
    hostname: Option<&str>,
    from: &[uwumail_store::EmailAddress],
) -> AuthenticationSignals {
    let from_domain = from
        .first()
        .and_then(|address| address.email.rsplit_once('@'))
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
        .filter(|domain| !domain.is_empty());
    let mut signals = AuthenticationSignals { from_domain, ..AuthenticationSignals::default() };
    let ours = trusted_headers(headers, hostname).iter().find(|(name, value)| {
        name.eq_ignore_ascii_case("Authentication-Results")
            && hostname.is_none_or(|hostname| {
                value.split(';').next().is_some_and(|id| id.trim().eq_ignore_ascii_case(hostname))
            })
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

/// The headers whose verdicts can be believed (security review 0.22, client C-1 checked on the
/// server). A sender can write any `Authentication-Results` or `X-Spam-Status` into its mail;
/// only what the receiving server put on top before its own `Received` line is that server's.
///
/// - With `hostname` (own mail): the block this server wrote, when the topmost `Received` is its
///   own — up to the next `Received`, which is where the sender's part starts. Mail that never
///   passed this server's SMTP (written here, imported) has no such block.
/// - Without (a mail of another account): what stands above the first `Received`, the way the
///   provider's own findings are read for fetched mail.
pub fn trusted_headers<'a>(headers: &'a [(String, String)], hostname: Option<&str>) -> &'a [(String, String)] {
    let mut received = headers.iter().enumerate().filter(|(_, (name, _))| name.eq_ignore_ascii_case("Received"));
    match hostname {
        None => &headers[..received.next().map_or(headers.len(), |(at, _)| at)],
        Some(hostname) => {
            let Some((_, (_, value))) = received.next() else { return &[] };
            let ours = format!("by {} (uwumail)", hostname.to_ascii_lowercase());
            if !value.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase().contains(&ours) {
                return &[];
            }
            &headers[..received.next().map_or(headers.len(), |(at, _)| at)]
        }
    }
}

/// The server's spam filter verdict: `X-Spam-Status: Yes, score=6.0 required=5.0 tests=A,B`. The
/// caller passes only [`trusted_headers`].
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

/// What the rules of this server's spam filter mean, for the model. Rules of other filters stay bare.
const RULE_MEANINGS: &[(&str, &str)] = &[
    ("BAYES_HAM", "the filter learned from mail marked as wanted or junk, and its words look like wanted mail"),
    ("BAYES_SPAM", "the filter learned from mail marked as wanted or junk, and its words look like junk"),
    ("KNOWN_GOOD_SENDER", "this sender's domain or network delivered mail here before, hardly any of it junk"),
    ("KNOWN_JUNK_SENDER", "much of this sender's earlier mail here was junk"),
    ("SPF_FAIL", "the sending server is not allowed to send for the envelope domain"),
    ("DKIM_FAIL", "a signature of the mail is broken"),
    ("DMARC_FAIL", "the From domain is not backed by SPF or DKIM: it may be forged"),
    ("NO_AUTH", "neither SPF nor DKIM vouched for the sender"),
    ("NO_REVERSE_DNS", "the sending server's address has no name"),
    ("GENERIC_REVERSE_DNS", "the sending server's name looks like a home connection"),
    ("HELO_NOT_A_NAME", "the sending server greeted with something that is not its name"),
    ("SPAMHAUS_ZEN", "the sending server is on a blocklist"),
    ("SPAMCOP", "the sending server is on a blocklist"),
    ("BARRACUDA", "the sending server is on a blocklist"),
    ("SPAMHAUS_DBL", "a domain in the mail is on a blocklist"),
    ("SPAMHAUS_DBL_ABUSED", "a domain in the mail is on a blocklist for abused domains"),
    ("SPAMHAUS_DBL_MALICIOUS", "a domain in the mail is on a blocklist for malware or phishing"),
    ("BAD_WORDS", "words typical of spam"),
    ("MALWARE_LINK", "a link leads to known malware or phishing"),
    ("MALWARE_ATTACHMENT", "an attachment is known malware"),
    ("DISPOSABLE_FROM", "the From address is a throwaway address"),
    ("FREEMAIL_REPLYTO", "answers go to a free mail address other than the sender"),
    ("LINK_SHORTENER", "a link goes through a link shortener"),
    ("PHISHING_LINK_TEXT", "a link shows one address and leads to another"),
    ("LINK_TO_IP", "a link leads to a bare IP address"),
    ("LOOKALIKE_LINK", "a link leads to a domain that imitates a known one"),
    ("FROM_NAME_SPOOFS_ADDRESS", "the sender's name shows an address other than the real one"),
    ("LOOKALIKE_BRAND_FROM", "the sender's domain is spelled to look like a known brand's"),
    ("BRAND_IN_FROM_DOMAIN", "the sender's domain carries a known brand's name but is not the brand's"),
    ("LOOKALIKE_CONTACT_FROM", "the sender's domain looks like the domain of one of the reader's contacts"),
    ("BRAND_IN_FROM_NAME", "the sender's name claims a known brand, the address is not the brand's"),
    ("REPLY_TO_OTHER_SITE", "answers go to another domain than the sender's"),
    ("LOOKALIKE_BRAND_LINK", "a link leads to a domain spelled to look like a known brand's"),
    ("BRAND_LINK_TEXT", "a link shows a known brand's or contact's address and leads somewhere else"),
    ("BRAND_IN_SUBJECT", "the subject names a known brand and asks to log in or confirm data, from another domain"),
    ("CREDENTIAL_REQUEST", "the mail asks to log in or confirm data, with links to another domain than the sender's"),
    ("HIDDEN_TEXT", "the mail hides text from the reader"),
    ("HTML_ONLY", "the mail has no plain text part"),
    ("BASE64_TEXT", "the text is encoded in an unusual way"),
    ("MISSING_DATE", "the mail has no date"),
    ("DATE_IN_FUTURE", "the mail is dated in the future"),
    ("DATE_IN_PAST", "the mail is dated long ago"),
    ("MISSING_MESSAGE_ID", "the mail has no Message-ID"),
    ("SUBJECT_ALL_CAPS", "the subject is in capitals"),
    ("EXECUTABLE_ATTACHMENT", "an attachment is a program"),
    ("MACRO_ATTACHMENT", "an attachment is a document with macros"),
    ("HTML_ATTACHMENT", "an attachment is a web page"),
    ("DISGUISED_ATTACHMENT", "an attachment's name hides its real type"),
    ("ARCHIVE_WITH_PROGRAM", "an archive attached holds a program"),
    ("FETCHED", "the mail was fetched from another mailbox"),
    ("PROVIDER_JUNK", "the other mail provider had put it in Junk"),
    ("PROVIDER_SPAM_FLAG", "the other mail provider marked it as spam"),
    ("NOT_ADDRESSED", "the mailbox it was fetched from is not among the recipients"),
    ("PROVIDER_SPF_FAIL", "the other mail provider's SPF check failed"),
    ("PROVIDER_DKIM_FAIL", "the other mail provider's DKIM check failed"),
    ("PROVIDER_DMARC_FAIL", "the other mail provider's DMARC check failed"),
    ("FETCHED_NO_AUTH", "nothing vouches for the sender of this fetched mail"),
];

/// What a rule of this server's spam filter or phishing checks means, for the model.
pub fn rule_meaning(rule: &str) -> Option<&'static str> {
    RULE_MEANINGS.iter().find(|(known, _)| *known == rule).map(|(_, meaning)| *meaning)
}

/// A spam check answer: verdict, confidence, and each reason with what it cites.
pub type SpamAnswer = (String, f64, Vec<(String, String)>);

/// Verdict, confidence and reasons (with what each one cites) out of the model's answer.
pub fn parse_spam(text: &str) -> Option<SpamAnswer> {
    let answer = llm::json_answer(text)?;
    let verdict = answer.get("verdict")?.as_str()?.trim().to_ascii_lowercase();
    if !matches!(verdict.as_str(), "legitimate" | "suspicious" | "spam" | "phishing") {
        return None;
    }
    let confidence = answer.get("confidence").and_then(Value::as_f64).filter(|c| c.is_finite()).unwrap_or(0.5);
    let reasons = crate::spam::parse_reasons(&answer, MAX_REASONS, MAX_REASON_CHARS);
    Some((verdict, confidence.clamp(0.0, 1.0), reasons))
}

/// The model's verdict on each of the person's labels it was asked about, with its reasons:
/// `"fits": "yes" | "no" | "unsure"` (or `true`/`false`). Names that are not labels are dropped;
/// each label counts once, by its first entry. A bare name counts as yes, an entry without a
/// verdict as unsure.
pub fn parse_labels(answer: &Value, labels: &[AssistLabel]) -> Vec<uwumail_labels::AiVerdict> {
    use uwumail_labels::{AiAnswer, AiVerdict};
    let mut out: Vec<AiVerdict> = Vec::new();
    let mut seen = HashSet::new();
    for entry in answer.get("labels").and_then(Value::as_array).into_iter().flatten().take(50) {
        let (name, reason, verdict) = match entry {
            Value::String(name) => (name.as_str(), "", AiAnswer::Yes),
            Value::Object(object) => (
                object.get("name").and_then(Value::as_str).unwrap_or_default(),
                object.get("reason").and_then(Value::as_str).unwrap_or_default(),
                object.get("fits").and_then(AiAnswer::parse).unwrap_or(AiAnswer::Unsure),
            ),
            _ => continue,
        };
        let name = name.trim();
        let Some(label) = labels.iter().find(|label| label.name.trim().to_lowercase() == name.to_lowercase()) else {
            continue;
        };
        if !seen.insert(label.id) {
            continue;
        }
        out.push(AiVerdict {
            label_id: label.id,
            verdict,
            reason: clean(&reason.replace('\n', " "), MAX_REASON_CHARS),
        });
    }
    out
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
        let end_when = entry.get("end").and_then(Value::as_str).and_then(parse_when);
        // "allDay" with a time of day contradicts itself; the time is what the mail said
        // ("zwischen 10:00 und 12:00" must not become a whole day).
        let timed = |when: &When| matches!(when, When::At(at) if at.time() != chrono::NaiveTime::MIN);
        // "23:59:59" is how some write the end of a whole day.
        let end_of_day = |when: &When| matches!(when, When::At(at) if at.time() >= chrono::NaiveTime::from_hms_opt(23, 59, 0).unwrap_or_default());
        if all_day && (timed(&start) || end_when.as_ref().is_some_and(|end| timed(end) && !end_of_day(end))) {
            all_day = false;
        }
        // Midnight to midnight on another day is whole days, whatever the flag says.
        if !all_day
            && let (When::At(from), Some(When::At(to))) = (&start, &end_when)
            && !timed(&start)
            && to.time() == chrono::NaiveTime::MIN
            && to.date() > from.date()
        {
            all_day = true;
        }
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
        let end = match end_when {
            // The last day, as people (and the prompt) write it: the end is the day after.
            Some(When::At(at)) if all_day => at.date().and_hms_opt(0, 0, 0).unwrap_or(at) + TimeDelta::days(1),
            Some(When::At(at)) => at,
            Some(When::Day(day)) => {
                let day = day.and_hms_opt(0, 0, 0).unwrap_or_default();
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
            ("Received".to_owned(), "from a.example by mx.example.org (UwUMail) with ESMTPS id 1".to_owned()),
            ("Authentication-Results".to_owned(), "mx.example.org; spf=fail smtp.mailfrom=x@bank.example; dkim=none; dkim=pass header.d=bank.example; dmarc=fail header.from=bank.example".to_owned()),
            ("Authentication-Results".to_owned(), "evil.example; spf=pass; dmarc=pass".to_owned()),
            ("X-Spam-Status".to_owned(), "Yes, score=6.0 required=5.0 tests=SPF_FAIL,SPAMHAUS_ZEN".to_owned()),
        ];
        let from = [uwumail_store::EmailAddress { name: None, email: "service@Bank.example".into() }];
        let auth = authentication(&headers, Some("MX.example.org"), &from);
        assert_eq!(auth.spf.as_deref(), Some("fail"));
        assert_eq!(auth.dkim.as_deref(), Some("pass"));
        assert_eq!(auth.dmarc.as_deref(), Some("fail"));
        assert_eq!(auth.from_domain.as_deref(), Some("bank.example"));
        assert_eq!(authentication(&headers[2..], Some("mx.example.org"), &from).spf, None, "a stranger's claim");
        // For a foreign mail, the topmost of any server: its own provider's.
        assert_eq!(authentication(&headers[2..], None, &from).spf.as_deref(), Some("pass"));
        let foreign = ForeignMail { headers: headers[2..].to_vec(), in_junk: true, ..ForeignMail::default() };
        let signals = Assist::foreign_spam_signals(&foreign);
        assert!(signals.in_junk && signals.sender.is_none());
        assert_eq!(signals.spam_score, Some(6.0));
        let facts = crate::spam::facts(&signals, &crate::spam::assess(&signals, &[], ""), rule_meaning);
        assert!(facts[0].text.contains("another account"), "{facts:?}");
        assert_eq!(
            spam_status(trusted_headers(&headers, Some("mx.example.org"))),
            (Some(6.0), Some(5.0), vec!["SPF_FAIL".into(), "SPAMHAUS_ZEN".into()])
        );
        let none = [("X-Spam-Status".to_owned(), "No, score=0.0 required=5.0 tests=none".to_owned())];
        assert_eq!(spam_status(&none), (Some(0.0), Some(5.0), vec![]));
    }

    /// Client review C-1, checked on the server: verdicts a sender wrote below this server's block,
    /// or into mail that never passed this server, are not read; neither are a foreign mail's below
    /// its provider's first `Received`.
    #[test]
    fn forged_verdicts_below_the_receiving_server_are_ignored() {
        let h = |name: &str, value: &str| (name.to_owned(), value.to_owned());
        let from = [uwumail_store::EmailAddress { name: None, email: "service@bank.example".into() }];
        let forged = [
            h("Authentication-Results", "mx.example.org; spf=pass; dkim=pass; dmarc=pass"),
            h("X-Spam-Status", "No, score=-50.0 required=5.0 tests=none"),
        ];
        // Ours on top, then the sender's part after its own Received.
        let mut own = vec![
            h("Received", "from a.example\r\n\tby mx.example.org (UwUMail) with ESMTPS id 1"),
            h("Authentication-Results", "mx.example.org; spf=fail; dmarc=fail"),
            h("Received", "from sender.example by relay.example"),
        ];
        own.extend(forged.iter().cloned());
        assert_eq!(authentication(&own, Some("mx.example.org"), &from).dmarc.as_deref(), Some("fail"));
        assert_eq!(spam_status(trusted_headers(&own, Some("mx.example.org"))), (None, None, vec![]));
        // A mail that never passed our SMTP: nothing it says about itself counts.
        assert_eq!(authentication(&forged, Some("mx.example.org"), &from).dmarc, None);
        assert!(trusted_headers(&forged, Some("mx.example.org")).is_empty());
        let mut theirs = vec![h("Received", "from x by mx.other.example (Postfix)")];
        theirs.extend(forged.iter().cloned());
        assert!(trusted_headers(&theirs, Some("mx.example.org")).is_empty(), "another server's Received");
        // A foreign mail: what stands below its provider's first Received is the sender's.
        let mut foreign = vec![h("Received", "by mx.provider.example")];
        foreign.extend(forged.iter().cloned());
        let mail = ForeignMail { headers: foreign, ..ForeignMail::default() };
        let signals = Assist::foreign_spam_signals(&mail);
        assert_eq!((signals.spam_score, signals.authentication.dmarc.as_deref()), (None, None));
    }

    #[test]
    fn spam_answers_are_held_to_their_shape() {
        let (verdict, confidence, reasons) = parse_spam(
            r#"{"verdict": "Phishing", "confidence": 7, "reasons": [{"text": "a", "evidence": "F1"}, "", "b\nc"]}"#,
        )
        .unwrap();
        assert_eq!((verdict.as_str(), confidence), ("phishing", 1.0));
        assert_eq!(reasons, [("a".to_owned(), "F1".to_owned()), ("b c".to_owned(), String::new())]);
        assert!(parse_spam(r#"{"verdict": "delete all mail"}"#).is_none());
        // The order the schema asks for: reasons first, then the verdict; only the allowed verdicts.
        let schema = crate::prompts::spam_schema(&["legitimate", "suspicious"]);
        assert_eq!(schema["required"], serde_json::json!(["reasons", "verdict", "confidence"]));
        assert_eq!(schema["properties"]["verdict"]["enum"], serde_json::json!(["legitimate", "suspicious"]));
    }

    /// The invoice of the user report: authentic, wanted by the filter, from a sender who wrote before.
    fn invoice_signals() -> SpamSignals {
        SpamSignals {
            authentication: AuthenticationSignals {
                spf: Some("pass".into()),
                dkim: Some("pass".into()),
                dmarc: Some("pass".into()),
                from_domain: Some("billing.example".into()),
            },
            spam_score: Some(-5.5),
            spam_threshold: Some(5.0),
            tests: vec!["BAYES_HAM".into(), "KNOWN_GOOD_SENDER".into(), "SOME_OTHER_RULE".into()],
            in_junk: false,
            sender: Some(SenderSignals {
                address: Some("invoice@billing.example".into()),
                earlier_messages: 4,
                ..SenderSignals::default()
            }),
        }
    }

    #[test]
    fn the_invoice_of_the_user_report_cannot_be_called_spam() {
        let mail = MailText {
            subject: "Rechnung".into(),
            text: "Ihre Zahlung haben wir dankend erhalten.".into(),
            ..MailText::default()
        };
        let check = spam_check_prompt(mail, invoice_signals(), &[], crate::spam::MailShape::default(), Some("de"));
        assert_eq!(check.assessment.allowed, ["legitimate"]);
        assert_eq!(
            check.prompt.schema.as_ref().unwrap().1["properties"]["verdict"]["enum"],
            serde_json::json!(["legitimate"])
        );
        let (verdict, confidence, moved) = crate::spam::settle(&check.assessment, "spam", 0.9);
        assert_eq!((verdict.as_str(), moved.as_deref()), ("legitimate", Some("spam")));
        assert!(confidence <= 0.6);
    }

    #[test]
    fn facts_explain_themselves() {
        let signals = invoice_signals();
        let facts = crate::spam::facts(&signals, &crate::spam::assess(&signals, &[], ""), rule_meaning);
        let text = facts.iter().map(|fact| format!("{}: {}", fact.id, fact.text)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("DMARC passed for billing.example"), "{text}");
        assert!(text.contains("-5.5 points, Junk from 5.0; this mail is under the limit"), "{text}");
        assert!(text.contains("Spam filter rule BAYES_HAM: the filter learned"), "{text}");
        assert!(text.contains("Spam filter rule KNOWN_GOOD_SENDER: this sender's"), "{text}");
        assert!(text.contains("Spam filter rule SOME_OTHER_RULE\n"), "{text}");
        assert!(text.starts_with("F1: SPF: pass"), "{text}");
    }

    #[test]
    fn a_foreign_mail_gets_the_phishing_checks_on_its_text() {
        let foreign = ForeignMail {
            from: vec![uwumail_store::EmailAddress {
                name: Some("PayPal Service".into()),
                email: "a@konto-hilfe.example".into(),
            }],
            subject: "Ihr Konto wurde gesperrt".into(),
            text: "Bitte bestätigen Sie Ihre Daten: https://konto-check.example/login".into(),
            ..ForeignMail::default()
        };
        let check = foreign_spam_check_prompt(&foreign, &[], None);
        let codes: Vec<&str> = check.assessment.evidence.iter().map(|evidence| evidence.code.as_str()).collect();
        assert!(codes.contains(&"BRAND_IN_FROM_NAME") && codes.contains(&"CREDENTIAL_REQUEST"), "{codes:?}");
        assert!(!check.assessment.allowed.contains(&"legitimate"), "{:?}", check.assessment);
    }

    /// Security review 0.22 SPAM-4: a client-supplied megabyte name and subject, and the most contact
    /// domains, stay cheap. Generous limit for loaded machines; uncapped this took many seconds.
    #[test]
    fn huge_foreign_fields_are_cheap() {
        let huge = "PayPal Service Konto ".repeat(50_000);
        let foreign = ForeignMail {
            from: vec![uwumail_store::EmailAddress { name: Some(huge.clone()), email: "a@konto-hilfe.example".into() }],
            subject: huge.clone(),
            text: "Bitte bestätigen Sie Ihre Daten: https://konto-check.example/login".into(),
            ..ForeignMail::default()
        };
        let contacts: Vec<String> = (0..MAX_CONTACT_DOMAINS).map(|i| format!("partner-firma-{i}.example")).collect();
        let started = std::time::Instant::now();
        let check = foreign_spam_check_prompt(&foreign, &contacts, None);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(check.assessment.evidence.iter().any(|evidence| evidence.code == "BRAND_IN_FROM_NAME"));
    }

    fn label(id: i64, name: &str) -> AssistLabel {
        AssistLabel {
            id,
            name: name.into(),
            description: String::new(),
            keyword: uwumail_store::label_keyword(name),
            color: None,
            created_at: 0,
            rules: None,
            detector: None,
            learn_senders: true,
            classifier: true,
            base: None,
            auto: true,
        }
    }

    fn suggest_label(id: Option<i64>, name: &str, is_set: bool) -> SuggestLabel {
        SuggestLabel { id, name: name.into(), description: String::new(), is_set }
    }

    #[test]
    fn verdicts_follow_the_labels_and_proposals_are_checked() {
        let labels = [suggest_label(Some(1), "Rechnungen", false), suggest_label(Some(2), "Reisen", true)];
        let answer = json!({
            "verdicts": [
                { "name": "reisen", "reason": "Keine Reise.", "fits": false },
                { "name": "Rechnungen", "reason": "Eine Rechnung\nder Stadtwerke.", "fits": true },
                { "name": "Rechnungen", "reason": "again", "fits": false },
                { "name": "Spam", "reason": "x", "fits": true },
                { "name": "Reisen", "reason": "no fits" }
            ],
            "newLabels": [{ "name": "Strom", "description": "", "color": "#FFAA00", "reason": "x" }]
        });
        let verdicts = parse_verdicts(&answer, &labels);
        let summary: Vec<(Option<i64>, bool, bool)> = verdicts.iter().map(|v| (v.label_id, v.fits, v.is_set)).collect();
        assert_eq!(summary, [(Some(1), true, false), (Some(2), false, true)]);
        assert_eq!(verdicts[0].reason, "Eine Rechnung der Stadtwerke.");

        let proposals = json!({ "newLabels": [
            { "name": "  Strom & Gas ", "description": "Abschläge und Jahresrechnungen", "color": "#FFAA00", "reason": "Neu" },
            { "name": "strom & gas", "description": "", "color": "#000000", "reason": "doppelt" },
            { "name": "reisen", "description": "", "color": "#000000", "reason": "gibt es schon" },
            { "name": "x".repeat(41), "description": "", "color": "#000000", "reason": "zu lang" },
            { "name": "Verein", "description": "d".repeat(301), "color": "#000000", "reason": "zu lang" },
            { "name": "Garten", "description": "", "color": "grün", "reason": "Farbe falsch" },
            { "name": "Dritter", "description": "", "color": "#123456", "reason": "zu viele" }
        ]});
        let new = parse_new_labels(&proposals, &labels, 2);
        assert_eq!(new.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Strom & Gas", "Garten"]);
        assert_eq!(new[0].color, "#ffaa00");
        assert!(LABEL_COLORS.contains(&new[1].color.as_str()));
        assert!(parse_new_labels(&proposals, &labels, 0).is_empty());
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
        let yes = |picks: &[uwumail_labels::AiVerdict]| -> Vec<i64> {
            picks.iter().filter(|p| p.verdict == uwumail_labels::AiAnswer::Yes).map(|p| p.label_id).collect()
        };
        // Without a verdict the first entry is unsure; a bare name is a yes.
        assert_eq!(picks.iter().map(|p| p.label_id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(yes(&picks), [2]);
        assert_eq!(picks[0].reason, "Eine Rechnung");
    }

    #[test]
    fn labels_judged_not_to_fit_are_dropped() {
        let labels = [label(1, "Rechnungen"), label(2, "Termine"), label(3, "Sicherheit")];
        let answer = json!({ "labels": [
            { "name": "Rechnungen", "reason": "Keine Rechnung, sondern ein Sicherheitshinweis.", "fits": false },
            { "name": "Rechnungen", "reason": "again", "fits": true },
            { "name": "Termine", "reason": "Kein Termin.", "fits": false },
            { "name": "Sicherheit", "reason": "Eine neue App hat Zugriff aufs Konto.", "fits": "yes" }
        ]});
        let picks = parse_labels(&answer, &labels);
        let yes: Vec<i64> =
            picks.iter().filter(|p| p.verdict == uwumail_labels::AiAnswer::Yes).map(|p| p.label_id).collect();
        assert_eq!(yes, [3]);
        let unsure = json!({ "labels": [{ "name": "Termine", "reason": "?", "fits": "unsure" }] });
        assert_eq!(parse_labels(&unsure, &labels)[0].verdict, uwumail_labels::AiAnswer::Unsure);
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

    #[test]
    fn an_all_day_answer_with_times_keeps_the_times() {
        let (links, people, mine) = (Vec::new(), Vec::new(), HashSet::new());
        let source = "Samstag 03.10.26, zwischen 10:00 und 12:00 ist Flohmarkt.";
        let context = EventContext { source, links: &links, people: &people, mine: &mine };
        let answer = json!({ "events": [
            { "title": "Flohmarkt", "start": "2026-10-03T10:00:00", "end": "2026-10-03T12:00:00", "allDay": true,
              "quote": "Samstag 03.10.26, zwischen 10:00 und 12:00", "participants": [] },
            { "title": "Flohmarkt", "start": "2026-10-03T00:00:00", "end": null, "allDay": true,
              "quote": "Samstag 03.10.26", "participants": [] }
        ]});
        let events = parse_events(&answer, &context);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(!events[0].all_day);
        assert_eq!((events[0].start.as_str(), events[0].end.as_str()), ("2026-10-03T10:00:00", "2026-10-03T12:00:00"));
        assert!(events[1].all_day);
        assert_eq!((events[1].start.as_str(), events[1].end.as_str()), ("2026-10-03T00:00:00", "2026-10-04T00:00:00"));
    }

    #[test]
    fn whole_days_end_after_the_last_day() {
        let (links, people, mine) = (Vec::new(), Vec::new(), HashSet::new());
        let source = "Die Messe läuft vom 12. bis 15. Oktober 2026.";
        let context = EventContext { source, links: &links, people: &people, mine: &mine };
        let quote = "Die Messe läuft vom 12. bis 15. Oktober 2026.";
        let answer = json!({ "events": [
            // Midnight to midnight, flagged as timed: whole days.
            { "title": "Messe", "start": "2026-10-12T00:00:00", "end": "2026-10-15T00:00:00", "allDay": false,
              "quote": quote, "participants": [] },
            { "title": "Messe", "start": "2026-10-12T00:00:00", "end": "2026-10-15T23:59:59", "allDay": true,
              "quote": quote, "participants": [] },
            { "title": "Messe", "start": "2026-10-12", "end": "2026-10-15", "allDay": true,
              "quote": quote, "participants": [] }
        ]});
        let events = parse_events(&answer, &context);
        assert_eq!(events.len(), 3, "{events:?}");
        for event in &events {
            assert!(event.all_day, "{event:?}");
            assert_eq!((event.start.as_str(), event.end.as_str()), ("2026-10-12T00:00:00", "2026-10-16T00:00:00"));
        }
    }

    #[test]
    fn models_that_think_are_known_by_the_lists_or_their_name() {
        for model in ["o3-mini", "gpt-5-mini", "openai/gpt-5", "gemini-2.5-flash", "deepseek-r1:14b", "qwen3:32b"] {
            assert!(thinks(Shape::Chat, None, model), "{model}");
        }
        for model in ["gpt-4o-mini", "gpt-5-chat-latest", "gemini-2.5-flash-lite", "mistral-small-latest", "llama3.1"] {
            assert!(!thinks(Shape::Chat, None, model), "{model}");
        }
        assert!(!thinks(Shape::Anthropic, None, "claude-sonnet-4-5"), "only when asked, which never happens");
        let mut listed = Price::free();
        listed.supports_reasoning = true;
        assert!(thinks(Shape::Chat, Some(&listed), "house-model"));
    }

    #[test]
    fn an_estimate_prices_pictures_retries_and_the_worst_case() {
        let prompt = Prompt {
            system: "s".repeat(400),
            user: "u".repeat(3600),
            schema: Some(("x", serde_json::json!({ "type": "object" }))),
            max_tokens: 2000,
        };
        let mut price = Price::free();
        price.input_per_million = 1.0;
        price.output_per_million = 4.0;
        price.reasoning_per_million = 4.0;
        price.per_request = 0.001;
        price.max_output_tokens = Some(1500);
        let plan = EstimatePlan {
            prompt: &prompt,
            shape: Shape::Chat,
            typical: 250,
            reasoning: Some(900),
            image_count: 2,
            image_tokens: 200,
            price: Some(&price),
            show_cost: true,
        };
        let calibration = Calibration { retry_rate: Some(0.2), ..Calibration::default() };
        let (calls, cost) = plan_estimate(&plan, &calibration);
        let input = llm::estimate_request(&prompt, Shape::Chat);
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[0].input_tokens, calls[0].output_tokens, calls[0].reasoning_tokens), (input, 250, 900));
        assert_eq!((calls[1].purpose, calls[1].weight, calls[1].input_tokens), ("retry", 0.2, input));
        let cost = cost.unwrap();
        let near = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert!(near(cost.parts.images, 200.0 / 1e6), "{cost:?}");
        assert!(near(cost.parts.input, (input - 200) as f64 / 1e6));
        assert!(near(cost.parts.output, 1e-3) && near(cost.parts.reasoning, 3.6e-3));
        assert!(near(cost.parts.requests, 0.001));
        assert!(near(cost.parts.other, 0.2 * (input as f64 / 1e6 + 0.001)), "the retry by its rate");
        assert!(near(cost.usd, cost.parts.total()));
        // At worst: the model's 1500 tokens (less than the call's 2000), and a retry.
        assert!(near(cost.max_usd, input as f64 / 1e6 + 1500.0 * 4.0 / 1e6 + 0.001 + input as f64 / 1e6 + 0.001));

        // Thinking never pushes past what the call allows; hidden costs are no costs.
        let tight = Prompt { max_tokens: 600, ..prompt.clone() };
        let hidden = EstimatePlan { prompt: &tight, show_cost: false, ..plan };
        let (calls, cost) = plan_estimate(&hidden, &Calibration::default());
        assert_eq!((calls.len(), calls[0].output_tokens, calls[0].reasoning_tokens), (1, 250, 350));
        assert!(cost.is_none());
    }
}
