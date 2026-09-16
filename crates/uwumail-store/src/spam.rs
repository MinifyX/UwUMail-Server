//! Greylisting and what a sender delivered so far.

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;

use crate::blobs::BlobHash;
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
            removed += tx.execute("DELETE FROM spam_verdicts WHERE counted_at < ?1", params![now - reputation_secs])?;
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

    /// Counts a delivered message for its sender and remembers how, so that a person marking it as
    /// spam or not spam later moves this count instead of adding one. A message counts once, however
    /// many of our people it reached.
    pub async fn record_delivery(&self, blob: BlobHash, subject: String, junk: bool) -> Result<()> {
        self.write(move |tx| {
            let now = now();
            let new = tx.execute(
                "INSERT INTO spam_verdicts (blob_hash, subject, junk, counted_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (blob_hash) DO NOTHING",
                params![blob.as_str(), subject, junk, now],
            )?;
            if new == 1 {
                let column = if junk { "junk" } else { "good" };
                tx.execute(
                    &format!(
                        "INSERT INTO spam_reputation (subject, {column}, first_seen, updated_at) VALUES (?1, 1, ?2, ?2)
                         ON CONFLICT (subject) DO UPDATE SET {column} = {column} + 1, updated_at = ?2"
                    ),
                    params![subject, now],
                )?;
            }
            Ok(())
        })
        .await
    }
}

