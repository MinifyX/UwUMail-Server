//! The label worker: puts the person's labels on mail that was delivered to them, in the background.
//!
//! Delivery only queues a mail (`Store::enqueue_auto_label`) and never waits for this. A job that
//! fails for a passing reason (the provider is busy or away) is tried again twice, a few minutes
//! apart; any other failure, and a job older than a day, is dropped: the mail simply keeps no label.
//!
//! It also learns from labels the person put on or took off by hand (docs/labels.md): the
//! classifier's examples. That needs no model.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::watch;
use uwumail_store::{LabelJob, LabelTraining, MailboxRole, StoreError};

use crate::mail::MAX_PARSE_BYTES;
use crate::{Assist, AssistError, now};

/// Tries per job.
const MAX_ATTEMPTS: i64 = 3;
/// Waits before the second and the third try.
const RETRY_SECS: [i64; 2] = [60, 300];
/// A job older than this is dropped.
const MAX_AGE_SECS: i64 = 86_400;
/// Jobs taken at once.
const BATCH: usize = 20;
/// Usage is kept this long.
const USAGE_DAYS: i64 = 400;
/// Looks again at least this often, for jobs that became due.
const IDLE: Duration = Duration::from_secs(300);
/// Jobs worked on side by side; a batch holds at most one job per person, so one person's slow
/// provider holds up nobody else's labels (AI-04 of the 0.18.0 audit).
const AT_ONCE: usize = 4;
/// The longest one job may take; it is tried again later, like a busy provider.
const JOB_TIMEOUT: Duration = Duration::from_secs(45);

/// What became of one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobOutcome {
    Labeled(usize),
    Skipped,
    Retry,
    Dropped,
}

/// What kind of failure it was, for the log: never the provider's own words, which may repeat
/// parts of the mail.
fn kind(err: &AssistError) -> &'static str {
    match err {
        AssistError::Unavailable(_) => "unavailable",
        AssistError::OverQuota(_) => "over quota",
        AssistError::ProviderFailed { transient: true, .. } => "provider failed for now",
        AssistError::ProviderFailed { .. } => "provider failed",
        AssistError::NotFound(_) => "not found",
        AssistError::Forbidden(_) => "forbidden",
        AssistError::Invalid { .. } => "invalid",
        AssistError::Busy => "busy",
        AssistError::Store(_) => "store",
    }
}

fn transient(err: &AssistError) -> bool {
    match err {
        AssistError::Busy => true,
        AssistError::ProviderFailed { transient, .. } => *transient,
        AssistError::Store(StoreError::Busy) => true,
        _ => false,
    }
}

impl Assist {
    /// Works off the queue until `shutdown` turns true.
    pub async fn run_label_worker(self, mut shutdown: watch::Receiver<bool>) {
        let mut last_prune = 0;
        loop {
            if *shutdown.borrow() {
                return;
            }
            if now() - last_prune > 3600 {
                last_prune = now();
                let before_day = uwumail_store::utc_day(now() - USAGE_DAYS * 86_400);
                if let Err(err) = self.store().prune_assist(before_day, now() - MAX_AGE_SECS).await {
                    tracing::warn!(%err, "pruning the AI assistant's queue and counts failed");
                }
            }
            let learned = self.learn_labels().await;
            let worked = self.work_queue().await || learned;
            let wait = if worked {
                Duration::ZERO
            } else {
                match self.store().next_label_job_at().await {
                    Ok(Some(at)) => {
                        Duration::from_secs(at.saturating_sub(now()).clamp(1, IDLE.as_secs() as i64) as u64)
                    }
                    _ => IDLE,
                }
            };
            if wait.is_zero() {
                continue;
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.store().assist_wakeup().notified() => {}
                _ = shutdown.changed() => {}
            }
        }
    }

    /// Learns from the hand-labelings waiting. `true` when there were any.
    pub async fn learn_labels(&self) -> bool {
        let jobs = match self.store().label_training().await {
            Ok(jobs) => jobs,
            Err(err) => {
                tracing::warn!(%err, "reading what labels learn failed");
                return false;
            }
        };
        for job in &jobs {
            if let Err(err) = self.learn_label(job).await {
                tracing::warn!(%err, account = job.account_id, "learning a label failed");
            }
            if let Err(err) = self.store().finish_label_training(job.id).await {
                tracing::warn!(%err, "updating what labels learn failed");
                return false;
            }
        }
        !jobs.is_empty()
    }

