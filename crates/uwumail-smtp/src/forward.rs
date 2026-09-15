//! Passing mail on to the addresses a person forwards to.

use uwumail_store::{Account, IngestRequest, MailboxRole, MailboxTarget, NewQueueRecipient};

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

/// Sends a received message on. `recipient` is the address it arrived for; it goes into a
/// Delivered-To header, which also stops mail going round in circles between forwards.
pub(crate) async fn send(
    ctx: &Context,
    account: &Account,
    recipient: &str,
    envelope_from: &str,
    message: &[u8],
    targets: &[(String, Option<i64>)],
) {
    if targets.is_empty() {
        return;
    }
    let (fields, _) = headers::split(message);
    let looped = fields
        .iter()
        .any(|field| field.name.eq_ignore_ascii_case("Delivered-To") && field.value().eq_ignore_ascii_case(recipient));
    if looped {
        tracing::warn!(login = %account.login, %recipient, "not forwarding a message that was here before");
        return;
    }
    let mut forwarded = format!("Delivered-To: {recipient}\r\n").into_bytes();
    forwarded.extend_from_slice(message);

    let mut remote = Vec::new();
    for (address, local) in targets {
        match local {
            // People on this server get it directly; their own forwarding does not apply again.
            Some(target_account) => {
                let request = IngestRequest {
                    account_id: *target_account,
                    raw: forwarded.clone(),
                    mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                    keywords: vec![],
                    received_at: None,
                };
                if let Err(err) = ctx.store.ingest(request).await {
                    tracing::warn!(%err, login = %account.login, to = %address, "forwarding to a local mailbox failed");
                }
            }
            None => remote.push(NewQueueRecipient { address: address.clone(), notify_flags: 0, orcpt: None }),
        }
    }

    if !remote.is_empty() {
        let our_domain = account.login.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(&ctx.hostname);
        let sender_domain = envelope_from.rsplit_once('@').map(|(_, domain)| domain).unwrap_or_default();
        let return_path = if envelope_from.is_empty() || ctx.store.is_local_domain(sender_domain).await.unwrap_or(false)
        {
            envelope_from.to_owned()
        } else {
            match srs::secret(&ctx.store).await {
                Some(secret) => srs::rewrite(&secret, envelope_from, our_domain),
                None => {
                    tracing::error!(login = %account.login, "no SRS secret, not forwarding to other servers");
                    return;
                }
            }
        };
        let lifetime = ctx.live().delivery.max_lifetime_hours as i64 * 3600;
        if let Err(err) = ctx.store.enqueue(&return_path, remote, &forwarded, Some(account.id), None, lifetime).await {
            tracing::error!(%err, login = %account.login, "queueing a forward failed");
            return;
        }
    }
    tracing::info!(login = %account.login, targets = targets.len(), "forwarded message");
}
