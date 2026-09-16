//! Greylisting and what a sender delivered so far.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Result, Store, now};

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

    /// Forgets greylist entries nobody came back for, and passed ones that stopped sending.
    pub async fn prune_greylist(&self, waiting_secs: i64, passed_secs: i64) -> Result<usize> {
        self.write(move |tx| {
            let now = now();
            let removed = tx.execute(
                "DELETE FROM spam_greylist
                 WHERE (passed_at IS NULL AND last_seen < ?1) OR (passed_at IS NOT NULL AND last_seen < ?2)",
                params![now - waiting_secs, now - passed_secs],
            )?;
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
