//! Sends what JMAP held back — for the undo window or until a `sendAt` — once its time has come.
//! The held messages are in the store, so a restart only delays them.

use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;
use uwumail_smtp::{Submission, SubmissionRecipient};
use uwumail_store::HeldSubmission;

use crate::Jmap;
use crate::methods::unix_now;

/// The longest the sender sleeps without looking, in case a wake-up got lost.
const MAX_SLEEP_SECS: i64 = 60;
/// How many held submissions are handed over in one go.
const BATCH: usize = 50;

impl Jmap {
    /// Runs until `shutdown`: sends held submissions when they are due.
    pub async fn run_scheduled_sending(self, mut shutdown: watch::Receiver<bool>) {
        loop {
            self.release_due_submissions().await;
            let wait = match self.inner.store.next_held_submission().await {
                Ok(Some(at)) => (at - unix_now()).clamp(1, MAX_SLEEP_SECS),
                Ok(None) => MAX_SLEEP_SECS,
                Err(err) => {
                    tracing::warn!(%err, "reading the held submissions failed");
                    MAX_SLEEP_SECS
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(wait as u64)) => {}
                _ = self.inner.wake.notified() => {}
                _ = shutdown.changed() => return,
            }
        }
    }

    /// Hands every held submission that is due to SMTP and returns how many there were.
    pub async fn release_due_submissions(&self) -> usize {
        let mut released = 0;
        loop {
            let claimed = match self.inner.store.claim_due_submissions(BATCH).await {
                Ok(claimed) => claimed,
                Err(err) => {
                    tracing::warn!(%err, "taking the due submissions failed");
                    return released;
                }
            };
            if claimed.is_empty() {
                return released;
            }
            for held in claimed {
                let id = held.id;
                let (queue_message_id, error) = match self.send_held(held).await {
                    Ok(queue_message_id) => (queue_message_id, None),
                    Err(error) => {
                        tracing::warn!(submission = id, %error, "a held message could not be sent");
                        (None, Some(error))
                    }
                };
                if let Err(err) = self.inner.store.finish_held_submission(id, queue_message_id, error).await {
                    tracing::error!(%err, submission = id, "recording a sent held message failed");
                }
                released += 1;
            }
        }
    }

    async fn send_held(&self, held: HeldSubmission) -> Result<Option<i64>, String> {
        let store = &self.inner.store;
        let account = match store.account_by_id(held.account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err("the account no longer exists".into()),
            Err(err) => return Err(err.to_string()),
        };
        let envelope: Value = serde_json::from_str(&held.envelope).map_err(|err| err.to_string())?;
        let mail_from = envelope.pointer("/mailFrom/email").and_then(Value::as_str).unwrap_or_default().to_owned();
        let recipients: Vec<SubmissionRecipient> = envelope
            .get("rcptTo")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|r| r.get("email").and_then(Value::as_str))
                    .map(SubmissionRecipient::new)
                    .collect()
            })
            .unwrap_or_default();
        let raw = store.blob(&held.blob).await.map_err(|err| err.to_string())?;
        let submission = Submission { account, mail_from, recipients, raw, env_id: None, trace: None };
        match self.inner.smtp.submit(submission).await {
            Ok(submitted) => Ok(submitted.queue_message_id),
            Err(err) => Err(err.to_string()),
        }
    }
}
