//! winmail.dat (`application/ms-tnef`) in stored mail, decoded when the mail is read.
//!
//! Outlook and Exchange sometimes send their own format instead of MIME: the body as RTF, the
//! attachments and meeting invitations packed into one `winmail.dat` part. Stored mail stays
//! exactly as it arrived (DKIM signatures cover it); what is inside the TNEF part is decoded with
//! `uwumail-tnef` whenever it is needed:
//!
//! - at delivery, for the search index, the preview and whether the mail has attachments
//!   (`parse::read`), and for the calendar (`uwumail-smtp` scheduling takes a TNEF meeting like an
//!   iMIP invitation);
//! - when JMAP shows the mail, as parts of their own (`uwumail-jmap` `email`): the attachments,
//!   the body where the MIME has none as good, and the meeting as a `text/calendar` attachment.
//!
//! Nothing decoded is stored. Decoding is quick and bounded (`uwumail_tnef::Limits`), gives the
//! same parts every time, so their ids stay the same, and a better decoder improves old mail too.

use mail_parser::{Message, MessagePart, MimeHeaders, PartType};
pub use uwumail_tnef as decoder;
use uwumail_tnef::{IcsOptions, Person};

/// TNEF parts of one message that are decoded, at most.
pub const MAX_TNEF_PARTS: usize = 4;

/// Whether a raw message may have a TNEF part at all, before it is parsed.
pub fn mentioned(raw: &[u8]) -> bool {
    let contains = |needle: &[u8]| raw.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle));
    contains(b"ms-tnef") || contains(b"winmail.dat")
}

/// The TNEF stream of a part: one of type `application/ms-tnef` (or `vnd.ms-tnef`), or named
/// `winmail.dat`, whose content really is TNEF.
pub fn stream<'a>(part: &'a MessagePart<'_>) -> Option<&'a [u8]> {
    let bytes: &[u8] = match &part.body {
        PartType::Binary(bytes) | PartType::InlineBinary(bytes) => bytes,
        _ => return None,
    };
    let typed = part.content_type().is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case("application")
            && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("ms-tnef") || s.eq_ignore_ascii_case("vnd.ms-tnef"))
    });
    let named = part.attachment_name().is_some_and(|n| n.trim().eq_ignore_ascii_case("winmail.dat"));
    ((typed || named) && uwumail_tnef::is_tnef(bytes)).then_some(bytes)
}

/// A part made of a TNEF part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sub {
    /// The body as text.
    Text,
    /// The body as HTML.
    Html,
    /// The meeting as iCalendar with a METHOD.
    Calendar,
    /// The attachment with this number, from 1.
    Attachment(usize),
}

impl Sub {
    pub fn as_string(self) -> String {
        match self {
            Sub::Text => "text".into(),
            Sub::Html => "html".into(),
            Sub::Calendar => "ics".into(),
            Sub::Attachment(n) => n.to_string(),
        }
    }

    pub fn parse(value: &str) -> Option<Sub> {
        match value {
            "text" => Some(Sub::Text),
            "html" => Some(Sub::Html),
            "ics" => Some(Sub::Calendar),
            n if !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0') => {
                n.parse().ok().map(Sub::Attachment)
            }
            _ => None,
        }
    }
}

/// The id of a part made of the TNEF part at `index`: `3.text`, `3.html`, `3.ics`, `3.1`, …
pub fn part_id(index: usize, sub: Sub) -> String {
    format!("{index}.{}", sub.as_string())
}

/// The TNEF part and the part made of it, of a part id [`part_id`] made.
pub fn parse_part_id(value: &str) -> Option<(usize, Sub)> {
    let (index, sub) = value.split_once('.')?;
    if index.is_empty() || index.len() > 6 || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((index.parse().ok()?, Sub::parse(sub)?))
}

/// One decoded TNEF part.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// The TNEF part's index among the message's parts.
    pub index: usize,
    pub message: uwumail_tnef::Message,
    /// The meeting as iCalendar with a METHOD, when it is one.
    pub calendar: Option<String>,
}

