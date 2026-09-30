//! A decoder for Microsoft TNEF, the `application/ms-tnef` part (usually named `winmail.dat`)
//! that Outlook and Exchange attach instead of plain MIME when they think the recipient is
//! another Outlook.
//!
//! What it reads (MS-OXTNEF, MS-OXRTFCP, MS-OXRTFEX, MS-OXOCAL, MS-OXCICAL):
//!
//! - **Attachments**, with their long (Unicode) names, content ids, the media type Outlook
//!   recorded (or one guessed from the name and bytes), and attached messages, decoded in turn.
//! - **The body**: plain text (`PR_BODY`), HTML (`PR_HTML`), and compressed RTF
//!   (`PR_RTF_COMPRESSED`), unpacked; HTML or text that Outlook wrapped in RTF comes out as it was,
//!   real RTF becomes text and simple HTML.
//! - **Meetings** (`IPM.Schedule.Meeting.*`): requests, answers and cancellations as an iCalendar
//!   object with a METHOD (iTIP, RFC 5546): times with their Windows time zone, location,
//!   organizer, attendees, UID from the GlobalObjectId, sequence and recurrence
//!   ([`Message::meeting`], [`Meeting::to_ical`]).
//!
//! The input is untrusted mail. Every read is bounds-checked, nothing recurses without a depth
//! limit, sizes and counts are limited ([`Limits`]), and no input makes it panic: a damaged stream
//! gives what could be read before the damage, with [`Message::complete`] false.
//!
//! [`safelinks`] turns Microsoft Defender "Safe Links" back into the links they wrap.
//!
//! The crate depends on nothing of UwUMail (only on `encoding_rs`), so the UwUMail client copies
//! it as it is. With the `builder` feature it also has a small TNEF writer for tests.

mod codepage;
mod html;
pub mod mapi;
mod meeting;
mod mime;
mod reader;
pub mod rtf;
pub mod safelinks;
mod time;

#[cfg(any(test, feature = "builder"))]
pub mod builder;

pub use html::{escape as escape_html, to_text as html_to_text};
pub use mapi::{PropId, Properties, Property, Value};
pub use meeting::{Attendee, IcsOptions, Meeting, MeetingKind, PartStat, Recurrence, TimeZone};

use mapi::*;
use reader::Reader;

/// The first four bytes of every TNEF stream.
pub const SIGNATURE: u32 = 0x223E_9F78;

/// Whether `data` starts like a TNEF stream.
pub fn is_tnef(data: &[u8]) -> bool {
    data.len() >= 6 && data[..4] == SIGNATURE.to_le_bytes()
}

/// How much one stream may make the decoder do.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Size of the stream.
    pub max_input: usize,
    /// Attributes in the stream, those of attached messages included.
    pub max_attributes: usize,
    /// MAPI property values in the stream.
    pub max_properties: usize,
    /// Attachments of one message.
    pub max_attachments: usize,
    /// Bytes of body: unpacked RTF, and each text or HTML made of it.
    pub max_body: usize,
    /// Bytes the stream may make in all, attached messages included: bodies, attachment data,
    /// and the attached streams being read. Twice the stream, at least `max_body`, at most this.
    pub max_output: usize,
    /// How deep attached messages are decoded.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input: 64 << 20,
            max_attributes: 100_000,
            max_properties: 200_000,
            max_attachments: 1_000,
            max_body: 16 << 20,
            max_output: 64 << 20,
            max_depth: 4,
        }
    }
}

/// Why a stream was not decoded at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// It does not start with the TNEF signature.
    NotTnef,
    /// It is larger than [`Limits::max_input`].
    TooLarge,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotTnef => write!(f, "not a TNEF stream"),
            Error::TooLarge => write!(f, "the TNEF stream is too large"),
        }
    }
}

impl std::error::Error for Error {}

/// Someone named in the message. `email` is only set for an Internet address (never for an
/// Exchange-internal `/O=…` one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Person {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientKind {
    To,
    Cc,
    /// Bcc, and for meetings a resource (a room).
    Bcc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub person: Person,
    pub kind: RecipientKind,
}

