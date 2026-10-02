//! Labels without a model (docs/labels.md): which of a person's labels a new mail gets by the
//! label's own rules, a built-in detector, a learned sender, its nearest neighbours or the label's
//! classifier, with the base labels and their definitions, the facts a model is given, and the
//! check whether a new label overlaps with others.
//!
//! Everything here is pure: the caller hands in the mail, the labels and what was learned (sender
//! counts, classifier counts for the mail's tokens), and gets the decisions back, each with the
//! `source`, `code`, `params` and English `reason` of its log entry. The server keeps what is
//! learned in its database; the UwUMail app, which copies this crate as it is for its other
//! accounts, keeps it on the device. Both decide alike.

mod base;
mod classifier;
mod detect;
mod facts;
mod mail;
mod overlap;
mod rules;
pub mod similar;
pub mod text;

use std::collections::HashMap;

use serde_json::{Value, json};

pub use base::{Base, BaseText};
pub use classifier::{
    BACKGROUND_CANDIDATES, BACKGROUND_DAYS, MAX_EXAMPLES, MAX_TOKENS, MIN_EVIDENCE, MIN_EXAMPLES, MIN_TOKEN_EXAMPLES,
    Model, THRESHOLD, TOKEN_TEXT_CHARS, TOP_TOKENS, Verdict, token_hash, tokens,
};
pub use detect::{Detector, Finding, amount, date, detect, time};
pub use facts::{FREEMAIL, Facts, SenderKind};
pub use mail::{Attachment, HEADERS, MAX_ATTACHMENTS, MAX_FIELD_CHARS, MAX_RECIPIENTS, MAX_TEXT_CHARS, Mail};
pub use overlap::{Overlap, OverlapLabel, overlaps};
pub use rules::{Condition, Field, MAX_CONDITIONS, MAX_VALUE_CHARS, Match, Rules};
pub use similar::Neighbour;

/// Hand-labelings of one sender after which their new mail gets the label.
pub const SENDER_MIN_COUNT: i64 = 2;
/// Labels one mail gets at most: a main label and a second one.
pub const MAX_LABELS: usize = 2;
/// How sure the deciding must be for the main label …
pub const MAIN_THRESHOLD: f64 = 0.8;
/// … and for a second one beside it. Below, a mail rather gets no label than a wrong one.
pub const SECOND_THRESHOLD: f64 = 0.88;

/// Who put a label on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Rule,
    Detector,
    Sender,
    Classifier,
    /// Like the person's mails with the label (nearest neighbours, [`similar`]).
    Similar,
    /// The model (only the server's and the app's assistant use it).
    Ai,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Rule => "rule",
            Source::Detector => "detector",
            Source::Sender => "sender",
            Source::Classifier => "classifier",
            Source::Similar => "similar",
            Source::Ai => "ai",
        }
    }

    /// Which of two equally sure sources wins: the person's own words first.
    fn rank(self) -> u8 {
        match self {
            Source::Rule => 0,
            Source::Sender => 1,
            Source::Detector => 2,
            Source::Similar => 3,
            Source::Classifier => 4,
            Source::Ai => 5,
        }
    }
}

/// A label as the deciding needs it.
#[derive(Debug, Clone, Copy)]
pub struct Label<'a> {
    pub id: i64,
    pub keyword: &'a str,
    pub rules: Option<&'a Rules>,
    /// The label's own detector; a base label without one uses its base's.
    pub detector: Option<Detector>,
    pub learn_senders: bool,
    pub classifier: bool,
    /// Which base label it is, if any.
    pub base: Option<Base>,
    /// Put on by itself at all; a switched-off label is only put on by hand.
    pub auto: bool,
}

impl Label<'_> {
    /// The detector that is run for it.
    pub fn detector(&self) -> Option<Detector> {
        self.detector.or(self.base.map(Base::detector))
    }

    /// The base label it stands for: its own, or the one its detector belongs to.
    pub fn meaning(&self) -> Option<Base> {
        self.base.or_else(|| {
            self.detector.and_then(|detector| Base::ALL.into_iter().find(|base| base.detector() == detector))
        })
    }
}

/// How like a label a mail is by its nearest neighbours among the person's labeled mails.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Likeness {
    pub confidence: f64,
    /// Neighbours with the label, and the best similarity among them.
    pub neighbours: usize,
    pub similarity: f64,
}

/// What was learned that matters for one mail.
#[derive(Debug, Clone, Default)]
pub struct Knowledge {
    /// Per label id: how often the person gave mail from this mail's sender the label by hand; below
    /// zero when they took it off such mail by hand, which keeps it off the sender's mail.
    pub senders: HashMap<i64, i64>,
    /// Per label id: the classifier, with the counts of this mail's tokens.
    pub models: HashMap<i64, Model>,
    /// Per label id: what the nearest neighbours say.
    pub similar: HashMap<i64, Likeness>,
}

/// One label a mail gets, with its log entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub label_id: i64,
    pub keyword: String,
    pub source: Source,
    /// `rule`, `sender`, `classifier`, `similar`, `ai`, or the detector's name.
    pub code: &'static str,
    pub params: Value,
    pub reason: String,
    /// How sure, 0 to 1.
    pub confidence: f64,
}

