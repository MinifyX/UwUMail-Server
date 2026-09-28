//! Sending mail for one of our own accounts, shared by SMTP submission and JMAP EmailSubmission.

use mail_builder::headers::date::Date;
use mail_parser::MessageParser;
use uwumail_store::{Account, IngestRequest, MailboxRole, MailboxTarget, MaskedState, NewQueueRecipient, StoreError};

use crate::dsn::{self, FailedRecipient};
use crate::{Smtp, clamav, dkim, forward, headers, random_id, vacation};

pub struct Submission {
    pub account: Account,
    /// Envelope sender.
    pub mail_from: String,
    pub recipients: Vec<SubmissionRecipient>,
    pub raw: Vec<u8>,
    pub env_id: Option<String>,
    /// Extra trace header (with CRLF) to put above the message, e.g. from the SMTP session.
    pub trace: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SubmissionRecipient {
    pub address: String,
    pub notify_flags: u64,
    pub orcpt: Option<String>,
}

impl SubmissionRecipient {
    pub fn new(address: impl Into<String>) -> SubmissionRecipient {
        SubmissionRecipient { address: address.into(), notify_flags: 0, orcpt: None }
    }
}

#[derive(Debug, Clone)]
pub struct Submitted {
    pub id: String,
    pub queue_message_id: Option<i64>,
    pub local_deliveries: usize,
    pub remote_recipients: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("the message has no From header")]
    NoFrom,
    #[error("you are not allowed to send as <{0}>")]
    ForbiddenFrom(String),
    #[error("a message may have only one Sender")]
    AmbiguousSender,
    #[error("there are no recipients")]
    NoRecipients,
    #[error("<{0}> is not a valid address")]
    InvalidRecipient(String),
    #[error("no recipient could take the message")]
    NobodyAccepted,
    #[error("sending through this server is switched off for this account")]
    SendingOff,
    #[error("this account is disabled or in the trash")]
    AccountLocked,
    #[error("too many recipients")]
    TooManyRecipients,
    #[error("the message is larger than this server accepts")]
    TooLarge,
    #[error("the message contains {0}")]
    Virus(String),
    #[error("the message could not be queued: {0}")]
    Queue(StoreError),
}

/// The author (`From`), submitter (`Sender`) and resender (`Resent-From`/`Resent-Sender`) addresses
/// a message claims, which all have to belong to the sending account. RFC 5322 §3.6 allows exactly
/// one `From` and at most one `Sender`; a hidden second `From`, a forged `Sender`, or a `Resent-*`
/// header naming another person (which some mail clients display) must never let a login send mail
/// that shows as someone else. So more than one `From` or `Sender` is refused, and every address of
/// each identity header — all occurrences, whatever the order — is returned for the ownership check.
fn claimed_addresses(raw: &[u8]) -> Result<(Vec<String>, Vec<String>), SubmitError> {
    if headers::count(raw, "From") > 1 {
        return Err(SubmitError::NoFrom);
    }
    if headers::count(raw, "Sender") > 1 {
        return Err(SubmitError::AmbiguousSender);
    }
    let parsed = MessageParser::new().parse_headers(raw);
    let mut from = Vec::new();
    let mut claimed = Vec::new();
    if let Some(message) = parsed.as_ref() {
        for header in message.headers() {
            let name = header.name();
            let bucket = if name.eq_ignore_ascii_case("From") {
                &mut from
            } else if ["Sender", "Resent-From", "Resent-Sender"].iter().any(|h| name.eq_ignore_ascii_case(h)) {
                &mut claimed
            } else {
                continue;
            };
            if let Some(address) = header.value().as_address() {
                bucket.extend(address.iter().filter_map(|a| a.address.as_deref().map(str::to_owned)));
            }
        }
    }
    if from.is_empty() {
        return Err(SubmitError::NoFrom);
    }
    Ok((from, claimed))
}

fn stored_error(err: StoreError) -> String {
    match err {
        StoreError::QuotaExceeded => "552 5.2.2 Mailbox is full".into(),
        other => format!("451 4.3.0 {other}"),
    }
}