/// Where [`Body::html`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlSource {
    /// `PR_HTML`.
    Html,
    /// HTML Outlook wrapped in RTF (`\fromhtml1`): the original HTML.
    RtfEncapsulated,
    /// Made here of real RTF: paragraphs, bold, italic, underline, strike-through, links.
    Rtf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body {
    /// The body as plain text: `PR_BODY`, else the text of the RTF, else that of the HTML.
    pub text: Option<String>,
    /// The body as HTML, when there is more than text.
    pub html: Option<String>,
    pub html_source: Option<HtmlSource>,
    /// The unpacked RTF, when there was some.
    pub rtf: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// The file name, without directories: the long name where there is one.
    pub name: Option<String>,
    /// `type/subtype`, lower-case.
    pub mime_type: String,
    pub data: Vec<u8>,
    /// Without angle brackets.
    pub content_id: Option<String>,
    pub content_location: Option<String>,
    /// Shown inside the HTML body rather than as a file (`ATT_MHTML_REF`, or hidden with a
    /// content id).
    pub inline: bool,
    /// `PR_ATTACHMENT_HIDDEN`.
    pub hidden: bool,
    /// An attached message (`ATTACH_EMBEDDED_MSG`), decoded.
    pub embedded: Option<Box<Message>>,
    /// Its MAPI properties, but for `PR_ATTACH_DATA`: that is `data` or `embedded`.
    pub properties: Properties,
}

/// A decoded TNEF stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    /// `IPM.Note`, `IPM.Schedule.Meeting.Request`, …
    pub message_class: Option<String>,
    pub subject: Option<String>,
    /// The code page of 8-bit strings (`attOemCodepage`), 1252 when not given.
    pub code_page: u32,
    /// Who it is from (`PR_SENT_REPRESENTING_*`, else `PR_SENDER_*`).
    pub sender: Option<Person>,
    /// Unix seconds.
    pub sent_at: Option<i64>,
    pub body: Body,
    pub attachments: Vec<Attachment>,
    /// The recipient table, when the stream has one (meeting requests usually do).
    pub recipients: Vec<Recipient>,
    /// All MAPI properties of the message.
    pub properties: Properties,
    /// False when the stream was damaged or cut short and only a part could be read.
    pub complete: bool,
    /// attDateStart / attDateEnd, Unix seconds.
    pub(crate) legacy_start: Option<i64>,
    pub(crate) legacy_end: Option<i64>,
}

/// Decodes a TNEF stream with the default [`Limits`].
pub fn decode(data: &[u8]) -> Result<Message, Error> {
    decode_with(data, &Limits::default())
}

/// Decodes a TNEF stream.
pub fn decode_with(data: &[u8], limits: &Limits) -> Result<Message, Error> {
    if data.len() > limits.max_input {
        return Err(Error::TooLarge);
    }
    if !is_tnef(data) {
        return Err(Error::NotTnef);
    }
    let mut budget = Budget {
        attributes: limits.max_attributes,
        properties: limits.max_properties,
        output: data.len().saturating_mul(2).max(limits.max_body).min(limits.max_output),
        exhausted: false,
    };
    let mut message = decode_stream(data, limits, &mut budget, 0);
    message.complete &= !budget.exhausted;
    Ok(message)
}

/// What is left for the whole stream, shared by the messages attached in it: each has its own
/// [`Limits::max_body`], but a few kilobytes of nested, well-packed RTF must not add up to
/// gigabytes.
struct Budget {
    attributes: usize,
    properties: usize,
    /// Bytes of output.
    output: usize,
    /// Something was left out because `output` ran out.
    exhausted: bool,
}

impl Budget {
    /// How much one more piece of output may take, at most `max`.
    fn room(&self, max: usize) -> usize {
        self.output.min(max)
    }

    /// Takes `n` bytes when they fit.
    fn take(&mut self, n: usize) -> bool {
        if n > self.output {
            self.exhausted = true;
            return false;
        }
        self.output -= n;
        true
    }

    /// Counts `n` bytes already made within [`Budget::room`] (give or take a few closing tags).
    fn spend(&mut self, n: usize) {
        self.output = self.output.saturating_sub(n);
    }
}

