//! Word lists: words, phrases and Rspamd-style regular expressions that count against a message, kept for one
//! person, one of our domains or the whole server, typed in, pasted or subscribed to by link.

use rusqlite::{Row, ToSql, Transaction, params, params_from_iter};
use serde::Serialize;

use crate::{ListOwner, ListScope, Result, Store, StoreError, now};

/// Points for a matching entry that sets none of its own.
pub const WORD_POINTS: f32 = 2.5;
/// Word lists together never add more than this to a message.
pub const WORD_POINTS_MAX: f32 = 10.0;
/// Entries typed in or pasted, per person.
pub const WORD_LIST_PERSONAL_LIMIT: i64 = 2_000;
/// Entries typed in or pasted, for the whole server and per domain.
pub const WORD_LIST_ADMIN_LIMIT: i64 = 20_000;
/// Subscribed lists, per person.
pub const WORD_SOURCES_PERSONAL_LIMIT: i64 = 5;
/// Subscribed lists, for the whole server and per domain.
pub const WORD_SOURCES_ADMIN_LIMIT: i64 = 50;
/// Entries taken from one subscribed list.
pub const WORD_SOURCE_ENTRY_LIMIT: usize = 20_000;
/// A subscribed list may be this big.
pub const WORD_SOURCE_MAX_BYTES: usize = 1024 * 1024;
const PATTERN_MAX_CHARS: usize = 1_000;
pub(crate) const NOTE_MAX_CHARS: usize = 200;
/// A pattern may compile to this much, so a single one cannot eat the server's memory.
pub const PATTERN_SIZE_LIMIT: usize = 1024 * 1024;
/// How many refused lines an import reports.
const MAX_REPORTED: usize = 20;
pub(crate) const WORDS: &str = "words";

fn invalid(message: String) -> StoreError {
    StoreError::Rule { code: "wordInvalid", message }
}

/// The regular expression behind an entry. A word or phrase matches as whole words in any case, with any
/// whitespace between the words; `/regex/flags` is taken as written, with the flags `i`, `m`, `s`, `x` and
/// `u` like Rspamd's regexp maps. What Rust's regex engine does not know (look-around, back-references) is
/// refused, and in exchange no pattern can take longer than linear time.
pub fn word_regex(pattern: &str) -> Result<String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Err(invalid("the entry is empty".into()));
    }
    if pattern.chars().count() > PATTERN_MAX_CHARS {
        return Err(invalid(format!("the entry is longer than {PATTERN_MAX_CHARS} characters")));
    }
    let source = match pattern.strip_prefix('/').and_then(|rest| rest.rsplit_once('/')) {
        Some((body, flags)) => {
            let flags = flags.split_whitespace().next().unwrap_or_default();
            if body.is_empty() {
                return Err(invalid(format!("'{pattern}' is an empty expression")));
            }
            if let Some(flag) = flags.chars().find(|flag| !"imsxu".contains(*flag)) {
                return Err(invalid(format!("'{pattern}' has the unknown flag '{flag}'")));
            }
            let flags: String = flags.chars().filter(|flag| *flag != 'u').collect();
            if flags.is_empty() { body.to_owned() } else { format!("(?{flags}){body}") }
        }
        None => {
            let words: Vec<String> = pattern.split_whitespace().map(regex::escape).collect();
            let first = pattern.chars().next().is_some_and(char::is_alphanumeric);
            let last = pattern.chars().last().is_some_and(char::is_alphanumeric);
            format!("(?i){}{}{}", if first { r"\b" } else { "" }, words.join(r"\s+"), if last { r"\b" } else { "" })
        }
    };
    let compiled = regex::RegexBuilder::new(&source).size_limit(PATTERN_SIZE_LIMIT).build();
    match compiled {
        Ok(regex) if regex.is_match("") => Err(invalid(format!("'{pattern}' matches every message"))),
        Ok(_) => Ok(source),
        Err(err) => {
            let reason = err.to_string().lines().last().unwrap_or_default().trim().to_owned();
            Err(invalid(format!("'{pattern}' is not a usable expression: {reason}")))
        }
    }
}

/// An entry in the form it is stored and compared in: words in lower case with single spaces, expressions
/// as written.
pub fn normalize_word(pattern: &str) -> Result<String> {
    word_regex(pattern)?;
    let pattern = pattern.trim();
    Ok(if pattern.starts_with('/') {
        pattern.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        pattern.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
    })
}