/// Removes Bcc headers: blind copies must not show up for anyone.
fn strip_bcc(raw: &[u8]) -> Vec<u8> {
    let (fields, _) = headers::split(raw);
    let mut out = Vec::with_capacity(raw.len());
    let mut pos = 0;
    for field in fields.iter().filter(|h| h.name.eq_ignore_ascii_case("Bcc")) {
        let start = field.raw.as_ptr() as usize - raw.as_ptr() as usize;
        out.extend_from_slice(&raw[pos..start]);
        pos = start + field.raw.len();
    }
    out.extend_from_slice(&raw[pos..]);
    out
}

impl Smtp {
    /// Checks the sender, completes and signs the message, delivers to local recipients and
    /// queues the rest. Failed local deliveries are bounced to the sender.
    pub async fn submit(&self, submission: Submission) -> Result<Submitted, SubmitError> {
        self.check_submission(&submission).await?;
        let Submission { account, mail_from, recipients, raw, env_id, trace } = submission;
        let raw = headers::normalize_line_endings(&raw);
        let (from, _) = claimed_addresses(&raw)?;
        // Our own people send viruses too, mostly without knowing. Turning one away here keeps it
        // out of other people's mailboxes and our name off their scanner's report.
        if let clamav::Checked::Found(name) = clamav::check(&self.inner.live().spam.antivirus, &raw).await {
            tracing::info!(login = %account.login, virus = %name, "refused to send, the virus scanner found something");
            return Err(SubmitError::Virus(name));
        }
        self.submit_checked(account, mail_from, recipients, raw, env_id, trace, from).await
    }

