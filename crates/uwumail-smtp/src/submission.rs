//! Sending mail for one of our own accounts, shared by SMTP submission and JMAP EmailSubmission.

use mail_builder::headers::date::Date;
use mail_parser::MessageParser;
use uwumail_store::{Account, IngestRequest, MailboxRole, MailboxTarget, NewQueueRecipient, StoreError};

use crate::dsn::{self, FailedRecipient};
use crate::{Smtp, dkim, forward, headers, random_id, vacation};

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
        let ctx = &self.inner;
        let Submission { account, mail_from, recipients, raw, env_id, trace } = submission;
        if recipients.is_empty() {
            return Err(SubmitError::NoRecipients);
        }
        if mail_from.is_empty() || !ctx.store.account_owns_address(account.id, &mail_from).await.unwrap_or(false) {
            return Err(SubmitError::ForbiddenFrom(mail_from));
        }
        let raw = headers::normalize_line_endings(&raw);
        let (from, sender) = claimed_addresses(&raw)?;
        // Every address the message shows as its author or submitter must belong to this account,
        // so a login cannot send mail that displays as someone else.
        for address in from.iter().chain(&sender) {
            if !ctx.store.account_owns_address(account.id, address).await.unwrap_or(false) {
                return Err(SubmitError::ForbiddenFrom(address.clone()));
            }
        }
        let from_domain = from[0].rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()).unwrap_or_default();
        let id = random_id();

        let mut message = String::new().into_bytes();
        if headers::first_value(&raw, "Date").is_none() {
            message.extend_from_slice(format!("Date: {}\r\n", Date::now().to_rfc822()).as_bytes());
        }
        if headers::first_value(&raw, "Message-ID").is_none() {
            message.extend_from_slice(format!("Message-ID: <{}@{from_domain}>\r\n", random_id()).as_bytes());
        }
        message.extend_from_slice(&strip_bcc(&raw));

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
        for recipient in &recipients {
            let Ok((local, domain)) = uwumail_store::normalize_address(&recipient.address) else {
                return Err(SubmitError::InvalidRecipient(recipient.address.clone()));
            };
            let address = format!("{local}@{domain}");
            match ctx.store.resolve_recipient(&address).await.ok().flatten() {
                Some(account_id) => {
                    let plan = forward::plan(ctx, account_id).await;
                    if !plan.targets.is_empty()
                        && let Ok(Some(target)) = ctx.store.account_by_id(account_id).await
                    {
                        let forwarder = forward::Forwarder { name: &target.login, account_id: Some(target.id) };
                        forward::send(ctx, forwarder, &address, &mail_from, &signed, &plan.targets).await;
                    }
                    if !plan.keep_copy {
                        local_deliveries += 1;
                        continue;
                    }
                    let request = IngestRequest {
                        account_id,
                        raw: signed.clone(),
                        mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                        keywords: vec![],
                        received_at: None,
                    };
                    match ctx.store.ingest(request).await {
                        Ok(_) => {
                            local_deliveries += 1;
                            vacation::maybe_reply(ctx, account_id, &mail_from, &signed).await;
                        }
                        Err(err) => failed.push(FailedRecipient {
                            address,
                            error: match err {
                                StoreError::QuotaExceeded => "552 5.2.2 Mailbox is full".into(),
                                other => format!("451 4.3.0 {other}"),
                            },
                        }),
                    }
                }
                None if ctx.store.is_local_domain(&domain).await.unwrap_or(false) => {
                    match ctx.store.forward_address_targets(&address).await.ok().flatten() {
                        Some(targets) => {
                            let forwarder = forward::Forwarder { name: &address, account_id: Some(account.id) };
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
        tracing::info!(%id, login = %account.login, local = local_deliveries, remote = remote_recipients, "submitted message");
        Ok(Submitted { id, queue_message_id, local_deliveries, remote_recipients })
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
