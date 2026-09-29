//! Passing mail on to the addresses a person or a forwarding address forwards to.

use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewQueueRecipient};

use crate::{Context, headers, srs};

/// What happens to a message for one person: whether it stays in the mailbox and where else it goes.
pub(crate) struct Plan {
    pub keep_copy: bool,
    /// Addresses with `Some(account id)` for people on this server.
    pub targets: Vec<(String, Option<i64>)>,
}

pub(crate) async fn plan(ctx: &Context, account_id: i64) -> Plan {
    match ctx.store.active_forwarding(account_id).await {
        Ok(active) => {
            let external_allowed = ctx.live().smtp.allow_external_forwarding;
            let targets: Vec<_> =
                active.targets.into_iter().filter(|(_, local)| local.is_some() || external_allowed).collect();
            // Without anywhere else to go, the mail always stays.
            Plan { keep_copy: active.keep_copy || targets.is_empty(), targets }
        }
        Err(err) => {
            tracing::error!(%err, account_id, "reading the forwarding failed, keeping the mail");
            Plan { keep_copy: true, targets: Vec::new() }
        }
    }
}

/// Whose mail is forwarded: a person's, or a forwarding address's, which has no account.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Forwarder<'a> {
    /// The person's login or the forwarding address, for the log and the SRS domain.
    pub name: &'a str,
    pub account_id: Option<i64>,
    /// What is known about who sent the message.
    pub proof: Proof<'a>,
}

/// What the checks at the door proved about who sent a message. Mail submitted here, and mail
/// nothing could be checked about (sender checks switched off), counts as proven.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Proof<'a> {
    /// SPF or DKIM passed for the envelope sender.
    pub envelope: bool,
    /// The From domain, when DMARC did not pass for it.
    pub unproven_from: Option<&'a str>,
}

impl<'a> Proof<'a> {
    pub const PROVEN: Proof<'static> = Proof { envelope: true, unproven_from: None };

    pub fn of(verdict: Option<&'a crate::checks::Verdict>) -> Self {
        match verdict {
            Some(verdict) => Proof {
                envelope: verdict.sender_verified,
                unproven_from: if verdict.dmarc_passed { None } else { verdict.from_domain.as_deref() },
            },
            None => Proof::PROVEN,
        }
    }
}

/// Whether two domains count as one for DMARC's relaxed alignment: the same registrable domain.
fn aligned(a: &str, b: &str) -> bool {
    let organizational = |domain: &str| {
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        psl::domain_str(&domain).map(str::to_owned).unwrap_or(domain)
    };
    organizational(a) == organizational(b)
}

/// Sends a received message on. `recipient` is the address it arrived for; it goes into a
/// Delivered-To header, which also stops mail going round in circles between forwards.
///
/// Returns whether the message reached at least one target (stored here or queued for elsewhere),
/// so a caller that keeps no copy of its own can keep one after all when it went nowhere.
pub(crate) async fn send(
    ctx: &Context,
    forwarder: Forwarder<'_>,
    recipient: &str,
    envelope_from: &str,
    message: &[u8],
    targets: &[(String, Option<i64>)],
) -> bool {
    if targets.is_empty() {
        return false;
    }
    let (fields, _) = headers::split(message);
    let looped = fields
        .iter()
        .any(|field| field.name.eq_ignore_ascii_case("Delivered-To") && field.value().eq_ignore_ascii_case(recipient));
    if looped {
        tracing::warn!(forwarder = %forwarder.name, %recipient, "not forwarding a message that was here before");
        return false;
    }
    let mut forwarded = format!("Delivered-To: {recipient}\r\n").into_bytes();
    forwarded.extend_from_slice(message);

    let mut remote = Vec::new();
    let mut reached = false;
    for (address, local) in targets {
        match local {
            // People on this server get it directly; their own forwarding does not apply again.
            // A target without a mailbox of its own passes it on once more, or takes nothing.
            Some(target_account) => {
                let Some(target_account) = ctx.store.delivery_target(*target_account).await.ok().flatten() else {
                    tracing::warn!(forwarder = %forwarder.name, to = %address, "the target takes no mail");
                    continue;
                };
                let request = IngestRequest {
                    account_id: target_account,
                    raw: forwarded.clone(),
                    mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                    keywords: vec![],
                    received_at: None,
                };
                match ctx.store.ingest(request).await {
                    Ok(_) => reached = true,
                    Err(err) => {
                        tracing::warn!(%err, forwarder = %forwarder.name, to = %address, "forwarding to a local mailbox failed");
                    }
                }
            }
            None => remote.push(NewQueueRecipient { address: address.clone(), notify_flags: 0, orcpt: None }),
        }
    }

    if !remote.is_empty() {
        let our_domain = forwarder.name.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(&ctx.hostname);
        // Sent on from here, the message passes SPF for the return path's domain. A From that did
        // not pass DMARC and lines up with that domain would pass it at the next server: a forgery
        // of our own domain made good by forwarding it (security-audit-0.16.0 SMTP-8). It stays
        // here instead.
        if let Some(from) = forwarder.proof.unproven_from
            && aligned(from, our_domain)
        {
            tracing::warn!(
                forwarder = %forwarder.name,
                %from,
                "not forwarding to other servers: the From did not pass DMARC and would look sent by us"
            );
            return reached;
        }
        let sender_domain = envelope_from.rsplit_once('@').map(|(_, domain)| domain).unwrap_or_default();
        // Our own sender keeps its address only when the checks proved it; otherwise it is
        // rewritten like anybody else's.
        let own_sender = forwarder.proof.envelope && ctx.store.is_local_domain(sender_domain).await.unwrap_or(false);
        let return_path = if envelope_from.is_empty() || own_sender {
            envelope_from.to_owned()
        } else {
            match srs::secret(&ctx.store).await {
                Some(secret) => srs::rewrite(&secret, envelope_from, our_domain),
                None => {
                    tracing::error!(forwarder = %forwarder.name, "no SRS secret, not forwarding to other servers");
                    return reached;
                }
            }
        };
        let lifetime = ctx.live().delivery.max_lifetime_hours as i64 * 3600;
        if let Err(err) =
            ctx.store.enqueue(&return_path, remote, &forwarded, forwarder.account_id, None, lifetime).await
        {
            tracing::error!(%err, forwarder = %forwarder.name, "queueing a forward failed");
            return reached;
        }
        reached = true;
    }
    tracing::info!(forwarder = %forwarder.name, targets = targets.len(), reached, "forwarded message");
    reached
}