    /// The classifier's tokens of an email: those it was learned with, or read from the message.
    /// `None` when the email is gone.
    async fn label_tokens(&self, account_id: i64, email_id: i64) -> Result<Option<Vec<i64>>, StoreError> {
        if let Some(tokens) = self.store().label_example_tokens(account_id, email_id).await? {
            return Ok(Some(tokens));
        }
        let record = match self.store().email(account_id, email_id).await {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) => return Ok(None),
            Err(err) => return Err(err),
        };
        let raw = self.store().blob(&record.blob).await?;
        let mail = uwumail_labels::Mail::parse(&raw[..raw.len().min(MAX_PARSE_BYTES)]);
        Ok(Some(uwumail_labels::tokens(&mail).iter().map(|token| uwumail_labels::token_hash(token)).collect()))
    }

    /// Learns one hand-labeling: the email as an example with or without the label and, for a new
    /// example with it, an ordinary inbox mail as one without any.
    async fn learn_label(&self, job: &LabelTraining) -> Result<(), StoreError> {
        let Some(tokens) = self.label_tokens(job.account_id, job.email_id).await? else { return Ok(()) };
        let newly =
            self.store().learn_label_example(job.account_id, job.email_id, job.label_id, job.positive, tokens).await?;
        if !newly {
            return Ok(());
        }
        let Some(other) = self.store().label_background_candidate(job.account_id, job.email_id).await? else {
            return Ok(());
        };
        if let Some(tokens) = self.label_tokens(job.account_id, other).await? {
            self.store().add_label_background(job.account_id, other, tokens).await?;
        }
        Ok(())
    }

    /// Takes the jobs that are due now. `true` when there were any.
    pub async fn work_queue(&self) -> bool {
        let jobs = match self.store().due_label_jobs(BATCH).await {
            Ok(jobs) => jobs,
            Err(err) => {
                tracing::warn!(%err, "reading the label queue failed");
                return false;
            }
        };
        let any = !jobs.is_empty();
        let work = |job: LabelJob| async move {
            let outcome = match tokio::time::timeout(JOB_TIMEOUT, self.work_job(&job)).await {
                Ok(outcome) => outcome,
                Err(_) if job.attempts + 1 < MAX_ATTEMPTS => JobOutcome::Retry,
                Err(_) => JobOutcome::Dropped,
            };
            (job, outcome)
        };
        let mut running = futures_util::stream::iter(jobs.into_iter().map(work)).buffer_unordered(AT_ONCE);
        while let Some((job, outcome)) = running.next().await {
            let result = match outcome {
                JobOutcome::Retry => {
                    let wait = RETRY_SECS.get(job.attempts as usize).copied().unwrap_or(300);
                    self.store().retry_label_job(job.id, now() + wait).await
                }
                _ => self.store().finish_label_job(job.id).await,
            };
            if let Err(err) = result {
                tracing::warn!(%err, "updating the label queue failed");
            }
        }
        any
    }

    /// Labels the mail of one job, if it still wants labels.
    pub async fn work_job(&self, job: &LabelJob) -> JobOutcome {
        if now() - job.queued_at > MAX_AGE_SECS {
            return JobOutcome::Dropped;
        }
        let account = match self.store().account_by_id(job.account_id).await {
            Ok(Some(account)) if account.deleted_at.is_none() => account,
            _ => return JobOutcome::Dropped,
        };
        match self.store().assist_prefs(account.id).await {
            Ok(prefs) if prefs.auto_labels => {}
            _ => return JobOutcome::Skipped,
        }
        // Mail that went to Junk or the Trash since, or is gone, keeps no label.
        let record = match self.store().email(account.id, job.email_id).await {
            Ok(record) => record,
            Err(_) => return JobOutcome::Skipped,
        };
        if let Ok(mailboxes) = self.store().mailboxes(account.id).await {
            let unwanted = mailboxes.iter().any(|mailbox| {
                matches!(
                    mailbox.role,
                    Some(MailboxRole::Junk | MailboxRole::Trash | MailboxRole::Sent | MailboxRole::Drafts)
                ) && record.mailbox_ids.contains(&mailbox.id)
            });
            if unwanted {
                return JobOutcome::Skipped;
            }
        }
        match self.label_email(&account, job.email_id).await {
            Ok(picks) => JobOutcome::Labeled(picks.len()),
            Err(err) if transient(&err) && job.attempts + 1 < MAX_ATTEMPTS => {
                tracing::info!(account = account.id, error = kind(&err), "labels for a mail have to wait");
                JobOutcome::Retry
            }
            Err(err) => {
                tracing::info!(account = account.id, error = kind(&err), "no labels for a mail");
                JobOutcome::Dropped
            }
        }
    }
}
