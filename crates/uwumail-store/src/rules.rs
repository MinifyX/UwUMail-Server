//! Sender and word list entries together, as the spam filter's rules: one table to search, filter, sort
//! and page through, however many domains, people and entries there are, and the changes that work on
//! several entries at once. Subscribed lists stay apart; their entries are the list's, not anyone's rules.

use rusqlite::{ToSql, params, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::sender_lists::{self, check_free, check_room, kind_from};
use crate::word_lists::{self, NOTE_MAX_CHARS, WORDS, bump_version, check_points, normalize_word};
use crate::{
    ListOwner, ListScope, NewSenderListEntry, RefusedWord, Result, SenderKind, SenderList, Store, StoreError, now,
};

/// Rules on one page at most.
pub const RULES_PAGE_MAX: usize = 500;
/// Lines one import takes.
pub const RULES_IMPORT_MAX: usize = 20_000;
/// Rules one bulk change takes.
pub const RULES_BULK_MAX: usize = 5_000;
const MAX_REPORTED: usize = 20;

/// Which table a rule lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleType {
    Sender,
    Word,
}

/// What a rule does: let a sender through, keep one out, or add points for words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleList {
    Allow,
    Block,
    Points,
}

/// Whose rule it is, with a name for people to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum RuleScope {
    Server,
    Domain { id: i64, name: String },
    Account { id: i64, name: String },
}

impl RuleScope {
    pub fn scope(&self) -> ListScope {
        match self {
            RuleScope::Server => ListScope::Server,
            RuleScope::Domain { id, .. } => ListScope::Domain(*id),
            RuleScope::Account { id, .. } => ListScope::Account(*id),
        }
    }

    /// `server`, `domain:example.de` or `account:leni@example.de`, as filters and exports write it.
    pub fn key(&self) -> String {
        match self {
            RuleScope::Server => "server".into(),
            RuleScope::Domain { name, .. } => format!("domain:{name}"),
            RuleScope::Account { name, .. } => format!("account:{name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    pub id: i64,
    pub list: RuleList,
    /// A sender kind (`address`, `domain`, `pattern`, `ip`, `host`), or `word` / `regex`.
    pub kind: String,
    pub value: String,
    pub note: String,
    /// Words only: the points, `None` for the default.
    pub points: Option<f32>,
    pub scope: RuleScope,
    pub expires_at: Option<i64>,
    pub hits: i64,
    pub last_hit_at: Option<i64>,
    pub created_at: i64,
    pub created_by: String,
}

/// Which scopes a query looks at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ScopeFilter {
    #[default]
    All,
    Server,
    /// Every domain's rules.
    Domains,
    /// Every person's rules.
    Accounts,
    One(ListScope),
}

/// Narrows rules down beyond their scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleState {
    /// Runs out at some point.
    Temporary,
    /// Never decided anything.
    Unused,
    /// Did not decide anything in 90 days (or never).
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleSort {
    #[default]
    Value,
    Created,
    Hits,
    LastHit,
    Expires,
}

#[derive(Debug, Clone, Default)]
pub struct RuleQuery {
    pub owner: Option<ListOwner>,
    pub scope: ScopeFilter,
    /// Part of the value or the note, any case.
    pub search: String,
    pub lists: Vec<RuleList>,
    pub kinds: Vec<String>,
    pub state: Option<RuleState>,
    pub sort: RuleSort,
    pub descending: bool,
    pub offset: usize,
    pub limit: usize,
}

/// One page of rules and what the filters would find.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePage {
    pub rules: Vec<Rule>,
    /// Rules matching every filter.
    pub total: i64,
    /// Per list and per kind, matching every other filter, for the counts on the filter chips.
    pub lists: std::collections::BTreeMap<String, i64>,
    pub kinds: std::collections::BTreeMap<String, i64>,
}

/// A change to one rule. Fields left out stay.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RuleChange {
    pub value: Option<String>,
    /// Senders only: `allow` or `block`.
    pub list: Option<RuleList>,
    /// Senders only; guessed again from a new value when left out.
    pub kind: Option<SenderKind>,
    pub note: Option<String>,
    /// Words only: `Some(None)` goes back to the default.
    #[serde(deserialize_with = "some")]
    pub points: Option<Option<f32>>,
    #[serde(deserialize_with = "some")]
    pub expires_at: Option<Option<i64>>,
    /// Moves the rule to another scope.
    #[serde(skip)]
    pub scope: Option<ListScope>,
}

