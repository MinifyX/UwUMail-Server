//! The look back for parcels (docs/jmap-shipments.md, "Looking back"): when a person switches
//! shipments on, after the update that brought them, and on `Shipment/rescan`, their mail of the
//! last days is read oldest first, so each status moves on in order. Reading a mail twice changes
//! nothing, so a look back cut off by a restart simply goes on.

use std::time::Duration;

use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};
use serde_json::Value;
use tokio::sync::watch;
use uwumail_labels::{Carrier, DateWindow, Day, MAX_NUMBERS, Mail, ShipmentFacts, ShipmentStatus};
use uwumail_store::{Account, EmailRecord, ShipmentRescanJob};

use crate::features::{authentication, delivered_headers};
use crate::mail::MailText;
use crate::{Assist, AssistError, Result, llm, prompts};

/// Mails read per batch, and the pause between batches, so a busy server does not notice it.
const BATCH: i64 = 50;
const PAUSE: Duration = Duration::from_millis(200);
/// How often it looks for new work when there is none.
const IDLE: Duration = Duration::from_secs(15);
/// Bytes of a message read, as at delivery.
const MAX_PARSE_BYTES: usize = 4 * 1024 * 1024;
/// Tokens a look at one shipping mail is expected to answer with.
const TYPICAL_SHIPMENT_TOKENS: i64 = 150;
/// How sure the rules and the model must be for a shipment to count.
const SURE: f32 = 0.8;

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
        // Switched off on the server: the looks back wait until it is on again.
        if !self.store().shipments_allowed() {
            return false;
        }
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

    /// Asks the model about a shipping mail the rules could not read in full (docs/jmap-shipments.md,
    /// "The model"). Called by the label worker, so only for people with AI labels on; the ticket
    /// holds it to the person's permissions and the provider's limits. Never fails the labeling.
    pub(crate) async fn shipment_by_model(
        &self,
        account: &Account,
        record: &EmailRecord,
        mail: &Mail,
        text: &MailText,
    ) {
        if let Err(err) = self.try_shipment_by_model(account, record, mail, text).await
            && !matches!(err, AssistError::Unavailable(_))
        {
            tracing::info!(account = account.id, email = record.id, %err, "the model read no parcel");
        }
    }

    async fn try_shipment_by_model(
        &self,
        account: &Account,
        record: &EmailRecord,
        mail: &Mail,
        text: &MailText,
    ) -> Result<()> {
        if !self.store().shipments_enabled(account.id).await? {
            return Ok(());
        }
        let owned = mail.clone();
        let ruled = tokio::task::spawn_blocking(move || uwumail_labels::shipment(&owned))
            .await
            .map_err(|err| AssistError::Store(uwumail_store::StoreError::Internal(err.to_string())))?;
        // What the rules read in full was noted at delivery already.
        if ruled.is_some_and(|facts| facts.confidence >= SURE && !facts.numbers.is_empty() && facts.status.is_some()) {
            return Ok(());
        }
        let ticket = self.prepare(account, "autoLabels").await?.expecting(TYPICAL_SHIPMENT_TOKENS);
        let (completion, _) = self.send(ticket, &prompts::shipment(text), None).await?;
        let Some(facts) = llm::json_answer(&completion.text).and_then(|answer| from_model(&answer, mail)) else {
            return Ok(());
        };
        self.store()
            .record_shipment_from_model(
                account.id,
                record.id,
                record.received_at,
                mail.subject.clone(),
                facts,
                mail.from_trusted,
            )
            .await?;
        Ok(())
    }

    /// Reads one mail; whether it told of a parcel the account did not know of yet.
    async fn shipment_one(&self, account_id: i64, record: EmailRecord) -> bool {
        let Ok(raw) = self.store().blob(&record.blob).await else { return false };
        // A mail the sender wrote is parsed off the async runtime (security review SPAM-4).
        let hostname = self.hostname().to_owned();
        let found = tokio::task::spawn_blocking(move || {
            let mut mail = Mail::parse(&raw[..raw.len().min(MAX_PARSE_BYTES)]);
            // As at delivery: only our own Authentication-Results, aligned with the From domain.
            let headers = MailText::read(&record, &raw, 0).headers;
            let auth = authentication(delivered_headers(&record, &headers), Some(&hostname), &record.from);
            mail.from_trusted = crate::spam::authentic(&auth);
            let authenticated = mail.from_trusted;
            uwumail_labels::shipment(&mail)
                .filter(|facts| facts.confidence >= 0.8)
                .map(|facts| (facts, mail.subject, authenticated, record))
        })
        .await
        .ok()
        .flatten();
        let Some((facts, subject, authenticated, record)) = found else { return false };
        match self
            .store()
            .record_shipment_mail(account_id, record.id, record.received_at, subject, facts, authenticated)
            .await
        {
            Ok(changed) => changed,
            Err(err) => {
                tracing::warn!(account = account_id, email = record.id, %err, "noting a parcel failed");
                false
            }
        }
    }
}

