//! Extracts the metadata the store indexes from a raw RFC 5322 message.

use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders};

use crate::address::EmailAddress;

const PREVIEW_CHARS: usize = 256;
const MAX_INDEXED_BODY_BYTES: usize = 512 * 1024;

#[derive(Debug, Default)]
pub struct EmailMeta {
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Vec<EmailAddress>,
    pub sender: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    pub sent_at: Option<i64>,
    pub preview: String,
    pub has_attachment: bool,
    pub search_addresses: String,
    pub search_body: String,
}

pub fn parse(raw: &[u8]) -> EmailMeta {
    let Some(message) = MessageParser::default().parse(raw) else {
        return EmailMeta::default();
    };

    let subject = message.subject().unwrap_or_default().trim().to_owned();
    let from = addresses(message.from());
    let to = addresses(message.to());
    let cc = addresses(message.cc());
    let bcc = addresses(message.bcc());
    let search_addresses = from
        .iter()
        .chain(&to)
        .chain(&cc)
        .chain(&bcc)
        .flat_map(|a| [a.name.as_deref().unwrap_or_default(), a.email.as_str()])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    EmailMeta {
        message_id: message.message_id().map(clean_id).filter(|id| !id.is_empty()),
        in_reply_to: id_list(message.in_reply_to()),
        references: id_list(message.references()),
        subject,
        sender: addresses(message.sender()),
        reply_to: addresses(message.reply_to()),
        sent_at: message.date().map(|d| d.to_timestamp()),
        preview: preview(&message),
        has_attachment: message.attachment_count() > 0,
        search_body: search_body(&message),
        search_addresses,
        from,
        to,
        cc,
        bcc,
    }
}

fn addresses(address: Option<&Address<'_>>) -> Vec<EmailAddress> {
    let Some(address) = address else {
        return Vec::new();
    };
    address
        .iter()
        .filter_map(|addr| {
            let email = addr.address.as_deref()?.trim();
            if email.is_empty() {
                return None;
            }
            let name = addr.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned);
            Some(EmailAddress { name, email: email.to_owned() })
        })
        .collect()
}

fn id_list(value: &HeaderValue<'_>) -> Vec<String> {
    let ids: Vec<String> = match value {
        HeaderValue::Text(id) => vec![clean_id(id)],
        HeaderValue::TextList(ids) => ids.iter().map(|id| clean_id(id)).collect(),
        _ => Vec::new(),
    };
    ids.into_iter().filter(|id| !id.is_empty()).collect()
}

fn clean_id(id: &str) -> String {
    id.trim().trim_start_matches('<').trim_end_matches('>').to_owned()
}

fn preview(message: &Message<'_>) -> String {
    let text = message.body_preview(PREVIEW_CHARS * 2).unwrap_or_default();
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(PREVIEW_CHARS).collect()
}

fn search_body(message: &Message<'_>) -> String {
    let mut body = String::new();
    for index in 0..message.text_body_count() {
        if let Some(text) = message.body_text(index) {
            body.push_str(&text);
            body.push('\n');
        }
        if body.len() > MAX_INDEXED_BODY_BYTES {
            break;
        }
    }
    for attachment in message.attachments() {
        if let Some(name) = attachment.attachment_name() {
            body.push_str(name);
            body.push('\n');
        }
    }
    if body.len() > MAX_INDEXED_BODY_BYTES {
        let mut cut = MAX_INDEXED_BODY_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_metadata() {
        let raw = concat!(
            "From: Nyu <nyu@example.de>\r\n",
            "To: Mini <mini@example.de>, ami@example.de\r\n",
            "Subject: Re: Fwd: Katzenfutter\r\n",
            "Date: Mon, 14 Sep 2026 10:00:00 +0200\r\n",
            "Message-ID: <abc@example.de>\r\n",
            "In-Reply-To: <parent@example.de>\r\n",
            "References: <root@example.de> <parent@example.de>\r\n",
            "\r\n",
            "Hallo   Mini,\r\n\r\nes gibt  Thunfisch.\r\n",
        );
        let meta = parse(raw.as_bytes());
        assert_eq!(meta.message_id.as_deref(), Some("abc@example.de"));
        assert_eq!(meta.in_reply_to, vec!["parent@example.de"]);
        assert_eq!(meta.references, vec!["root@example.de", "parent@example.de"]);
        assert_eq!(meta.subject, "Re: Fwd: Katzenfutter");
        assert_eq!(meta.from, vec![EmailAddress { name: Some("Nyu".into()), email: "nyu@example.de".into() }]);
        assert_eq!(meta.to.len(), 2);
        assert_eq!(meta.preview, "Hallo Mini, es gibt Thunfisch.");
        assert_eq!(meta.sent_at, Some(1_789_372_800));
        assert!(meta.search_body.contains("Thunfisch"));
        assert!(!meta.has_attachment);
    }

    #[test]
    fn survives_garbage() {
        let meta = parse(b"\xff\xfe not a message");
        assert!(meta.subject.is_empty());
    }
}
