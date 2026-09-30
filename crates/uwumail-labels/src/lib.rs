//! Labels without a model (docs/labels.md): which of a person's labels a new mail gets by the
//! label's own rules, a built-in detector, a learned sender or the label's classifier.
//!
//! Everything here is pure: the caller hands in the mail, the labels and what was learned (sender
//! counts, classifier counts for the mail's tokens), and gets the decisions back, each with the
//! `source`, `code`, `params` and English `reason` of its log entry. The server keeps what is
//! learned in its database; the UwUMail app, which copies this crate as it is for its other
//! accounts, keeps it on the device. Both decide alike.

mod classifier;
mod detect;
mod mail;
mod rules;
pub mod text;

use std::collections::HashMap;

use serde_json::{Value, json};

pub use classifier::{
    BACKGROUND_CANDIDATES, BACKGROUND_DAYS, MAX_EXAMPLES, MAX_TOKENS, MIN_EVIDENCE, MIN_EXAMPLES, MIN_TOKEN_EXAMPLES,
    Model, THRESHOLD, TOKEN_TEXT_CHARS, TOP_TOKENS, Verdict, token_hash, tokens,
};
pub use detect::{Detector, Finding, amount, date, detect, time};
pub use mail::{Attachment, HEADERS, MAX_TEXT_CHARS, Mail};
pub use rules::{Condition, Field, MAX_CONDITIONS, MAX_VALUE_CHARS, Match, Rules};

/// Hand-labelings of one sender after which their new mail gets the label.
pub const SENDER_MIN_COUNT: i64 = 2;

/// Who put a label on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Rule,
    Detector,
    Sender,
    Classifier,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Rule => "rule",
            Source::Detector => "detector",
            Source::Sender => "sender",
            Source::Classifier => "classifier",
        }
    }
}

/// A label as the deciding needs it.
#[derive(Debug, Clone, Copy)]
pub struct Label<'a> {
    pub id: i64,
    pub keyword: &'a str,
    pub rules: Option<&'a Rules>,
    pub detector: Option<Detector>,
    pub learn_senders: bool,
    pub classifier: bool,
}

/// What was learned that matters for one mail.
#[derive(Debug, Clone, Default)]
pub struct Knowledge {
    /// Per label id: how often the person gave mail from this mail's sender the label by hand.
    pub senders: HashMap<i64, i64>,
    /// Per label id: the classifier, with the counts of this mail's tokens.
    pub models: HashMap<i64, Model>,
}

/// One label a mail gets, with its log entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub label_id: i64,
    pub keyword: String,
    pub source: Source,
    /// `rule`, `sender`, `classifier`, or the detector's name.
    pub code: &'static str,
    pub params: Value,
    pub reason: String,
}

/// The labels `mail` gets. `present` are the keywords it has already (lower case): those labels are
/// skipped. `tokens` are the hashes of [`tokens`] of the mail, for the classifier.
pub fn decide(
    labels: &[Label<'_>],
    mail: &Mail,
    present: &[String],
    knowledge: &Knowledge,
    tokens: &[i64],
) -> Vec<Decision> {
    let mut found: HashMap<Detector, Option<Finding>> = HashMap::new();
    let mut out = Vec::new();
    for label in labels {
        if present.iter().any(|keyword| keyword.eq_ignore_ascii_case(label.keyword)) {
            continue;
        }
        let decision = |source: Source, code: &'static str, params: Value, reason: String| Decision {
            label_id: label.id,
            keyword: label.keyword.to_owned(),
            source,
            code,
            params,
            reason,
        };
        if let Some(matched) = label.rules.and_then(|rules| rules.matches(mail).map(|m| (rules.match_, m))) {
            out.push(decision(
                Source::Rule,
                "rule",
                rule_params(matched.0, &matched.1),
                rule_reason(matched.0, &matched.1),
            ));
            continue;
        }
        if let Some(detector) = label.detector
            && let Some(finding) = found.entry(detector).or_insert_with(|| detect(detector, mail))
        {
            out.push(decision(Source::Detector, detector.as_str(), finding.params.clone(), finding.reason.clone()));
            continue;
        }
        if label.learn_senders
            && let Some(&count) = knowledge.senders.get(&label.id)
            && count >= SENDER_MIN_COUNT
            && !mail.from.is_empty()
        {
            out.push(decision(
                Source::Sender,
                "sender",
                json!({ "address": mail.from, "count": count }),
                format!("{} got this label by hand {count} times", mail.from),
            ));
            continue;
        }
        if label.classifier
            && let Some(verdict) = knowledge.models.get(&label.id).and_then(|model| model.classify(tokens))
        {
            let probability = (verdict.probability * 1000.0).floor() / 1000.0;
            out.push(decision(
                Source::Classifier,
                "classifier",
                json!({ "probability": probability, "examples": verdict.examples }),
                format!(
                    "Similar to the {} mails with this label ({:.1} % sure)",
                    verdict.examples,
                    probability * 100.0
                ),
            ));
        }
    }
    out
}

fn rule_params(match_: Match, matched: &[Condition]) -> Value {
    json!({
        "match": if match_ == Match::All { "all" } else { "any" },
        "conditions": matched,
    })
}

fn rule_reason(match_: Match, matched: &[Condition]) -> String {
    let parts: Vec<String> = matched
        .iter()
        .map(|condition| match condition.field {
            Field::From => format!("sender is {}", condition.value),
            Field::Subject => format!("subject contains \"{}\"", condition.value),
            Field::Text => format!("text contains \"{}\"", condition.value),
            Field::HasAttachment if condition.value == "true" => "has an attachment".to_owned(),
            Field::HasAttachment => "has no attachment".to_owned(),
        })
        .collect();
    let joined = parts.join(if match_ == Match::All { " and " } else { " or " });
    format!("Matches the label's rules: {joined}")
}
