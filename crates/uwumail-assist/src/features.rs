//! The features: writing, summaries, the spam check, events and labels. Each one reads what it needs
//! of the mail, asks the model through [`Assist::run`] and checks the answer before anything of it is
//! handed on: free text is only ever shown to the person, JSON answers are held to their shape and to
//! what the mail and the person's own data allow.

use std::collections::HashSet;

use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uwumail_store::{Account, AssistLabel, EmailRecord, KeywordsChange, MailboxRole, SenderHistory, StoreError};

use crate::access::Effective;
use crate::llm::{self, Completion, Prompt};
use crate::mail::{MAX_MAIL_CHARS, MailText};
use crate::prompts::{self, ComposeRequest, SUBJECT_MARK};
use crate::{Assist, AssistError, MAX_INSTRUCTION_CHARS, MAX_TEXT_CHARS, Result, now};

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

impl Usage {
    fn of(completion: &Completion) -> Usage {
        Usage { input_tokens: completion.input_tokens, output_tokens: completion.output_tokens }
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
    /// Reads one of the account's mails for a prompt.
    async fn mail(&self, account: &Account, email_id: i64, max_chars: usize) -> Result<(EmailRecord, MailText)> {
        let record = match self.store().email(account.id, email_id).await {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) => return Err(AssistError::NotFound(format!("email {email_id}"))),
            Err(err) => return Err(err.into()),
        };
        let raw = self.store().blob(&record.blob).await?;
        let text = MailText::read(&record, &raw, max_chars);
        Ok((record, text))
    }

    /// Asks the model, streaming when `events` listens.
    async fn ask(
        &self,
        account: &Account,
        feature: &str,
        prompt: &Prompt,
        events: Option<&mpsc::Sender<StreamEvent>>,
        want_subject: bool,
    ) -> Result<(Completion, Effective)> {
        let Some(events) = events else {
            return self.run(account, feature, prompt, None).await;
        };
        let (tx, rx) = mpsc::channel::<String>(64);
        let work = async move {
            let result = self.run(account, feature, prompt, Some(&tx)).await;
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
        let instruction = args.instruction.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let text = args.text.as_deref().filter(|s| !s.trim().is_empty());
        if instruction.is_some_and(|i| chars(i) > MAX_INSTRUCTION_CHARS) {
            return Err(invalid(
                "instruction",
                format!("an instruction has at most {MAX_INSTRUCTION_CHARS} characters"),
            ));
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
        let reply_to = match args.reply_to_email_id {
            Some(id) => Some(self.mail(account, id, 8000).await?.1),
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
        let (completion, effective) = self.ask(account, "compose", &prompt, events, want_subject).await?;
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

    /// `Assist/summarize`.
    pub async fn summarize(
        &self,
        account: &Account,
        args: SummarizeArgs,
        events: Option<&mpsc::Sender<StreamEvent>>,
    ) -> Result<SummaryResult> {
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
        let per_mail = (MAX_MAIL_CHARS / ids.len()).max(2000);
        let mut mails = Vec::new();
        for id in ids {
            mails.push(self.mail(account, id, per_mail).await?);
        }
        mails.sort_by_key(|(record, _)| (record.sent_at.unwrap_or(record.received_at), record.id));
        let texts: Vec<MailText> = mails.into_iter().map(|(_, text)| text).collect();
        let prompt = prompts::summarize(&texts, args.language.as_deref());
        let (completion, effective) = self.ask(account, "summarize", &prompt, events, false).await?;
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
        let (record, mail) = self.mail(account, args.email_id, MAX_MAIL_CHARS).await?;
        let signals = self.spam_signals(account, &record, &mail).await?;
        let prompt = prompts::spam_check(&mail, &findings(&signals), args.language.as_deref());
        let (completion, effective) = self.run(account, "spamCheck", &prompt, None).await?;
        let (verdict, confidence, reasons) =
            parse_spam(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
                description: "the model's answer was not a verdict".into(),
                retry_after: None,
                transient: false,
            })?;
        Ok(SpamResult { verdict, confidence, reasons, signals, effective, usage: Usage::of(&completion) })
    }

    /// `Assist/extractEvents`.
    pub async fn extract_events(&self, account: &Account, args: EventsArgs) -> Result<EventsResult> {
        let (record, mail) = self.mail(account, args.email_id, MAX_MAIL_CHARS).await?;
        let image_text = match (&self.inner.image_text, args.include_images) {
            (Some(read), true) => read(account.id, record.id).await.unwrap_or_default(),
            _ => Vec::new(),
        };
        let image_text: Vec<String> = image_text.iter().take(20).map(|text| clean(text, 4000)).collect();
        let prompt = prompts::extract_events(&mail, &image_text);
        let (completion, effective) = self.run(account, "extractEvents", &prompt, None).await?;
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

    /// Asks which of the person's labels fit one of their mails and puts them on. Labels already on
    /// the mail are left alone and not logged again.
    pub async fn label_email(&self, account: &Account, email_id: i64) -> Result<Vec<LabelPick>> {
        let labels = self.store().assist_labels(account.id).await?;
        if labels.is_empty() {
            return Ok(Vec::new());
        }
        let (record, mail) = self.mail(account, email_id, LABEL_MAIL_CHARS).await?;
        let list: Vec<(String, String)> = labels.iter().map(|l| (l.name.clone(), l.description.clone())).collect();
        let prompt = prompts::labels(&mail, &list);
        let (completion, effective) = self.run(account, "autoLabels", &prompt, None).await?;
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

    /// `Assist/usage`: the person's rows since `days` days ago (today included), and today.
    pub async fn usage(
        &self,
        account: &Account,
        days: u32,
    ) -> Result<(Vec<uwumail_store::UsageRow>, Vec<crate::TodayUsage>)> {
        let since = uwumail_store::utc_day(now() - i64::from(days.clamp(1, 400) - 1) * 86_400);
        let rows = self.store().assist_usage(Some(account.id), since).await?;
        Ok((rows, self.today(account).await?))
    }
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