/// `null` as "set to none", and a missing field as "leave it".
fn some<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// What to do with several rules at once.
#[derive(Debug, Clone)]
pub enum BulkAction {
    Delete,
    /// Senders only.
    SetList(SenderList),
    SetScope(ListScope),
    SetExpiry(Option<i64>),
}

/// What a bulk change did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkReport {
    pub changed: usize,
    /// Rules the change did not fit, with the reason; at most the first few.
    pub skipped: Vec<RefusedWord>,
    pub skipped_count: usize,
}

/// Many rules at once, one per line.
#[derive(Debug, Clone)]
pub struct RuleImport {
    pub rule_type: RuleType,
    pub scope: ListScope,
    /// Senders: the list every line goes on, unless the line says otherwise.
    pub list: SenderList,
    pub text: String,
    pub note: String,
    pub points: Option<f32>,
    pub expires_at: Option<i64>,
    pub created_by: String,
}

/// What an import did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub added: usize,
    pub duplicates: usize,
    pub refused: Vec<RefusedWord>,
    pub refused_count: usize,
}

const RULES: &str = "WITH rules AS (
    SELECT 'sender' AS type, l.id AS id, l.list AS list, l.kind AS kind, l.value AS value, l.note AS note,
           NULL AS points, l.account_id AS account_id, l.domain_id AS domain_id, d.name AS domain, a.login AS account,
           l.expires_at AS expires_at, l.hits AS hits, l.last_hit_at AS last_hit_at, l.created_at AS created_at,
           l.created_by AS created_by
    FROM sender_lists l LEFT JOIN domains d ON d.id = l.domain_id LEFT JOIN accounts a ON a.id = l.account_id
    UNION ALL
    SELECT 'word', w.id, 'points', CASE WHEN substr(w.pattern, 1, 1) = '/' THEN 'regex' ELSE 'word' END, w.pattern,
           w.note, w.points, w.account_id, w.domain_id, d.name, a.login, w.expires_at, w.hits, w.last_hit_at,
           w.created_at, w.created_by
    FROM word_entries w LEFT JOIN domains d ON d.id = w.domain_id LEFT JOIN accounts a ON a.id = w.account_id
    WHERE w.source_id IS NULL
)";

const COLUMNS: &str = "type, id, list, kind, value, note, points, account_id, domain_id, domain, account, expires_at,
    hits, last_hit_at, created_at, created_by";

fn rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Rule> {
    let rule_type: String = row.get(0)?;
    let list: String = row.get(2)?;
    let account_id: Option<i64> = row.get(7)?;
    let domain_id: Option<i64> = row.get(8)?;
    let scope = match (account_id, domain_id) {
        (Some(id), _) => RuleScope::Account { id, name: row.get::<_, Option<String>>(10)?.unwrap_or_default() },
        (None, Some(id)) => RuleScope::Domain { id, name: row.get::<_, Option<String>>(9)?.unwrap_or_default() },
        (None, None) => RuleScope::Server,
    };
    Ok(Rule {
        rule_type: if rule_type == "word" { RuleType::Word } else { RuleType::Sender },
        id: row.get(1)?,
        list: match list.as_str() {
            "allow" => RuleList::Allow,
            "block" => RuleList::Block,
            _ => RuleList::Points,
        },
        kind: row.get(3)?,
        value: row.get(4)?,
        note: row.get(5)?,
        points: row.get::<_, Option<f64>>(6)?.map(|points| points as f32),
        scope,
        expires_at: row.get(11)?,
        hits: row.get(12)?,
        last_hit_at: row.get(13)?,
        created_at: row.get(14)?,
        created_by: row.get(15)?,
    })
}

fn list_word(list: RuleList) -> &'static str {
    match list {
        RuleList::Allow => "allow",
        RuleList::Block => "block",
        RuleList::Points => "points",
    }
}

