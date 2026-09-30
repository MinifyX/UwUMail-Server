//! Delivery status notifications (bounces, RFC 3464).

use mail_builder::MessageBuilder;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::text::Text;
use mail_builder::mime::MimePart;
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewQueueRecipient};

use crate::config::Language;
use crate::microsoft::IssueKind;
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
    // A bounce for a mailbox-less service goes where its mail goes, or out over the queue.
    let sender_account = ctx.store.resolve_recipient(return_path).await.ok().flatten();
    let local_account = match sender_account {
        Some(account_id) => ctx.store.delivery_target(account_id).await.ok().flatten(),
        None => None,
    };
    // Our own people read it in the language they chose.
    let mut tone = ctx.tone();
    if let Some(account_id) = sender_account {
        let preferences = ctx.store.preferences(account_id).await.unwrap_or_default();
        tone.language = Language::preferred(preferences.get("language").and_then(|v| v.as_str()), tone.language);
    }
    let mut texts = texts::bounce(tone, local_account.is_some(), ctx.brand().name());
    // A typo is not why Microsoft refused: don't send the person looking for one.
    if only_microsoft(failed) {
        texts.outro = texts::outro_without_typo_hint(tone, local_account.is_some());
    }
    let note = microsoft_kind(failed).map(|kind| texts::microsoft_note(tone.language, local_account.is_some(), kind));
    let raw = match build(ctx, &texts, note, return_path, original, failed) {
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

/// Whether Microsoft refused or held back any of the recipients, and how: a block counts before
/// a domain that fails its checks, which counts before throttling.
fn microsoft_kind(failed: &[FailedRecipient]) -> Option<IssueKind> {
    let rank = |kind: &IssueKind| match kind {
        IssueKind::Blocked => 2,
        IssueKind::Authentication => 1,
        IssueKind::Throttled => 0,
    };
    failed
        .iter()
        .filter_map(|recipient| crate::microsoft::classify(&recipient.error, false))
        .map(|refusal| refusal.group.kind())
        .max_by_key(rank)
}

/// Whether every recipient failed because Microsoft refused or held back the mail.
fn only_microsoft(failed: &[FailedRecipient]) -> bool {
    failed.iter().all(|recipient| crate::microsoft::classify(&recipient.error, false).is_some())
}

fn build(
    ctx: &Context,
    texts: &texts::BounceTexts,
    note: Option<&str>,
    return_path: &str,
    original: &[u8],
    failed: &[FailedRecipient],
) -> std::io::Result<Vec<u8>> {
    let host = ctx.hostname.as_str();

    let mut text = format!("{}\r\n\r\n", texts.intro);
    for recipient in failed {
        text.push_str(&format!("  {}\r\n    {}\r\n\r\n", recipient.address, single_line(&recipient.error)));
    }
    if let Some(note) = note {
        text.push_str(note);
        text.push_str("\r\n\r\n");
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
        .from((texts.sender_name.as_str(), daemon.as_str()))
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
    fn microsoft_refusals_are_told_apart_from_other_bounces() {
        let failed = |error: &str| FailedRecipient { address: "ami@example.com".into(), error: error.into() };
        assert_eq!(microsoft_kind(&[failed("550 5.1.1 unknown user")]), None);
        let throttled = failed(
            "example-com.mail.protection.outlook.com [192.0.2.1]: 451 4.7.650 The mail server [203.0.113.5] has been \
             temporarily rate limited due to IP reputation.",
        );
        let blocked = failed("550 5.7.1 Unfortunately, messages from [203.0.113.5] weren't sent (S3150).");
        assert_eq!(microsoft_kind(std::slice::from_ref(&throttled)), Some(IssueKind::Throttled));
        assert_eq!(microsoft_kind(&[throttled, blocked]), Some(IssueKind::Blocked));
        // Every language has its words.
        for language in Language::ALL {
            for local in [true, false] {
                assert!(texts::microsoft_note(language, local, IssueKind::Blocked).contains("Microsoft"));
            }
        }
    }

    #[test]
    fn no_typo_hint_when_only_microsoft_refused() {
        use crate::config::{ExternalTone, InternalTone, ToneConfig};
        let failed = |error: &str| FailedRecipient { address: "ami@example.com".into(), error: error.into() };
        let blocked = failed("550 5.7.1 Unfortunately, messages from [192.0.2.1] weren't sent. (S3150)");
        assert!(only_microsoft(std::slice::from_ref(&blocked)));
        assert!(!only_microsoft(&[blocked, failed("550 5.1.1 unknown user")]));
        for language in Language::ALL {
            for local in [true, false] {
                for internal in [InternalTone::Playful, InternalTone::Neutral] {
                    for external in [ExternalTone::Neutral, ExternalTone::Light] {
                        let tone = ToneConfig { language, internal, external };
                        let outro = texts::outro_without_typo_hint(tone, local);
                        for word in ["Tippfehler", "typo", "frappe", "typefout", "打ち間違い", "拼写"] {
                            assert!(!outro.contains(word), "{language:?} {local}: {outro}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn extracts_status_codes() {
        assert_eq!(status_code("550 5.1.1 <x@y>: Recipient address rejected"), "5.1.1");
        assert_eq!(status_code("connection refused"), "5.0.0");
        assert_eq!(status_code("452 4.2.2 Mailbox full"), "4.2.2");
    }
}