    /// The checks [`Smtp::submit`] makes before it touches the message: may this account send at
    /// all, as this sender, to this many recipients, this much? For sending that is held back
    /// (JMAP's undo window and send later), so a message that could never go is refused at once and
    /// not only when its time comes. `submit` checks everything again.
    pub async fn check_submission(&self, submission: &Submission) -> Result<(), SubmitError> {
        let ctx = &self.inner;
        let Submission { account, mail_from, recipients, raw, .. } = submission;
        if recipients.is_empty() {
            return Err(SubmitError::NoRecipients);
        }
        // The account as it is now, not as it was when the session or the held message began: a
        // disabled or trashed account sends nothing more, from any door, and neither does mail it
        // held back for later (security audit 0.16.0 PROTOCOLS-10).
        let current = ctx.store.account_by_id(account.id).await.map_err(SubmitError::Queue)?;
        let Some(current) = current.filter(|current| current.can_log_in()) else {
            return Err(SubmitError::AccountLocked);
        };
        // The SMTP switch governs sending through this server from every door, not only ports
        // 587/465: the JMAP path (webmail and any client) calls submit directly, so it is checked
        // here (security-audit-0.5.2 S-11).
        if !current.may_use("smtp") {
            return Err(SubmitError::SendingOff);
        }
        // The same limits the SMTP port enforces at RCPT and DATA, so a policy an admin sets holds
        // on both doors, not only on 587/465 (security-audit-0.5.2 S-30).
        let live = ctx.live();
        if recipients.len() > live.smtp.max_recipients {
            return Err(SubmitError::TooManyRecipients);
        }
        if raw.len() > live.smtp.max_message_size {
            return Err(SubmitError::TooLarge);
        }
        if mail_from.is_empty() || !ctx.store.account_owns_address(account.id, mail_from).await.unwrap_or(false) {
            return Err(SubmitError::ForbiddenFrom(mail_from.clone()));
        }
        let raw = headers::normalize_line_endings(raw);
        let (from, sender) = claimed_addresses(&raw)?;
        // Every address the message shows as its author or submitter must belong to this account,
        // so a login cannot send mail that displays as someone else.
        for address in from.iter().chain(&sender) {
            if !ctx.store.account_owns_address(account.id, address).await.unwrap_or(false) {
                return Err(SubmitError::ForbiddenFrom(address.clone()));
            }
        }
        for recipient in recipients {
            if uwumail_store::normalize_address(&recipient.address).is_err() {
                return Err(SubmitError::InvalidRecipient(recipient.address.clone()));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_checked(
        &self,
        account: Account,
        mail_from: String,
        recipients: Vec<SubmissionRecipient>,
        raw: Vec<u8>,
        env_id: Option<String>,
        trace: Option<String>,
        from: Vec<String>,
    ) -> Result<Submitted, SubmitError> {
        let ctx = &self.inner;
        let from_domain = from[0].rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()).unwrap_or_default();
        let id = random_id();

        let mut added = String::new().into_bytes();
        if headers::first_value(&raw, "Date").is_none() {
            added.extend_from_slice(format!("Date: {}\r\n", Date::now().to_rfc822()).as_bytes());
        }
        if headers::first_value(&raw, "Message-ID").is_none() {
            added.extend_from_slice(format!("Message-ID: <{}@{from_domain}>\r\n", random_id()).as_bytes());
        }
        let mut message = added.clone();
        // Signed as it will be sent: a lone CR or LF becomes CRLF on the way out (SMTP-9), and the
        // signature has to hold for that.
        message.extend_from_slice(&headers::crlf_only(&strip_bcc(&raw)));

        let signatures = match dkim::ensure_domain_keys(&ctx.store, &from_domain).await {
            Ok(keys) => dkim::sign(&message, &keys).unwrap_or_else(|err| {
                tracing::error!(%err, domain = %from_domain, "DKIM signing failed");
                String::new()
            }),
            Err(err) => {
                tracing::error!(%err, domain = %from_domain, "no DKIM keys");
                String::new()
            }
        };
        let mut signed = signatures.into_bytes();
        if let Some(trace) = &trace {
            signed.extend_from_slice(trace.as_bytes());
        }
        signed.extend_from_slice(&message);

        let mut failed = Vec::new();
        let mut local_deliveries = 0;
        let mut remote = Vec::new();
        // People here get a message once, however many of their addresses and groups it names.
        let mut reached: Vec<i64> = Vec::new();
        let delivered_to = headers::values(&signed, "Delivered-To");
        for recipient in &recipients {
            let Ok((local, domain)) = uwumail_store::normalize_address(&recipient.address) else {
                return Err(SubmitError::InvalidRecipient(recipient.address.clone()));
            };
            let address = format!("{local}@{domain}");
            // A group: the sender has to be allowed to write to it, and then every member gets it.
            if let Some(group) = ctx.store.group_delivery(&address).await.ok().flatten() {
                if !ctx.store.group_accepts(&group, &mail_from, Some(account.id)).await.unwrap_or(false) {
                    failed.push(FailedRecipient {
                        address: address.clone(),
                        error: format!("550 5.7.1 <{address}>: You may not write to this group"),
                    });
                    continue;
                }
                if delivered_to.iter().any(|seen| seen.eq_ignore_ascii_case(&group.address)) {
                    tracing::warn!(%id, group = %group.address, "not delivering a message to a group it went through before");
                    local_deliveries += 1;
                    continue;
                }
                let mut got_it = false;
                let mut failure = None;
                for member in &group.members {
                    let Some(target) = ctx.store.delivery_target(*member).await.ok().flatten() else { continue };
                    if reached.contains(&target) {
                        got_it = true;
                        continue;
                    }
                    reached.push(target);
                    match self.deliver_locally(target, &address, &mail_from, &signed, &from[0], false).await {
                        Ok(()) => got_it = true,
                        Err(error) => failure = Some(error),
                    }
                }
                if got_it {
                    local_deliveries += 1;
                } else {
                    let error = failure.unwrap_or_else(|| "550 5.1.1 This address does not take mail".into());
                    failed.push(FailedRecipient { address, error });
                }
                continue;
            }
            // The recipient is looked up again here, so the mailbox-less service has to be
            // asked about again too: mail for it belongs to the address it hands its mail to.
            let resolved = match ctx.store.resolve_recipient(&address).await.ok().flatten() {
                Some(account_id) => match ctx.store.delivery_target(account_id).await.ok().flatten() {
                    Some(target) => Some(target),
                    None => {
                        failed.push(FailedRecipient {
                            address: address.clone(),
                            error: "550 5.1.1 This address does not take mail".into(),
                        });
                        continue;
                    }
                },
                None => None,
            };
            match resolved {
                Some(account_id) => {
                    if reached.contains(&account_id) {
                        local_deliveries += 1;
                        continue;
                    }
                    reached.push(account_id);
                    // A disabled masked address takes the message into the Trash, as from anyone.
                    let masked = ctx.store.masked_delivery(&address).await.ok().flatten();
                    if let Some(masked) = masked
                        && let Err(err) = ctx.store.note_masked_message(masked.id).await
                    {
                        tracing::warn!(%id, %err, "noting mail for a masked address failed");
                    }
                    let to_trash = masked.is_some_and(|masked| masked.state == MaskedState::Disabled);
                    match self.deliver_locally(account_id, &address, &mail_from, &signed, &from[0], to_trash).await {
                        Ok(()) => local_deliveries += 1,
                        Err(error) => failed.push(FailedRecipient { address, error }),
                    }
                }
                None if ctx.store.is_local_domain(&domain).await.unwrap_or(false) => {
                    match ctx.store.forward_address_targets(&address).await.ok().flatten() {
                        Some(targets) => {
                            let forwarder = forward::Forwarder {
                                name: &address,
                                account_id: Some(account.id),
                                proof: forward::Proof::PROVEN,
                            };
                            forward::send(ctx, forwarder, &address, &mail_from, &signed, &targets).await;
                            local_deliveries += 1;
                        }
                        None => failed.push(FailedRecipient {
                            address: address.clone(),
                            error: format!("550 5.1.1 <{address}>: No such mailbox here"),
                        }),
                    }
                }
                None => remote.push(NewQueueRecipient {
                    address,
                    notify_flags: recipient.notify_flags,
                    orcpt: recipient.orcpt.clone(),
                }),
            }
        }

        let remote_recipients = remote.len();
        let mut queue_message_id = None;
        if !remote.is_empty() {
            let lifetime = ctx.live().delivery.max_lifetime_hours as i64 * 3600;
            let queued = ctx
                .store
                .enqueue(&mail_from, remote, &signed, Some(account.id), env_id, lifetime)
                .await
                .map_err(SubmitError::Queue)?;
            queue_message_id = Some(queued);
        }
        if local_deliveries == 0 && remote_recipients == 0 && failed.len() == recipients.len() {
            return Err(SubmitError::NobodyAccepted);
        }
        if !failed.is_empty() {
            dsn::bounce(ctx, &mail_from, &signed, &failed).await;
        }
        // Sent as a shared mailbox: its own Sent folder keeps a copy too, so everyone who uses it
        // sees what was answered. The sender's own copy is up to their mail app, as always.
        if let Some(shared) = ctx.store.shared_mailbox_sending_as(account.id, &from[0]).await.ok().flatten() {
            let mut copy = added;
            copy.extend_from_slice(&raw);
            let request = IngestRequest {
                account_id: shared,
                raw: copy,
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Sent)],
                keywords: vec!["$seen".into()],
                received_at: None,
            };
            if let Err(err) = ctx.store.ingest(request).await {
                tracing::warn!(%id, %err, shared, "keeping a copy in the shared mailbox's Sent folder failed");
            }
        }
        tracing::info!(%id, login = %account.login, local = local_deliveries, remote = remote_recipients, "submitted message");
        ctx.store.stats().count(uwumail_store::Stat::Submitted);
        Ok(Submitted { id, queue_message_id, local_deliveries, remote_recipients })
    }