/// The WHERE clause for a query, leaving out the list filter or the kind filter, for the chip counts.
fn filter(query: &RuleQuery, lists: bool, kinds: bool) -> (String, Vec<Box<dyn ToSql>>) {
    let mut parts: Vec<String> = vec!["1 = 1".into()];
    let mut values: Vec<Box<dyn ToSql>> = Vec::new();
    match query.owner {
        Some(ListOwner::Account(id)) => {
            parts.push("account_id = ?".into());
            values.push(Box::new(id));
        }
        Some(ListOwner::Admin) | None => {}
    }
    match &query.scope {
        ScopeFilter::All => {}
        ScopeFilter::Server => parts.push("account_id IS NULL AND domain_id IS NULL".into()),
        ScopeFilter::Domains => parts.push("domain_id IS NOT NULL".into()),
        ScopeFilter::Accounts => parts.push("account_id IS NOT NULL".into()),
        ScopeFilter::One(ListScope::Server) => parts.push("account_id IS NULL AND domain_id IS NULL".into()),
        ScopeFilter::One(ListScope::Domain(id)) => {
            parts.push("domain_id = ?".into());
            values.push(Box::new(*id));
        }
        ScopeFilter::One(ListScope::Account(id)) => {
            parts.push("account_id = ?".into());
            values.push(Box::new(*id));
        }
    }
    let search = query.search.trim().to_lowercase();
    if !search.is_empty() {
        let like = format!("%{}%", search.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        parts.push(
            "(lower(value) LIKE ? ESCAPE '\\' OR lower(note) LIKE ? ESCAPE '\\' OR lower(coalesce(domain, account, ''))
              LIKE ? ESCAPE '\\')"
                .into(),
        );
        for _ in 0..3 {
            values.push(Box::new(like.clone()));
        }
    }
    if lists && !query.lists.is_empty() {
        parts.push(format!("list IN ({})", vec!["?"; query.lists.len()].join(", ")));
        values.extend(query.lists.iter().map(|list| Box::new(list_word(*list)) as Box<dyn ToSql>));
    }
    if kinds && !query.kinds.is_empty() {
        parts.push(format!("kind IN ({})", vec!["?"; query.kinds.len()].join(", ")));
        values.extend(query.kinds.iter().map(|kind| Box::new(kind.clone()) as Box<dyn ToSql>));
    }
    match query.state {
        Some(RuleState::Temporary) => parts.push("expires_at IS NOT NULL".into()),
        Some(RuleState::Unused) => parts.push("hits = 0".into()),
        Some(RuleState::Stale) => {
            parts.push("(last_hit_at IS NULL OR last_hit_at < ?)".into());
            values.push(Box::new(now() - 90 * 86_400));
        }
        None => {}
    }
    (parts.join(" AND "), values)
}

fn order(query: &RuleQuery) -> String {
    let direction = if query.descending { "DESC" } else { "ASC" };
    let column = match query.sort {
        RuleSort::Value => "lower(value)",
        RuleSort::Created => "created_at",
        RuleSort::Hits => "hits",
        RuleSort::LastHit => "coalesce(last_hit_at, 0)",
        RuleSort::Expires => "coalesce(expires_at, 9223372036854775807)",
    };
    format!("{column} {direction}, type, id")
}

