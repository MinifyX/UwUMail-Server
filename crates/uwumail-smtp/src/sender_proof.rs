//! Proof that a fetched address is its owner's, before anyone may send as it (security audit
//! 2026-11 M-01, docs/fetch.md).
//!
//! One successful fetch shows that a login opens a mailbox somewhere, not that the address typed in
//! next to it is the one that mailbox receives. So this server mails the address itself a secret
//! code and waits for that message to turn up in the fetched mailbox. Only then may the account send
//! as the address -- through its provider, and to people here, who would otherwise see a message
//! from an address the sender never showed was theirs.
//!
//! Nobody has to type the code anywhere: the fetcher finds it in the mail it brings. Someone whose
//! address was named by a stranger gets one short message a day at most and can ignore it.

use mail_builder::MessageBuilder;
use mail_builder::headers::date::Date;
use mail_builder::headers::text::Text;
use uwumail_store::{NewQueueRecipient, SenderProof};

use crate::config::Language;
use crate::{Smtp, dkim, random_id};

/// What the code is written behind, in a header of its own and once more in the text, so it is
/// found whichever of the two a provider keeps as it was.
const MARK: &str = "uwumail-proof-";
/// The header the code travels in.
const HEADER: &str = "X-UwUMail-Sender-Proof";
/// How long the proof message is tried before the queue gives up: the next one follows a day later.
const LIFETIME_SECS: i64 = 24 * 3600;

impl Smtp {
    /// Mails a fetched address its proof code, when one is due (see
    /// [`uwumail_store::Store::fetch_sender_proof_due`]). Called after a fetch run that went well.
    pub async fn ask_for_sender_proof(&self, fetch_id: i64) {
        let proof = match self.inner.store.fetch_sender_proof_due(fetch_id).await {
            Ok(Some(proof)) => proof,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(fetch_id, %err, "reading the sender proof of a fetched mailbox failed");
                return;
            }
        };
        // Only a message in the queue counts as sent; otherwise the next run tries again.
        if !self.mail_sender_proof(&proof).await
            && let Err(err) = self.inner.store.release_sender_proof(fetch_id, proof.address.clone()).await
        {
            tracing::warn!(fetch_id, %err, "giving back the sender proof of a fetched mailbox failed");
        }
    }

    /// Builds, signs and queues the proof message. Whether it is in the queue.
    async fn mail_sender_proof(&self, proof: &SenderProof) -> bool {
        let ctx = &self.inner;
        let Ok(Some(account)) = ctx.store.account_by_id(proof.account_id).await else { return false };
        let preferences = ctx.store.preferences(account.id).await.unwrap_or_default();
        let language =
            Language::preferred(preferences.get("language").and_then(|value| value.as_str()), ctx.tone().language);
        let brand = ctx.brand().name().to_owned();
        let (subject, body) = texts(language, &account.login, &proof.address, &ctx.hostname, &brand, &proof.code);
        let domain = account.login.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(&ctx.hostname).to_owned();
        let postmaster = format!("postmaster@{domain}");
        let message = MessageBuilder::new()
            .from((brand, postmaster))
            .to(proof.address.clone())
            .subject(subject)
            .date(Date::now())
            .message_id(format!("{}.sender-proof@{domain}", random_id()))
            .header("Auto-Submitted", Text::new("auto-generated"))
            .header(HEADER, Text::new(format!("{MARK}{}", proof.code)))
            .text_body(body)
            .write_to_vec();
        let Ok(message) = message else { return false };
        let signatures = match dkim::ensure_domain_keys(&ctx.store, &domain).await {
            Ok(keys) => dkim::sign(&message, &keys).unwrap_or_default(),
            Err(err) => {
                tracing::warn!(%err, %domain, "no DKIM keys for the sender proof");
                String::new()
            }
        };
        let mut signed = signatures.into_bytes();
        signed.extend_from_slice(&message);
        // The null sender: a proof for an address that does not exist bounces to nobody.
        let recipient = NewQueueRecipient { address: proof.address.clone(), notify_flags: 0, orcpt: None };
        match ctx.store.enqueue("", vec![recipient], &signed, None, None, LIFETIME_SECS).await {
            Ok(_) => {
                tracing::info!(login = %account.login, address = %proof.address, "mailed the proof code to a fetched address");
                true
            }
            Err(err) => {
                tracing::warn!(%err, address = %proof.address, "queueing the proof code for a fetched address failed");
                false
            }
        }
    }

    /// Takes the address as the account's when a fetched message carries the code that is out for
    /// this fetched mailbox. The message itself is delivered like any other.
    pub async fn take_sender_proof(&self, fetch_id: i64, raw: &[u8]) {
        let store = &self.inner.store;
        let Ok(Some(code)) = store.fetch_sender_proof(fetch_id).await else { return };
        if !carries(raw, &code) {
            return;
        }
        match store.confirm_fetch_sender(fetch_id, code).await {
            Ok(true) => tracing::info!(fetch_id, "a fetched address showed it is its owner's; it may be sent from now"),
            Ok(false) => {}
            Err(err) => tracing::warn!(fetch_id, %err, "noting the sender proof of a fetched mailbox failed"),
        }
    }
}

/// Whether `raw` contains the code behind its mark. The code is 32 hex digits, short enough that no
/// encoding breaks the line it is on.
fn carries(raw: &[u8], code: &str) -> bool {
    let needle = format!("{MARK}{code}");
    memchr::memmem::find(raw, needle.as_bytes()).is_some()
}

fn texts(language: Language, login: &str, address: &str, hostname: &str, brand: &str, code: &str) -> (String, String) {
    match language {
        Language::De => (
            format!("Bestätigung: {address} als Absender"),
            format!(
                "Hallo,\n\n{login} auf {hostname} möchte Mails mit dieser Adresse ({address}) als Absender \
                 verschicken und ruft dieses Postfach dafür ab.\n\n\
                 Wenn du das selbst eingerichtet hast, musst du nichts tun: Sobald diese Nachricht abgeholt \
                 wird, ist die Adresse bestätigt.\n\n\
                 Wenn nicht, kannst du diese Nachricht einfach ignorieren. Ohne sie kann niemand über {brand} \
                 als {address} senden.\n\n\
                 Bestätigungscode: {MARK}{code}\n\n{brand} ({hostname})\n"
            ),
        ),
        _ => (
            format!("Confirm {address} as a sender"),
            format!(
                "Hello,\n\n{login} at {hostname} wants to send mail from this address ({address}) and fetches \
                 this mailbox to do so.\n\n\
                 If you set this up yourself, there is nothing to do: once this message is fetched, the \
                 address is confirmed.\n\n\
                 If not, simply ignore this message. Without it nobody can send as {address} through \
                 {brand}.\n\n\
                 Confirmation code: {MARK}{code}\n\n{brand} ({hostname})\n"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_whole_code_behind_its_mark_counts() {
        let code = "0123456789abcdef0123456789abcdef";
        let raw = format!("X-UwUMail-Sender-Proof: {MARK}{code}\r\nSubject: x\r\n\r\nhi\r\n");
        assert!(carries(raw.as_bytes(), code));
        assert!(!carries(raw.as_bytes(), "0123456789abcdef0123456789abcdee"));
        assert!(!carries(format!("Subject: {code}\r\n\r\nhi\r\n").as_bytes(), code), "the bare code is not enough");
        // Both texts name the code with its mark, so it survives in the body too.
        for language in [Language::De, Language::En] {
            let (_, body) = texts(language, "mini@uwu.test", "mini@freemail.example", "mail.uwu.test", "UwUMail", code);
            assert!(carries(body.as_bytes(), code));
        }
    }
}