// TNEF attribute ids (MS-OXTNEF 2.1.3.2): the id and, in the high word, the type.
const ATT_SUBJECT: u32 = 0x0001_8004;
const ATT_DATE_SENT: u32 = 0x0003_8005;
const ATT_DATE_START: u32 = 0x0003_0006;
const ATT_DATE_END: u32 = 0x0003_0007;
const ATT_MESSAGE_CLASS: u32 = 0x0007_8008;
const ATT_BODY: u32 = 0x0002_800C;
const ATT_ATTACH_DATA: u32 = 0x0006_800F;
const ATT_ATTACH_TITLE: u32 = 0x0001_8010;
const ATT_ATTACH_TRANSPORT_FILENAME: u32 = 0x0006_9001;
const ATT_ATTACH_REND_DATA: u32 = 0x0006_9002;
const ATT_MSG_PROPS: u32 = 0x0006_9003;
const ATT_RECIP_TABLE: u32 = 0x0006_9004;
const ATT_ATTACHMENT: u32 = 0x0006_9005;
const ATT_OEM_CODEPAGE: u32 = 0x0006_9007;

const LEVEL_MESSAGE: u8 = 1;
const LEVEL_ATTACHMENT: u8 = 2;

/// An attachment as read, its data still in the stream: copied only once it is kept.
#[derive(Default)]
struct RawAttachment<'a> {
    title: Option<String>,
    data: Option<&'a [u8]>,
    props: Properties,
}

