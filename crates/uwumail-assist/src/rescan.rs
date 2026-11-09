//! The one-time look for codes after an update (docs/verification-codes.md, "After an update"):
//! the mail of the last day that has none is read once more with what the new version finds.
//!
//! The sender is only believed by the `Authentication-Results` this server wrote itself when its
//! SMTP delivery stored the mail ([`delivered_headers`], [`authentication`]), the same verdicts the
//! spam check reads. Mail without them (imported, appended, from before `smtp_delivered`) is not
//! read at all.
//!
//! shortcut: mail that apps on this server sent in through submission before 0.30.0 was stored
//! without this mark and has no `Authentication-Results`, so it is skipped; its codes were good for
//! minutes and have run out by the time anybody updates. Upgrade if a re-scan must cover it.

use std::time::Duration;

use tokio::sync::watch;
use uwumail_store::EmailRecord;
use uwumail_store::verification::{self, Authenticity, DETECTOR_VERSION, MAX_MESSAGE, registrable_domain};

use crate::features::{authentication, delivered_headers};
use crate::mail::MailText;
use crate::{Assist, now};

/// How far back the re-scan reads.
const RESCAN_SECS: i64 = 24 * 3600;
/// Mails read per batch, and the pause between batches, so a busy server does not notice it.
const BATCH: i64 = 50;
const PAUSE: Duration = Duration::from_millis(200);

/// What this server's own verdicts in `raw` say about the sender of `record`.
pub(crate) fn authenticity(record: &EmailRecord, raw: &[u8], hostname: &str) -> Authenticity {
    let mail = MailText::read(record, raw, 0);
    let auth = authentication(delivered_headers(record, &mail.headers), Some(hostname), &record.from);
    let aligned = |domain: &str| {
        auth.from_domain
            .as_deref()
            .and_then(registrable_domain)
            .is_some_and(|from| registrable_domain(domain) == Some(from))
    };
    // Alignment only speaks for a domain without a DMARC record (`dmarc=none`), as at delivery (R30-02).
    let no_dmarc_record = matches!(auth.dmarc.as_deref(), None | Some("none"));
    Authenticity {
        // The header's own signature is not in the results; a Verification-Info header is read as a guess.
        header_signed: false,
        dmarc_passed: auth.dmarc.as_deref() == Some("pass"),
        dkim_aligned: no_dmarc_record && auth.dkim_pass_domains.iter().any(|domain| aligned(domain)),
        spf_aligned: no_dmarc_record && auth.spf_pass_domain.as_deref().is_some_and(aligned),
        from_domain: auth.from_domain.clone(),
    }
}

impl Assist {
    /// Runs the re-scan once per [`DETECTOR_VERSION`], marked done in the settings as
    /// `verification-rescan:<detector version>`: a release that leaves the detector alone reads nothing
    /// again (security-audit-0.30.0 R30-05).
    pub async fn run_verification_rescan(self, mut shutdown: watch::Receiver<bool>) {
        let key = format!("verification-rescan:{DETECTOR_VERSION}");
        match self.store().setting(&key).await {
            Ok(None) => {}
            Ok(Some(_)) => return,
            Err(err) => return tracing::warn!(%err, "reading whether codes were looked for again failed"),
        }
        let (since, mut after, mut found) = (now() - RESCAN_SECS, 0, 0);
        loop {
            if *shutdown.borrow() {
                return;
            }
            let batch = match self.store().verification_rescan_candidates(since, after, BATCH).await {
                Ok(batch) => batch,
                Err(err) => return tracing::warn!(%err, "looking for codes again failed"),
            };
            let Some((_, last)) = batch.last() else { break };
            after = last.id;
            for (account_id, record) in batch {
                found += usize::from(self.rescan_one(account_id, record).await);
            }
            tokio::select! {
                _ = tokio::time::sleep(PAUSE) => {}
                _ = shutdown.changed() => {}
            }
        }
        tracing::info!(found, "looked for codes again in the mail of the last day");
        if let Err(err) = self.store().set_setting(&key, &now().to_string()).await {
            tracing::warn!(%err, "marking the look for codes as done failed");
        }
    }

    /// Reads one mail again; whether a code was kept.
    async fn rescan_one(&self, account_id: i64, record: EmailRecord) -> bool {
        if record.size > MAX_MESSAGE as i64 {
            return false;
        }
        let Ok(raw) = self.store().blob(&record.blob).await else { return false };
        let hostname = self.hostname().to_owned();
        let (id, received_at) = (record.id, record.received_at);
        // A mail the sender wrote is parsed off the async runtime (security review SPAM-4).
        let found = tokio::task::spawn_blocking(move || {
            let auth = authenticity(&record, &raw, &hostname);
            let author = auth.from_domain.as_deref().and_then(registrable_domain);
            // Against the time it arrived: a code that had run out by then is still dropped (VC-08).
            verification::detect(&raw, &auth, received_at).zip(author)
        })
        .await
        .ok()
        .flatten();
        let Some((found, author)) = found else { return false };
        // Only a code still good now is kept, outside the sender's hourly share (R30-04).
        match self.store().record_rescanned_verification(account_id, id, &found, &author).await {
            Ok(kept) => !kept.is_empty(),
            Err(err) => {
                tracing::warn!(account = account_id, email = id, %err, "keeping a code found again failed");
                false
            }
        }
    }
}
