//! Greylisted mail, kept while its sender is being asked to come back.
//!
//! Greylisting is a good filter and a bad experience: the mail a person is waiting for sits
//! somewhere invisible for a few minutes, and the mail that never comes back leaves no trace at
//! all. Keeping the message here gives them something to look at and, if they want it now, a way
//! to have it now — without weakening what greylisting does to the senders that never return.
//!
//! What the portal shows of it is deliberately thin: the sender and the subject. The body stays in
//! the blob store until someone delivers the message to their own mailbox, where they read it with
//! everything a mail client does about remote images and links. Nothing here is rendered in the
//! portal, so a greylisted phishing mail cannot phish from the page that is warning about it.
//!
//! A settled row stays behind as a tombstone, with its message emptied out. That is what stops the
//! retry a few minutes later from delivering a message that was already delivered by hand, or from
//! quietly resurrecting one that was discarded.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{BlobHash, Result, Store, now};

/// Messages above this are greylisted the old way, without being kept: the point is to help with
/// the mail someone is waiting for, not to hold copies of every large attachment a stranger sends.
pub const MAX_HELD_SIZE: i64 = 5 * 1024 * 1024;

/// What became of a held message once its recipient decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Put into the mailbox by hand, before the sender came back.
    Delivered,
    /// Thrown away. A retry of it is accepted and dropped, so it does not come back.
    Discarded,
}

impl Settled {
    pub fn as_str(self) -> &'static str {
        match self {
            Settled::Delivered => "delivered",
            Settled::Discarded => "discarded",
        }
    }

    pub fn parse(value: &str) -> Option<Settled> {
        Some(match value {
            "delivered" => Settled::Delivered,
            "discarded" => Settled::Discarded,
            _ => return None,
        })
    }
}

/// A message to keep while its sender is asked to come back.
#[derive(Debug, Clone, Default)]
pub struct NewGreylistHold {
    pub account_id: i64,
    pub address: String,
    pub envelope_from: String,
    pub header_from: String,
    pub subject: Option<String>,
    pub message_id: Option<String>,
    pub smtp_id: String,
    pub client_ip: String,
    pub score: Option<f32>,
    /// The message as it would have been delivered, our own headers included.
    pub message: Vec<u8>,
    /// A hash of what the sending server handed us, which a retry repeats.
    pub raw_hash: String,
    /// How long this is worth keeping, from now.
    pub keep_secs: i64,
}

/// A waiting message as the portal lists it. Deliberately without a body: the page shows who wrote
/// and what about, and that is all it is allowed to know.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GreylistHold {
    pub id: i64,
    pub at: i64,
    pub address: String,
    pub envelope_from: String,
    pub header_from: String,
    pub subject: Option<String>,
    pub client_ip: String,
    pub score: Option<f32>,
    pub size: i64,
    pub expires_at: i64,
}

/// A kept message, read back to be delivered or learned from.
#[derive(Debug, Clone)]
pub struct GreylistHoldMessage {
    /// The message as it would have been delivered.
    pub message: Vec<u8>,
    pub hash: BlobHash,
    /// When it first arrived, so delivering it late still files it under the day it was sent.
    pub received_at: i64,
    pub envelope_from: String,
}

/// What a returning message means for a recipient who already decided about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Returning {
    /// Nobody decided; deliver it as usual.
    Fresh,
    /// Already in their mailbox. Accept the retry and drop it for them.
    Delivered,
    /// Thrown away. Accept the retry and drop it for them.
    Discarded,
}