/// The shipment in the model's answer, held to the mail: a number or order reference counts only
/// when the mail has it word for word, so a made-up one never becomes a parcel.
fn from_model(answer: &Value, mail: &Mail) -> Option<ShipmentFacts> {
    let confidence = answer["confidence"].as_f64()? as f32;
    if !(SURE..=1.0).contains(&confidence) {
        return None;
    }
    let haystack = format!("{}\n{}", mail.subject, mail.text).to_uppercase();
    let literal = |value: &str| {
        let value = value.trim().to_uppercase();
        let fits = (6..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric());
        (fits && haystack.contains(&value)).then_some(value)
    };
    let mut numbers: Vec<String> = Vec::new();
    for number in answer["numbers"].as_array()?.iter().filter_map(Value::as_str).filter_map(literal) {
        if numbers.len() < MAX_NUMBERS && !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    let order_ref = answer["orderRef"].as_str().and_then(|text| {
        let text = text.trim();
        (!text.is_empty() && text.len() <= 64 && haystack.contains(&text.to_uppercase())).then(|| text.to_string())
    });
    if numbers.is_empty() && order_ref.is_none() {
        return None;
    }
    let day = answer["expectedDate"].as_str().and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());
    let time = |key: &str| {
        let time = NaiveTime::parse_from_str(answer[key].as_str()?, "%H:%M").ok()?;
        Some((time.hour(), time.minute()))
    };
    let expected = day.map(|day| DateWindow {
        day: Day::On { year: Some(day.year()), month: day.month(), day: day.day() },
        from: time("expectedFrom"),
        to: time("expectedTo"),
    });
    // A shop name only when the mail names it, so a mail cannot make the model vouch for another shop.
    let named = format!("{}\n{}\n{}\n{}", mail.from_name, mail.from, mail.subject, mail.text).to_lowercase();
    let merchant = answer["merchant"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 100 && named.contains(&name.to_lowercase()));
    Some(ShipmentFacts {
        carrier: answer["carrier"].as_str().and_then(Carrier::from_id),
        numbers,
        tracking_url: None,
        shop_tracking_url: None,
        status: answer["status"].as_str().and_then(ShipmentStatus::from_id),
        expected,
        merchant: merchant.map(str::to_string),
        order_ref,
        pickup: None,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_numbers_the_mail_has_count() {
        let mail = Mail {
            subject: "Ihre Bestellung 4711-0815".into(),
            text: "Sendungsnummer: ZZ1234567890 – Lieferung am 2026-11-20 zwischen 10:00 und 12:00. Dein Laden".into(),
            ..Mail::default()
        };
        let answer = json!({
            "carrier": "hermes", "numbers": ["zz1234567890", "ZZ1234567890", "QQ9999999999", "ZZ12"],
            "status": "shipped", "expectedDate": "2026-11-20", "expectedFrom": "10:00", "expectedTo": "12:00",
            "merchant": "Laden", "orderRef": "4711-0815", "confidence": 0.9
        });
        let facts = from_model(&answer, &mail).unwrap();
        assert_eq!(facts.numbers, ["ZZ1234567890"]);
        assert_eq!(facts.order_ref.as_deref(), Some("4711-0815"));
        assert_eq!(facts.status, Some(ShipmentStatus::Shipped));
        assert_eq!(facts.carrier, Some(Carrier::Hermes));
        assert_eq!(facts.expected.unwrap().from, Some((10, 0)));
        assert_eq!(facts.merchant.as_deref(), Some("Laden"));
        let elsewhere = json!({ "numbers": ["ZZ1234567890"], "merchant": "Großbank", "confidence": 0.9 });
        assert_eq!(from_model(&elsewhere, &mail).unwrap().merchant, None, "a shop the mail does not name");

        let made_up = json!({ "carrier": null, "numbers": ["QQ9999999999"], "status": null, "expectedDate": null,
            "expectedFrom": null, "expectedTo": null, "merchant": null, "orderRef": "A-1", "confidence": 0.95 });
        assert!(from_model(&made_up, &mail).is_none());
        let unsure = json!({ "numbers": ["ZZ1234567890"], "orderRef": null, "confidence": 0.5 });
        assert!(from_model(&unsure, &mail).is_none());
    }
}
