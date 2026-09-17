//! Word lists, compiled into regular expression sets per scope: the whole server's, each domain's and each
//! person's. They are compiled again only when the lists changed.

use std::collections::HashMap;
use std::sync::Arc;

use regex::{RegexSet, RegexSetBuilder};
use uwumail_store::{CompiledWord, ListScope, PATTERN_SIZE_LIMIT, WORD_POINTS_MAX, word_regex};

use crate::Context;

/// Patterns per compiled set; a set of thousands of big patterns could exceed the size limits at once.
const CHUNK: usize = 250;
/// How many matching entries a hit names.
const NAMED: usize = 3;

/// The entries of one scope.
#[derive(Default)]
pub(crate) struct Scope {
    text: Vec<(RegexSet, Vec<(f32, String)>)>,
    subject: Vec<(RegexSet, Vec<(f32, String)>)>,
}

/// What a scope found: the points, and the first entries that matched.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Found {
    pub points: f32,
    pub entries: Vec<String>,
}

impl Found {
    pub fn detail(&self) -> String {
        self.entries.iter().take(NAMED).cloned().collect::<Vec<_>>().join(", ")
    }
}

fn compile(words: Vec<(f32, String)>) -> Vec<(RegexSet, Vec<(f32, String)>)> {
    let mut sets = Vec::new();
    let usable: Vec<(String, (f32, String))> = words
        .into_iter()
        .filter_map(|(points, pattern)| Some((word_regex(&pattern).ok()?, (points, pattern))))
        .collect();
    for chunk in usable.chunks(CHUNK) {
        let sources = chunk.iter().map(|(source, _)| source.as_str());
        let limit = PATTERN_SIZE_LIMIT.saturating_mul(chunk.len()).min(256 * 1024 * 1024);
        match RegexSetBuilder::new(sources).size_limit(limit).build() {
            Ok(set) => sets.push((set, chunk.iter().map(|(_, entry)| entry.clone()).collect())),
            Err(err) => tracing::warn!(%err, "a part of a word list does not compile and is left out"),
        }
    }
    sets
}

impl Scope {
    fn new(words: Vec<(f32, String, bool)>) -> Scope {
        let (subject, text): (Vec<_>, Vec<_>) = words.into_iter().partition(|(_, _, subject_only)| *subject_only);
        Scope {
            text: compile(text.into_iter().map(|(points, pattern, _)| (points, pattern)).collect()),
            subject: compile(subject.into_iter().map(|(points, pattern, _)| (points, pattern)).collect()),
        }
    }

    /// What matches in the subject and the text. Each entry counts once, the sum at most the maximum.
    pub fn find(&self, subject: &str, text: &str) -> Found {
        let mut found = Found::default();
        let sets = self.text.iter().map(|set| (set, text)).chain(self.subject.iter().map(|set| (set, subject)));
        for ((set, entries), haystack) in sets {
            for index in set.matches(haystack).iter() {
                let (points, pattern) = &entries[index];
                found.points += points;
                found.entries.push(pattern.clone());
            }
        }
        found.points = found.points.min(WORD_POINTS_MAX);
        found
    }
}

/// Every scope's compiled entries at one version of the lists.
#[derive(Default)]
pub(crate) struct Compiled {
    version: i64,
    pub server: Scope,
    pub domains: HashMap<String, Scope>,
    pub accounts: HashMap<i64, Scope>,
}

impl Compiled {
    fn new(version: i64, words: Vec<CompiledWord>) -> Compiled {
        let mut server = Vec::new();
        let mut domains: HashMap<String, Vec<_>> = HashMap::new();
        let mut accounts: HashMap<i64, Vec<_>> = HashMap::new();
        for word in words {
            let entry = (word.points, word.pattern, word.subject_only);
            match (word.scope, word.domain) {
                (ListScope::Server, _) => server.push(entry),
                (ListScope::Domain(_), Some(domain)) => domains.entry(domain).or_default().push(entry),
                (ListScope::Domain(_), None) => {}
                (ListScope::Account(id), _) => accounts.entry(id).or_default().push(entry),
            }
        }
        Compiled {
            version,
            server: Scope::new(server),
            domains: domains.into_iter().map(|(domain, words)| (domain, Scope::new(words))).collect(),
            accounts: accounts.into_iter().map(|(id, words)| (id, Scope::new(words))).collect(),
        }
    }
}

/// The word lists as they are now, compiled again when they changed since the last message.
pub(crate) async fn compiled(ctx: &Context) -> Arc<Compiled> {
    let mut cached = ctx.word_lists.lock().await;
    let version = match ctx.store.word_lists_version().await {
        Ok(version) => version,
        Err(err) => {
            tracing::warn!(%err, "reading the version of the word lists failed");
            return cached.clone().unwrap_or_default();
        }
    };
    if let Some(compiled) = cached.as_ref().filter(|compiled| compiled.version == version) {
        return compiled.clone();
    }
    let words = match ctx.store.compiled_words().await {
        Ok(words) => words,
        Err(err) => {
            tracing::warn!(%err, "reading the word lists failed");
            return cached.clone().unwrap_or_default();
        }
    };
    let count = words.len();
    let compiled =
        tokio::task::spawn_blocking(move || Arc::new(Compiled::new(version, words))).await.unwrap_or_default();
    tracing::debug!(entries = count, version, "compiled the word lists");
    *cached = Some(compiled.clone());
    compiled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(scope: ListScope, domain: Option<&str>, pattern: &str, points: f32, subject_only: bool) -> CompiledWord {
        CompiledWord { scope, domain: domain.map(str::to_owned), pattern: pattern.into(), points, subject_only }
    }

    #[test]
    fn each_scope_counts_its_own_entries_up_to_the_maximum() {
        let compiled = Compiled::new(
            1,
            vec![
                word(ListScope::Server, None, "casino", 2.5, false),
                word(ListScope::Server, None, r"/\slottery\s/i", 4.0, true),
                word(ListScope::Server, None, "/(?=x)/", 9.0, false),
                word(ListScope::Domain(1), Some("example.de"), "gewinn", 6.0, false),
                word(ListScope::Domain(1), Some("example.de"), "jackpot", 6.0, false),
                word(ListScope::Account(7), None, "fußball", 1.0, false),
            ],
        );
        let subject = "Your LOTTERY win";
        let text = "Visit our casino, win the lottery jackpot and a Gewinn";
        let server = compiled.server.find(subject, text);
        assert_eq!(server.points, 6.5, "the subject-only entry counts once, the text lottery not at all");
        assert_eq!(server.detail(), "casino, /\\slottery\\s/i");
        let domain = compiled.domains["example.de"].find(subject, text);
        assert_eq!(domain.points, WORD_POINTS_MAX, "capped");
        assert_eq!(compiled.accounts[&7].find("", "Fußball am Sonntag").points, 1.0);
    }
}