fn decode_stream(data: &[u8], limits: &Limits, budget: &mut Budget, depth: usize) -> Message {
    let mut reader = Reader::new(data);
    let mut message = Message { code_page: 1252, complete: true, ..Message::default() };
    // Signature and legacy key.
    if reader.skip(6).is_err() {
        message.complete = false;
        return message;
    }
    let mut legacy_class = None;
    let mut legacy_subject = None;
    let mut legacy_body = None;
    let mut legacy_sent = None;
    let mut raw_attachments: Vec<RawAttachment> = Vec::new();
    let mut too_many_attachments = false;
    while !reader.is_empty() {
        if budget.attributes == 0 {
            message.complete = false;
            break;
        }
        budget.attributes -= 1;
        let header = (|| Ok::<_, reader::Eof>((reader.u8()?, reader.u32()?, reader.u32()? as usize)))();
        let Ok((level, id, len)) = header else {
            message.complete = false;
            break;
        };
        let Ok(value) = reader.take(len) else {
            message.complete = false;
            break;
        };
        // The checksum; a wrong one is forgiven, a missing one ends the stream.
        if reader.u16().is_err() {
            message.complete = false;
        }
        let cp = message.code_page;
        match (level, id) {
            (LEVEL_MESSAGE, ATT_OEM_CODEPAGE) => {
                if let Some(bytes) = value.get(..4) {
                    let page = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    if page != 0 {
                        message.code_page = page;
                    }
                }
            }
            (LEVEL_MESSAGE, ATT_MESSAGE_CLASS) => legacy_class = Some(codepage::string8(cp, value)),
            (LEVEL_MESSAGE, ATT_SUBJECT) => legacy_subject = Some(codepage::string8(cp, value)),
            (LEVEL_MESSAGE, ATT_BODY) => legacy_body = Some(codepage::string8(cp, value)),
            (LEVEL_MESSAGE, ATT_DATE_SENT) => legacy_sent = dtr(value),
            (LEVEL_MESSAGE, ATT_DATE_START) => message.legacy_start = dtr(value),
            (LEVEL_MESSAGE, ATT_DATE_END) => message.legacy_end = dtr(value),
            (LEVEL_MESSAGE, ATT_MSG_PROPS) => {
                let (props, complete) = mapi::parse_block(value, cp, &mut budget.properties);
                message.properties.0.extend(props.0);
                message.complete &= complete;
            }
            (LEVEL_MESSAGE, ATT_RECIP_TABLE) => {
                let (rows, complete) = mapi::parse_rows(value, cp, &mut budget.properties);
                message.complete &= complete;
                message.recipients.extend(rows.iter().filter_map(recipient));
            }
            (LEVEL_ATTACHMENT, _) => {
                if id == ATT_ATTACH_REND_DATA || raw_attachments.is_empty() {
                    if raw_attachments.len() >= limits.max_attachments {
                        too_many_attachments = true;
                        continue;
                    }
                    raw_attachments.push(RawAttachment::default());
                }
                if too_many_attachments {
                    continue;
                }
                let Some(current) = raw_attachments.last_mut() else { continue };
                match id {
                    ATT_ATTACH_TITLE | ATT_ATTACH_TRANSPORT_FILENAME => {
                        if current.title.is_none() || id == ATT_ATTACH_TRANSPORT_FILENAME {
                            current.title = Some(codepage::string8(cp, value));
                        }
                    }
                    ATT_ATTACH_DATA => current.data = Some(value),
                    ATT_ATTACHMENT => {
                        let (props, complete) = mapi::parse_block(value, cp, &mut budget.properties);
                        current.props.0.extend(props.0);
                        message.complete &= complete;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if too_many_attachments {
        message.complete = false;
    }

    let props = &message.properties;
    message.message_class = props.str(PR_MESSAGE_CLASS).map(str::to_owned).or(legacy_class).filter(|c| !c.is_empty());
    message.subject = props.str(PR_SUBJECT).map(str::to_owned).or(legacy_subject);
    message.sent_at = props.tag(PR_CLIENT_SUBMIT_TIME).and_then(Value::as_time).or(legacy_sent);
    message.sender = person(
        props,
        PR_SENT_REPRESENTING_NAME,
        PR_SENT_REPRESENTING_ADDRTYPE,
        PR_SENT_REPRESENTING_EMAIL_ADDRESS,
        PR_SENT_REPRESENTING_SMTP_ADDRESS,
    )
    .or_else(|| person(props, PR_SENDER_NAME, PR_SENDER_ADDRTYPE, PR_SENDER_EMAIL_ADDRESS, PR_SENDER_SMTP_ADDRESS));
    let (body, complete) = body(props, legacy_body, message.code_page, limits, budget);
    message.body = body;
    message.complete &= complete;
    let exhausted = budget.exhausted;
    message.attachments =
        raw_attachments.into_iter().filter_map(|raw| attachment(raw, limits, budget, depth)).collect();
    if budget.exhausted && !exhausted {
        message.complete = false;
    }
    message
}

/// A DTR (seven 16-bit fields: year, month, day, hour, minute, second, weekday) as Unix seconds.
fn dtr(value: &[u8]) -> Option<i64> {
    let mut r = Reader::new(value);
    let (y, mo, d, h, mi, s) =
        (r.u16().ok()?, r.u16().ok()?, r.u16().ok()?, r.u16().ok()?, r.u16().ok()?, r.u16().ok()?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 || y < 1601 {
        return None;
    }
    let days = time::days_from_civil(i64::from(y), u32::from(mo), u32::from(d));
    Some(days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s))
}

/// An Internet address, never an Exchange-internal one.
pub(crate) fn internet_address(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches("SMTP:").trim_start_matches("smtp:").trim();
    let value = value.strip_prefix("mailto:").unwrap_or(value);
    let ok = value.len() <= 254
        && value.contains('@')
        && !value.starts_with('/')
        && !value.starts_with('@')
        && !value.ends_with('@')
        && value.chars().all(|c| !c.is_whitespace() && !c.is_control() && !"<>\"(),;:[]\\".contains(c));
    ok.then(|| value.to_owned())
}

fn person(props: &Properties, name: u16, addrtype: u16, email: u16, smtp: u16) -> Option<Person> {
    let name = props.str(name).map(|n| n.trim().to_owned());
    let email = props.str(smtp).and_then(internet_address).or_else(|| {
        let kind = props.str(addrtype).unwrap_or("SMTP");
        kind.eq_ignore_ascii_case("SMTP").then(|| props.str(email).and_then(internet_address)).flatten()
    });
    (name.is_some() || email.is_some()).then_some(Person { name, email })
}

fn recipient(row: &Properties) -> Option<Recipient> {
    let person = person(row, PR_DISPLAY_NAME, PR_ADDRTYPE, PR_EMAIL_ADDRESS, PR_SMTP_ADDRESS)?;
    let kind = match row.tag(PR_RECIPIENT_TYPE).and_then(Value::as_i64).map(|t| t & 0xF) {
        Some(2) => RecipientKind::Cc,
        Some(3) => RecipientKind::Bcc,
        _ => RecipientKind::To,
    };
    Some(Recipient { person, kind })
}

/// The body, and whether all of it fit.
fn body(
    props: &Properties,
    legacy: Option<String>,
    code_page: u32,
    limits: &Limits,
    budget: &mut Budget,
) -> (Body, bool) {
    let mut complete = true;
    let mut body = Body {
        text: props.str(PR_BODY).map(str::to_owned).or(legacy).filter(|t| !t.trim().is_empty()),
        ..Body::default()
    };
    if let Some(bytes) = props.tag(PR_HTML).and_then(Value::as_bytes).filter(|b| !b.is_empty()) {
        let room = budget.room(limits.max_body);
        if bytes.len() > room {
            complete = false;
            budget.exhausted |= room < limits.max_body;
        }
        let bytes = &bytes[..bytes.len().min(room)];
        let encoding = props
            .tag(PR_INTERNET_CPID)
            .and_then(Value::as_i64)
            .filter(|cp| *cp > 0 && *cp < 100_000)
            .map(|cp| codepage::encoding(cp as u32))
            .or_else(|| html::meta_charset(bytes).and_then(|label| codepage::from_label(&label)))
            .unwrap_or_else(|| {
                if std::str::from_utf8(bytes).is_ok() { encoding_rs::UTF_8 } else { codepage::encoding(code_page) }
            });
        let html = encoding.decode_without_bom_handling(bytes).0;
        // A code page can make a byte three: what does not fit is cut.
        let mut html = html.trim_end_matches('\0').to_owned();
        if html.len() > room {
            complete = false;
            html.truncate(floor_char_boundary(&html, room));
        }
        budget.spend(html.len());
        body.html = Some(html);
        body.html_source = Some(HtmlSource::Html);
    }
    let rtf = props
        .tag(PR_RTF_COMPRESSED)
        .and_then(Value::as_bytes)
        .map(|packed| rtf::decompress(packed, budget.room(limits.max_body)));
    if let Some(Err(rtf::RtfError::TooLarge)) = rtf {
        complete = false;
        budget.exhausted |= budget.output < limits.max_body;
    }
    if let Some(Ok(rtf)) = rtf {
        budget.spend(rtf.len());
        let (content, whole) = rtf::convert_bounded(&rtf, budget.room(limits.max_body));
        complete &= whole;
        budget.exhausted |= !whole && budget.output < limits.max_body;
        budget.spend(match &content {
            rtf::Content::Html(text) | rtf::Content::Text(text) => text.len(),
            rtf::Content::Rtf { text, html } => text.len() + html.len(),
        });
        match content {
            rtf::Content::Html(html) => {
                if body.html.is_none() && !html.trim().is_empty() {
                    body.html = Some(html);
                    body.html_source = Some(HtmlSource::RtfEncapsulated);
                }
            }
            rtf::Content::Text(text) => {
                if body.text.is_none() && !text.trim().is_empty() {
                    body.text = Some(text);
                }
            }
            rtf::Content::Rtf { text, html } => {
                if body.text.is_none() && !text.trim().is_empty() {
                    body.text = Some(text);
                }
                if body.html.is_none() && !html.trim().is_empty() {
                    body.html = Some(html);
                    body.html_source = Some(HtmlSource::Rtf);
                }
            }
        }
        body.rtf = Some(rtf);
    }
    if body.text.is_none()
        && let Some(html) = &body.html
    {
        // The text is never longer than the HTML it is made of.
        if budget.take(html.len()) {
            let text = html::to_text(html);
            budget.output += html.len() - text.len().min(html.len());
            body.text = Some(text).filter(|t| !t.is_empty());
        } else {
            complete = false;
        }
    }
    (body, complete)
}

/// The largest index up to `max` that starts a character of `text`.
fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut cut = max.min(text.len());
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

fn attachment(raw: RawAttachment<'_>, limits: &Limits, budget: &mut Budget, depth: usize) -> Option<Attachment> {
    let mut props = raw.props;
    let name = props
        .str(PR_ATTACH_LONG_FILENAME)
        .or_else(|| props.str(PR_ATTACH_FILENAME))
        .or_else(|| props.str(PR_DISPLAY_NAME))
        .map(str::to_owned)
        .or(raw.title)
        .and_then(|n| mime::clean_name(&n));
    let method = props.tag(PR_ATTACH_METHOD).and_then(Value::as_i64);
    // The attached bytes leave the properties, so that they are kept once: as the data, or as
    // the message decoded of them.
    let attached = props.0.iter().rposition(|p| p.id == PropId::Tag(PR_ATTACH_DATA)).map(|i| props.0.remove(i).value);
    props.0.retain(|p| p.id != PropId::Tag(PR_ATTACH_DATA));
    let mut embedded = None;
    let mut spare = None;
    match attached {
        Some(Value::Object(iid, bytes)) if iid == IID_IMESSAGE && is_tnef(&bytes) && depth < limits.max_depth => {
            embedded = embed(&bytes, limits, budget, depth);
            if embedded.is_none() {
                spare = Some(bytes);
            }
        }
        Some(Value::Binary(bytes) | Value::Object(_, bytes)) => spare = Some(bytes),
        _ => {}
    }
    let raw_data = raw.data.filter(|d| !d.is_empty());
    let source = raw_data.or(spare.as_deref()).unwrap_or_default();
    if embedded.is_none() && method == Some(5) && is_tnef(source) && depth < limits.max_depth {
        embedded = embed(source, limits, budget, depth);
    }
    let data = if embedded.is_some() {
        Vec::new()
    } else if budget.take(source.len()) {
        match raw_data {
            Some(d) => d.to_vec(),
            None => spare.unwrap_or_default(),
        }
    } else {
        Vec::new()
    };
    if data.is_empty() && embedded.is_none() {
        return None;
    }
    let content_id = props
        .str(PR_ATTACH_CONTENT_ID)
        .map(|c| c.trim().trim_start_matches('<').trim_end_matches('>').to_owned())
        .filter(|c| !c.is_empty() && c.len() <= 998 && !c.chars().any(char::is_control));
    let hidden = props.tag(PR_ATTACHMENT_HIDDEN).and_then(Value::as_bool).unwrap_or(false);
    let flags = props.tag(PR_ATTACH_FLAGS).and_then(Value::as_i64).unwrap_or(0);
    let inline = content_id.is_some() && (flags & 4 != 0 || hidden);
    let mime_type = if embedded.is_some() {
        "message/rfc822".to_owned()
    } else {
        mime::guess(props.str(PR_ATTACH_MIME_TAG), name.as_deref(), &data)
    };
    Some(Attachment {
        name,
        mime_type,
        data,
        content_id,
        content_location: props.str(PR_ATTACH_CONTENT_LOCATION).map(str::to_owned),
        inline,
        hidden,
        embedded,
        properties: props,
    })
}

/// The message in an attached stream. The stream is held while it is read, so it counts against
/// the budget until then.
fn embed(bytes: &[u8], limits: &Limits, budget: &mut Budget, depth: usize) -> Option<Box<Message>> {
    if !budget.take(bytes.len()) {
        return None;
    }
    let message = decode_stream(bytes, limits, budget, depth + 1);
    budget.output += bytes.len();
    Some(Box::new(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_tnef() {
        assert_eq!(decode(b"hello"), Err(Error::NotTnef));
        assert_eq!(decode(&[]), Err(Error::NotTnef));
        let limits = Limits { max_input: 3, ..Limits::default() };
        assert_eq!(decode_with(&[0x78, 0x9f, 0x3e, 0x22, 0, 0], &limits), Err(Error::TooLarge));
        let empty = decode(&[0x78, 0x9f, 0x3e, 0x22, 0, 0]).unwrap();
        assert!(empty.complete);
        assert!(empty.attachments.is_empty());
    }

    #[test]
    fn addresses() {
        assert_eq!(internet_address(" SMTP:nyu@example.com "), Some("nyu@example.com".into()));
        assert_eq!(internet_address("/O=EXCHANGE/OU=X/CN=RECIPIENTS/CN=NYU"), None);
        assert_eq!(internet_address("a b@example.com"), None);
        assert_eq!(internet_address("x@example.com\r\nBCC: y"), None);
    }
}
