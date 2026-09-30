//! Extracts the metadata the store indexes from a raw RFC 5322 message.

use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders};

use crate::address::EmailAddress;
use crate::mime_limits::{self, MimeFault};
use crate::tnef;

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

/// The metadata of a message, or why [`mime_limits`](crate::mime_limits) refuses its structure.
/// One that cannot be read at all has empty metadata.
pub fn read(raw: &[u8]) -> Result<EmailMeta, MimeFault> {
    mime_limits::check(raw)?;
    let Some(message) = MessageParser::default().parse(raw) else {
        return Ok(EmailMeta::default());
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

    // winmail.dat: its text and file names count as the mail's own (see `tnef`).
    let tnef = if tnef::mentioned(raw) { tnef::decode(&message) } else { Vec::new() };
    let mut preview = preview(&message);
    if preview.is_empty()
        && let Some(text) = tnef.iter().find_map(|d| d.message.body.text.as_deref())
    {
        preview = collapse(text);
    }
    let own_attachments = message.attachments().filter(|part| tnef::stream(part).is_none()).count();
    let tnef_parts = message.attachments().filter(|part| tnef::stream(part).is_some()).count();
    let has_attachment = own_attachments > 0
        || tnef_parts > tnef.len()
        || tnef.iter().any(|d| d.message.attachments.iter().any(|a| !a.inline));

    Ok(EmailMeta {
        message_id: message.message_id().map(clean_id).filter(|id| !id.is_empty()),
        in_reply_to: id_list(message.in_reply_to()),
        references: id_list(message.references()),
        subject,
        sender: addresses(message.sender()),
        reply_to: addresses(message.reply_to()),
        sent_at: message.date().map(|d| d.to_timestamp()),
        preview,
        has_attachment,
        search_body: search_body(&message, &tnef),
        search_addresses,
        from,
        to,
        cc,
        bcc,
    })
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
    collapse(&text)
}

/// Whitespace collapsed, Safe Links unwrapped, cut to [`PREVIEW_CHARS`].
fn collapse(text: &str) -> String {
    let start: String = text.chars().take(PREVIEW_CHARS * 4).collect();
    let unwrapped = tnef::decoder::safelinks::unwrap_in_text(&start);
    let collapsed = unwrapped.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(PREVIEW_CHARS).collect()
}

fn search_body(message: &Message<'_>, tnef: &[tnef::Decoded]) -> String {
    let mut body = String::new();
    for index in 0..message.text_body_count() {
        if let Some(text) = message.body_text(index) {
            body.push_str(&tnef::decoder::safelinks::unwrap_in_text(&text));
            body.push('\n');
        }
        if body.len() > MAX_INDEXED_BODY_BYTES {
            break;
        }
    }
    for decoded in tnef {
        if body.len() > MAX_INDEXED_BODY_BYTES {
            break;
        }
        if let Some(text) = &decoded.message.body.text {
            body.push_str(&tnef::decoder::safelinks::unwrap_in_text(text));
            body.push('\n');
        }
    }
    for attachment in message.attachments() {
        if let Some(name) = attachment.attachment_name().filter(|_| tnef::stream(attachment).is_none()) {
            body.push_str(name);
            body.push('\n');
        }
    }
    for name in tnef.iter().flat_map(tnef::Decoded::names) {
        body.push_str(&name);
        body.push('\n');
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
            "From: Nyu <nyu@example.org>\r\n",
            "To: Mini <mini@example.org>, ami@example.org\r\n",
            "Subject: Re: Fwd: Katzenfutter\r\n",
            "Date: Mon, 14 Sep 2026 10:00:00 +0200\r\n",
            "Message-ID: <abc@example.org>\r\n",
            "In-Reply-To: <parent@example.org>\r\n",
            "References: <root@example.org> <parent@example.org>\r\n",
            "\r\n",
            "Hallo   Mini,\r\n\r\nes gibt  Thunfisch.\r\n",
        );
        let meta = read(raw.as_bytes()).unwrap();
        assert_eq!(meta.message_id.as_deref(), Some("abc@example.org"));
        assert_eq!(meta.in_reply_to, vec!["parent@example.org"]);
        assert_eq!(meta.references, vec!["root@example.org", "parent@example.org"]);
        assert_eq!(meta.subject, "Re: Fwd: Katzenfutter");
        assert_eq!(meta.from, vec![EmailAddress { name: Some("Nyu".into()), email: "nyu@example.org".into() }]);
        assert_eq!(meta.to.len(), 2);
        assert_eq!(meta.preview, "Hallo Mini, es gibt Thunfisch.");
        assert_eq!(meta.sent_at, Some(1_789_372_800));
        assert!(meta.search_body.contains("Thunfisch"));
        assert!(!meta.has_attachment);
    }

    /// A message wrapped in `message/rfc822` a hundred thousand times over, about 5 MB. Parsing it
    /// used to succeed and then overflow the stack while the result was dropped, which aborts the
    /// whole server (security-audit-0.16.0 SMTP-1). It runs on a thread with the 2 MiB stack every
    /// Tokio thread has.
    #[test]
    fn deeply_nested_message_does_not_overflow_the_stack() {
        let mut raw = Vec::new();
        for _ in 0..100_000 {
            raw.extend_from_slice(b"Subject: layer\r\nContent-Type: message/rfc822\r\n\r\n");
        }
        raw.extend_from_slice(b"Subject: bottom\r\n\r\nhi\r\n");
        let meta = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || read(&raw))
            .unwrap()
            .join()
            .expect("parsing must not panic");
        assert_eq!(meta.unwrap_err(), MimeFault::TooDeep);
    }

    #[test]
    fn winmail_dat_counts_as_the_mail() {
        use uwumail_tnef::builder::{Props, Tnef, compressed_rtf};
        use uwumail_tnef::mapi;
        let rtf = br"{\rtf1\ansi\fromhtml1 {\*\htmltag64 <p>}Die Kisten stehen bereit, siehe https://nam12.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.org%2Fkisten&data=1{\*\htmltag72 </p>}}";
        let mut tnef = Tnef::new();
        tnef.message_class("IPM.Note");
        tnef.message_props(&Props::new().binary(mapi::PR_RTF_COMPRESSED, &compressed_rtf(rtf)));
        tnef.attachment(
            "PACKLI~1.XLS",
            b"cells",
            &Props::new().unicode(mapi::PR_ATTACH_LONG_FILENAME, "Packliste Küche.xlsx"),
        );
        let raw = uwumail_tnef::builder::mime_with_winmail(
            "From: Mini <mini@example.com>\r\nTo: nyu@example.com\r\nSubject: Umzug\r\n",
            Some(""),
            &tnef.build(),
        );
        let meta = read(&raw).unwrap();
        assert_eq!(meta.preview, "Die Kisten stehen bereit, siehe https://example.org/kisten");
        assert!(meta.has_attachment);
        assert!(meta.search_body.contains("Packliste Küche.xlsx"), "{}", meta.search_body);
        assert!(meta.search_body.contains("https://example.org/kisten"));
        assert!(!meta.search_body.contains("winmail.dat"));

        // Only a body in the TNEF: no attachment.
        let mut tnef = Tnef::new();
        tnef.message_props(&Props::new().unicode(mapi::PR_BODY, "Nur Text"));
        let raw = uwumail_tnef::builder::mime_with_winmail("Subject: x\r\n", None, &tnef.build());
        let meta = read(&raw).unwrap();
        assert!(!meta.has_attachment);
        assert_eq!(meta.preview, "Nur Text");
    }

    #[test]
    fn survives_garbage() {
        let meta = read(b"\xff\xfe not a message").unwrap();
        assert!(meta.subject.is_empty());
    }
}
