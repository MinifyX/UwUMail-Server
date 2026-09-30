//! What labels without a model learn and need (docs/labels.md): senders the person labeled by hand,
//! the classifier's examples and token counts, the queue of hand-labelings still to learn, and the
//! counts of each label's mail. The deciding itself is `uwumail_labels`.

use std::collections::{BTreeSet, HashMap};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use uwumail_labels::{BACKGROUND_CANDIDATES, BACKGROUND_DAYS, MAX_EXAMPLES, Model};

use crate::assist::{LABEL_COLUMNS, LabelCounts, label_row};
use crate::db::{get_setting, next_modseq, record_change};
use crate::{AssistLabel, Result, Store, now};

/// Hand-labelings learned at once.
const TRAINING_BATCH: usize = 50;

/// A hand-labeling waiting to be learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelTraining {
    pub id: i64,
    pub account_id: i64,
    pub email_id: i64,
    pub label_id: i64,
    /// Put on (`true`) or taken off.
    pub positive: bool,
}

/// What the deciding of one mail needs of an account (see [`Store::label_setup`]).
#[derive(Debug, Clone, Default)]
pub struct LabelSetup {
    pub labels: Vec<AssistLabel>,
}

/// The first From address of an email, lower case.
fn from_address(conn: &Connection, email_id: i64) -> Result<String> {
    let json: Option<String> =
        conn.query_row("SELECT from_addr FROM emails WHERE id = ?1", [email_id], |row| row.get(0)).optional()?;
    let list: Vec<serde_json::Value> = json.and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default();
    Ok(list
        .first()
        .and_then(|address| address.get("email"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase())
}

/// Hand-labelings of one account waiting to be learned, at most: more are not queued until the
/// worker caught up (security audit 0.21.0 LABELS-M1).
pub const MAX_TRAINING_PER_ACCOUNT: i64 = 500;
/// Senders learned per label, at most; beyond, the least counted gives way.
pub const MAX_SENDERS_PER_LABEL: i64 = 5_000;
/// Longer From addresses are not learned (RFC 5321 allows 256 characters for a path).
const MAX_SENDER_CHARS: usize = 320;

/// A person changed an email's keywords by hand: for every label keyword put on or taken off, the
/// sender is counted or forgotten for the label (when it learns senders), and the change is queued
/// for the classifier (when it has one). Nothing is learned while labels without a model are off.
/// Answers whether anything was queued. Runs inside the transaction of the change.
pub(crate) fn learn_by_hand(
    tx: &Transaction<'_>,
    account_id: i64,
    email_id: i64,
    old: &BTreeSet<String>,
    new: &BTreeSet<String>,
) -> Result<bool> {
    let changed: Vec<(&String, bool)> = new
        .difference(old)
        .map(|keyword| (keyword, true))
        .chain(old.difference(new).map(|keyword| (keyword, false)))
        .filter(|(keyword, _)| !keyword.starts_with('$'))
        .collect();
    if changed.is_empty() {
        return Ok(false);
    }
    let keywords = serde_json::to_string(&changed.iter().map(|(keyword, _)| keyword).collect::<Vec<_>>())
        .unwrap_or_else(|_| "[]".into());
    let mut stmt = tx.prepare_cached(
        "SELECT id, keyword, learn_senders, classifier FROM assist_labels
         WHERE account_id = ?1 AND keyword IN (SELECT value FROM json_each(?2))
           AND NOT EXISTS (SELECT 1 FROM assist_prefs p WHERE p.account_id = ?1 AND p.non_ai_labels = 0)",
    )?;
    let labels: Vec<(i64, String, bool, bool)> = stmt
        .query_map(params![account_id, keywords], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    if labels.is_empty() {
        return Ok(false);
    }
    let from = from_address(tx, email_id)?;
    let from = if from.chars().count() > MAX_SENDER_CHARS { String::new() } else { from };
    let now = now();
    let mut queued = false;
    for (label_id, keyword, learn_senders, classifier) in labels {
        let positive = changed.iter().any(|(changed, on)| **changed == keyword && *on);
        if learn_senders && !from.is_empty() {
            if positive {
                learn_sender(tx, account_id, label_id, &from)?;
            } else {
                tx.execute("DELETE FROM label_senders WHERE label_id = ?1 AND address = ?2", params![label_id, from])?;
            }
        }
        if !classifier {
            continue;
        }
        // Once per email and label: the last change wins. A full queue takes new ones again once
        // the worker caught up.
        let updated = tx.execute(
            "UPDATE label_training SET positive = ?4, queued_at = ?5
             WHERE account_id = ?1 AND email_id = ?2 AND label_id = ?3",
            params![account_id, email_id, label_id, positive, now],
        )?;
        if updated == 0 {
            let waiting: i64 =
                tx.query_row("SELECT COUNT(*) FROM label_training WHERE account_id = ?1", [account_id], |row| {
                    row.get(0)
                })?;
            if waiting >= MAX_TRAINING_PER_ACCOUNT {
                continue;
            }
            tx.execute(
                "INSERT INTO label_training (account_id, email_id, label_id, positive, queued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![account_id, email_id, label_id, positive, now],
            )?;
        }
        queued = true;
    }
    Ok(queued)
}

/// Whether labeling by hand by someone an account is shared with teaches its labels: only in a
/// shared mailbox, whose labels belong to all its members. Elsewhere the owner's labels learn from
/// the owner alone.
pub(crate) fn teaches_in_share(conn: &Connection, account_id: i64) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row("SELECT shared_mailbox FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
        .optional()?
        .unwrap_or(false))
}

/// Counts one more hand-labeling of `from` for a label; a new sender beyond
/// [`MAX_SENDERS_PER_LABEL`] takes the place of the least counted one.
fn learn_sender(tx: &Transaction<'_>, account_id: i64, label_id: i64, from: &str) -> Result<()> {
    let known = tx.execute(
        "UPDATE label_senders SET count = count + 1 WHERE label_id = ?1 AND address = ?2",
        params![label_id, from],
    )?;
    if known > 0 {
        return Ok(());
    }
    let senders: i64 =
        tx.query_row("SELECT COUNT(*) FROM label_senders WHERE label_id = ?1", [label_id], |row| row.get(0))?;
    if senders >= MAX_SENDERS_PER_LABEL {
        tx.execute(
            "DELETE FROM label_senders WHERE label_id = ?1 AND address IN (
                 SELECT address FROM label_senders WHERE label_id = ?1 ORDER BY count, address LIMIT ?2)",
            params![label_id, senders - MAX_SENDERS_PER_LABEL + 1],
        )?;
    }
    tx.execute(
        "INSERT INTO label_senders (account_id, label_id, address, count) VALUES (?1, ?2, ?3, 1)",
        params![account_id, label_id, from],
    )?;
    Ok(())
}

/// Adds `delta` to the counts of `tokens` for the account (`label` `None`) or a label.
fn count_tokens(tx: &Transaction<'_>, account_id: i64, label: Option<i64>, tokens: &[i64], delta: i64) -> Result<()> {
    match label {
        None => {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO label_tokens (account_id, token, examples) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account_id, token) DO UPDATE SET examples = examples + ?3",
            )?;
            for token in tokens {
                stmt.execute(params![account_id, token, delta])?;
            }
            // Only the tokens just counted down can have reached zero: no walk over all of them.
            if delta < 0 {
                let mut stmt = tx.prepare_cached(
                    "DELETE FROM label_tokens WHERE account_id = ?1 AND token = ?2 AND examples <= 0",
                )?;
                for token in tokens {
                    stmt.execute(params![account_id, token])?;
                }
            }
        }
        Some(label_id) => {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO label_positive_tokens (label_id, token, examples) VALUES (?1, ?2, ?3)
                 ON CONFLICT (label_id, token) DO UPDATE SET examples = examples + ?3",
            )?;
            for token in tokens {
                stmt.execute(params![label_id, token, delta])?;
            }
            if delta < 0 {
                let mut stmt = tx.prepare_cached(
                    "DELETE FROM label_positive_tokens WHERE label_id = ?1 AND token = ?2 AND examples <= 0",
                )?;
                for token in tokens {
                    stmt.execute(params![label_id, token])?;
                }
            }
        }
    }
    Ok(())
}

