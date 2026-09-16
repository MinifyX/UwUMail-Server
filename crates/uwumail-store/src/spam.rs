//! Greylisting and what a sender delivered so far.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Result, Store, now};

/// Greylisted senders that never came back are forgotten after two days.
pub const GREYLIST_WAITING_SECS: i64 = 2 * 24 * 3600;
/// Senders that passed greylisting stay known while they keep sending, and are forgotten after
/// 35 quiet days, like postgrey does.
pub const GREYLIST_PASSED_SECS: i64 = 35 * 24 * 3600;
/// What a sender delivered is forgotten after 180 days without mail from it.
pub const REPUTATION_RETENTION_SECS: i64 = 180 * 24 * 3600;

/// What should happen with a message whose sender is being greylisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Greylist {
    /// Seen for the first time (or too soon after it): ask the sender to come back later.
    Wait { seconds: i64 },
    /// The sender came back as a well-behaved server does.
    Pass,
}

/// How a sender behaved so far.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reputation {
    /// Messages that were delivered to an inbox.
    pub good: i64,
    /// Messages the filter or a person put into Junk.
    pub junk: i64,
}

impl Reputation {
    /// Enough history to say something about this sender.
    pub fn is_known(&self) -> bool {
        self.good + self.junk >= 5
    }

    /// Share of this sender's mail that ended up as junk, between 0 and 1.
    pub fn junk_share(&self) -> f32 {
        let total = self.good + self.junk;
        if total == 0 { 0.0 } else { self.junk as f32 / total as f32 }
    }
}

