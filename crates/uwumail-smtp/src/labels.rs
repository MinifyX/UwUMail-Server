//! Labels without a model, put on at delivery before the person's Sieve rules run (docs/labels.md,
//! docs/jmap-assist.md "Auto-labels"): the label's rules, its detector, learned senders and its
//! classifier, at most a main label and a second one, and none when in doubt. The decisions are made by `uwumail_labels`; this reads what they need and logs what
//! they put on.
//!
//! It is cheap and it never stands in the way of the mail: after [`TIMEOUT`], or on any error, the
//! message is simply stored without these labels.

use std::sync::Arc;
use std::time::Duration;

use uwumail_labels::{Base, Decision, Detector, Label, Mail, Rules};
use uwumail_store::LabelLogWrite;

use crate::Context;
use crate::headers;

/// The longest delivery waits for these labels.
const TIMEOUT: Duration = Duration::from_secs(1);
/// Bytes of a message read for them, at most: the text comes first in practically every mail.
const MAX_PARSE_BYTES: usize = 4 * 1024 * 1024;
/// The header a Sieve script sees these labels in.
pub(crate) const LABEL_HEADER: &str = "X-UwUMail-Label";

/// What was decided for one message.
#[derive(Debug, Default)]
pub(crate) struct Labeled {
    pub decisions: Vec<Decision>,
}

impl Labeled {
    /// The keywords to put on every stored copy.
    pub fn keywords(&self) -> Vec<String> {
        self.decisions.iter().map(|decision| decision.keyword.clone()).collect()
    }
}

/// The message as the deciding reads it, parsed once for all recipients of a delivery.
#[derive(Default)]
pub(crate) struct Parsed(tokio::sync::OnceCell<Arc<(Mail, Vec<i64>)>>);

impl Parsed {
    async fn get(&self, message: &[u8]) -> Result<Arc<(Mail, Vec<i64>)>, uwumail_store::StoreError> {
        self.0
            .get_or_try_init(|| async {
                let raw = message[..message.len().min(MAX_PARSE_BYTES)].to_vec();
                tokio::task::spawn_blocking(move || {
                    let mail = Mail::parse(&raw);
                    let tokens: Vec<i64> =
                        uwumail_labels::tokens(&mail).iter().map(|token| uwumail_labels::token_hash(token)).collect();
                    Arc::new((mail, tokens))
                })
                .await
                .map_err(|err| uwumail_store::StoreError::Internal(err.to_string()))
            })
            .await
            .cloned()
    }
}

/// Whether the From address of a message says who sent it, for learned senders: a sender's label
/// goes only on mail that really comes from that sender, or anyone could get a label (and whatever
/// Sieve rule sorts by it) onto their mail by writing a known address into From (security audit
/// 0.21.0 LABELS-L3).
#[derive(Debug, Clone)]
pub(crate) enum SenderTrust {
    /// SPF or DKIM vouch for this From address (normalized, lower case).
    Verified(String),
    /// Nothing vouches for the From address.
    Unverified,
}

impl SenderTrust {
    fn vouches_for(&self, from: &str) -> bool {
        match self {
            SenderTrust::Verified(address) => address.eq_ignore_ascii_case(from),
            SenderTrust::Unverified => false,
        }
    }
}

/// The labels for a message delivered to `account_id`; none when the person switched them off, has
/// none, sent it themselves, or when deciding takes too long or fails.
pub(crate) async fn decide(
    ctx: &Context,
    account_id: i64,
    message: &[u8],
    parsed: &Parsed,
    sender: &SenderTrust,
) -> Labeled {
    match tokio::time::timeout(TIMEOUT, decide_now(ctx, account_id, message, parsed, sender)).await {
        Ok(Ok(labeled)) => labeled,
        Ok(Err(err)) => {
            tracing::warn!(account = account_id, %err, "labels without a model failed, storing the message without them");
            Labeled::default()
        }
        Err(_) => {
            tracing::info!(
                account = account_id,
                "labels without a model took too long, storing the message without them"
            );
            Labeled::default()
        }
    }
}