fn counts(
    conn: &rusqlite::Connection,
    column: &str,
    query: &RuleQuery,
) -> Result<std::collections::BTreeMap<String, i64>> {
    let (lists, kinds) = (column != "list", column != "kind");
    let (clause, values) = filter(query, lists, kinds);
    let sql = format!("{RULES} SELECT {column}, COUNT(*) FROM rules WHERE {clause} GROUP BY {column}");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn one(conn: &rusqlite::Connection, rule_type: RuleType, id: i64) -> Result<Option<Rule>> {
    let kind = if rule_type == RuleType::Word { "word" } else { "sender" };
    let sql = format!("{RULES} SELECT {COLUMNS} FROM rules WHERE type = ?1 AND id = ?2");
    match conn.query_row(&sql, params![kind, id], rule) {
        Ok(rule) => Ok(Some(rule)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn not_found(rule_type: RuleType, id: i64) -> StoreError {
    StoreError::NotFound(format!("{} rule {id}", if rule_type == RuleType::Word { "word" } else { "sender" }))
}

fn note_ok(note: &str) -> Result<String> {
    let note = note.trim().to_owned();
    if note.chars().count() > NOTE_MAX_CHARS {
        return Err(StoreError::Rule {
            code: "ruleInvalid",
            message: format!("the note is longer than {NOTE_MAX_CHARS} characters"),
        });
    }
    Ok(note)
}

/// Applies a change to one rule inside a transaction. Returns whether the word lists changed.
fn change_one(
    tx: &rusqlite::Transaction<'_>,
    owner: ListOwner,
    rule_type: RuleType,
    id: i64,
    change: &RuleChange,
) -> Result<bool> {
    let current = one(tx, rule_type, id)?.filter(|rule| owner.looks_after(rule.scope.scope()));
    let current = current.ok_or_else(|| not_found(rule_type, id))?;
    let scope = change.scope.unwrap_or(current.scope.scope());
    if !owner.looks_after(scope) {
        return Err(not_found(rule_type, id));
    }
    let moved = scope != current.scope.scope();
    let note = match &change.note {
        Some(note) => note_ok(note)?,
        None => current.note.clone(),
    };
    let expires_at = change.expires_at.unwrap_or(current.expires_at);
    let (account_id, domain_id) = scope.ids();
    match rule_type {
        RuleType::Sender => {
            let value = change.value.clone().unwrap_or_else(|| current.value.clone());
            let kind = match (change.kind, &change.value) {
                (Some(kind), _) => kind,
                (None, Some(value)) => sender_lists::guess_sender_kind(value),
                (None, None) => kind_from(&current.kind),
            };
            let value = sender_lists::normalize_sender(kind, &value)?;
            let list = match change.list.unwrap_or(current.list) {
                RuleList::Allow => SenderList::Allow,
                RuleList::Block => SenderList::Block,
                RuleList::Points => {
                    return Err(StoreError::Rule {
                        code: "ruleInvalid",
                        message: "a sender is allowed or blocked, not given points".into(),
                    });
                }
            };
            check_free(tx, scope, kind, &value, Some(id))?;
            if moved {
                check_room(tx, scope)?;
            }
            tx.execute(
                "UPDATE sender_lists SET account_id = ?1, domain_id = ?2, list = ?3, kind = ?4, value = ?5, note = ?6,
                        expires_at = ?7
                 WHERE id = ?8",
                params![account_id, domain_id, list.as_str(), kind.as_str(), value, note, expires_at, id],
            )?;
            Ok(false)
        }
        RuleType::Word => {
            let pattern = match &change.value {
                Some(value) => normalize_word(value)?,
                None => current.value.clone(),
            };
            let points = change.points.unwrap_or(current.points);
            check_points(points)?;
            let (clause, mut values) = word_lists::scope_filter("w", scope);
            values.push(Box::new(pattern.clone()));
            values.push(Box::new(id));
            let sql = format!(
                "SELECT COUNT(*) FROM word_entries w WHERE w.source_id IS NULL AND {clause} AND w.pattern = ? AND w.id != ?"
            );
            let taken: i64 = tx.query_row(&sql, params_from_iter(values), |row| row.get(0))?;
            if taken > 0 {
                return Err(StoreError::Rule {
                    code: "wordListed",
                    message: format!("{pattern} is already on the list"),
                });
            }
            tx.execute(
                "UPDATE word_entries SET account_id = ?1, domain_id = ?2, pattern = ?3, points = ?4, note = ?5,
                        expires_at = ?6
                 WHERE id = ?7",
                params![account_id, domain_id, pattern, points.map(f64::from), note, expires_at, id],
            )?;
            Ok(true)
        }
    }
}

fn delete_one(tx: &rusqlite::Transaction<'_>, owner: ListOwner, rule_type: RuleType, id: i64) -> Result<Rule> {
    let rule = one(tx, rule_type, id)?.filter(|rule| owner.looks_after(rule.scope.scope()));
    let rule = rule.ok_or_else(|| not_found(rule_type, id))?;
    match rule_type {
        RuleType::Sender => tx.execute("DELETE FROM sender_lists WHERE id = ?1", [id])?,
        RuleType::Word => tx.execute("DELETE FROM word_entries WHERE id = ?1", [id])?,
    };
    Ok(rule)
}

/// A line of an import: the value, and for senders optionally `allow` / `block` after a comma or tab, and a
/// note after that. `# comments` and empty lines are skipped.
fn import_line(line: &str) -> Option<(String, Option<SenderList>, Option<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut parts = line.splitn(3, [',', '\t', ';']).map(str::trim);
    let value = parts.next()?.trim_matches('"').to_owned();
    let second = parts.next().map(|part| part.trim_matches('"').to_ascii_lowercase());
    let (list, note) = match second.as_deref() {
        Some("allow") | Some("erlauben") | Some("allowed") => (Some(SenderList::Allow), parts.next()),
        Some("block") | Some("blockieren") | Some("blocked") => (Some(SenderList::Block), parts.next()),
        Some(_) => (None, line.split_once([',', '\t', ';']).map(|(_, rest)| rest.trim())),
        None => (None, None),
    };
    Some((value, list, note.map(|note| note.trim_matches('"').to_owned()).filter(|note| !note.is_empty())))
}

impl Store {
    /// One page of rules, with the counts for the filters.
    pub async fn rules(&self, query: RuleQuery) -> Result<RulePage> {
        self.read(move |conn| {
            let (clause, mut values) = filter(&query, true, true);
            let total: i64 = conn.query_row(
                &format!("{RULES} SELECT COUNT(*) FROM rules WHERE {clause}"),
                params_from_iter(values.iter().map(|value| value.as_ref())),
                |row| row.get(0),
            )?;
            let limit = query.limit.clamp(1, RULES_PAGE_MAX);
            values.push(Box::new(limit as i64));
            values.push(Box::new(query.offset as i64));
            let sql = format!(
                "{RULES} SELECT {COLUMNS} FROM rules WHERE {clause} ORDER BY {} LIMIT ? OFFSET ?",
                order(&query)
            );
            let mut stmt = conn.prepare(&sql)?;
            let rules = stmt.query_map(params_from_iter(values), rule)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(RulePage { rules, total, lists: counts(conn, "list", &query)?, kinds: counts(conn, "kind", &query)? })
        })
        .await
    }

    /// Every rule matching the filters, for an export.
    pub async fn all_rules(&self, mut query: RuleQuery) -> Result<Vec<Rule>> {
        let mut rules = Vec::new();
        query.offset = 0;
        query.limit = RULES_PAGE_MAX;
        loop {
            let page = self.rules(query.clone()).await?;
            let done = page.rules.len() < RULES_PAGE_MAX;
            rules.extend(page.rules);
            if done {
                return Ok(rules);
            }
            query.offset += RULES_PAGE_MAX;
        }
    }

    pub async fn rule(&self, rule_type: RuleType, id: i64) -> Result<Option<Rule>> {
        self.read(move |conn| one(conn, rule_type, id)).await
    }

    /// Changes one rule its owner looks after, and returns it as it is now.
    pub async fn change_rule(
        &self,
        owner: ListOwner,
        rule_type: RuleType,
        id: i64,
        change: RuleChange,
    ) -> Result<Rule> {
        self.write(move |tx| {
            if change_one(tx, owner, rule_type, id, &change)? {
                bump_version(tx, WORDS)?;
            }
            one(tx, rule_type, id)?.ok_or_else(|| not_found(rule_type, id))
        })
        .await
    }

    /// Does one thing to many rules. What does not fit (a value already on the target list, a full list) is
    /// skipped and reported, the rest changes.
    pub async fn bulk_rules(
        &self,
        owner: ListOwner,
        items: Vec<(RuleType, i64)>,
        action: BulkAction,
    ) -> Result<BulkReport> {
        if items.len() > RULES_BULK_MAX {
            return Err(StoreError::Rule {
                code: "ruleInvalid",
                message: format!("at most {RULES_BULK_MAX} rules at once"),
            });
        }
        self.write(move |tx| {
            let mut report = BulkReport::default();
            let mut skipped = Vec::new();
            let mut words_changed = false;
            for (rule_type, id) in items {
                let outcome = match &action {
                    BulkAction::Delete => delete_one(tx, owner, rule_type, id).map(|_| rule_type == RuleType::Word),
                    BulkAction::SetList(_) if rule_type == RuleType::Word => Err(StoreError::Rule {
                        code: "ruleInvalid",
                        message: "word rules add points; they are not allowed or blocked".into(),
                    }),
                    BulkAction::SetList(list) => {
                        let list = if *list == SenderList::Allow { RuleList::Allow } else { RuleList::Block };
                        let change = RuleChange { list: Some(list), ..RuleChange::default() };
                        change_one(tx, owner, rule_type, id, &change)
                    }
                    BulkAction::SetScope(scope) => {
                        let change = RuleChange { scope: Some(*scope), ..RuleChange::default() };
                        change_one(tx, owner, rule_type, id, &change)
                    }
                    BulkAction::SetExpiry(expires_at) => {
                        let change = RuleChange { expires_at: Some(*expires_at), ..RuleChange::default() };
                        change_one(tx, owner, rule_type, id, &change)
                    }
                };
                match outcome {
                    Ok(words) => {
                        report.changed += 1;
                        words_changed |= words;
                    }
                    Err(StoreError::Rule { message, .. }) | Err(StoreError::NotFound(message)) => {
                        skipped.push(RefusedWord { line: format!("{id}"), reason: message });
                    }
                    Err(err) => return Err(err),
                }
            }
            if words_changed {
                bump_version(tx, WORDS)?;
            }
            report.skipped_count = skipped.len();
            skipped.truncate(MAX_REPORTED);
            report.skipped = skipped;
            Ok(report)
        })
        .await
    }

    /// Adds many rules, one per line. Words go through the word list import; senders may say `allow` or
    /// `block` and a note after the value.
    pub async fn import_rules(&self, import: RuleImport) -> Result<ImportReport> {
        if import.text.lines().count() > RULES_IMPORT_MAX {
            return Err(StoreError::Rule {
                code: "ruleInvalid",
                message: format!("at most {RULES_IMPORT_MAX} lines at once"),
            });
        }
        if import.rule_type == RuleType::Word {
            let words = self
                .add_words_until(
                    import.scope,
                    import.text,
                    import.points,
                    import.note,
                    import.created_by,
                    import.expires_at,
                )
                .await?;
            return Ok(ImportReport {
                added: words.added,
                duplicates: words.duplicates,
                refused: words.refused,
                refused_count: words.refused_count,
            });
        }
        let note = note_ok(&import.note)?;
        self.write(move |tx| {
            let mut report = ImportReport::default();
            let mut refused = Vec::new();
            for line in import.text.lines() {
                let Some((value, list, line_note)) = import_line(line) else { continue };
                let new = NewSenderListEntry {
                    scope: import.scope,
                    list: list.unwrap_or(import.list),
                    kind: None,
                    value: value.clone(),
                    note: line_note.unwrap_or_else(|| note.clone()),
                    created_by: import.created_by.clone(),
                    expires_at: import.expires_at,
                };
                let outcome = sender_lists::checked(&new)
                    .and_then(|(kind, value, note)| sender_lists::insert(tx, &new, kind, &value, &note));
                match outcome {
                    Ok(_) => report.added += 1,
                    Err(StoreError::Rule { code: "senderListed", .. }) => report.duplicates += 1,
                    Err(StoreError::Rule { message, .. }) => refused.push(RefusedWord { line: value, reason: message }),
                    Err(err) => return Err(err),
                }
            }
            report.refused_count = refused.len();
            refused.truncate(MAX_REPORTED);
            report.refused = refused;
            Ok(report)
        })
        .await
    }

    /// The scopes rules can belong to, with how many each has: the whole server, the domains and the people
    /// matching `search`, the people with the most rules first.
    pub async fn rule_scopes(&self, search: String, limit: usize) -> Result<Vec<(RuleScope, i64)>> {
        self.read(move |conn| {
            let like = format!("%{}%", search.trim().to_lowercase().replace(['%', '_'], ""));
            let limit = limit.clamp(1, 200) as i64;
            let mut found = Vec::new();
            let server: i64 = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM sender_lists WHERE account_id IS NULL AND domain_id IS NULL)
                      + (SELECT COUNT(*) FROM word_entries
                         WHERE account_id IS NULL AND domain_id IS NULL AND source_id IS NULL)",
                [],
                |row| row.get(0),
            )?;
            found.push((RuleScope::Server, server));
            let mut domains = conn.prepare(
                "SELECT d.id, d.name,
                        (SELECT COUNT(*) FROM sender_lists WHERE domain_id = d.id)
                      + (SELECT COUNT(*) FROM word_entries WHERE domain_id = d.id AND source_id IS NULL)
                 FROM domains d WHERE lower(d.name) LIKE ?1 ORDER BY d.name LIMIT ?2",
            )?;
            let rows = domains.query_map(params![like, limit], |row| {
                Ok((RuleScope::Domain { id: row.get(0)?, name: row.get(1)? }, row.get(2)?))
            })?;
            found.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
            let mut accounts = conn.prepare(
                "SELECT a.id, a.login,
                        (SELECT COUNT(*) FROM sender_lists WHERE account_id = a.id)
                      + (SELECT COUNT(*) FROM word_entries WHERE account_id = a.id AND source_id IS NULL) AS rules
                 FROM accounts a WHERE lower(a.login) LIKE ?1 ORDER BY rules DESC, a.login LIMIT ?2",
            )?;
            let rows = accounts.query_map(params![like, limit], |row| {
                Ok((RuleScope::Account { id: row.get(0)?, name: row.get(1)? }, row.get(2)?))
            })?;
            found.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
            Ok(found)
        })
        .await
    }

    /// Counts that these rules decided for a message.
    pub async fn note_rule_hits(&self, senders: Vec<i64>, words: Vec<i64>) -> Result<()> {
        if senders.is_empty() && words.is_empty() {
            return Ok(());
        }
        self.write(move |tx| {
            let at = now();
            for id in senders {
                tx.execute("UPDATE sender_lists SET hits = hits + 1, last_hit_at = ?1 WHERE id = ?2", params![at, id])?;
            }
            for id in words {
                tx.execute("UPDATE word_entries SET hits = hits + 1, last_hit_at = ?1 WHERE id = ?2", params![at, id])?;
            }
            Ok(())
        })
        .await
    }

    /// Removes rules that ran out. Returns how many.
    pub async fn remove_expired_rules(&self) -> Result<usize> {
        self.write(|tx| {
            let at = now();
            let senders =
                tx.execute("DELETE FROM sender_lists WHERE expires_at IS NOT NULL AND expires_at <= ?1", [at])?;
            let words =
                tx.execute("DELETE FROM word_entries WHERE expires_at IS NOT NULL AND expires_at <= ?1", [at])?;
            if words > 0 {
                bump_version(tx, WORDS)?;
            }
            Ok(senders + words)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    async fn store() -> (Store, tempfile::TempDir, i64, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let domain = store.create_domain("example.de").await.unwrap().id;
        let account = NewAccount {
            address: "leni@example.de".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let leni = store.create_account(account).await.unwrap().id;
        (store, dir, domain, leni)
    }

    fn import(rule_type: RuleType, scope: ListScope, text: &str) -> RuleImport {
        RuleImport {
            rule_type,
            scope,
            list: SenderList::Block,
            text: text.into(),
            note: String::new(),
            points: None,
            expires_at: None,
            created_by: "nyu".into(),
        }
    }

    #[tokio::test]
    async fn rules_of_every_scope_are_searched_filtered_and_paged_together() {
        let (store, _dir, domain, leni) = store().await;
        let text = (0..60).map(|n| format!("spam{n}@evil.example")).collect::<Vec<_>>().join("\n");
        let report = store.import_rules(import(RuleType::Sender, ListScope::Server, &text)).await.unwrap();
        assert_eq!(report.added, 60);
        let lines = "news.example.org, allow, the newsletter\n*.xyz\n# a comment\nnot valid@\nspam1@evil.example";
        let report = store.import_rules(import(RuleType::Sender, ListScope::Domain(domain), lines)).await.unwrap();
        assert_eq!((report.added, report.refused_count, report.duplicates), (3, 1, 0));
        store.import_rules(import(RuleType::Word, ListScope::Account(leni), "casino\n/lotto/i")).await.unwrap();

        let page = store.rules(RuleQuery { limit: 25, ..RuleQuery::default() }).await.unwrap();
        assert_eq!((page.total, page.rules.len()), (65, 25));
        assert_eq!((page.lists["block"], page.lists["allow"], page.lists["points"]), (62, 1, 2));
        assert_eq!(page.kinds["regex"], 1);

        let domain_only =
            RuleQuery { scope: ScopeFilter::One(ListScope::Domain(domain)), limit: 50, ..Default::default() };
        let page = store.rules(domain_only).await.unwrap();
        assert_eq!(page.total, 3);
        let allowed = page.rules.iter().find(|rule| rule.list == RuleList::Allow).unwrap();
        assert_eq!((allowed.value.as_str(), allowed.note.as_str()), ("news.example.org", "the newsletter"));
        assert_eq!(allowed.scope, RuleScope::Domain { id: domain, name: "example.de".into() });

        let search = RuleQuery { search: "SPAM1".into(), limit: 50, ..Default::default() };
        assert_eq!(store.rules(search).await.unwrap().total, 12, "spam1, spam10..19 and example.de's spam1");
        let words = RuleQuery { lists: vec![RuleList::Points], limit: 50, ..Default::default() };
        assert_eq!(store.rules(words).await.unwrap().total, 2);
        let own = RuleQuery { owner: Some(ListOwner::Account(leni)), limit: 50, ..Default::default() };
        let page = store.rules(own).await.unwrap();
        assert_eq!(page.total, 2, "a person sees their own rules only");
        assert!(matches!(&page.rules[0].scope, RuleScope::Account { name, .. } if name == "leni@example.de"));
        let everything = store.all_rules(RuleQuery::default()).await.unwrap();
        assert_eq!(everything.len(), 65);
    }

    #[tokio::test]
    async fn rules_are_changed_moved_and_removed_one_by_one_or_together() {
        let (store, _dir, domain, leni) = store().await;
        store
            .import_rules(import(RuleType::Sender, ListScope::Server, "a@evil.example\nb@evil.example"))
            .await
            .unwrap();
        store.import_rules(import(RuleType::Sender, ListScope::Domain(domain), "a@evil.example")).await.unwrap();
        store.import_rules(import(RuleType::Word, ListScope::Server, "casino")).await.unwrap();
        let all = store.all_rules(RuleQuery::default()).await.unwrap();
        let id = |value: &str, scope: RuleScope| {
            all.iter()
                .find(|rule| rule.value == value && rule.scope == scope)
                .map(|rule| (rule.rule_type, rule.id))
                .unwrap()
        };
        let a = id("a@evil.example", RuleScope::Server);
        let b = id("b@evil.example", RuleScope::Server);
        let casino = id("casino", RuleScope::Server);

        let change = RuleChange {
            value: Some("*@evil.example".into()),
            list: Some(RuleList::Allow),
            note: Some(" friends ".into()),
            expires_at: Some(Some(now() + 3600)),
            ..RuleChange::default()
        };
        let changed = store.change_rule(ListOwner::Admin, a.0, a.1, change).await.unwrap();
        assert_eq!(
            (changed.kind.as_str(), changed.list, changed.note.as_str()),
            ("pattern", RuleList::Allow, "friends")
        );
        assert!(changed.expires_at.is_some());
        let taken = RuleChange { value: Some("b@evil.example".into()), ..RuleChange::default() };
        assert!(store.change_rule(ListOwner::Admin, a.0, a.1, taken).await.is_err(), "already on a list here");
        let points = RuleChange { points: Some(Some(4.0)), ..RuleChange::default() };
        assert_eq!(store.change_rule(ListOwner::Admin, casino.0, casino.1, points).await.unwrap().points, Some(4.0));
        let stranger = store.change_rule(ListOwner::Account(leni), b.0, b.1, RuleChange::default()).await;
        assert!(matches!(stranger, Err(StoreError::NotFound(_))), "people only touch their own rules");

        // Moving both server rules to example.de: b fits, a's value is new there too.
        let report = store
            .bulk_rules(ListOwner::Admin, vec![a, b], BulkAction::SetScope(ListScope::Domain(domain)))
            .await
            .unwrap();
        assert_eq!(report.changed, 2);
        let blocked =
            store.bulk_rules(ListOwner::Admin, vec![a, casino], BulkAction::SetList(SenderList::Block)).await.unwrap();
        assert_eq!((blocked.changed, blocked.skipped_count), (1, 1), "words are neither allowed nor blocked");
        let gone = store.bulk_rules(ListOwner::Admin, vec![a, b, casino], BulkAction::Delete).await.unwrap();
        assert_eq!(gone.changed, 3);
        assert_eq!(store.all_rules(RuleQuery::default()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn hits_are_counted_and_rules_run_out() {
        let (store, _dir, _, _) = store().await;
        let mut temporary = import(RuleType::Sender, ListScope::Server, "old@evil.example");
        temporary.expires_at = Some(now() - 1);
        store.import_rules(temporary).await.unwrap();
        store.import_rules(import(RuleType::Sender, ListScope::Server, "new@evil.example")).await.unwrap();
        let mut word = import(RuleType::Word, ListScope::Server, "casino");
        word.expires_at = Some(now() - 1);
        store.import_rules(word).await.unwrap();
        assert!(store.compiled_words().await.unwrap().is_empty(), "a word that ran out no longer counts");
        let listed = store.sender_lists_for(vec![], vec![]).await.unwrap();
        assert_eq!(listed.len(), 1, "a sender that ran out no longer counts");

        store.note_rule_hits(vec![listed[0].id, listed[0].id], vec![]).await.unwrap();
        let unused = RuleQuery { state: Some(RuleState::Unused), ..Default::default() };
        assert_eq!(store.rules(unused.clone()).await.unwrap().total, 2, "the two that ran out");
        let hit = store.rule(RuleType::Sender, listed[0].id).await.unwrap().unwrap();
        assert_eq!(hit.hits, 2);
        assert!(hit.last_hit_at.is_some());

        let version = store.word_lists_version().await.unwrap();
        assert_eq!(store.remove_expired_rules().await.unwrap(), 2);
        assert!(store.word_lists_version().await.unwrap() > version, "the filter compiles its words again");
        assert_eq!(store.all_rules(RuleQuery::default()).await.unwrap().len(), 1);
    }
}
