//! A mail as the deciding sees it (docs/labels.md, "The mail as the deciding sees it").

/// Characters of the text that are looked at, at most.
pub const MAX_TEXT_CHARS: usize = 100_000;

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

impl Mail {
    /// A mail from its parts; the text is cut to [`MAX_TEXT_CHARS`], headers not in [`HEADERS`]
    /// are left out, and `calendar` also holds for an `.ics` attachment.
    pub fn new(
        from: &str,
        subject: &str,
        text: &str,
        attachments: Vec<Attachment>,
        calendar: bool,
        headers: Vec<(String, String)>,
    ) -> Mail {
        let text = match text.char_indices().nth(MAX_TEXT_CHARS) {
            Some((cut, _)) => text[..cut].to_owned(),
            None => text.to_owned(),
        };
        let calendar = calendar
            || attachments.iter().any(|a| {
                a.name.to_lowercase().ends_with(".ics")
                    || matches!(a.content_type.as_str(), "text/calendar" | "application/ics")
            });
        let headers = headers
            .into_iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value))
            .filter(|(name, _)| HEADERS.contains(&name.as_str()))
            .collect();
        Mail {
            from: from.trim().to_lowercase(),
            subject: subject.to_owned(),
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
            .take(100)
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
