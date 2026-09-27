//! Word lists, compiled into regular expression sets per scope: the whole server's, each domain's and each
//! person's.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use regex_automata::util::syntax;
use regex_automata::{Input, MatchKind, PatternSet, meta};
use uwumail_store::{CompiledWord, ListScope, PATTERN_SIZE_LIMIT, WORD_POINTS_MAX, word_regex};

/// Patterns per compiled set; a set of thousands of big patterns could exceed the size limits at once.
const CHUNK: usize = 250;
/// How many matching entries a hit names.
const NAMED: usize = 3;
/// The most one person's lists may take compiled, own entries and subscribed lists together. Each
/// expression may compile to up to [`PATTERN_SIZE_LIMIT`], and a person may keep thousands: without
/// a budget, one person's lists took gigabytes (security-audit-0.16.0 SMTP-7). Entries beyond it
/// are left out, in the order the lists hold them.
pub(crate) const PERSON_BUDGET: usize = 16 * 1024 * 1024;
/// The same for the whole server's lists and for each domain's, which admins keep.
pub(crate) const SHARED_BUDGET: usize = 128 * 1024 * 1024;

/// One compiled set of entries.
struct Set {
    regex: meta::Regex,
    words: Vec<Word>,
}

/// The entries of one scope.
#[derive(Default)]
pub(crate) struct Scope {
    text: Vec<Set>,
    subject: Vec<Set>,
}

/// One compiled entry: its points, its pattern and its id (0 for built-in lists).
#[derive(Clone)]
struct Word {
    points: f32,
    pattern: String,
    id: i64,
}

impl Hash for Word {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.points.to_bits().hash(state);
        self.pattern.hash(state);
        self.id.hash(state);
    }
}

/// What a scope found: the points, and the first entries that matched.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Found {
    pub points: f32,
    pub entries: Vec<String>,
    /// The ids of the entries that matched, to count their hits.
    pub ids: Vec<i64>,
}

impl Found {
    pub fn detail(&self) -> String {
        self.entries.iter().take(NAMED).cloned().collect::<Vec<_>>().join(", ")
    }
}

/// Compiles `words` in sets of [`CHUNK`], as long as `budget` (bytes, shared with the scope's other
/// sets) lasts.
fn compile(words: Vec<Word>, budget: &mut usize) -> Vec<Set> {
    let mut sets = Vec::new();
    let usable: Vec<(String, Word)> =
        words.into_iter().filter_map(|word| Some((word_regex(&word.pattern).ok()?, word))).collect();
    for chunk in usable.chunks(CHUNK) {
        if !compile_part(chunk, budget, &mut sets) {
            tracing::warn!("a word list is bigger than its scope may take compiled; the rest is left out");
            break;
        }
    }
    sets
}

/// Compiles `part` into one set if it fits `budget`, or in halves down to single entries. `false`
/// once an entry does not fit any more.
fn compile_part(part: &[(String, Word)], budget: &mut usize, sets: &mut Vec<Set>) -> bool {
    let full = PATTERN_SIZE_LIMIT.saturating_mul(part.len()).min(256 * 1024 * 1024);
    let limit = full.min(*budget);
    let config = meta::Config::new()
        .match_kind(MatchKind::All)
        .utf8_empty(true)
        .nfa_size_limit(Some(limit))
        .hybrid_cache_capacity(2 * 1024 * 1024);
    let sources: Vec<&str> = part.iter().map(|(source, _)| source.as_str()).collect();
    let built = meta::Builder::new().configure(config).syntax(syntax::Config::new().utf8(true)).build_many(&sources);
    let too_big = match built {
        Ok(regex) if regex.memory_usage() <= *budget => {
            *budget -= regex.memory_usage();
            sets.push(Set { regex, words: part.iter().map(|(_, entry)| entry.clone()).collect() });
            return true;
        }
        Ok(_) => true,
        Err(_) if limit < full => true,
        Err(err) => {
            tracing::warn!(%err, "a part of a word list does not compile and is left out");
            return true;
        }
    };
    if too_big && part.len() > 1 {
        let (first, second) = part.split_at(part.len() / 2);
        return compile_part(first, budget, sets) && compile_part(second, budget, sets);
    }
    false
}