/// The entries of a list as text, one per line: blank lines and `#` comments are skipped, the rest is
/// normalized. Returns what can be used, repeated lines included, and each refused line with the reason.
pub fn parse_word_lines(text: &str) -> (Vec<String>, Vec<(String, String)>) {
    let mut good = Vec::new();
    let mut refused = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match normalize_word(line) {
            Ok(pattern) => good.push(pattern),
            Err(StoreError::Rule { message, .. }) => refused.push((line.to_owned(), message)),
            Err(err) => refused.push((line.to_owned(), err.to_string())),
        }
    }
    (good, refused)
}

pub(crate) fn check_points(points: Option<f32>) -> Result<()> {
    match points {
        Some(points) if !(0.1..=WORD_POINTS_MAX).contains(&points) => {
            Err(invalid(format!("points must be between 0.1 and {WORD_POINTS_MAX}")))
        }
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordEntry {
    pub id: i64,
    pub pattern: String,
    pub points: Option<f32>,
    pub note: String,
    /// The domain of a domain-wide entry.
    pub domain: Option<String>,
    #[serde(skip)]
    pub scope: ListScope,
    pub created_at: i64,
    pub created_by: String,
    pub expires_at: Option<i64>,
    pub hits: i64,
    pub last_hit_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordSource {
    pub id: i64,
    pub url: String,
    pub subject_only: bool,
    pub points: Option<f32>,
    pub domain: Option<String>,
    #[serde(skip)]
    pub scope: ListScope,
    pub fetched_at: Option<i64>,
    #[serde(skip)]
    pub validator: Option<String>,
    pub error: Option<String>,
    /// How many entries the last good fetch brought.
    pub entries: i64,
    pub created_at: i64,
    pub created_by: String,
}

/// What adding several lines at once did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordImport {
    pub added: usize,
    /// Lines already on the list.
    pub duplicates: usize,
    /// Lines that are no usable entry, with the reason; at most the first few.
    pub refused: Vec<RefusedWord>,
    pub refused_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RefusedWord {
    pub line: String,
    pub reason: String,
}

/// One entry as the SMTP side compiles it.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledWord {
    /// The entry, to count its hits; 0 for built-in lists.
    pub id: i64,
    pub scope: ListScope,
    /// The domain of a domain-wide entry.
    pub domain: Option<String>,
    pub pattern: String,
    pub points: f32,
    pub subject_only: bool,
}

pub(crate) fn scope_ids(scope: ListScope) -> (Option<i64>, Option<i64>) {
    match scope {
        ListScope::Server => (None, None),
        ListScope::Domain(id) => (None, Some(id)),
        ListScope::Account(id) => (Some(id), None),
    }
}

pub(crate) fn scope_of(account_id: Option<i64>, domain_id: Option<i64>) -> ListScope {
    match (account_id, domain_id) {
        (Some(id), _) => ListScope::Account(id),
        (None, Some(id)) => ListScope::Domain(id),
        (None, None) => ListScope::Server,
    }
}

/// `alias.account_id` and `alias.domain_id` for a scope, with its values.
pub(crate) fn scope_filter(alias: &str, scope: ListScope) -> (String, Vec<Box<dyn ToSql>>) {
    match scope {
        ListScope::Server => (format!("{alias}.account_id IS NULL AND {alias}.domain_id IS NULL"), vec![]),
        ListScope::Domain(id) => (format!("{alias}.domain_id = ?"), vec![Box::new(id)]),
        ListScope::Account(id) => (format!("{alias}.account_id = ?"), vec![Box::new(id)]),
    }
}

fn owned_by(owner: ListOwner, scope: ListScope) -> bool {
    owner.looks_after(scope)
}

pub(crate) fn bump_version(tx: &Transaction<'_>, name: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO list_versions (name, version) VALUES (?1, 1)
         ON CONFLICT (name) DO UPDATE SET version = version + 1",
        [name],
    )?;
    Ok(())
}

const ENTRY_SELECT: &str = "SELECT w.id, w.pattern, w.points, w.note, d.name, w.account_id, w.domain_id, w.created_at,
        w.created_by, w.expires_at, w.hits, w.last_hit_at
     FROM word_entries w LEFT JOIN domains d ON d.id = w.domain_id";

fn entry(row: &Row<'_>) -> rusqlite::Result<WordEntry> {
    Ok(WordEntry {
        id: row.get(0)?,
        pattern: row.get(1)?,
        points: row.get::<_, Option<f64>>(2)?.map(|points| points as f32),
        note: row.get(3)?,
        domain: row.get(4)?,
        scope: scope_of(row.get(5)?, row.get(6)?),
        created_at: row.get(7)?,
        created_by: row.get(8)?,
        expires_at: row.get(9)?,
        hits: row.get(10)?,
        last_hit_at: row.get(11)?,
    })
}

fn entries(conn: &rusqlite::Connection, filter: &str, values: Vec<Box<dyn ToSql>>) -> Result<Vec<WordEntry>> {
    let mut stmt =
        conn.prepare(&format!("{ENTRY_SELECT} WHERE w.source_id IS NULL AND ({filter}) ORDER BY w.pattern"))?;
    let rows = stmt.query_map(params_from_iter(values), entry)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

const SOURCE_SELECT: &str =
    "SELECT s.id, s.url, s.subject_only, s.points, d.name, s.account_id, s.domain_id, s.fetched_at,
        s.validator, s.error, (SELECT COUNT(*) FROM word_entries w WHERE w.source_id = s.id), s.created_at, s.created_by
     FROM word_sources s LEFT JOIN domains d ON d.id = s.domain_id";

fn source(row: &Row<'_>) -> rusqlite::Result<WordSource> {
    Ok(WordSource {
        id: row.get(0)?,
        url: row.get(1)?,
        subject_only: row.get(2)?,
        points: row.get::<_, Option<f64>>(3)?.map(|points| points as f32),
        domain: row.get(4)?,
        scope: scope_of(row.get(5)?, row.get(6)?),
        fetched_at: row.get(7)?,
        validator: row.get(8)?,
        error: row.get(9)?,
        entries: row.get(10)?,
        created_at: row.get(11)?,
        created_by: row.get(12)?,
    })
}

fn sources(conn: &rusqlite::Connection, filter: &str, values: Vec<Box<dyn ToSql>>) -> Result<Vec<WordSource>> {
    let mut stmt = conn.prepare(&format!("{SOURCE_SELECT} WHERE {filter} ORDER BY s.url"))?;
    let rows = stmt.query_map(params_from_iter(values), source)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn find_source(conn: &rusqlite::Connection, id: i64) -> Result<Option<WordSource>> {
    Ok(sources(conn, "s.id = ?", vec![Box::new(id)])?.pop())
}

impl Store {
    /// One scope's own entries, typed in or pasted.
    pub async fn word_entries(&self, scope: ListScope) -> Result<Vec<WordEntry>> {
        self.read(move |conn| {
            let (filter, values) = scope_filter("w", scope);
            entries(conn, &filter, values)
        })
        .await
    }

    /// The entries admins look after: the whole server's and every domain's.
    pub async fn admin_word_entries(&self) -> Result<Vec<WordEntry>> {
        self.read(|conn| entries(conn, "w.account_id IS NULL", vec![])).await
    }

    /// Adds entries, one per line. Lines already on the list, and what does not fit anymore, are counted and
    /// reported rather than refused as a whole.
    pub async fn add_words(
        &self,
        scope: ListScope,
        text: String,
        points: Option<f32>,
        note: String,
        created_by: String,
    ) -> Result<WordImport> {
        self.add_words_until(scope, text, points, note, created_by, None).await
    }

    /// Adds entries, one per line, that run out at `expires_at` (Unix seconds) when it is set.
    pub async fn add_words_until(
        &self,
        scope: ListScope,
        text: String,
        points: Option<f32>,
        note: String,
        created_by: String,
        expires_at: Option<i64>,
    ) -> Result<WordImport> {
        check_points(points)?;
        let note = note.trim().to_owned();
        if note.chars().count() > NOTE_MAX_CHARS {
            return Err(invalid(format!("the note is longer than {NOTE_MAX_CHARS} characters")));
        }
        let (patterns, refused) = parse_word_lines(&text);
        if patterns.is_empty() && refused.is_empty() {
            return Err(invalid("there is no entry in the text".into()));
        }
        self.write(move |tx| {
            let (filter, values) = scope_filter("w", scope);
            let sql = format!("SELECT COUNT(*) FROM word_entries w WHERE w.source_id IS NULL AND {filter}");
            let mut count: i64 = tx.query_row(&sql, params_from_iter(values), |row| row.get(0))?;
            let limit =
                if matches!(scope, ListScope::Account(_)) { WORD_LIST_PERSONAL_LIMIT } else { WORD_LIST_ADMIN_LIMIT };
            let (account_id, domain_id) = scope_ids(scope);
            let mut report = WordImport::default();
            let mut refused: Vec<RefusedWord> =
                refused.into_iter().map(|(line, reason)| RefusedWord { line, reason }).collect();
            for pattern in patterns {
                let (filter, mut values) = scope_filter("w", scope);
                values.push(Box::new(pattern.clone()));
                let sql = format!(
                    "SELECT COUNT(*) FROM word_entries w WHERE w.source_id IS NULL AND {filter} AND w.pattern = ?"
                );
                let exists: i64 = tx.query_row(&sql, params_from_iter(values), |row| row.get(0))?;
                if exists > 0 {
                    report.duplicates += 1;
                    continue;
                }
                if count >= limit {
                    refused
                        .push(RefusedWord { line: pattern, reason: format!("the list already holds {limit} entries") });
                    continue;
                }
                tx.execute(
                    "INSERT INTO word_entries (account_id, domain_id, pattern, points, note, created_at, created_by,
                                               expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![account_id, domain_id, pattern, points.map(f64::from), note, now(), created_by, expires_at],
                )?;
                count += 1;
                report.added += 1;
            }
            report.refused_count = refused.len();
            refused.truncate(MAX_REPORTED);
            report.refused = refused;
            if report.added > 0 {
                bump_version(tx, WORDS)?;
            }
            Ok(report)
        })
        .await
    }

    /// Removes an entry typed in or pasted, when its owner looks after it.
    pub async fn remove_word_entry(&self, owner: ListOwner, id: i64) -> Result<WordEntry> {
        self.write(move |tx| {
            let found = entries(tx, "w.id = ?", vec![Box::new(id)])?.pop().filter(|entry| owned_by(owner, entry.scope));
            let entry = found.ok_or_else(|| StoreError::NotFound(format!("word list entry {id}")))?;
            tx.execute("DELETE FROM word_entries WHERE id = ?1", [id])?;
            bump_version(tx, WORDS)?;
            Ok(entry)
        })
        .await
    }

    pub async fn word_sources(&self, scope: ListScope) -> Result<Vec<WordSource>> {
        self.read(move |conn| {
            let (filter, values) = scope_filter("s", scope);
            sources(conn, &filter, values)
        })
        .await
    }

    /// The subscribed lists admins look after: the whole server's and every domain's.
    pub async fn admin_word_sources(&self) -> Result<Vec<WordSource>> {
        self.read(|conn| sources(conn, "s.account_id IS NULL", vec![])).await
    }

    /// Subscribes to a list by link. Fetching it is up to the caller.
    pub async fn add_word_source(
        &self,
        scope: ListScope,
        url: String,
        subject_only: bool,
        points: Option<f32>,
        created_by: String,
    ) -> Result<WordSource> {
        check_points(points)?;
        let url = url.trim().to_owned();
        if url.is_empty() || url.len() > 2_000 {
            return Err(StoreError::Rule {
                code: "wordSourceInvalid",
                message: "the link is empty or too long".into(),
            });
        }
        self.write(move |tx| {
            let (filter, mut values) = scope_filter("s", scope);
            let sql = format!("SELECT COUNT(*) FROM word_sources s WHERE {filter}");
            let count: i64 =
                tx.query_row(&sql, params_from_iter(values.iter().map(|v| v.as_ref())), |row| row.get(0))?;
            let limit = if matches!(scope, ListScope::Account(_)) {
                WORD_SOURCES_PERSONAL_LIMIT
            } else {
                WORD_SOURCES_ADMIN_LIMIT
            };
            if count >= limit {
                let message = format!("there are already {count} subscribed lists");
                return Err(StoreError::Rule { code: "wordSourcesFull", message });
            }
            values.push(Box::new(url.clone()));
            let sql = format!("SELECT COUNT(*) FROM word_sources s WHERE {filter} AND s.url = ?");
            let exists: i64 = tx.query_row(&sql, params_from_iter(values), |row| row.get(0))?;
            if exists > 0 {
                return Err(StoreError::Rule {
                    code: "wordSourceListed",
                    message: format!("{url} is subscribed already"),
                });
            }
            let (account_id, domain_id) = scope_ids(scope);
            tx.execute(
                "INSERT INTO word_sources (account_id, domain_id, url, subject_only, points, created_at, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![account_id, domain_id, url, subject_only, points.map(f64::from), now(), created_by],
            )?;
            find_source(tx, tx.last_insert_rowid())?.ok_or_else(|| StoreError::Internal("the new list vanished".into()))
        })
        .await
    }

    pub async fn word_source(&self, id: i64) -> Result<Option<WordSource>> {
        self.read(move |conn| find_source(conn, id)).await
    }

    /// Unsubscribes, together with what the list brought.
    pub async fn remove_word_source(&self, owner: ListOwner, id: i64) -> Result<WordSource> {
        self.write(move |tx| {
            let found = find_source(tx, id)?.filter(|source| owned_by(owner, source.scope));
            let source = found.ok_or_else(|| StoreError::NotFound(format!("subscribed list {id}")))?;
            tx.execute("DELETE FROM word_sources WHERE id = ?1", [id])?;
            bump_version(tx, WORDS)?;
            Ok(source)
        })
        .await
    }

    /// Subscribed lists not fetched since `before`.
    pub async fn word_sources_due(&self, before: i64) -> Result<Vec<WordSource>> {
        self.read(move |conn| sources(conn, "s.fetched_at IS NULL OR s.fetched_at < ?", vec![Box::new(before)])).await
    }

    /// Replaces what a subscribed list brought with a fresh fetch.
    pub async fn replace_word_source_entries(
        &self,
        id: i64,
        patterns: Vec<String>,
        validator: Option<String>,
    ) -> Result<()> {
        self.write(move |tx| {
            let Some(source) = find_source(tx, id)? else { return Ok(()) };
            let (account_id, domain_id) = scope_ids(source.scope);
            tx.execute("DELETE FROM word_entries WHERE source_id = ?1", [id])?;
            let at = now();
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO word_entries (account_id, domain_id, source_id, pattern, created_at, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, '')",
            )?;
            for pattern in patterns.iter().take(WORD_SOURCE_ENTRY_LIMIT) {
                insert.execute(params![account_id, domain_id, id, pattern, at])?;
            }
            drop(insert);
            tx.execute(
                "UPDATE word_sources SET fetched_at = ?1, validator = ?2, error = NULL WHERE id = ?3",
                params![at, validator, id],
            )?;
            bump_version(tx, WORDS)
        })
        .await
    }

    /// Notes a fetch that brought nothing new (`error` is `None`) or failed.
    pub async fn word_source_fetched(&self, id: i64, error: Option<String>) -> Result<()> {
        self.write(move |tx| {
            tx.execute("UPDATE word_sources SET fetched_at = ?1, error = ?2 WHERE id = ?3", params![now(), error, id])?;
            Ok(())
        })
        .await
    }

    /// Changes whenever an entry is added or removed or a list is fetched anew.
    pub async fn word_lists_version(&self) -> Result<i64> {
        self.read(|conn| {
            let version =
                conn.query_row("SELECT version FROM list_versions WHERE name = ?1", [WORDS], |row| row.get(0));
            match version {
                Ok(version) => Ok(version),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
                Err(err) => Err(err.into()),
            }
        })
        .await
    }

    /// Every entry of every scope, with the points and place it counts with, to compile them.
    pub async fn compiled_words(&self) -> Result<Vec<CompiledWord>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT w.account_id, w.domain_id, d.name, w.pattern, COALESCE(w.points, s.points),
                        COALESCE(s.subject_only, 0), w.id
                 FROM word_entries w
                 LEFT JOIN word_sources s ON s.id = w.source_id
                 LEFT JOIN domains d ON d.id = w.domain_id
                 WHERE w.expires_at IS NULL OR w.expires_at > strftime('%s', 'now')",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(CompiledWord {
                    id: row.get(6)?,
                    scope: scope_of(row.get(0)?, row.get(1)?),
                    domain: row.get(2)?,
                    pattern: row.get(3)?,
                    points: row.get::<_, Option<f64>>(4)?.map_or(WORD_POINTS, |points| points as f32),
                    subject_only: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    #[test]
    fn words_become_whole_word_patterns_and_expressions_stay_as_written() {
        let matches =
            |pattern: &str, text: &str| regex::Regex::new(&word_regex(pattern).unwrap()).unwrap().is_match(text);
        assert!(matches("Viagra", "Günstig VIAGRA kaufen"));
        assert!(!matches("sex", "Sussex ist schön"), "whole words only");
        assert!(matches("web development", "Your web\n development partner"));
        assert!(matches("/\\sviagra\\s/i", "buy Viagra now"));
        assert!(matches("/bitcoin/i", "BITCOINS"));
        assert!(matches("50% off", "now 50% off!"), "no word boundary after a sign");

        assert!(word_regex("/(?=x)y/i").is_err(), "look-around is not supported");
        assert!(word_regex("/.*/").is_err(), "matches everything");
        assert!(word_regex("/abc/g").is_err(), "unknown flag");
        assert!(word_regex("   ").is_err());
        assert_eq!(normalize_word("  Web   Development ").unwrap(), "web development");
        assert_eq!(normalize_word("/\\sWeb\\s/i").unwrap(), "/\\sWeb\\s/i");
    }

    #[test]
    fn a_pasted_list_skips_comments_and_reports_what_cannot_be_used() {
        let text = "# bad words\n/\\serotic\\s/i\n\n/\\serotic\\s/i\ncasino\n/(?<=x)y/\n";
        let (good, refused) = parse_word_lines(text);
        assert_eq!(good, ["/\\serotic\\s/i", "/\\serotic\\s/i", "casino"], "repeats count as duplicates later");
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].0, "/(?<=x)y/");
    }

    #[tokio::test]
    async fn entries_and_subscribed_lists_belong_to_their_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let domain = store.create_domain("example.de").await.unwrap();
        let account = NewAccount {
            address: "leni@example.de".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let leni = store.create_account(account).await.unwrap().id;
        let version = store.word_lists_version().await.unwrap();

        let report = store
            .add_words(ListScope::Server, "casino\nCasino\n/(?=x)/\nlottery".into(), None, String::new(), "cli".into())
            .await
            .unwrap();
        assert_eq!((report.added, report.duplicates, report.refused_count), (2, 1, 1));
        assert!(store.word_lists_version().await.unwrap() > version);
        let again =
            store.add_words(ListScope::Server, "casino".into(), None, String::new(), "cli".into()).await.unwrap();
        assert_eq!((again.added, again.duplicates), (0, 1));
        store
            .add_words(ListScope::Account(leni), "casino".into(), Some(5.0), String::new(), String::new())
            .await
            .unwrap();
        assert!(
            store
                .add_words(ListScope::Account(leni), "x".into(), Some(11.0), String::new(), String::new())
                .await
                .is_err()
        );

        let source = store
            .add_word_source(
                ListScope::Domain(domain.id),
                "https://lists.example/bad.map".into(),
                true,
                Some(3.0),
                "chef".into(),
            )
            .await
            .unwrap();
        assert!(
            store
                .add_word_source(ListScope::Domain(domain.id), source.url.clone(), false, None, String::new())
                .await
                .is_err()
        );
        store
            .replace_word_source_entries(source.id, vec!["/\\sviagra\\s/i".into()], Some("\"v1\"".into()))
            .await
            .unwrap();
        let sources = store.admin_word_sources().await.unwrap();
        assert_eq!((sources[0].entries, sources[0].validator.as_deref()), (1, Some("\"v1\"")));
        assert!(
            store.admin_word_entries().await.unwrap().iter().all(|entry| entry.pattern != "/\\sviagra\\s/i"),
            "own entries only"
        );

        let compiled = store.compiled_words().await.unwrap();
        assert_eq!(compiled.len(), 4);
        let viagra = compiled.iter().find(|word| word.pattern.contains("viagra")).unwrap();
        assert_eq!((viagra.points, viagra.subject_only, viagra.domain.as_deref()), (3.0, true, Some("example.de")));
        let own = compiled.iter().find(|word| word.scope == ListScope::Account(leni)).unwrap();
        assert_eq!(own.points, 5.0);
        let lottery = compiled.iter().find(|word| word.pattern == "lottery").unwrap();
        assert_eq!(lottery.points, WORD_POINTS);

        let entry = store.word_entries(ListScope::Account(leni)).await.unwrap().remove(0);
        assert!(
            store.remove_word_entry(ListOwner::Account(leni + 1), entry.id).await.is_err(),
            "nobody else removes a person's entry"
        );
        store.remove_word_entry(ListOwner::Admin, entry.id).await.unwrap();
        store.remove_word_source(ListOwner::Admin, source.id).await.unwrap();
        assert_eq!(store.compiled_words().await.unwrap().len(), 2, "a list takes its entries along");
    }
}