impl Store {
    /// Whether this triplet may deliver now. The first attempt is recorded and told to wait;
    /// a retry after `delay_secs` passes and is remembered, so later mail is not delayed again.
    pub async fn greylist(
        &self,
        network: String,
        sender: String,
        recipient: String,
        delay_secs: i64,
    ) -> Result<Greylist> {
        self.write(move |tx| {
            let now = now();
            let seen: Option<(i64, Option<i64>)> = tx
                .query_row(
                    "SELECT first_seen, passed_at FROM spam_greylist
                     WHERE network = ?1 AND sender = ?2 AND recipient = ?3",
                    params![network, sender, recipient],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            match seen {
                Some((_, Some(_))) => {
                    tx.execute(
                        "UPDATE spam_greylist SET last_seen = ?4
                         WHERE network = ?1 AND sender = ?2 AND recipient = ?3",
                        params![network, sender, recipient, now],
                    )?;
                    Ok(Greylist::Pass)
                }
                Some((first_seen, None)) if now - first_seen >= delay_secs => {
                    tx.execute(
                        "UPDATE spam_greylist SET last_seen = ?4, passed_at = ?4
                         WHERE network = ?1 AND sender = ?2 AND recipient = ?3",
                        params![network, sender, recipient, now],
                    )?;
                    Ok(Greylist::Pass)
                }
                Some((first_seen, None)) => Ok(Greylist::Wait { seconds: delay_secs - (now - first_seen) }),
                None => {
                    tx.execute(
                        "INSERT INTO spam_greylist (network, sender, recipient, first_seen, last_seen)
                         VALUES (?1, ?2, ?3, ?4, ?4)",
                        params![network, sender, recipient, now],
                    )?;
                    Ok(Greylist::Wait { seconds: delay_secs })
                }
            }
        })
        .await
    }

    /// Forgets greylist entries nobody came back for, passed ones that stopped sending, and the
    /// reputation of senders that have been quiet for long. Returns how many rows went.
    pub async fn prune_spam_history(
        &self,
        greylist_waiting_secs: i64,
        greylist_passed_secs: i64,
        reputation_secs: i64,
    ) -> Result<usize> {
        self.write(move |tx| {
            let now = now();
            let mut removed = tx.execute(
                "DELETE FROM spam_greylist
                 WHERE (passed_at IS NULL AND last_seen < ?1) OR (passed_at IS NOT NULL AND last_seen < ?2)",
                params![now - greylist_waiting_secs, now - greylist_passed_secs],
            )?;
            removed +=
                tx.execute("DELETE FROM spam_reputation WHERE updated_at < ?1", params![now - reputation_secs])?;
            Ok(removed)
        })
        .await
    }

    pub async fn reputation(&self, subject: String) -> Result<Reputation> {
        self.read(move |conn| {
            let found = conn
                .query_row("SELECT good, junk FROM spam_reputation WHERE subject = ?1", params![subject], |row| {
                    Ok(Reputation { good: row.get(0)?, junk: row.get(1)? })
                })
                .optional()?;
            Ok(found.unwrap_or_default())
        })
        .await
    }

    /// Counts one message for this sender, either as delivered or as junk.
    pub async fn record_reputation(&self, subject: String, junk: bool) -> Result<()> {
        self.write(move |tx| {
            let now = now();
            let column = if junk { "junk" } else { "good" };
            tx.execute(
                &format!(
                    "INSERT INTO spam_reputation (subject, {column}, first_seen, updated_at) VALUES (?1, 1, ?2, ?2)
                     ON CONFLICT (subject) DO UPDATE SET {column} = {column} + 1, updated_at = ?2"
                ),
                params![subject, now],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_first_attempt_waits_and_the_retry_passes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let triplet = || ("192.0.2.0/24".to_owned(), "spam@example.com".to_owned(), "me@uwu.test".to_owned());
        let (network, sender, recipient) = triplet();
        assert_eq!(store.greylist(network, sender, recipient, 300).await.unwrap(), Greylist::Wait { seconds: 300 });
        // Coming back too early still waits.
        let (network, sender, recipient) = triplet();
        assert!(matches!(store.greylist(network, sender, recipient, 300).await.unwrap(), Greylist::Wait { .. }));
        // Once the delay has passed, the retry is let through and stays through.
        let (network, sender, recipient) = triplet();
        assert_eq!(store.greylist(network, sender, recipient, 0).await.unwrap(), Greylist::Pass);
        let (network, sender, recipient) = triplet();
        assert_eq!(store.greylist(network, sender, recipient, 300).await.unwrap(), Greylist::Pass);
    }

    #[tokio::test]
    async fn old_greylist_entries_and_reputation_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let (network, sender) = ("192.0.2.0/24".to_owned(), "news@example.com".to_owned());
        // One sender still waiting, one that came back and passed, and a reputation entry.
        store.greylist(network.clone(), sender.clone(), "a@uwu.test".into(), 300).await.unwrap();
        store.greylist(network.clone(), sender.clone(), "b@uwu.test".into(), 300).await.unwrap();
        store.greylist(network, sender, "b@uwu.test".into(), 0).await.unwrap();
        store.record_reputation("domain:example.com".into(), false).await.unwrap();

        let long = 365 * 24 * 3600;
        assert_eq!(store.prune_spam_history(long, long, long).await.unwrap(), 0, "nothing is old yet");
        // A negative age puts the cut-off in the future, so it catches everything of that kind.
        assert_eq!(store.prune_spam_history(-1, long, long).await.unwrap(), 1, "only the waiting entry");
        assert_eq!(store.prune_spam_history(long, -1, long).await.unwrap(), 1, "then the passed one");
        assert_eq!(store.prune_spam_history(long, long, -1).await.unwrap(), 1, "then the reputation");
        assert_eq!(store.reputation("domain:example.com".into()).await.unwrap().good, 0);
    }

    #[tokio::test]
    async fn reputation_counts_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let subject = "domain:example.com";
        assert_eq!(store.reputation(subject.to_owned()).await.unwrap().good, 0);
        for _ in 0..4 {
            store.record_reputation(subject.to_owned(), false).await.unwrap();
        }
        store.record_reputation(subject.to_owned(), true).await.unwrap();
        let reputation = store.reputation(subject.to_owned()).await.unwrap();
        assert_eq!((reputation.good, reputation.junk), (4, 1));
        assert!(reputation.is_known());
        assert!((reputation.junk_share() - 0.2).abs() < f32::EPSILON);
    }
}