/// A person marked an email as junk (`true`) or not junk (`false`): moves how its delivery counts for
/// the sender. Mail that was never counted, like mail from our own people or network or from before
/// the filter, has nothing to move, and marking it the way it already counts changes nothing.
pub(crate) fn rebook_verdict(tx: &Transaction<'_>, email_id: i64, junk: bool) -> Result<()> {
    let counted: Option<(String, String, bool)> = tx
        .query_row(
            "SELECT v.blob_hash, v.subject, v.junk FROM emails e JOIN spam_verdicts v ON v.blob_hash = e.blob_hash
             WHERE e.id = ?1",
            params![email_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((blob_hash, subject, counted_junk)) = counted else { return Ok(()) };
    if counted_junk == junk {
        return Ok(());
    }
    let (good, bad) = if junk { (-1, 1) } else { (1, -1) };
    tx.execute(
        "UPDATE spam_reputation SET good = MAX(good + ?2, 0), junk = MAX(junk + ?3, 0), updated_at = ?4
         WHERE subject = ?1",
        params![subject, good, bad, now()],
    )?;
    tx.execute("UPDATE spam_verdicts SET junk = ?2 WHERE blob_hash = ?1", params![blob_hash, junk])?;
    Ok(())
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
        // One sender still waiting, one that came back and passed, and a counted delivery.
        store.greylist(network.clone(), sender.clone(), "a@uwu.test".into(), 300).await.unwrap();
        store.greylist(network.clone(), sender.clone(), "b@uwu.test".into(), 300).await.unwrap();
        store.greylist(network, sender, "b@uwu.test".into(), 0).await.unwrap();
        store.record_delivery(BlobHash::of(b"one"), "domain:example.com".into(), false).await.unwrap();

        let long = 365 * 24 * 3600;
        assert_eq!(store.prune_spam_history(long, long, long).await.unwrap(), 0, "nothing is old yet");
        // A negative age puts the cut-off in the future, so it catches everything of that kind.
        assert_eq!(store.prune_spam_history(-1, long, long).await.unwrap(), 1, "only the waiting entry");
        assert_eq!(store.prune_spam_history(long, -1, long).await.unwrap(), 1, "then the passed one");
        assert_eq!(store.prune_spam_history(long, long, -1).await.unwrap(), 2, "then the reputation and its verdict");
        assert_eq!(store.reputation("domain:example.com".into()).await.unwrap().good, 0);
    }

    async fn counts(store: &Store, subject: &str) -> (i64, i64) {
        let reputation = store.reputation(subject.to_owned()).await.unwrap();
        (reputation.good, reputation.junk)
    }

    async fn apply(store: &Store, account_id: i64, update: crate::EmailUpdate) {
        let results = store.update_emails(account_id, vec![update]).await.unwrap();
        assert!(results.iter().all(Result::is_ok), "{results:?}");
    }

    #[tokio::test]
    async fn spam_and_not_spam_from_a_person_move_the_count() {
        use crate::{
            EmailUpdate, IngestRequest, KeywordsChange, MailboxRole, MailboxTarget, MailboxesChange, NewAccount, Role,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("uwu.test").await.unwrap();
        let account = NewAccount {
            address: "mini@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
        };
        let account_id = store.create_account(account).await.unwrap().id;
        let boxes = store.mailboxes(account_id).await.unwrap();
        let mailbox = |role| boxes.iter().find(|mailbox| mailbox.role == Some(role)).unwrap().id;
        let (inbox, junk, trash) =
            (mailbox(MailboxRole::Inbox), mailbox(MailboxRole::Junk), mailbox(MailboxRole::Trash));
        let deliver = |raw: &[u8]| IngestRequest {
            account_id,
            raw: raw.to_vec(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        };
        let subject = "domain:example.com";
        let raw = b"From: news@example.com\r\nSubject: Angebot\r\n\r\nNur heute\r\n";
        let email = store.ingest(deliver(raw)).await.unwrap().id;
        store.record_delivery(BlobHash::of(raw), subject.into(), false).await.unwrap();
        store.record_delivery(BlobHash::of(raw), subject.into(), false).await.unwrap();
        assert_eq!(counts(&store, subject).await, (1, 0), "one delivery counts once");

        let change = |keywords, mailboxes| EmailUpdate { id: email, keywords, mailboxes };
        // "Spam" the way the UwUMail apps say it: keyword and move together, counted once.
        apply(
            &store,
            account_id,
            change(KeywordsChange::Patch(vec![("$junk".into(), true)]), MailboxesChange::Replace(vec![junk])),
        )
        .await;
        assert_eq!(counts(&store, subject).await, (0, 1));
        // Emptying Junk into the Trash is tidying up, not "Not spam".
        apply(&store, account_id, change(KeywordsChange::Keep, MailboxesChange::Replace(vec![trash]))).await;
        assert_eq!(counts(&store, subject).await, (0, 1));
        // Back into Junk: it already counts as junk.
        apply(&store, account_id, change(KeywordsChange::Keep, MailboxesChange::Replace(vec![junk]))).await;
        assert_eq!(counts(&store, subject).await, (0, 1));
        // "Not spam" by moving it out of Junk, as any mail app can.
        apply(&store, account_id, change(KeywordsChange::Keep, MailboxesChange::Replace(vec![inbox]))).await;
        assert_eq!(counts(&store, subject).await, (1, 0));
        // And by the keyword alone, which it already is.
        apply(
            &store,
            account_id,
            change(KeywordsChange::Patch(vec![("$notjunk".into(), true)]), MailboxesChange::Keep),
        )
        .await;
        assert_eq!(counts(&store, subject).await, (1, 0));

        // Mail that was never counted, e.g. from before the filter, has nothing to move.
        let other = store.ingest(deliver(b"From: old@example.com\r\nSubject: Alt\r\n\r\nalt\r\n")).await.unwrap().id;
        let other =
            EmailUpdate { id: other, keywords: KeywordsChange::Keep, mailboxes: MailboxesChange::Replace(vec![junk]) };
        apply(&store, account_id, other).await;
        assert_eq!(counts(&store, subject).await, (1, 0));
    }

    #[tokio::test]
    async fn reputation_counts_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let subject = "domain:example.com";
        assert_eq!(store.reputation(subject.to_owned()).await.unwrap().good, 0);
        for n in 0..4 {
            store
                .record_delivery(BlobHash::of(format!("good {n}").as_bytes()), subject.to_owned(), false)
                .await
                .unwrap();
        }
        store.record_delivery(BlobHash::of(b"junk"), subject.to_owned(), true).await.unwrap();
        let reputation = store.reputation(subject.to_owned()).await.unwrap();
        assert_eq!((reputation.good, reputation.junk), (4, 1));
        assert!(reputation.is_known());
        assert!((reputation.junk_share() - 0.2).abs() < f32::EPSILON);
    }
}