    /// Delivers a submitted message to someone here: their forwarding, then their Inbox (or the
    /// Trash, for a disabled masked address), a vacation reply and calendar invitations. The error
    /// is the answer for a bounce.
    async fn deliver_locally(
        &self,
        account_id: i64,
        address: &str,
        mail_from: &str,
        signed: &[u8],
        author: &str,
        to_trash: bool,
    ) -> Result<(), String> {
        let ctx = &self.inner;
        if to_trash {
            let request = IngestRequest {
                account_id,
                raw: signed.to_vec(),
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Trash)],
                keywords: vec!["$seen".into()],
                received_at: None,
            };
            return ctx.store.ingest(request).await.map(|_| ()).map_err(stored_error);
        }
        let plan = forward::plan(ctx, account_id).await;
        if !plan.targets.is_empty()
            && let Ok(Some(target)) = ctx.store.account_by_id(account_id).await
        {
            let forwarder =
                forward::Forwarder { name: &target.login, account_id: Some(target.id), proof: forward::Proof::PROVEN };
            forward::send(ctx, forwarder, address, mail_from, signed, &plan.targets).await;
        }
        if !plan.keep_copy {
            return Ok(());
        }
        let request = IngestRequest {
            account_id,
            raw: signed.to_vec(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        };
        ctx.store.ingest(request).await.map_err(stored_error)?;
        // The sender is an authenticated local account, so it is verified.
        vacation::maybe_reply(ctx, account_id, mail_from, true, signed).await;
        // Calendar apps that send invitations themselves reach people here too; the From address
        // was checked to be the sender's own.
        let sender = crate::scheduling::Sender { verified_from: Some(author), local: true };
        crate::scheduling::incoming(ctx, account_id, signed, sender).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bcc_headers() {
        let raw = b"From: a@x\r\nBcc: secret@x,\r\n other@x\r\nSubject: hi\r\n\r\nBcc: stays in the body\r\n";
        assert_eq!(strip_bcc(raw), b"From: a@x\r\nSubject: hi\r\n\r\nBcc: stays in the body\r\n");
    }

    fn err(raw: &[u8]) -> Option<SubmitError> {
        claimed_addresses(raw).err()
    }

    #[test]
    fn every_from_and_sender_address_is_returned_for_checking() {
        let (from, sender) =
            claimed_addresses(b"From: Mini <mini@a.test>\r\nTo: x@y\r\nSubject: hi\r\n\r\nhi\r\n").unwrap();
        assert_eq!(from, ["mini@a.test"]);
        assert!(sender.is_empty());

        // A Sender header naming someone else is returned, so the caller refuses it.
        let (from, sender) =
            claimed_addresses(b"From: mini@a.test\r\nSender: ami@a.test\r\nSubject: hi\r\n\r\nhi\r\n").unwrap();
        assert_eq!(from, ["mini@a.test"]);
        assert_eq!(sender, ["ami@a.test"]);

        // Resent-From and Resent-Sender count as claimed identities too, whatever their order.
        let (from, claimed) = claimed_addresses(
            b"Resent-Sender: rs@a.test\r\nFrom: mini@a.test\r\nResent-From: rf@a.test\r\nSubject: hi\r\n\r\nhi\r\n",
        )
        .unwrap();
        assert_eq!(from, ["mini@a.test"]);
        assert!(claimed.contains(&"rf@a.test".to_string()) && claimed.contains(&"rs@a.test".to_string()));
    }

    #[test]
    fn a_second_sender_is_refused() {
        assert!(matches!(
            err(b"From: mini@a.test\r\nSender: mini@a.test\r\nSender: ami@a.test\r\nSubject: hi\r\n\r\nhi\r\n"),
            Some(SubmitError::AmbiguousSender)
        ));
    }

    #[test]
    fn a_hidden_second_from_header_is_refused() {
        // Only the last From is parsed, but the first is delivered and shown: refuse the message.
        assert!(matches!(
            err(b"From: ami@a.test\r\nFrom: mini@a.test\r\nSubject: hi\r\n\r\nhi\r\n"),
            Some(SubmitError::NoFrom)
        ));
        assert!(matches!(
            err(b"From: mini@a.test\r\nFrom: ami@a.test\r\nSubject: hi\r\n\r\nhi\r\n"),
            Some(SubmitError::NoFrom)
        ));
        assert!(matches!(err(b"Subject: no from\r\n\r\nhi\r\n"), Some(SubmitError::NoFrom)));
    }
}