impl Scope {
    fn new(words: Vec<(Word, bool)>, budget: usize) -> Scope {
        let mut budget = budget;
        let (subject, text): (Vec<_>, Vec<_>) = words.into_iter().partition(|(_, subject_only)| *subject_only);
        Scope {
            text: compile(text.into_iter().map(|(word, _)| word).collect(), &mut budget),
            subject: compile(subject.into_iter().map(|(word, _)| word).collect(), &mut budget),
        }
    }

    /// What the compiled sets take.
    #[cfg(test)]
    fn memory(&self) -> usize {
        self.text.iter().chain(&self.subject).map(|set| set.regex.memory_usage()).sum()
    }

    /// What matches in the subject and the text. Each entry counts once, the sum at most the maximum.
    pub fn find(&self, subject: &str, text: &str) -> Found {
        let mut found = Found::default();
        let sets = self.text.iter().map(|set| (set, text)).chain(self.subject.iter().map(|set| (set, subject)));
        for (set, haystack) in sets {
            let mut matched = PatternSet::new(set.regex.pattern_len());
            set.regex.which_overlapping_matches(&Input::new(haystack), &mut matched);
            for index in matched.iter() {
                let word = &set.words[index.as_usize()];
                found.points += word.points;
                found.entries.push(word.pattern.clone());
                if word.id > 0 {
                    found.ids.push(word.id);
                }
            }
        }
        found.points = found.points.min(WORD_POINTS_MAX);
        found
    }
}

/// Every scope's compiled entries.
#[derive(Default)]
pub(crate) struct Compiled {
    pub server: Arc<Scope>,
    pub domains: HashMap<String, Arc<Scope>>,
    pub accounts: HashMap<i64, Arc<Scope>>,
    /// Each scope's fingerprint, so a new compile can take over what did not change.
    fingerprints: HashMap<ScopeKey, (u64, Arc<Scope>)>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ScopeKey {
    Server,
    Domain(String),
    Account(i64),
}

fn fingerprint(words: &[(Word, bool)]) -> u64 {
    let mut hasher = DefaultHasher::new();
    words.hash(&mut hasher);
    hasher.finish()
}

impl Compiled {
    #[cfg(test)]
    pub fn new(words: Vec<CompiledWord>) -> Compiled {
        Compiled::rebuild(&Compiled::default(), words)
    }

    /// Compiles `words`, taking over from `previous` every scope whose entries did not change: one
    /// person's edit compiles only that person's lists again, not everybody's.
    pub fn rebuild(previous: &Compiled, words: Vec<CompiledWord>) -> Compiled {
        Compiled::rebuild_within(previous, words, PERSON_BUDGET, SHARED_BUDGET)
    }

