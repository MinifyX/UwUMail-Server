//! A mail as the deciding sees it (docs/labels.md, "The mail as the deciding sees it").

/// Characters of the text that are looked at, at most.
pub const MAX_TEXT_CHARS: usize = 100_000;

/// Characters of the subject, the From address, a header value and an attachment name that are
/// looked at, at most: a line of mail is at most 998 characters, and a longer one written on
/// purpose must not cost more than the text does (security audit 0.21.0 LABELS-H1).
pub const MAX_FIELD_CHARS: usize = 1_000;

/// Attachments that are looked at, at most.
pub const MAX_ATTACHMENTS: usize = 100;

/// The headers the detectors read; others are not kept.
pub const HEADERS: [&str; 5] = ["list-unsubscribe", "list-unsubscribe-post", "list-id", "list-post", "precedence"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attachment {
    /// The file name; empty when the part has none.
    pub name: String,
    /// `type/subtype`, lower case.
    pub content_type: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mail {
    /// The first From address, lower case.
    pub from: String,
    /// Whether the From address says who really sent the mail; only then do learned senders give
    /// it a label. [`Mail::new`] assumes so; a caller that knows better (the server checks SPF,
    /// DKIM and DMARC at delivery) sets it to `false` when nothing vouches for the address.
    pub from_trusted: bool,
    pub subject: String,
    /// The text body, or the HTML body turned into text, at most [`MAX_TEXT_CHARS`].
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub has_attachment: bool,
    /// A `text/calendar` or `application/ics` part, or an attachment named `*.ics`.
    pub calendar: bool,
    /// Of [`HEADERS`], name (lower case) and value.
    pub headers: Vec<(String, String)>,
}

/// `text` up to its first `max` characters.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

impl Mail {
    /// A mail from its parts; the text is cut to [`MAX_TEXT_CHARS`], the subject, From, header
    /// values and attachment names to [`MAX_FIELD_CHARS`], headers not in [`HEADERS`] are left out,
    /// and `calendar` also holds for an `.ics` attachment.
    pub fn new(
        from: &str,
        subject: &str,
        text: &str,
        attachments: Vec<Attachment>,
        calendar: bool,
        headers: Vec<(String, String)>,
    ) -> Mail {
        let text = cut(text, MAX_TEXT_CHARS);
        let attachments: Vec<Attachment> = attachments
            .into_iter()
            .take(MAX_ATTACHMENTS)
            .map(|a| Attachment {
                name: cut(&a.name, MAX_FIELD_CHARS),
                content_type: cut(&a.content_type, MAX_FIELD_CHARS),
            })
            .collect();
        let calendar = calendar
            || attachments.iter().any(|a| {
                a.name.to_lowercase().ends_with(".ics")
                    || matches!(a.content_type.as_str(), "text/calendar" | "application/ics")
            });
        let headers = headers
            .into_iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value))
            .filter(|(name, _)| HEADERS.contains(&name.as_str()))
            .map(|(name, value)| (name, cut(&value, MAX_FIELD_CHARS)))
            .collect();
        Mail {
            from: cut(from.trim(), MAX_FIELD_CHARS).to_lowercase(),
            from_trusted: true,
            subject: cut(subject, MAX_FIELD_CHARS),
            text,
            has_attachment: !attachments.is_empty(),
            attachments,
            calendar,
            headers,
        }
    }

    /// The value of a header (name in lower case), the first one.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(known, _)| known == name).map(|(_, value)| value.as_str())
    }

    /// The domain of `from`.
    pub fn from_domain(&self) -> &str {
        self.from.rsplit_once('@').map_or("", |(_, domain)| domain)
    }

    /// Reads a raw message.
    #[cfg(feature = "parse")]
    pub fn parse(raw: &[u8]) -> Mail {
        use mail_parser::{MessageParser, MimeHeaders};
        let Some(message) = MessageParser::default().parse(raw) else { return Mail::default() };
        let from = message
            .from()
            .and_then(|from| from.first())
            .and_then(|address| address.address.as_deref())
            .unwrap_or_default()
            .to_owned();
        let subject = message.subject().unwrap_or_default().to_owned();
        let text = message.body_text(0).map(|text| text.into_owned()).unwrap_or_default();
        let content_type = |part: &mail_parser::MessagePart<'_>| {
            part.content_type()
                .map(|ct| format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or_default()).to_ascii_lowercase())
                .unwrap_or_default()
        };
        let attachments = message
            .attachments()
            .take(MAX_ATTACHMENTS)
            .map(|part| Attachment {
                name: part.attachment_name().unwrap_or_default().to_owned(),
                content_type: content_type(part),
            })
            .collect();
        let calendar =
            message.parts.iter().any(|part| matches!(content_type(part).as_str(), "text/calendar" | "application/ics"));
        let headers = message
            .headers_raw()
            .filter(|(name, _)| HEADERS.iter().any(|known| known.eq_ignore_ascii_case(name)))
            .take(20)
            .map(|(name, value)| (name.to_owned(), value.split_whitespace().collect::<Vec<_>>().join(" ")))
            .collect();
        Mail::new(&from, &subject, &text, attachments, calendar, headers)
    }
}