async fn decide_now(
    ctx: &Context,
    account_id: i64,
    message: &[u8],
    parsed: &Parsed,
    sender: &SenderTrust,
) -> Result<Labeled, uwumail_store::StoreError> {
    ctx.store.ensure_base_labels(account_id, ctx.tone().language.code()).await?;
    let setup = ctx.store.label_setup(account_id).await?;
    if setup.labels.is_empty() {
        return Ok(Labeled::default());
    }
    let parsed = parsed.get(message).await?;
    let (mail, tokens) = &*parsed;
    if !mail.from.is_empty() && ctx.store.account_owns_address(account_id, &mail.from).await? {
        return Ok(Labeled::default());
    }
    let classifiers = setup.labels.iter().filter(|label| label.classifier).map(|label| label.id).collect();
    let knowledge = ctx.store.label_knowledge(account_id, mail.from.clone(), tokens.clone(), classifiers).await?;
    let from_trusted = sender.vouches_for(&mail.from);
    let known_sender = !mail.from.is_empty() && ctx.store.knows_sender(account_id, mail.from.clone()).await?;
    // Deciding is plain computing: on the blocking pool, so the timeout above always ends the wait
    // and no worker of the server is held by one message.
    tokio::task::spawn_blocking(move || {
        let (mail, tokens) = &*parsed;
        let mut mail = mail.clone();
        mail.from_trusted = from_trusted;
        mail.known_sender = known_sender;
        // Rules were checked when they were written; one that no longer reads is left out.
        let rules: Vec<Option<Rules>> = setup
            .labels
            .iter()
            .map(|label| label.rules.as_ref().and_then(|rules| Rules::check(rules).ok().flatten()))
            .collect();
        let labels: Vec<Label<'_>> = setup
            .labels
            .iter()
            .zip(&rules)
            .map(|(label, rules)| Label {
                id: label.id,
                keyword: &label.keyword,
                rules: rules.as_ref(),
                detector: label.detector.as_deref().and_then(Detector::parse),
                learn_senders: label.learn_senders,
                classifier: label.classifier,
                base: label.base.as_deref().and_then(Base::parse),
                auto: label.auto,
            })
            .collect();
        Labeled { decisions: uwumail_labels::decide(&labels, &mail, &[], &knowledge, tokens) }
    })
    .await
    .map_err(|err| uwumail_store::StoreError::Internal(err.to_string()))
}

/// The copy of `message` a Sieve script reads: without `X-UwUMail-Label` headers of its own, with
/// one per label put on.
pub(crate) fn for_sieve(message: &[u8], labeled: &Labeled) -> Vec<u8> {
    let stripped = headers::strip_label_headers(message);
    if labeled.decisions.is_empty() {
        return stripped;
    }
    let mut out = Vec::with_capacity(stripped.len() + 40 * labeled.decisions.len());
    for decision in &labeled.decisions {
        out.extend_from_slice(format!("{LABEL_HEADER}: {}\r\n", decision.keyword).as_bytes());
    }
    out.extend_from_slice(&stripped);
    out
}

/// Logs the labels put on the stored copies. A failure is only logged.
pub(crate) async fn log(ctx: &Context, account_id: i64, email_ids: &[i64], labeled: &Labeled) {
    if labeled.decisions.is_empty() || email_ids.is_empty() {
        return;
    }
    let mut entries = Vec::new();
    for email_id in email_ids {
        for decision in &labeled.decisions {
            entries.push(LabelLogWrite {
                email_id: *email_id,
                label_id: decision.label_id,
                source: decision.source.as_str().to_owned(),
                code: decision.code.to_owned(),
                params: decision.params.clone(),
                reason: decision.reason.clone(),
                provider: String::new(),
                model: String::new(),
            });
        }
    }
    if let Err(err) = ctx.store.add_label_log_entries(account_id, entries).await {
        tracing::warn!(account = account_id, %err, "logging labels failed");
    }
}