    fn rebuild_within(previous: &Compiled, words: Vec<CompiledWord>, person: usize, shared: usize) -> Compiled {
        let mut grouped: HashMap<ScopeKey, Vec<(Word, bool)>> = HashMap::new();
        for word in words {
            let key = match (word.scope, word.domain) {
                (ListScope::Server, _) => ScopeKey::Server,
                (ListScope::Domain(_), Some(domain)) => ScopeKey::Domain(domain),
                (ListScope::Domain(_), None) => continue,
                (ListScope::Account(id), _) => ScopeKey::Account(id),
            };
            let entry = (Word { points: word.points, pattern: word.pattern, id: word.id }, word.subject_only);
            grouped.entry(key).or_default().push(entry);
        }
        grouped.entry(ScopeKey::Server).or_default();
        let mut compiled = Compiled::default();
        for (key, words) in grouped {
            let print = fingerprint(&words);
            let scope = match previous.fingerprints.get(&key) {
                Some((known, scope)) if *known == print => scope.clone(),
                _ => {
                    let budget = if matches!(key, ScopeKey::Account(_)) { person } else { shared };
                    Arc::new(Scope::new(words, budget))
                }
            };
            match &key {
                ScopeKey::Server => compiled.server = scope.clone(),
                ScopeKey::Domain(domain) => {
                    compiled.domains.insert(domain.clone(), scope.clone());
                }
                ScopeKey::Account(id) => {
                    compiled.accounts.insert(*id, scope.clone());
                }
            }
            compiled.fingerprints.insert(key, (print, scope));
        }
        compiled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(scope: ListScope, domain: Option<&str>, pattern: &str, points: f32, subject_only: bool) -> CompiledWord {
        CompiledWord { id: 1, scope, domain: domain.map(str::to_owned), pattern: pattern.into(), points, subject_only }
    }

    #[test]
    fn each_scope_counts_its_own_entries_up_to_the_maximum() {
        let compiled = Compiled::new(vec![
            word(ListScope::Server, None, "casino", 2.5, false),
            word(ListScope::Server, None, r"/\slottery\s/i", 4.0, true),
            word(ListScope::Server, None, "/(?=x)/", 9.0, false),
            word(ListScope::Domain(1), Some("example.org"), "gewinn", 6.0, false),
            word(ListScope::Domain(1), Some("example.org"), "jackpot", 6.0, false),
            word(ListScope::Account(7), None, "fußball", 1.0, false),
        ]);
        let subject = "Your LOTTERY win";
        let text = "Visit our casino, win the lottery jackpot and a Gewinn";
        let server = compiled.server.find(subject, text);
        assert_eq!(server.points, 6.5, "the subject-only entry counts once, the text lottery not at all");
        assert_eq!(server.detail(), "casino, /\\slottery\\s/i");
        let domain = compiled.domains["example.org"].find(subject, text);
        assert_eq!(domain.points, WORD_POINTS_MAX, "capped");
        assert_eq!(compiled.accounts[&7].find("", "Fußball am Sonntag").points, 1.0);
    }

    #[test]
    fn a_person_s_lists_take_no_more_than_their_budget() {
        // security-audit-0.16.0 SMTP-7: every expression could compile to a megabyte, and a person
        // may keep thousands of them.
        let heavy = |n: usize| word(ListScope::Account(7), None, &format!(r"/\w{{4}}x{n}y/"), 1.0, false);
        let one = Compiled::new(vec![heavy(0)]).accounts[&7].memory();
        let budget = one * 5;
        let compiled =
            Compiled::rebuild_within(&Compiled::default(), (0..20).map(heavy).collect(), budget, SHARED_BUDGET);
        let scope = &compiled.accounts[&7];
        assert!(scope.memory() <= budget, "{} of {budget} bytes", scope.memory());
        assert_eq!(scope.find("", "abcdx0y").points, 1.0, "what fits is used");
        assert_eq!(scope.find("", "abcdx19y").points, 0.0, "what does not is left out");
    }

    #[test]
    fn one_person_s_edit_compiles_only_their_lists() {
        let words = |mini: &str| {
            vec![
                word(ListScope::Server, None, "casino", 2.5, false),
                word(ListScope::Account(7), None, mini, 1.0, false),
                word(ListScope::Account(8), None, "fußball", 1.0, false),
            ]
        };
        let first = Compiled::new(words("katzen"));
        let second = Compiled::rebuild(&first, words("hunde"));
        assert!(!Arc::ptr_eq(&first.accounts[&7], &second.accounts[&7]), "the edited list is compiled again");
        assert!(Arc::ptr_eq(&first.accounts[&8], &second.accounts[&8]), "the others are taken over");
        assert!(Arc::ptr_eq(&first.server, &second.server));
        assert_eq!(second.accounts[&7].find("", "Hunde und Katzen").entries, vec!["hunde"]);
    }
}