fn example_tokens(text: &str) -> Vec<i64> {
    serde_json::from_str(text).unwrap_or_default()
}

/// The example of an email, making it from `tokens` when there is none yet.
fn ensure_example(tx: &Transaction<'_>, account_id: i64, email_id: i64, tokens: &[i64]) -> Result<(i64, Vec<i64>)> {
    let found: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, tokens FROM label_examples WHERE account_id = ?1 AND email_id = ?2",
            params![account_id, email_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, text)) = found {
        return Ok((id, example_tokens(&text)));
    }
    let text = serde_json::to_string(tokens).unwrap_or_else(|_| "[]".into());
    tx.execute(
        "INSERT INTO label_examples (account_id, email_id, tokens, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![account_id, email_id, text, now()],
    )?;
    let id = tx.last_insert_rowid();
    count_tokens(tx, account_id, None, tokens, 1)?;
    forget_oldest(tx, account_id)?;
    Ok((id, tokens.to_vec()))
}

/// Forgets the oldest examples beyond [`MAX_EXAMPLES`], and their counts.
fn forget_oldest(tx: &Transaction<'_>, account_id: i64) -> Result<()> {
    let mut stmt =
        tx.prepare("SELECT id, tokens FROM label_examples WHERE account_id = ?1 ORDER BY id DESC LIMIT -1 OFFSET ?2")?;
    let old: Vec<(i64, String)> = stmt
        .query_map(params![account_id, MAX_EXAMPLES as i64], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (id, text) in old {
        let tokens = example_tokens(&text);
        count_tokens(tx, account_id, None, &tokens, -1)?;
        let mut stmt = tx.prepare("SELECT label_id FROM label_example_labels WHERE example_id = ?1")?;
        let labels: Vec<i64> = stmt.query_map([id], |row| row.get(0))?.collect::<Result<_, _>>()?;
        drop(stmt);
        for label in labels {
            count_tokens(tx, account_id, Some(label), &tokens, -1)?;
        }
        tx.execute("DELETE FROM label_examples WHERE id = ?1", [id])?;
    }
    Ok(())
}

/// Accounts whose label counts are kept at once; more empty the cache.
const MAX_CACHED_COUNTS: usize = 10_000;

/// Label counts cached per account, with the account's modseq they were read at.
pub(crate) type LabelCountCache = std::sync::Mutex<HashMap<i64, (i64, HashMap<i64, LabelCounts>)>>;

/// The counts of an account's labels (`?1`), see [`count_labels`]. `+k.keyword` keeps the keyword
/// index (across all accounts) out: the account's folders lead, and each of their mails is looked
/// up by its own keywords.
const COUNT_SQL: &str =
        "WITH labels AS (SELECT id, keyword FROM assist_labels WHERE account_id = ?1),
         shown AS (
             SELECT DISTINCT k.keyword, em.email_id
             FROM mailboxes m
             JOIN email_mailboxes em ON em.mailbox_id = m.id
             JOIN email_keywords k ON k.email_id = em.email_id AND +k.keyword IN (SELECT keyword FROM labels)
             WHERE m.account_id = ?1 AND (m.role IS NULL OR m.role NOT IN ('junk', 'trash'))),
         counted AS (
             SELECT s.keyword, COUNT(*) AS total,
                    SUM(NOT EXISTS (SELECT 1 FROM email_keywords x WHERE x.email_id = s.email_id AND x.keyword = '$seen'))
                        AS unread
             FROM shown s GROUP BY s.keyword)
         SELECT l.id, COALESCE(c.total, 0), COALESCE(c.unread, 0),
                (SELECT COUNT(*) FROM label_example_labels x WHERE x.label_id = l.id)
         FROM labels l LEFT JOIN counted c ON c.keyword = l.keyword";

fn count_labels(conn: &Connection, account_id: i64) -> Result<HashMap<i64, LabelCounts>> {
    let mut stmt = conn.prepare_cached(COUNT_SQL)?;
    let rows = stmt.query_map([account_id], |row| {
        Ok((row.get(0)?, LabelCounts { total: row.get(1)?, unread: row.get(2)?, examples: row.get(3)? }))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

impl Store {
    /// Whether labels were learned from by hand in a change just written: wakes the worker.
    pub(crate) fn labels_learned(&self) {
        self.inner.assist_wakeup.notify_one();
    }

    /// The account's labels when labels without a model are on for it and it has any; empty
    /// otherwise. Cheap: delivery asks this for every message.
    pub async fn label_setup(&self, account_id: i64) -> Result<LabelSetup> {
        self.read(move |conn| {
            let off: bool = conn
                .query_row("SELECT non_ai_labels = 0 FROM assist_prefs WHERE account_id = ?1", [account_id], |row| {
                    row.get(0)
                })
                .optional()?
                .unwrap_or(false);
            if off {
                return Ok(LabelSetup::default());
            }
            let mut stmt =
                conn.prepare(&format!("SELECT {LABEL_COLUMNS} FROM assist_labels WHERE account_id = ?1 ORDER BY id"))?;
            let labels = stmt.query_map([account_id], label_row)?.collect::<Result<_, _>>()?;
            Ok(LabelSetup { labels })
        })
        .await
    }

    /// What was learned that matters for a mail from `from` with `tokens`: per label, how often the
    /// person labeled that sender by hand, and the classifier of every label in `classifiers` that
    /// has enough examples, with the counts of these tokens.
    pub async fn label_knowledge(
        &self,
        account_id: i64,
        from: String,
        tokens: Vec<i64>,
        classifiers: Vec<i64>,
    ) -> Result<uwumail_labels::Knowledge> {
        self.read(move |conn| {
            let mut knowledge = uwumail_labels::Knowledge::default();
            if !from.is_empty() {
                let mut stmt = conn.prepare_cached(
                    "SELECT label_id, count FROM label_senders WHERE account_id = ?1 AND address = ?2",
                )?;
                for row in stmt.query_map(params![account_id, from], |row| Ok((row.get(0)?, row.get(1)?)))? {
                    let (label, count) = row?;
                    knowledge.senders.insert(label, count);
                }
            }
            if classifiers.is_empty() || tokens.is_empty() {
                return Ok(knowledge);
            }
            let total: i64 =
                conn.query_row("SELECT COUNT(*) FROM label_examples WHERE account_id = ?1", [account_id], |row| {
                    row.get(0)
                })?;
            let tokens_json = serde_json::to_string(&tokens).unwrap_or_else(|_| "[]".into());
            let mut all: Option<HashMap<i64, i64>> = None;
            for label in classifiers {
                let positives: i64 =
                    conn.query_row("SELECT COUNT(*) FROM label_example_labels WHERE label_id = ?1", [label], |row| {
                        row.get(0)
                    })?;
                let model = Model { positives, negatives: total - positives, counts: HashMap::new() };
                if !model.ready() {
                    continue;
                }
                let all = match &all {
                    Some(all) => all,
                    None => {
                        let mut stmt = conn.prepare(
                            "SELECT token, examples FROM label_tokens
                             WHERE account_id = ?1 AND token IN (SELECT value FROM json_each(?2))",
                        )?;
                        let rows =
                            stmt.query_map(params![account_id, tokens_json], |row| Ok((row.get(0)?, row.get(1)?)))?;
                        all.insert(rows.collect::<Result<_, _>>()?)
                    }
                };
                let mut stmt = conn.prepare_cached(
                    "SELECT token, examples FROM label_positive_tokens
                     WHERE label_id = ?1 AND token IN (SELECT value FROM json_each(?2))",
                )?;
                let with: HashMap<i64, i64> = stmt
                    .query_map(params![label, tokens_json], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                let counts = all
                    .iter()
                    .map(|(token, examples)| {
                        let p = with.get(token).copied().unwrap_or(0);
                        (*token, (p, (examples - p).max(0)))
                    })
                    .collect();
                knowledge.models.insert(label, Model { counts, ..model });
            }
            Ok(knowledge)
        })
        .await
    }

    /// Hand-labelings waiting to be learned, oldest first, taking turns between accounts: one
    /// account's many changes hold up nobody else's (security audit 0.21.0 LABELS-M1).
    pub async fn label_training(&self) -> Result<Vec<LabelTraining>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_id, email_id, label_id, positive FROM (
                     SELECT *, ROW_NUMBER() OVER (PARTITION BY account_id ORDER BY id) AS turn FROM label_training)
                 ORDER BY turn, id LIMIT ?1",
            )?;
            let rows = stmt.query_map([TRAINING_BATCH as i64], |row| {
                Ok(LabelTraining {
                    id: row.get(0)?,
                    account_id: row.get(1)?,
                    email_id: row.get(2)?,
                    label_id: row.get(3)?,
                    positive: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Done with a hand-labeling learned as `positive`. Changed again in the meantime, it stays to
    /// be learned the other way.
    pub async fn finish_label_training(&self, id: i64, positive: bool) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM label_training WHERE id = ?1 AND positive = ?2", params![id, positive])?;
            Ok(())
        })
        .await
    }

    /// The tokens of the example an email is, if it is one.
    pub async fn label_example_tokens(&self, account_id: i64, email_id: i64) -> Result<Option<Vec<i64>>> {
        self.read(move |conn| {
            let text: Option<String> = conn
                .query_row(
                    "SELECT tokens FROM label_examples WHERE account_id = ?1 AND email_id = ?2",
                    params![account_id, email_id],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(text.map(|text| example_tokens(&text)))
        })
        .await
    }

    /// Learns an email as an example with the label (`positive`) or without it. `tokens` are used
    /// when the email is no example yet. Answers whether it became an example with the label just
    /// now. A label that is gone learns nothing.
    pub async fn learn_label_example(
        &self,
        account_id: i64,
        email_id: i64,
        label_id: i64,
        positive: bool,
        tokens: Vec<i64>,
    ) -> Result<bool> {
        let (newly, modseq) = self
            .write(move |tx| {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM assist_labels WHERE id = ?1 AND account_id = ?2)",
                    params![label_id, account_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Ok((false, None));
                }
                let (example, tokens) = ensure_example(tx, account_id, email_id, &tokens)?;
                let has: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM label_example_labels WHERE label_id = ?1 AND example_id = ?2)",
                    params![label_id, example],
                    |row| row.get(0),
                )?;
                let newly = positive && !has;
                if newly {
                    tx.execute(
                        "INSERT INTO label_example_labels (label_id, example_id) VALUES (?1, ?2)",
                        params![label_id, example],
                    )?;
                    count_tokens(tx, account_id, Some(label_id), &tokens, 1)?;
                } else if !positive && has {
                    tx.execute(
                        "DELETE FROM label_example_labels WHERE label_id = ?1 AND example_id = ?2",
                        params![label_id, example],
                    )?;
                    count_tokens(tx, account_id, Some(label_id), &tokens, -1)?;
                }
                let modseq = next_modseq(tx, account_id)?;
                record_change(tx, account_id, modseq, "AssistLabel", label_id, "updated")?;
                Ok((newly, Some(modseq)))
            })
            .await?;
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
        Ok(newly)
    }

    /// An ordinary mail to learn as an example without any label, beside one given a label by hand
    /// (`labeled`): of the newest inbox mail of the last days without a label that is no example
    /// yet, the one at `labeled` mod their number.
    pub async fn label_background_candidate(&self, account_id: i64, labeled: i64) -> Result<Option<i64>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT e.id FROM emails e
                 WHERE e.account_id = ?1 AND e.received_at >= ?2 AND e.id != ?3
                   AND EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                               WHERE em.email_id = e.id AND m.role = 'inbox')
                   AND NOT EXISTS (SELECT 1 FROM email_keywords k JOIN assist_labels l ON l.keyword = k.keyword
                                   WHERE k.email_id = e.id AND l.account_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM label_examples x WHERE x.account_id = ?1 AND x.email_id = e.id)
                 ORDER BY e.received_at DESC, e.id DESC LIMIT ?4",
            )?;
            let since = now() - BACKGROUND_DAYS * 86_400;
            let ids: Vec<i64> = stmt
                .query_map(params![account_id, since, labeled, BACKGROUND_CANDIDATES as i64], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            if ids.is_empty() {
                return Ok(None);
            }
            Ok(Some(ids[(labeled.rem_euclid(ids.len() as i64)) as usize]))
        })
        .await
    }

    /// Learns an email as an example without any label, unless it is one already.
    pub async fn add_label_background(&self, account_id: i64, email_id: i64, tokens: Vec<i64>) -> Result<()> {
        self.write(move |tx| ensure_example(tx, account_id, email_id, &tokens).map(|_| ())).await
    }

    /// Per label: its mail (not only in Junk or the Trash), how much of it is unread, and the
    /// classifier's examples with it.
    ///
    /// Read from the account's own folders, never through the keyword across all accounts: a label
    /// named like a keyword others use a lot (or someone else's mail with it) costs this account
    /// nothing (security audit 0.21.0 LABELS-M2). Kept until the account's next change, since every
    /// change pushes the labels' state and each app asks again.
    pub async fn label_counts(&self, account_id: i64) -> Result<HashMap<i64, LabelCounts>> {
        let cache = self.inner.label_counts.clone();
        self.read(move |conn| {
            let modseq: i64 = conn
                .query_row("SELECT modseq FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .unwrap_or(0);
            if let Some((seen, counts)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&account_id)
                && *seen == modseq
            {
                return Ok(counts.clone());
            }
            let counts = count_labels(conn, account_id)?;
            let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= MAX_CACHED_COUNTS {
                cache.clear();
            }
            cache.insert(account_id, (modseq, counts.clone()));
            Ok(counts)
        })
        .await
    }

    /// The JMAP state of `AssistLabel`: moves with the labels, the person's assist objects, the
    /// admin's changes and every change to the account's mail (the counts).
    pub async fn assist_label_state(&self, account_id: i64) -> Result<String> {
        self.read(move |conn| {
            let version: i64 = get_setting(conn, "assist.version")?.and_then(|v| v.parse().ok()).unwrap_or(0);
            let prefs: i64 = conn
                .query_row("SELECT modseq FROM assist_prefs WHERE account_id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .unwrap_or(0);
            let modseq: i64 = conn
                .query_row("SELECT modseq FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .unwrap_or(0);
            Ok(format!("{version}-{prefs}-{modseq}"))
        })
        .await
    }

    /// Whether the account has any label, for push.
    pub async fn has_assist_labels(&self, account_id: i64) -> Result<bool> {
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM assist_labels WHERE account_id = ?1)",
                [account_id],
                |row| row.get(0),
            )?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;

    /// The counts start from the account's own folders, never from the keyword index that holds
    /// every account's mail (security audit 0.21.0 LABELS-M2).
    #[tokio::test]
    async fn label_counts_read_only_the_accounts_own_mail() {
        let (store, _dir) = store().await;
        let plan: Vec<String> = store
            .read(|conn| {
                let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {COUNT_SQL}"))?;
                let rows = stmt.query_map([1], |row| row.get::<_, String>(3))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
            .unwrap();
        assert!(!plan.iter().any(|step| step.contains("email_keywords_keyword")), "{plan:#?}");
        assert!(plan.iter().any(|step| step.contains("SCAN m") || step.contains("SEARCH m")), "{plan:#?}");
    }
}
