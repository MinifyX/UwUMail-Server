//! Delivery status notifications (bounces, RFC 3464).

use mail_builder::MessageBuilder;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::text::Text;
use mail_builder::mime::MimePart;
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewQueueRecipient};

use crate::{Context, headers, random_id, texts};

#[derive(Debug, Clone)]
pub struct FailedRecipient {
    pub address: String,
    /// The remote server's answer or our own explanation.
    pub error: String,
}

/// Tells the sender which recipients did not get the message.
pub async fn bounce(ctx: &Context, return_path: &str, original: &[u8], failed: &[FailedRecipient]) {
    if return_path.is_empty() || failed.is_empty() {
        return;
    }
    // A forwarded message: the bounce belongs to the sender before the rewrite.
    let unwrapped = match crate::srs::looks_like_srs(return_path) {
        true => match crate::srs::secret(&ctx.store).await {
            Some(secret) => crate::srs::reverse(&secret, return_path),
            None => None,
        },
        false => None,
    };
    let return_path = unwrapped.as_deref().unwrap_or(return_path);
    let local_account = ctx.store.resolve_recipient(return_path).await.ok().flatten();
    let texts = texts::bounce(ctx.live().tone, local_account.is_some());
    let raw = match build(ctx, &texts, return_path, original, failed) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::error!(%err, "could not build a bounce message");
            return;
        }
    };
    let result = match local_account {
        Some(account_id) => ctx
            .store
            .ingest(IngestRequest {
                account_id,
                raw,
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec![],
                received_at: None,
            })
            .await
            .map(|_| ()),
        None => {
            let recipient = NewQueueRecipient { address: return_path.to_owned(), notify_flags: 0, orcpt: None };
            let lifetime = ctx.live().delivery.max_lifetime_hours as i64 * 3600;
            ctx.store.enqueue("", vec![recipient], &raw, None, None, lifetime).await.map(|_| ())
        }
    };
    if let Err(err) = result {
        tracing::error!(%err, %return_path, "could not deliver a bounce message");
    }
}

fn build(
    ctx: &Context,
    texts: &texts::BounceTexts,
    return_path: &str,
    original: &[u8],
    failed: &[FailedRecipient],
) -> std::io::Result<Vec<u8>> {
    let host = ctx.hostname.as_str();

    let mut text = format!("{}\r\n\r\n", texts.intro);
    for recipient in failed {
        text.push_str(&format!("  {}\r\n    {}\r\n\r\n", recipient.address, single_line(&recipient.error)));
    }
    text.push_str(texts.outro);
    text.push_str("\r\n");

    let mut status = format!("Reporting-MTA: dns; {host}\r\nArrival-Date: {}\r\n", Date::now().to_rfc822());
    for recipient in failed {
        status.push_str(&format!(
            "\r\nFinal-Recipient: rfc822; {}\r\nAction: failed\r\nStatus: {}\r\nDiagnostic-Code: smtp; {}\r\n",
            recipient.address,
            status_code(&recipient.error),
            single_line(&recipient.error),
        ));
    }

    let original_headers = String::from_utf8_lossy(headers::header_block(original)).into_owned();
    let daemon = format!("MAILER-DAEMON@{host}");

    MessageBuilder::new()
        .from((texts.sender_name, daemon.as_str()))
        .to(return_path)
        .subject(texts.subject)
        .date(Date::now())
        .message_id(format!("{}.bounce@{host}", random_id()))
        .header("Auto-Submitted", Text::new("auto-replied"))
        .body(MimePart::new(
            ContentType::new("multipart/report").attribute("report-type", "delivery-status"),
            vec![
                MimePart::new("text/plain; charset=utf-8", text),
                MimePart::new("message/delivery-status", status).transfer_encoding("7bit"),
                MimePart::new("text/rfc822-headers", original_headers),
            ],
        ))
        .write_to_vec()
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pulls an enhanced status code like `5.1.1` out of a server response.
pub fn status_code(error: &str) -> String {
    error
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .find(|token| {
            let parts: Vec<&str> = token.split('.').collect();
            parts.len() == 3
                && matches!(parts[0], "2" | "4" | "5")
                && parts[1..].iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
        .unwrap_or_else(|| "5.0.0".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_status_codes() {
        assert_eq!(status_code("550 5.1.1 <x@y>: Recipient address rejected"), "5.1.1");
        assert_eq!(status_code("connection refused"), "5.0.0");
        assert_eq!(status_code("452 4.2.2 Mailbox full"), "4.2.2");
    }
}
