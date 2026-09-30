//! The classifier: tokens of a mail and a naive Bayes model per label, learned from the person's
//! own hand-labeling (docs/labels.md, "Classifier").

use std::collections::{HashMap, HashSet};

use crate::Mail;
use crate::text::words;

/// Tokens of one mail, at most.
pub const MAX_TOKENS: usize = 400;
/// Characters of the text that give tokens.
pub const TOKEN_TEXT_CHARS: usize = 20_000;
const MIN_WORD_CHARS: usize = 3;
const MAX_WORD_CHARS: usize = 24;
/// Examples with and without the label before the classifier decides anything.
pub const MIN_EXAMPLES: i64 = 15;
/// Tokens seen in fewer examples than this say nothing.
pub const MIN_TOKEN_EXAMPLES: i64 = 2;
/// The most telling tokens that are summed.
pub const TOP_TOKENS: usize = 40;
/// How sure it must be.
pub const THRESHOLD: f64 = 0.99;
/// Summed tokens that must speak for the label.
pub const MIN_EVIDENCE: usize = 3;
/// Examples one person keeps; the oldest go first.
pub const MAX_EXAMPLES: usize = 3000;
/// Recent inbox mail one unlabeled example is picked from, and how recent it must be.
pub const BACKGROUND_CANDIDATES: usize = 200;
pub const BACKGROUND_DAYS: i64 = 60;

/// The tokens of a mail, each once, in order: sender, subject words, text words.
pub fn tokens(mail: &Mail) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut push = |token: String| {
        if out.len() < MAX_TOKENS && seen.insert(token.clone()) {
            out.push(token);
        }
    };
    if !mail.from.is_empty() {
        push(format!("from:{}", mail.from));
        let domain = mail.from_domain();
        if !domain.is_empty() {
            push(format!("domain:{domain}"));
        }
    }
    for word in token_words(&mail.subject) {
        push(format!("subject:{word}"));
    }
    let text = match mail.text.char_indices().nth(TOKEN_TEXT_CHARS) {
        Some((cut, _)) => &mail.text[..cut],
        None => &mail.text,
    };
    for word in token_words(text) {
        push(word);
    }
    out
}

fn token_words(text: &str) -> impl Iterator<Item = String> + '_ {
    words(text).filter_map(|(_, word)| {
        let chars = word.chars().count();
        let digits = word.chars().all(|c| c.is_ascii_digit());
        ((MIN_WORD_CHARS..=MAX_WORD_CHARS).contains(&chars) && !digits).then(|| word.to_lowercase())
    })
}

/// A token as it is stored: the 64-bit FNV-1a hash of its UTF-8 bytes.
pub fn token_hash(token: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in token.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as i64
}

/// What a label's classifier learned about the tokens of one mail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    /// Examples with the label, and all other examples.
    pub positives: i64,
    pub negatives: i64,
    /// Per token hash: in how many examples with and without the label it occurs.
    pub counts: HashMap<i64, (i64, i64)>,
}

/// The classifier's verdict: how sure it is, and how many examples it has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub probability: f64,
    pub examples: i64,
}

impl Model {
    /// Whether it has learned enough to decide.
    pub fn ready(&self) -> bool {
        self.positives >= MIN_EXAMPLES && self.negatives >= MIN_EXAMPLES
    }

    /// The probability that a mail with `tokens` (hashes, in token order) has the label, and how
    /// many of the summed tokens speak for it; `None` before the model is ready.
    pub fn probability(&self, tokens: &[i64]) -> Option<(f64, usize)> {
        if !self.ready() {
            return None;
        }
        let (pos, neg) = (self.positives as f64, self.negatives as f64);
        let mut weights: Vec<(usize, f64)> = Vec::new();
        let mut seen = HashSet::new();
        for (index, token) in tokens.iter().enumerate() {
            if !seen.insert(*token) {
                continue;
            }
            let Some(&(p, n)) = self.counts.get(token) else { continue };
            // Counts come from a store: a damaged one must neither overflow nor turn into NaN.
            let (p, n) = (p.max(0), n.max(0));
            if p.saturating_add(n) < MIN_TOKEN_EXAMPLES {
                continue;
            }
            let weight = ((p as f64 + 1.0) / (pos + 2.0)).ln() - ((n as f64 + 1.0) / (neg + 2.0)).ln();
            weights.push((index, weight));
        }
        // The most telling first; equally telling ones in token order.
        weights.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()).then(a.0.cmp(&b.0)));
        weights.truncate(TOP_TOKENS);
        let prior = (pos / neg).ln().min(0.0);
        let logit = prior + weights.iter().map(|(_, w)| w).sum::<f64>();
        let evidence = weights.iter().filter(|(_, w)| *w > 0.0).count();
        Some((1.0 / (1.0 + (-logit).exp()), evidence))
    }

    /// A verdict when the model is sure enough that the mail has the label.
    pub fn classify(&self, tokens: &[i64]) -> Option<Verdict> {
        let (probability, evidence) = self.probability(tokens)?;
        (probability >= THRESHOLD && evidence >= MIN_EVIDENCE)
            .then_some(Verdict { probability, examples: self.positives })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_fnv1a() {
        assert_eq!(token_hash(""), 0xcbf2_9ce4_8422_2325_u64 as i64);
        assert_eq!(token_hash("a"), 0xaf63_dc4c_8601_ec8c_u64 as i64);
    }
}