/// The labels `mail` gets: [`candidates`], then [`choose`]. `present` are the keywords it has
/// already (lower case). `tokens` are the hashes of [`tokens`] of the mail, for the classifier.
pub fn decide(
    labels: &[Label<'_>],
    mail: &Mail,
    present: &[String],
    knowledge: &Knowledge,
    tokens: &[i64],
) -> Vec<Decision> {
    let found = candidates(labels, mail, present, knowledge, tokens);
    choose(labels, present, found)
}

/// Every label that speaks for itself, each once with its surest source, however sure: labels
/// switched off and labels already on the mail are left out, and a sender the person took a label
/// off by hand gets it only by the label's rules.
pub fn candidates(
    labels: &[Label<'_>],
    mail: &Mail,
    present: &[String],
    knowledge: &Knowledge,
    tokens: &[i64],
) -> Vec<Decision> {
    let view = detect::View::new(mail);
    let mut found: HashMap<Detector, Option<Finding>> = HashMap::new();
    let mut out = Vec::new();
    for label in labels {
        if !label.auto || present.iter().any(|keyword| keyword.eq_ignore_ascii_case(label.keyword)) {
            continue;
        }
        let decision = |source: Source, code: &'static str, params: Value, reason: String, confidence: f64| Decision {
            label_id: label.id,
            keyword: label.keyword.to_owned(),
            source,
            code,
            params,
            reason,
            confidence,
        };
        if let Some(matched) = label.rules.and_then(|rules| rules.matches(mail).map(|m| (rules.match_, m))) {
            out.push(decision(
                Source::Rule,
                "rule",
                rule_params(matched.0, &matched.1),
                rule_reason(matched.0, &matched.1),
                1.0,
            ));
            continue;
        }
        let count = knowledge.senders.get(&label.id).copied().unwrap_or(0);
        if count < 0 && !mail.from.is_empty() {
            continue;
        }
        let mut best: Option<Decision> = None;
        let mut offer = |candidate: Decision| {
            if best.as_ref().is_none_or(|known| {
                candidate.confidence > known.confidence
                    || (candidate.confidence == known.confidence && candidate.source.rank() < known.source.rank())
            }) {
                best = Some(candidate);
            }
        };
        if let Some(detector) = label.detector()
            && let Some(finding) = found.entry(detector).or_insert_with(|| view.run(detector))
        {
            offer(decision(
                Source::Detector,
                detector.as_str(),
                finding.params.clone(),
                finding.reason.clone(),
                finding.confidence,
            ));
        }
        if label.learn_senders && count >= SENDER_MIN_COUNT && !mail.from.is_empty() && mail.from_trusted {
            offer(decision(
                Source::Sender,
                "sender",
                json!({ "address": mail.from, "count": count }),
                format!("{} got this label by hand {count} times", mail.from),
                if count >= 5 { 0.95 } else { 0.9 },
            ));
        }
        if let Some(likeness) = knowledge.similar.get(&label.id)
            && likeness.confidence > 0.0
        {
            let similarity = (likeness.similarity * 1000.0).floor() / 1000.0;
            offer(decision(
                Source::Similar,
                "similar",
                json!({ "neighbours": likeness.neighbours, "similarity": similarity }),
                format!(
                    "Like {} of your mails with this label ({:.0} % alike)",
                    likeness.neighbours,
                    similarity * 100.0
                ),
                likeness.confidence.clamp(0.0, 0.95),
            ));
        }
        if label.classifier
            && let Some(verdict) = knowledge.models.get(&label.id).and_then(|model| model.classify(tokens))
        {
            let probability = (verdict.probability * 1000.0).floor() / 1000.0;
            offer(decision(
                Source::Classifier,
                "classifier",
                json!({ "probability": probability, "examples": verdict.examples }),
                format!(
                    "Similar to the {} mails with this label ({:.1} % sure)",
                    verdict.examples,
                    probability * 100.0
                ),
                0.9,
            ));
        }
        out.extend(best);
    }
    out
}

/// Of `candidates`, the main label and perhaps a second one: the main one when it is sure enough
/// ([`MAIN_THRESHOLD`]), a second one only when it is surer still ([`SECOND_THRESHOLD`]), does not
/// exclude the main one ([`Base::excludes`]) and was not found the same way. Labels on the mail
/// already (`present`) count: with one there, only a second one may come; with two, none.
pub fn choose(labels: &[Label<'_>], present: &[String], mut candidates: Vec<Decision>) -> Vec<Decision> {
    let order = |id: i64| labels.iter().position(|label| label.id == id).unwrap_or(usize::MAX);
    let meaning = |id: i64| labels.iter().find(|label| label.id == id).and_then(Label::meaning);
    let on: Vec<&Label<'_>> = labels
        .iter()
        .filter(|label| present.iter().any(|keyword| keyword.eq_ignore_ascii_case(label.keyword)))
        .collect();
    if on.len() >= MAX_LABELS {
        return Vec::new();
    }
    candidates.retain(|candidate| !on.iter().any(|label| label.id == candidate.label_id));
    candidates.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then(a.source.rank().cmp(&b.source.rank()))
            .then(order(a.label_id).cmp(&order(b.label_id)))
    });
    let excluded = |a: Option<Base>, b: Option<Base>| matches!((a, b), (Some(a), Some(b)) if a.excludes(b));
    let mut chosen: Vec<Decision> = Vec::new();
    for candidate in candidates {
        let main = on.is_empty() && chosen.is_empty();
        let threshold = if main { MAIN_THRESHOLD } else { SECOND_THRESHOLD };
        if candidate.confidence < threshold {
            break;
        }
        let its = meaning(candidate.label_id);
        let clash = on.iter().any(|label| excluded(label.meaning(), its))
            || chosen.iter().any(|known| {
                excluded(meaning(known.label_id), its)
                    || (known.source == Source::Detector
                        && candidate.source == Source::Detector
                        && known.code == candidate.code)
            });
        if clash {
            continue;
        }
        chosen.push(candidate);
        if on.len() + chosen.len() >= MAX_LABELS {
            break;
        }
    }
    chosen
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
