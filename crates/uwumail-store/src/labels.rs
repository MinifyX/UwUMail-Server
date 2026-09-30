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

/// A person changed an email's keywords by hand: for every label keyword put on or taken off, the
/// sender is counted or forgotten for the label, and the change is queued for the classifier.
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
        "SELECT id, keyword FROM assist_labels WHERE account_id = ?1 AND keyword IN (SELECT value FROM json_each(?2))",
    )?;
    let labels: Vec<(i64, String)> = stmt
        .query_map(params![account_id, keywords], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    if labels.is_empty() {
        return Ok(false);
    }
    let from = from_address(tx, email_id)?;
    let now = now();
    for (label_id, keyword) in labels {
        let positive = changed.iter().any(|(changed, on)| **changed == keyword && *on);
        if !from.is_empty() {
            if positive {
                tx.execute(
                    "INSERT INTO label_senders (account_id, label_id, address, count) VALUES (?1, ?2, ?3, 1)
                     ON CONFLICT (label_id, address) DO UPDATE SET count = count + 1",
                    params![account_id, label_id, from],
                )?;
            } else {
                tx.execute("DELETE FROM label_senders WHERE label_id = ?1 AND address = ?2", params![label_id, from])?;
            }
        }
        tx.execute(
            "INSERT INTO label_training (account_id, email_id, label_id, positive, queued_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account_id, email_id, label_id, positive, now],
        )?;
    }
    Ok(true)
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
            tx.execute("DELETE FROM label_tokens WHERE account_id = ?1 AND examples <= 0", [account_id])?;
        }
        Some(label_id) => {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO label_positive_tokens (label_id, token, examples) VALUES (?1, ?2, ?3)
                 ON CONFLICT (label_id, token) DO UPDATE SET examples = examples + ?3",
            )?;
            for token in tokens {
                stmt.execute(params![label_id, token, delta])?;
            }
            tx.execute("DELETE FROM label_positive_tokens WHERE label_id = ?1 AND examples <= 0", [label_id])?;
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

    /// Hand-labelings waiting to be learned, oldest first.
    pub async fn label_training(&self) -> Result<Vec<LabelTraining>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_id, email_id, label_id, positive FROM label_training ORDER BY id LIMIT ?1",
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

    pub async fn finish_label_training(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM label_training WHERE id = ?1", [id])?;
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
    pub async fn label_counts(&self, account_id: i64) -> Result<HashMap<i64, LabelCounts>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "WITH shown AS (
                     SELECT k.keyword, e.id,
                            NOT EXISTS (SELECT 1 FROM email_keywords s WHERE s.email_id = e.id AND s.keyword = '$seen')
                                AS unread
                     FROM assist_labels l
                     JOIN email_keywords k ON k.keyword = l.keyword
                     JOIN emails e ON e.id = k.email_id AND e.account_id = l.account_id
                     WHERE l.account_id = ?1
                       AND EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                                   WHERE em.email_id = e.id AND (m.role IS NULL OR m.role NOT IN ('junk', 'trash'))))
                 SELECT l.id,
                        (SELECT COUNT(*) FROM shown WHERE shown.keyword = l.keyword),
                        (SELECT COUNT(*) FROM shown WHERE shown.keyword = l.keyword AND shown.unread),
                        (SELECT COUNT(*) FROM label_example_labels x WHERE x.label_id = l.id)
                 FROM assist_labels l WHERE l.account_id = ?1",
            )?;
            let rows = stmt.query_map([account_id], |row| {
                Ok((row.get(0)?, LabelCounts { total: row.get(1)?, unread: row.get(2)?, examples: row.get(3)? }))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
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
