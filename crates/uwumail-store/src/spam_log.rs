//! What the spam filter decided about each message, so an admin can look it up afterwards.
//!
//! Only mail from other servers gets an entry: outgoing mail is never scored, and neither is
//! anything from inside the network, so there would be nothing to explain. The entries that matter
//! most are the ones that leave no other trace — refused, greylisted, turned away by DMARC or by a
//! sender list — because those never reach a mailbox.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Result, Store, now};

/// A spam wave can send far more in a day than the age limit would ever clear, so there is a
/// ceiling on rows as well. The oldest go first.
pub const SPAM_LOG_MAX_ROWS: i64 = 200_000;

/// What became of a message. `Delivered` means it went to an inbox untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpamAction {
    Delivered,
    /// Filed as junk for at least one recipient.
    Junk,
    /// Asked to come back later; the message was never taken.
    Greylist,
    /// Refused in the SMTP dialogue for its score.
    Reject,
    /// Turned away because DMARC said to.
    Dmarc,
    /// Turned away by a sender list.
    Blocked,
}

impl SpamAction {
    pub fn as_str(self) -> &'static str {
        match self {
            SpamAction::Delivered => "delivered",
            SpamAction::Junk => "junk",
            SpamAction::Greylist => "greylist",
            SpamAction::Reject => "reject",
            SpamAction::Dmarc => "dmarc",
            SpamAction::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Option<SpamAction> {
        Some(match value {
            "delivered" => SpamAction::Delivered,
            "junk" => SpamAction::Junk,
            "greylist" => SpamAction::Greylist,
            "reject" => SpamAction::Reject,
            "dmarc" => SpamAction::Dmarc,
            "blocked" => SpamAction::Blocked,
            _ => return None,
        })
    }

    /// Whether the filter held this message back. Those keep their subject; mail that arrived
    /// normally only does when an admin asks for it.
    pub fn held_back(self) -> bool {
        self != SpamAction::Delivered
    }
}

/// One recipient of a message and what happened for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpamLogRecipient {
    pub address: String,
    /// The same words as [`SpamAction`], for this one recipient.
    pub action: String,
    /// Where it landed, when it landed: "inbox" or "junk".
    pub mailbox: Option<String>,
}

/// A rule that fired, as the filter counted it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpamLogHit {
    pub rule: String,
    pub points: f32,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSpamLogEntry {
    pub smtp_id: String,
    pub message_id: Option<String>,
    pub action: String,
    pub envelope_from: String,
    pub header_from: String,
    /// Left out for mail that arrived normally, unless an admin asked for those too.
    pub subject: Option<String>,
    pub client_ip: String,
    pub helo: String,
    pub reverse_name: Option<String>,
    pub size: i64,
    pub score: Option<f32>,
    pub hits: Vec<SpamLogHit>,
    pub auth: Option<String>,
    pub blob_hash: Option<String>,
    pub recipients: Vec<SpamLogRecipient>,
}

/// An entry as the portal reads it, with what the person said about it afterwards.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpamLogEntry {
    pub id: i64,
    pub at: i64,
    pub smtp_id: String,
    pub message_id: Option<String>,
    pub action: String,
    pub envelope_from: String,
    pub header_from: String,
    pub subject: Option<String>,
    pub client_ip: String,
    pub helo: String,
    pub reverse_name: Option<String>,
    pub size: i64,
    pub score: Option<f32>,
    pub hits: Vec<SpamLogHit>,
    pub auth: Option<String>,
    pub recipients: Vec<SpamLogRecipient>,
    /// What someone said later with Spam / Not spam: `Some(true)` is spam, `Some(false)` is not,
    /// `None` is nobody said anything.
    pub corrected_to_junk: Option<bool>,
}

/// What to show: everything, or one kind of decision, or one sender.
#[derive(Debug, Clone, Default)]
pub struct SpamLogFilter {
    pub action: Option<String>,
    /// Matches the envelope sender, the From header or the sending address.
    pub search: Option<String>,
    pub min_score: Option<f32>,
    pub before: Option<i64>,
    pub limit: usize,
}

fn json_of<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".into())
}

fn from_json<T: for<'a> Deserialize<'a> + Default>(raw: &str) -> T {
    serde_json::from_str(raw).unwrap_or_default()
}