impl Decoded {
    /// The parts made of it, in order: body text, body HTML, meeting, then the attachments.
    pub fn subs(&self) -> Vec<Sub> {
        let mut subs = Vec::new();
        if self.message.body.text.is_some() {
            subs.push(Sub::Text);
        }
        if self.message.body.html.is_some() {
            subs.push(Sub::Html);
        }
        if self.calendar.is_some() {
            subs.push(Sub::Calendar);
        }
        subs.extend((1..=self.message.attachments.len()).map(Sub::Attachment));
        subs
    }

    pub fn attachment(&self, sub: Sub) -> Option<&uwumail_tnef::Attachment> {
        match sub {
            Sub::Attachment(n) => self.message.attachments.get(n.checked_sub(1)?),
            _ => None,
        }
    }

    /// File names of the attachments, attached messages' own included.
    pub fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut work: Vec<&uwumail_tnef::Message> = vec![&self.message];
        while let Some(message) = work.pop() {
            for attachment in &message.attachments {
                if let Some(name) = &attachment.name {
                    names.push(name.clone());
                }
                if let Some(inner) = &attachment.embedded {
                    work.push(inner);
                }
            }
        }
        names
    }
}

fn people(address: Option<&mail_parser::Address<'_>>) -> Vec<Person> {
    address
        .map(|a| {
            a.iter()
                .filter_map(|addr| {
                    Some(Person {
                        name: addr.name.as_deref().map(str::to_owned),
                        email: Some(addr.address.as_deref()?.to_owned()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What the mail's headers say, for the meeting's iCalendar.
fn ics_options(message: &Message<'_>) -> IcsOptions {
    IcsOptions {
        // The message's own date, so the same mail always gives the same calendar part.
        now: message.date().map_or(0, |d| d.to_timestamp()),
        from: people(message.from()).into_iter().next(),
        to: people(message.to()),
        cc: people(message.cc()),
    }
}

/// Decodes the TNEF part at `index`, if it is one.
pub fn decode_part(message: &Message<'_>, index: usize) -> Option<Decoded> {
    let bytes = stream(message.parts.get(index)?)?;
    let decoded = uwumail_tnef::decode(bytes).ok()?;
    let calendar = decoded.meeting().and_then(|meeting| meeting.to_ical(&ics_options(message)));
    Some(Decoded { index, message: decoded, calendar })
}

/// Decodes the TNEF parts of a message (the first [`MAX_TNEF_PARTS`]).
pub fn decode(message: &Message<'_>) -> Vec<Decoded> {
    (0..message.parts.len())
        .filter(|index| message.parts.get(*index).and_then(stream).is_some())
        .take(MAX_TNEF_PARTS)
        .filter_map(|index| decode_part(message, index))
        .collect()
}

/// Whether the message's own MIME body has some text worth showing.
pub fn mime_has_text(message: &Message<'_>) -> bool {
    message.text_body.iter().chain(&message.html_body).any(|index| {
        matches!(message.parts.get(*index as usize).map(|p| &p.body),
            Some(PartType::Text(text) | PartType::Html(text)) if !text.trim().is_empty())
    })
}

/// Whether the message's own MIME has an HTML body.
pub fn mime_has_html(message: &Message<'_>) -> bool {
    message
        .html_body
        .iter()
        .any(|index| matches!(message.parts.get(*index as usize).map(|p| &p.body), Some(PartType::Html(_))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_ids() {
        for sub in [Sub::Text, Sub::Html, Sub::Calendar, Sub::Attachment(12)] {
            assert_eq!(parse_part_id(&part_id(3, sub)), Some((3, sub)));
        }
        assert_eq!(parse_part_id("3"), None);
        assert_eq!(parse_part_id("3.0"), None);
        assert_eq!(parse_part_id("3.01"), None);
        assert_eq!(parse_part_id(".text"), None);
        assert_eq!(parse_part_id("3.css"), None);
        assert_eq!(parse_part_id("99999999999.1"), None);
    }
}