impl Store {
    /// Keeps a greylisted message for one recipient. The message goes into the blob store, the row
    /// holds the reference, and both go away together when it expires.
    pub async fn hold_greylisted(&self, hold: NewGreylistHold) -> Result<i64> {
        let hash = self.put_blob(&hold.message).await?;
        let size = hold.message.len() as i64;
        let expires_at = now() + hold.keep_secs.max(0);
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO greylist_hold (at, account_id, address, envelope_from, header_from, subject, message_id,
                                            smtp_id, client_ip, score, size, blob_hash, raw_hash, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    now(),
                    hold.account_id,
                    hold.address,
                    hold.envelope_from,
                    hold.header_from,
                    hold.subject,
                    hold.message_id,
                    hold.smtp_id,
                    hold.client_ip,
                    hold.score,
                    size,
                    hash.as_str(),
                    hold.raw_hash,
                    expires_at,
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .await
    }

    /// What one person is still waiting on, newest first.
    pub async fn greylist_holds(&self, account_id: i64) -> Result<Vec<GreylistHold>> {
        self.read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, at, address, envelope_from, header_from, subject, client_ip, score, size, expires_at
                 FROM greylist_hold
                 WHERE account_id = ?1 AND settled IS NULL
                 ORDER BY id DESC
                 LIMIT 200",
            )?;
            let found = statement
                .query_map(params![account_id], |row| {
                    Ok(GreylistHold {
                        id: row.get(0)?,
                        at: row.get(1)?,
                        address: row.get(2)?,
                        envelope_from: row.get(3)?,
                        header_from: row.get(4)?,
                        subject: row.get(5)?,
                        client_ip: row.get(6)?,
                        score: row.get(7)?,
                        size: row.get(8)?,
                        expires_at: row.get(9)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(found)
        })
        .await
    }

    /// How many messages one person has waiting, for the badge on the tab.
    pub async fn greylist_hold_count(&self, account_id: i64) -> Result<i64> {
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM greylist_hold WHERE account_id = ?1 AND settled IS NULL",
                params![account_id],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// Reads the kept message of one waiting row, without settling it. The caller stores it
    /// somewhere that holds a reference of its own before settling, so the blob is never left
    /// unreferenced in between.
    ///
    /// Only ever answers for the account the row belongs to, which is what keeps one person from
    /// reading another's mail by guessing a number.
    pub async fn greylist_hold_message(&self, account_id: i64, id: i64) -> Result<Option<GreylistHoldMessage>> {
        let found: Option<(Option<String>, i64, String)> = self
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT blob_hash, at, envelope_from FROM greylist_hold
                         WHERE id = ?1 AND account_id = ?2 AND settled IS NULL",
                        params![id, account_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?)
            })
            .await?;
        let Some((Some(hash), received_at, envelope_from)) = found else { return Ok(None) };
        let hash = BlobHash::parse(&hash)?;
        let message = self.blob(&hash).await?;
        Ok(Some(GreylistHoldMessage { message, hash, received_at, envelope_from }))
    }

    /// Marks a waiting message as decided. `keep_message` leaves the blob in place, which the spam
    /// filter needs: it learns from the message after the fact, off its own queue, and a message
    /// deleted out from under it would only be dropped unlearned. Those blobs go when the row
    /// expires.
    ///
    /// Returns whether there was anything to settle.
    pub async fn settle_greylist_hold(
        &self,
        account_id: i64,
        id: i64,
        how: Settled,
        keep_message: bool,
    ) -> Result<bool> {
        self.write(move |tx| {
            let changed = if keep_message {
                tx.execute(
                    "UPDATE greylist_hold SET settled = ?3, settled_at = ?4
                     WHERE id = ?1 AND account_id = ?2 AND settled IS NULL",
                    params![id, account_id, how.as_str(), now()],
                )?
            } else {
                tx.execute(
                    "UPDATE greylist_hold SET settled = ?3, settled_at = ?4, blob_hash = NULL
                     WHERE id = ?1 AND account_id = ?2 AND settled IS NULL",
                    params![id, account_id, how.as_str(), now()],
                )?
            };
            Ok(changed > 0)
        })
        .await
    }

    /// What to do with a message arriving for someone who was greylisted on it before.
    ///
    /// A row that is still waiting means the sender came back on their own: the message is about to
    /// be delivered the normal way, so the row goes and the person never sees it in the list. A
    /// settled row means they already dealt with it by hand, and the retry must not undo that.
    pub async fn returning_greylist_hold(
        &self,
        account_id: i64,
        raw_hash: &str,
        message_id: Option<&str>,
    ) -> Result<Returning> {
        let (raw_hash, message_id) = (raw_hash.to_owned(), message_id.map(str::to_owned));
        self.write(move |tx| {
            // Same bytes, or failing that the same Message-ID: a sender that rewrites something on
            // the retry still gets recognised, and one that sends neither is simply delivered.
            let found: Option<(i64, Option<String>)> = tx
                .query_row(
                    "SELECT id, settled FROM greylist_hold
                     WHERE account_id = ?1
                       AND (raw_hash = ?2 OR (?3 IS NOT NULL AND message_id = ?3))
                     ORDER BY settled IS NULL DESC, id DESC
                     LIMIT 1",
                    params![account_id, raw_hash, message_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((id, settled)) = found else { return Ok(Returning::Fresh) };
            match settled.as_deref().and_then(Settled::parse) {
                // It is coming back by itself, so nothing needs to be held any longer.
                None => {
                    tx.execute("DELETE FROM greylist_hold WHERE id = ?1", params![id])?;
                    Ok(Returning::Fresh)
                }
                Some(Settled::Delivered) => Ok(Returning::Delivered),
                Some(Settled::Discarded) => Ok(Returning::Discarded),
            }
        })
        .await
    }

    /// Drops what nobody came back for and what nobody decided about. Returns how many went.
    pub async fn prune_greylist_holds(&self) -> Result<usize> {
        self.write(move |tx| Ok(tx.execute("DELETE FROM greylist_hold WHERE expires_at <= ?1", params![now()])?)).await
    }

    /// Everything that is being kept, for switching the feature off and clearing it out.
    pub async fn clear_greylist_holds(&self) -> Result<usize> {
        self.write(move |tx| Ok(tx.execute("DELETE FROM greylist_hold", [])?)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;

    /// A store with the domain the test accounts live in.
    async fn ready() -> (Store, tempfile::TempDir) {
        let (store, dir) = store().await;
        store.create_domain("uwu.test").await.unwrap();
        (store, dir)
    }

    async fn account(store: &Store, login: &str) -> i64 {
        let account = crate::NewAccount {
            address: format!("{login}@uwu.test"),
            display_name: String::new(),
            password: None,
            role: crate::Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        store.create_account(account).await.unwrap().id
    }

    fn hold(account_id: i64, subject: &str, message: &[u8]) -> NewGreylistHold {
        NewGreylistHold {
            account_id,
            address: "nyu@uwu.test".into(),
            envelope_from: "fremder@example.org".into(),
            header_from: "fremder@example.org".into(),
            subject: Some(subject.into()),
            message_id: Some(format!("<{subject}@example.org>")),
            smtp_id: "abc123".into(),
            client_ip: "198.51.100.7".into(),
            score: Some(3.0),
            message: message.to_vec(),
            raw_hash: BlobHash::of(message).as_str().to_owned(),
            keep_secs: 2 * 24 * 3600,
        }
    }

    #[tokio::test]
    async fn a_held_message_is_listed_read_and_settled() {
        let (store, _dir) = ready().await;
        let account = account(&store, "nyu").await;
        let raw = b"Subject: Rechnung\r\n\r\nHallo".to_vec();
        let id = store.hold_greylisted(hold(account, "Rechnung", &raw)).await.unwrap();

        let waiting = store.greylist_holds(account).await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].subject.as_deref(), Some("Rechnung"));
        assert_eq!(waiting[0].envelope_from, "fremder@example.org");
        assert_eq!(store.greylist_hold_count(account).await.unwrap(), 1);

        let held = store.greylist_hold_message(account, id).await.unwrap().unwrap();
        assert_eq!(held.message, raw, "the kept message comes back byte for byte");
        assert_eq!(held.envelope_from, "fremder@example.org");
        assert!(held.received_at > 0);

        assert!(store.settle_greylist_hold(account, id, Settled::Delivered, false).await.unwrap());
        assert!(
            !store.settle_greylist_hold(account, id, Settled::Delivered, false).await.unwrap(),
            "settling twice changes nothing"
        );
        assert_eq!(store.greylist_holds(account).await.unwrap().len(), 0, "settled rows leave the list");
        assert!(store.greylist_hold_message(account, id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn nobody_reads_or_settles_another_persons_mail() {
        let (store, _dir) = ready().await;
        let (mine, theirs) = (account(&store, "nyu").await, account(&store, "lorin").await);
        let id = store.hold_greylisted(hold(mine, "Privat", b"Subject: Privat\r\n\r\nGeheim")).await.unwrap();

        assert_eq!(store.greylist_holds(theirs).await.unwrap().len(), 0);
        assert!(store.greylist_hold_message(theirs, id).await.unwrap().is_none(), "not even by guessing the number");
        assert!(!store.settle_greylist_hold(theirs, id, Settled::Discarded, false).await.unwrap());
        assert_eq!(store.greylist_holds(mine).await.unwrap().len(), 1, "and it is still there for its owner");
    }

    #[tokio::test]
    async fn a_returning_message_is_recognised_by_what_was_decided() {
        let (store, _dir) = ready().await;
        let account = account(&store, "nyu").await;
        let raw = b"Subject: Angebot\r\n\r\nHallo".to_vec();
        let raw_hash = BlobHash::of(&raw).as_str().to_owned();
        let id = store.hold_greylisted(hold(account, "Angebot", &raw)).await.unwrap();

        // Coming back while it still waits: delivered as usual, and the row goes.
        assert_eq!(
            store.returning_greylist_hold(account, &raw_hash, Some("<Angebot@example.org>")).await.unwrap(),
            Returning::Fresh
        );
        assert_eq!(store.greylist_holds(account).await.unwrap().len(), 0, "it is arriving normally now");

        // Delivered by hand: the retry must not deliver it a second time.
        let id2 = store.hold_greylisted(hold(account, "Angebot", &raw)).await.unwrap();
        store.settle_greylist_hold(account, id2, Settled::Delivered, false).await.unwrap();
        assert_eq!(store.returning_greylist_hold(account, &raw_hash, None).await.unwrap(), Returning::Delivered);

        // Discarded: the retry must not bring it back.
        let id3 = store.hold_greylisted(hold(account, "Weg", &raw)).await.unwrap();
        store.settle_greylist_hold(account, id3, Settled::Discarded, false).await.unwrap();
        assert_eq!(store.returning_greylist_hold(account, &raw_hash, None).await.unwrap(), Returning::Discarded);

        // Something nobody was greylisted on is none of its business.
        let other = BlobHash::of(b"etwas ganz anderes").as_str().to_owned();
        assert_eq!(store.returning_greylist_hold(account, &other, None).await.unwrap(), Returning::Fresh);
        let _ = id;
    }

    #[tokio::test]
    async fn a_message_nobody_came_back_for_is_forgotten() {
        let (store, _dir) = ready().await;
        let account = account(&store, "nyu").await;
        let mut expiring = hold(account, "Alt", b"Subject: Alt\r\n\r\nHallo");
        expiring.keep_secs = -1;
        store.hold_greylisted(expiring).await.unwrap();
        store.hold_greylisted(hold(account, "Neu", b"Subject: Neu\r\n\r\nHallo")).await.unwrap();

        assert_eq!(store.prune_greylist_holds().await.unwrap(), 1);
        let left = store.greylist_holds(account).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].subject.as_deref(), Some("Neu"));

        assert_eq!(store.clear_greylist_holds().await.unwrap(), 1);
        assert_eq!(store.greylist_holds(account).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn the_kept_message_is_freed_with_the_row() {
        let (store, _dir) = ready().await;
        let account = account(&store, "nyu").await;
        let raw = b"Subject: Muell\r\n\r\nHallo".to_vec();
        let id = store.hold_greylisted(hold(account, "Muell", &raw)).await.unwrap();
        let hash = BlobHash::of(&raw);

        assert!(store.blob_hashes().await.unwrap().iter().any(|(each, _)| each == &hash), "held while it waits");

        // Discarding without keeping the message lets go of it at once.
        store.settle_greylist_hold(account, id, Settled::Discarded, false).await.unwrap();
        assert!(!store.blob_hashes().await.unwrap().iter().any(|(each, _)| each == &hash), "and let go when discarded");

        // Keeping it for the spam filter holds on until the row itself expires.
        let mut learned = hold(account, "Muell", &raw);
        learned.keep_secs = -1;
        store.hold_greylisted(learned).await.unwrap();
        let id2 = store.greylist_holds(account).await.unwrap()[0].id;
        store.settle_greylist_hold(account, id2, Settled::Discarded, true).await.unwrap();
        assert!(store.blob_hashes().await.unwrap().iter().any(|(each, _)| each == &hash), "still there to learn from");
        store.prune_greylist_holds().await.unwrap();
        assert!(!store.blob_hashes().await.unwrap().iter().any(|(each, _)| each == &hash), "and gone with the row");
    }
}