impl Store {
    /// Writes one decision. Failures are the caller's to log and never stop a delivery: a history
    /// that cannot be written is worth less than the mail it would describe.
    pub async fn add_spam_log(&self, entry: NewSpamLogEntry) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO spam_log (at, smtp_id, message_id, action, envelope_from, header_from, subject,
                                       client_ip, helo, reverse_name, size, score, hits, auth, blob_hash, recipients)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    now(),
                    entry.smtp_id,
                    entry.message_id,
                    entry.action,
                    entry.envelope_from,
                    entry.header_from,
                    entry.subject,
                    entry.client_ip,
                    entry.helo,
                    entry.reverse_name,
                    entry.size.max(0),
                    entry.score,
                    json_of(&entry.hits),
                    entry.auth,
                    entry.blob_hash,
                    json_of(&entry.recipients)
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// The history, newest first. `before` continues after the smallest id of the page before.
    pub async fn spam_log(&self, filter: SpamLogFilter) -> Result<Vec<SpamLogEntry>> {
        let limit = filter.limit.clamp(1, 200) as i64;
        self.read(move |conn| {
            // The left join is what turns "it went to junk" into "it went to junk and the person
            // said it was not", without a column of our own that would have to be kept in step.
            let mut sql = String::from(
                "SELECT l.id, l.at, l.smtp_id, l.message_id, l.action, l.envelope_from, l.header_from, l.subject,
                        l.client_ip, l.helo, l.reverse_name, l.size, l.score, l.hits, l.auth, l.recipients, v.junk
                 FROM spam_log l
                 LEFT JOIN spam_verdicts v ON v.blob_hash = l.blob_hash
                 WHERE l.id < ?1",
            );
            let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(filter.before.unwrap_or(i64::MAX))];
            if let Some(action) = &filter.action {
                values.push(Box::new(action.clone()));
                sql.push_str(&format!(" AND l.action = ?{}", values.len()));
            }
            if let Some(score) = filter.min_score {
                values.push(Box::new(f64::from(score)));
                sql.push_str(&format!(" AND l.score >= ?{}", values.len()));
            }
            if let Some(search) = filter.search.as_ref().map(|text| format!("%{}%", text.trim().to_lowercase()))
                && search.len() > 2
            {
                values.push(Box::new(search));
                let at = values.len();
                sql.push_str(&format!(
                    " AND (lower(l.envelope_from) LIKE ?{at} OR lower(l.header_from) LIKE ?{at} OR l.client_ip LIKE ?{at})"
                ));
            }
            values.push(Box::new(limit));
            sql.push_str(&format!(" ORDER BY l.id DESC LIMIT ?{}", values.len()));

            let mut statement = conn.prepare(&sql)?;
            let borrowed: Vec<&dyn rusqlite::ToSql> = values.iter().map(|value| value.as_ref()).collect();
            let found = statement
                .query_map(borrowed.as_slice(), |row| {
                    Ok(SpamLogEntry {
                        id: row.get(0)?,
                        at: row.get(1)?,
                        smtp_id: row.get(2)?,
                        message_id: row.get(3)?,
                        action: row.get(4)?,
                        envelope_from: row.get(5)?,
                        header_from: row.get(6)?,
                        subject: row.get(7)?,
                        client_ip: row.get(8)?,
                        helo: row.get(9)?,
                        reverse_name: row.get(10)?,
                        size: row.get(11)?,
                        score: row.get(12)?,
                        hits: from_json(&row.get::<_, String>(13)?),
                        auth: row.get(14)?,
                        recipients: from_json(&row.get::<_, String>(15)?),
                        corrected_to_junk: row.get(16)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(found)
        })
        .await
    }

    /// One entry by its id.
    pub async fn spam_log_entry(&self, id: i64) -> Result<Option<SpamLogEntry>> {
        let entries = self.spam_log(SpamLogFilter { before: Some(id + 1), limit: 1, ..Default::default() }).await?;
        Ok(entries.into_iter().find(|entry| entry.id == id))
    }

    /// How many entries there are, and the oldest one's time, for the page to say what it covers.
    pub async fn spam_log_extent(&self) -> Result<(i64, Option<i64>)> {
        self.read(move |conn| {
            let count: i64 = conn.query_row("SELECT count(*) FROM spam_log", [], |row| row.get(0))?;
            let oldest: Option<i64> =
                conn.query_row("SELECT min(at) FROM spam_log", [], |row| row.get(0)).optional()?.flatten();
            Ok((count, oldest))
        })
        .await
    }

    /// Removes entries older than `older_than_secs`, and then the oldest above the row ceiling.
    /// Returns how many went.
    pub async fn prune_spam_log(&self, older_than_secs: i64) -> Result<usize> {
        self.write(move |tx| {
            let mut removed = tx.execute("DELETE FROM spam_log WHERE at < ?1", params![now() - older_than_secs])?;
            removed += tx.execute(
                "DELETE FROM spam_log WHERE id <= (SELECT id FROM spam_log ORDER BY id DESC LIMIT 1 OFFSET ?1)",
                params![SPAM_LOG_MAX_ROWS],
            )?;
            Ok(removed)
        })
        .await
    }

    /// Everything the history holds, for switching it off and clearing it out.
    pub async fn clear_spam_log(&self) -> Result<usize> {
        self.write(move |tx| Ok(tx.execute("DELETE FROM spam_log", [])?)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;

    fn entry(smtp_id: &str, action: SpamAction, from: &str, score: f32) -> NewSpamLogEntry {
        NewSpamLogEntry {
            smtp_id: smtp_id.into(),
            action: action.as_str().into(),
            envelope_from: from.into(),
            header_from: from.into(),
            subject: action.held_back().then(|| "Gewinnen Sie ein Auto".into()),
            client_ip: "198.51.100.7".into(),
            helo: "mail.spammer.example".into(),
            size: 4096,
            score: Some(score),
            hits: vec![SpamLogHit { rule: "BAYES_SPAM".into(), points: 3.5, detail: Some("92 %".into()) }],
            recipients: vec![SpamLogRecipient {
                address: "nyu@example.de".into(),
                action: action.as_str().into(),
                mailbox: action.held_back().then(|| "junk".into()),
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_history_keeps_what_the_filter_decided_and_can_be_filtered() {
        let (store, _dir) = store().await;
        store.add_spam_log(entry("a1", SpamAction::Delivered, "freund@example.org", 0.5)).await.unwrap();
        store.add_spam_log(entry("a2", SpamAction::Junk, "werbung@shop.example", 6.0)).await.unwrap();
        store.add_spam_log(entry("a3", SpamAction::Reject, "boese@spammer.example", 14.0)).await.unwrap();

        let all = store.spam_log(SpamLogFilter { limit: 50, ..Default::default() }).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].smtp_id, "a3", "newest first");
        assert_eq!(all[0].hits[0].rule, "BAYES_SPAM");
        assert_eq!(all[0].recipients[0].address, "nyu@example.de");
        assert_eq!(all[2].subject, None, "mail that arrived normally keeps no subject");
        assert!(all[0].subject.is_some(), "what was held back does");

        let junk = store
            .spam_log(SpamLogFilter { action: Some("junk".into()), limit: 50, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(junk.iter().map(|e| e.smtp_id.as_str()).collect::<Vec<_>>(), vec!["a2"]);

        let loud =
            store.spam_log(SpamLogFilter { min_score: Some(5.0), limit: 50, ..Default::default() }).await.unwrap();
        assert_eq!(loud.len(), 2);

        let found = store
            .spam_log(SpamLogFilter { search: Some("SPAMMER.example".into()), limit: 50, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(found.iter().map(|e| e.smtp_id.as_str()).collect::<Vec<_>>(), vec!["a3"]);

        // Paging by the smallest id of the page before.
        let page = store.spam_log(SpamLogFilter { limit: 2, ..Default::default() }).await.unwrap();
        let next =
            store.spam_log(SpamLogFilter { before: Some(page[1].id), limit: 2, ..Default::default() }).await.unwrap();
        assert_eq!(next.iter().map(|e| e.smtp_id.as_str()).collect::<Vec<_>>(), vec!["a1"]);

        let (count, oldest) = store.spam_log_extent().await.unwrap();
        assert_eq!(count, 3);
        assert!(oldest.is_some());

        let one = store.spam_log_entry(all[1].id).await.unwrap().unwrap();
        assert_eq!(one.smtp_id, "a2");

        assert_eq!(store.prune_spam_log(3600).await.unwrap(), 0, "nothing is old yet");
        assert_eq!(store.prune_spam_log(-1).await.unwrap(), 3, "a cut-off in the future takes everything");
        assert_eq!(store.spam_log(SpamLogFilter { limit: 50, ..Default::default() }).await.unwrap().len(), 0);
    }
}
