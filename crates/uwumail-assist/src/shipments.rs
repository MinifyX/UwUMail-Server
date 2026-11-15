//! The look back for parcels (docs/jmap-shipments.md, "Looking back"): when a person switches
//! shipments on, after the update that brought them, and on `Shipment/rescan`, their mail of the
//! last days is read oldest first, so each status moves on in order. Reading a mail twice changes
//! nothing, so a look back cut off by a restart simply goes on.

use std::time::Duration;

use tokio::sync::watch;
use uwumail_labels::Mail;
use uwumail_store::{EmailRecord, ShipmentRescanJob};

use crate::Assist;

/// Mails read per batch, and the pause between batches, so a busy server does not notice it.
const BATCH: i64 = 50;
const PAUSE: Duration = Duration::from_millis(200);
/// How often it looks for new work when there is none.
const IDLE: Duration = Duration::from_secs(15);
/// Bytes of a message read, as at delivery.
const MAX_PARSE_BYTES: usize = 4 * 1024 * 1024;

impl Assist {
    pub async fn run_shipment_rescan(self, mut shutdown: watch::Receiver<bool>) {
        loop {
            let worked = self.shipment_rescan_round(&shutdown).await;
            if *shutdown.borrow() {
                return;
            }
            let wait = if worked { PAUSE } else { IDLE };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.changed() => {}
            }
        }
    }

    /// Reads one batch of every look back waiting; whether there was any.
    pub async fn shipment_rescan_round(&self, shutdown: &watch::Receiver<bool>) -> bool {
        let jobs = match self.store().shipment_rescan_jobs().await {
            Ok(jobs) => jobs,
            Err(err) => {
                tracing::warn!(%err, "reading the looks back for parcels failed");
                return false;
            }
        };
        let worked = !jobs.is_empty();
        for job in jobs {
            if *shutdown.borrow() {
                break;
            }
            self.shipment_batch(job).await;
        }
        worked
    }

    /// Reads the next batch of one look back and notes how far it got.
    async fn shipment_batch(&self, job: ShipmentRescanJob) {
        let store = self.store();
        let batch = match store.shipment_rescan_candidates(job.account_id, job.since, job.after_id, BATCH).await {
            Ok(batch) => batch,
            Err(err) => return tracing::warn!(account = job.account_id, %err, "looking back for parcels failed"),
        };
        let done = (batch.len() as i64) < BATCH;
        let after = batch.last().map_or(job.after_id, |record| record.id);
        let mut found = 0;
        for record in batch {
            found += i64::from(self.shipment_one(job.account_id, record).await);
        }
        if let Err(err) = store.shipment_rescan_progress(job.account_id, after, found, done).await {
            tracing::warn!(account = job.account_id, %err, "noting how far the look back for parcels got failed");
        }
    }

    /// Reads one mail; whether it told of a parcel the account did not know of yet.
    async fn shipment_one(&self, account_id: i64, record: EmailRecord) -> bool {
        let Ok(raw) = self.store().blob(&record.blob).await else { return false };
        // A mail the sender wrote is parsed off the async runtime (security review SPAM-4).
        let found = tokio::task::spawn_blocking(move || {
            let mail = Mail::parse(&raw[..raw.len().min(MAX_PARSE_BYTES)]);
            uwumail_labels::shipment(&mail).filter(|facts| facts.confidence >= 0.8).map(|facts| (facts, mail.subject))
        })
        .await
        .ok()
        .flatten();
        let Some((facts, subject)) = found else { return false };
        match self.store().record_shipment_mail(account_id, record.id, record.received_at, subject, facts).await {
            Ok(changed) => changed,
            Err(err) => {
                tracing::warn!(account = account_id, email = record.id, %err, "noting a parcel failed");
                false
            }
        }
    }
}
