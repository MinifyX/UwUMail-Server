//! Vacation auto-replies (RFC 3834, RFC 8621 VacationResponse).

use mail_builder::MessageBuilder;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::text::Text;
use mail_parser::MessageParser;
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewQueueRecipient};

use crate::{Context, dkim, headers, random_id};

/// Senders that are machines and must never get an auto-reply.
fn is_automated_sender(address: &str) -> bool {
    let local = address.split('@').next().unwrap_or_default().to_ascii_lowercase();
    [
        "mailer-daemon",
        "postmaster",
        "noreply",
        "no-reply",
        "donotreply",
        "do-not-reply",
        "listserv",
        "majordomo",
        "bounce",
    ]
    .iter()
    .any(|word| local.contains(word))
        || local.starts_with("owner-")
        || local.ends_with("-request")
}

/// Headers that mark bulk, list or automatic mail.
fn is_automated_message(raw: &[u8]) -> bool {
    let value = |name: &str| headers::first_value(raw, name).map(|v| v.to_ascii_lowercase());
    if value("Auto-Submitted").is_some_and(|v| v != "no") {
        return true;
    }
    if value("Precedence").is_some_and(|v| matches!(v.as_str(), "bulk" | "list" | "junk")) {
        return true;
    }
    if value("X-Auto-Response-Suppress").is_some_and(|v| v.contains("all") || v.contains("oof")) {
        return true;
    }
    ["List-Id", "List-Unsubscribe", "List-Post", "Feedback-ID"]
        .iter()
        .any(|name| headers::first_value(raw, name).is_some())
}

/// Sends the account's vacation response to the sender of `raw`, when it is on and due.
///
/// Only to a sender the checks verified (SPF pass for the MAIL FROM domain, or an aligned DKIM
/// pass): a reply to a forged envelope sender is backscatter (security-audit-0.5.2 S-16).
pub async fn maybe_reply(ctx: &Context, account_id: i64, envelope_from: &str, sender_verified: bool, raw: &[u8]) {
    if !sender_verified || envelope_from.is_empty() || is_automated_sender(envelope_from) || is_automated_message(raw) {
        return;
    }
    let Ok(Some(account)) = ctx.store.account_by_id(account_id).await else { return };
    let Some(message) = MessageParser::default().parse_headers(raw) else { return };

    // Only answer mail sent to this person directly, not through a list or as Bcc.
    let own = ctx.store.addresses(&account.login).await.unwrap_or_default();
    let addressed = message
        .to()
        .into_iter()
        .chain(message.cc())
        .flat_map(|list| list.iter())
        .filter_map(|addr| addr.address.as_deref())
        .any(|address| {
            let address = address.to_ascii_lowercase();
            own.iter()
                .any(|mine| *mine == address || address.split_once('+').is_some_and(|_| strip_tag(&address) == *mine))
        });
    if !addressed {
        return;
    }

    let Ok(Some(vacation)) = ctx.store.take_vacation_reply(account_id, envelope_from).await else { return };

    let original_subject = message.subject().unwrap_or_default().to_owned();
    let subject = vacation
        .subject
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Auto: {original_subject}"));
    let domain = account.login.rsplit_once('@').map(|(_, d)| d.to_owned()).unwrap_or_default();
    let mut builder = MessageBuilder::new()
        .from((account.display_name.clone(), account.login.clone()))
        .to(envelope_from.to_owned())
        .subject(subject)
        .date(Date::now())
        .message_id(format!("{}.vacation@{domain}", random_id()))
        .header("Auto-Submitted", Text::new("auto-replied"))
        .header("X-Auto-Response-Suppress", Text::new("All"));
    if let Some(id) = message.message_id() {
        builder = builder.in_reply_to(MessageId::new(id.to_owned())).references(MessageId::new(id.to_owned()));
    }
    let text = vacation.text_body.clone().unwrap_or_default();
    builder = match vacation.html_body.clone() {
        Some(html) if !html.trim().is_empty() => builder.text_body(text).html_body(html),
        _ => builder.text_body(text),
    };
    let Ok(reply) = builder.write_to_vec() else { return };

    let signatures = match dkim::ensure_domain_keys(&ctx.store, &domain).await {
        Ok(keys) => dkim::sign(&reply, &keys).unwrap_or_default(),
        Err(_) => String::new(),
    };
    let mut signed = signatures.into_bytes();
    signed.extend_from_slice(&reply);

    let local = match ctx.store.resolve_recipient(envelope_from).await.ok().flatten() {
        Some(account_id) => ctx.store.delivery_target(account_id).await.ok().flatten(),
        None => None,
    };
    let result = match local {
        Some(local_account) => ctx
            .store
            .ingest(IngestRequest {
                account_id: local_account,
                raw: signed,
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec![],
                received_at: None,
            })
            .await
            .map(|_| ()),
        None => {
            // Auto-replies use the null sender so they can never bounce in a loop.
            let recipient = NewQueueRecipient { address: envelope_from.to_owned(), notify_flags: 0, orcpt: None };
            let lifetime = ctx.live().delivery.max_lifetime_hours as i64 * 3600;
            ctx.store.enqueue("", vec![recipient], &signed, Some(account_id), None, lifetime).await.map(|_| ())
        }
    };
    match result {
        Ok(()) => tracing::info!(login = %account.login, to = %envelope_from, "sent vacation reply"),
        Err(err) => tracing::warn!(%err, login = %account.login, "vacation reply failed"),
    }
}

fn strip_tag(address: &str) -> String {
    match address.split_once('@') {
        Some((local, domain)) => format!("{}@{domain}", local.split('+').next().unwrap_or(local)),
        None => address.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_automated_mail() {
        assert!(is_automated_sender("MAILER-DAEMON@example.de"));
        assert!(is_automated_sender("no-reply@shop.de"));
        assert!(is_automated_sender("list-request@lists.org"));
        assert!(!is_automated_sender("nyu@example.de"));
        assert!(is_automated_message(b"Auto-Submitted: auto-replied\r\n\r\n"));
        assert!(is_automated_message(b"List-Id: <cats.lists.org>\r\n\r\n"));
        assert!(is_automated_message(b"Precedence: bulk\r\n\r\n"));
        assert!(!is_automated_message(b"Auto-Submitted: no\r\nSubject: hi\r\n\r\n"));
        assert_eq!(strip_tag("mini+shop@example.de"), "mini@example.de");
    }
}
