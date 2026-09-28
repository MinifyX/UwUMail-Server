//! Limits on the MIME structure of a message, checked before anything parses it in full.
//!
//! `mail-parser` reads a message of any shape without complaint, and that is the problem with a
//! hostile one:
//!
//! - A part of type `message/rfc822` opens a nested message, and there is no limit to how often
//!   that repeats. The result is a chain of messages inside messages that Rust takes apart
//!   recursively when it is dropped. A few hundred kilobytes of `Content-Type: message/rfc822`
//!   lines are enough to overflow a thread's stack, and a stack overflow aborts the whole server
//!   (security-audit-0.16.0 SMTP-1).
//! - Every header field and every part costs ten to forty times its size in memory once parsed,
//!   so a message made of millions of tiny fields or empty parts costs gigabytes (SMTP-4).
//! - A multipart part whose boundary never comes makes the parser search to the end of the
//!   message, once for every such part.
//!
//! [`check`] walks the message the way `mail-parser` 0.11 does -- with its own stream functions,
//! so both agree on where every header block and every part begins and ends -- but it only counts:
//! how deep parts nest, how many there are, how many header fields they have and how much
//! searching for boundaries costs. It keeps nothing, needs no stack of its own worth mentioning,
//! and stops at the first limit it passes. Every place that parses a message it did not write
//! itself goes through [`check`] first, usually by way of [`parse_message`].
//!
//! When `mail-parser` is updated, compare `parsers/message.rs` (`parse_`) with [`scan`]; the tests
//! at the bottom check that both count the same parts.

use std::fmt;

use mail_parser::parsers::MessageStream;
use mail_parser::{ContentType, HeaderName, HeaderValue, Message, MessageParser};

/// How deep parts may nest: a multipart part or an enclosed message is one level each. A message
/// forwarded as an attachment ten times over is about 20 levels deep; mail programs stop long
/// before this.
pub const MAX_MIME_DEPTH: usize = 64;
/// Parts in the whole message, the enclosed messages included.
pub const MAX_MIME_PARTS: usize = 5_000;
/// Header fields in the whole message, those of every part and enclosed message included.
pub const MAX_HEADER_FIELDS: usize = 20_000;
/// How often `mail-parser` opens an enclosed message that is itself encoded (base64 or
/// quoted-printable); deeper ones stay attachments. This is its own constant, repeated here.
const MAX_NESTED_ENCODED: usize = 3;
/// Searching for a boundary that never comes costs a pass to the end of the message. Such passes
/// may add up to this many times the size of the message, plus [`SEARCH_ALLOWANCE`].
const SEARCH_FACTOR: usize = 8;
const SEARCH_ALLOWANCE: usize = 1 << 20;

/// Why a message is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MimeFault {
    TooDeep,
    TooManyParts,
    TooManyHeaderFields,
    /// Boundaries that are announced but never come, over and over.
    TooComplex,
}

impl fmt::Display for MimeFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MimeFault::TooDeep => write!(f, "the message nests parts more than {MAX_MIME_DEPTH} levels deep"),
            MimeFault::TooManyParts => write!(f, "the message has more than {MAX_MIME_PARTS} parts"),
            MimeFault::TooManyHeaderFields => {
                write!(f, "the message has more than {MAX_HEADER_FIELDS} header fields")
            }
            MimeFault::TooComplex => write!(f, "the message has too many parts whose boundary never comes"),
        }
    }
}

impl std::error::Error for MimeFault {}

/// What [`check`] counted; for tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MimeShape {
    pub depth: usize,
    pub parts: usize,
    pub header_fields: usize,
}

/// Whether `raw` stays within the limits above. Cheap: one pass over the message, like parsing
/// it, but without keeping anything.
pub fn check(raw: &[u8]) -> Result<(), MimeFault> {
    shape(raw).map(|_| ())
}

/// The counts [`check`] works with, or the first limit the message passes.
pub fn shape(raw: &[u8]) -> Result<MimeShape, MimeFault> {
    let mut budget = Budget {
        shape: MimeShape::default(),
        search_left: raw.len().saturating_mul(SEARCH_FACTOR).saturating_add(SEARCH_ALLOWANCE),
    };
    scan(raw, MAX_NESTED_ENCODED, 0, &mut budget)?;
    Ok(budget.shape)
}

/// Parses a message that passed [`check`]; `None` for one that did not, or that has no headers
/// at all. Use this instead of `MessageParser::parse` for every message from outside.
pub fn parse_message(raw: &[u8]) -> Option<Message<'_>> {
    if let Err(fault) = check(raw) {
        tracing::debug!(%fault, size = raw.len(), "not parsing a message");
        return None;
    }
    MessageParser::default().parse(raw)
}

struct Budget {
    shape: MimeShape,
    search_left: usize,
}

impl Budget {
    fn part(&mut self) -> Result<(), MimeFault> {
        self.shape.parts += 1;
        if self.shape.parts > MAX_MIME_PARTS { Err(MimeFault::TooManyParts) } else { Ok(()) }
    }

    fn field(&mut self) -> Result<(), MimeFault> {
        self.shape.header_fields += 1;
        if self.shape.header_fields > MAX_HEADER_FIELDS { Err(MimeFault::TooManyHeaderFields) } else { Ok(()) }
    }

    fn depth(&mut self, depth: usize) -> Result<(), MimeFault> {
        self.shape.depth = self.shape.depth.max(depth);
        if depth > MAX_MIME_DEPTH { Err(MimeFault::TooDeep) } else { Ok(()) }
    }

    /// A search for `--boundary` in `rest`. Whether it finds one is decided with a fast search
    /// first; one that finds nothing is charged against the budget and not run at all, since
    /// `mail-parser` would find nothing either and leave the stream where it was.
    fn boundary_ahead(&mut self, rest: &[u8], boundary: &[u8]) -> Result<bool, MimeFault> {
        let mut needle = Vec::with_capacity(boundary.len() + 2);
        needle.extend_from_slice(b"--");
        needle.extend_from_slice(boundary);
        if memchr::memmem::find(rest, &needle).is_some() {
            return Ok(true);
        }
        self.search_left = self.search_left.checked_sub(rest.len()).ok_or(MimeFault::TooComplex)?;
        Ok(false)
    }
}

/// The kinds of part `mail-parser` tells apart, as far as they change the structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Multipart,
    /// In a `multipart/digest`, a part without a Content-Type is an enclosed message.
    Digest,
    Message,
    Other,
}

/// `mail-parser`'s `mime_type`, reduced to what matters here.
fn kind(content_type: Option<&ContentType<'_>>, parent: Kind) -> (bool, Kind) {
    match content_type {
        Some(content_type) => match content_type.ctype() {
            "multipart" if content_type.subtype() == Some("digest") => (true, Kind::Digest),
            "multipart" => (true, Kind::Multipart),
            "message" if [Some("rfc822"), Some("global")].contains(&content_type.subtype()) => (false, Kind::Message),
            _ => (false, Kind::Other),
        },
        None if parent == Kind::Digest => (false, Kind::Message),
        None => (false, Kind::Other),
    }
}

#[derive(Debug)]
struct State {
    kind: Kind,
    boundary: Option<Vec<u8>>,
    /// The index of this part among the parts of the message it belongs to.
    part_id: usize,
}

/// The two header fields that decide the structure of a part.
#[derive(Default)]
struct PartFields<'x> {
    content_type: Option<HeaderValue<'x>>,
    encoding: Option<HeaderValue<'x>>,
    /// Whether the block had a field at all.
    any: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    None,
    Base64,
    QuotedPrintable,
}

/// `mail-parser`'s `parse_`, counting instead of building. `encoded_left` is its `depth`: how many
/// more encoded messages it opens. `depth` is how deep this message itself sits.
fn scan(raw: &[u8], encoded_left: usize, depth: usize, budget: &mut Budget) -> Result<(), MimeFault> {
    let mut stream = MessageStream::new(raw);
    let mut state = State { kind: Kind::Message, boundary: None, part_id: 0 };
    // `Some(parts)` where an enclosed message began: how many parts the message around it had.
    let mut stack: Vec<(State, Option<usize>)> = Vec::new();
    // Parts of the message being read right now.
    let mut parts_here = 0usize;
    let parts_before = budget.shape.parts;
    let mut fields: PartFields<'_>;

    'outer: loop {
        fields = PartFields::default();
        if !read_headers(&mut stream, &mut fields, budget)? {
            break;
        }
        budget.part()?;

        let content_type = fields.content_type.as_ref().and_then(HeaderValue::as_content_type);
        let (is_multipart, mut kind) = kind(content_type, state.kind);

        if is_multipart && let Some(boundary) = content_type.and_then(|c| c.attribute("boundary")) {
            let boundary = boundary.as_bytes();
            let offset = stream.offset();
            if !boundary.is_empty() && budget.boundary_ahead(&raw[offset..], boundary)? {
                if !stream.seek_next_part(boundary) {
                    unreachable!("the boundary was found ahead");
                }
                let part_id = parts_here;
                parts_here += 1;
                stack.push((state, None));
                budget.depth(depth + stack.len())?;
                state = State { kind, boundary: Some(boundary.to_vec()), part_id };
                stream.skip_crlf();
                continue;
            }
            kind = Kind::Other;
        }

        let encoding = match &fields.encoding {
            Some(HeaderValue::Text(encoding)) if encoding.eq_ignore_ascii_case("base64") => Encoding::Base64,
            Some(HeaderValue::Text(encoding)) if encoding.eq_ignore_ascii_case("quoted-printable") => {
                Encoding::QuotedPrintable
            }
            _ => Encoding::None,
        };

        if kind == Kind::Message && encoding == Encoding::None {
            let new_state = State { kind: Kind::Message, boundary: state.boundary.take(), part_id: parts_here };
            parts_here += 1;
            stack.push((state, Some(parts_here)));
            budget.depth(depth + stack.len())?;
            parts_here = 0;
            state = new_state;
            continue;
        }

        let boundary = state.boundary.as_deref().unwrap_or(b"");
        let (offset_end, bytes) = match encoding {
            Encoding::Base64 => stream.decode_base64_mime(boundary),
            Encoding::QuotedPrintable => stream.decode_quoted_printable_mime(boundary),
            Encoding::None => stream.mime_part(boundary),
        };
        if offset_end == usize::MAX {
            // Not decodable: mail-parser keeps it as text and looks for the end of the part.
            let (_, found) = stream.seek_part_end(state.boundary.as_deref());
            if !found {
                state.boundary = None;
            }
        } else if kind == Kind::Message && encoded_left != 0 {
            // An encoded message is decoded and read as a message of its own.
            scan(&bytes, encoded_left - 1, depth + stack.len() + 1, budget)?;
        }
        drop(bytes);
        parts_here += 1;

        if state.boundary.is_some() {
            loop {
                if state.kind == Kind::Message {
                    // The enclosed message ends with the part around it.
                    match stack.pop() {
                        Some((mut outer, Some(outer_parts))) => {
                            parts_here = outer_parts;
                            outer.boundary = state.boundary.take();
                            state = outer;
                        }
                        _ => break 'outer,
                    }
                }
                if stream.is_multipart_end() {
                    if state.part_id < parts_here
                        && let Some((outer, _)) = stack.pop()
                    {
                        state = outer;
                        if let Some(boundary) = state.boundary.clone() {
                            let offset = stream.offset();
                            if budget.boundary_ahead(&raw[offset..], &boundary)? {
                                if stream.seek_next_part_offset(&boundary).is_none() {
                                    unreachable!("the boundary was found ahead");
                                }
                                continue;
                            }
                        }
                    }
                    break 'outer;
                }
                break;
            }
        } else if stream.offset() >= raw.len() {
            break;
        }
    }
    // A header block that runs to the end of the message without an empty line is still a
    // message, of one part.
    if budget.shape.parts == parts_before && fields.any {
        budget.part()?;
    }
    Ok(())
}

/// `mail-parser`'s `parse_headers` with the default parser: the same value parsers for the same
/// fields, so every field ends where it ends there. Only Content-Type and
/// Content-Transfer-Encoding are kept (the last of each, as `header_value` picks). Every field it
/// starts on counts, readable or not.
fn read_headers<'x>(
    stream: &mut MessageStream<'x>,
    fields: &mut PartFields<'x>,
    budget: &mut Budget,
) -> Result<bool, MimeFault> {
    loop {
        loop {
            match stream.peek() {
                Some(b'\n') => {
                    stream.next();
                    return Ok(true);
                }
                None => return Ok(false),
                Some(ch) if !ch.is_ascii_whitespace() => break,
                _ => {
                    stream.next();
                }
            }
        }
        budget.field()?;
        if let Some(name) = stream.parse_header_name() {
            fields.any = true;
            let value = match &name {
                HeaderName::Subject
                | HeaderName::Comments
                | HeaderName::ContentDescription
                | HeaderName::ContentLocation
                | HeaderName::ContentTransferEncoding => stream.parse_unstructured(),
                HeaderName::From
                | HeaderName::To
                | HeaderName::Cc
                | HeaderName::Bcc
                | HeaderName::ReplyTo
                | HeaderName::Sender
                | HeaderName::ResentTo
                | HeaderName::ResentFrom
                | HeaderName::ResentBcc
                | HeaderName::ResentCc
                | HeaderName::ResentSender
                | HeaderName::ListArchive
                | HeaderName::ListHelp
                | HeaderName::ListId
                | HeaderName::ListOwner
                | HeaderName::ListPost
                | HeaderName::ListSubscribe
                | HeaderName::ListUnsubscribe => stream.parse_address(),
                HeaderName::Date | HeaderName::ResentDate => stream.parse_date(),
                HeaderName::MessageId
                | HeaderName::References
                | HeaderName::InReplyTo
                | HeaderName::ReturnPath
                | HeaderName::ContentId
                | HeaderName::ResentMessageId => stream.parse_id(),
                HeaderName::Keywords | HeaderName::ContentLanguage => stream.parse_comma_separared(),
                HeaderName::Received => stream.parse_received(),
                HeaderName::MimeVersion => stream.parse_raw(),
                HeaderName::ContentType | HeaderName::ContentDisposition => stream.parse_content_type(),
                _ => stream.parse_raw(),
            };
            match name {
                HeaderName::ContentType => fields.content_type = Some(value),
                HeaderName::ContentTransferEncoding => fields.encoding = Some(value),
                _ => {}
            }
        } else if stream.is_eof() {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use mail_parser::{MessageParser, PartType};

    use super::*;

    /// Every part `mail-parser` builds, enclosed messages included, and how deep they go.
    fn parsed_shape(message: &Message<'_>) -> (usize, usize) {
        let mut parts = 0;
        let mut depth = 0;
        let mut work = vec![(message, 0usize)];
        while let Some((message, level)) = work.pop() {
            parts += message.parts.len();
            depth = depth.max(level);
            for part in &message.parts {
                if let PartType::Message(inner) = &part.body {
                    work.push((inner, level + 1));
                }
            }
        }
        (parts, depth)
    }

    fn nested_rfc822(levels: usize) -> Vec<u8> {
        let mut raw = Vec::new();
        for _ in 0..levels {
            raw.extend_from_slice(b"Subject: layer\r\nContent-Type: message/rfc822\r\n\r\n");
        }
        raw.extend_from_slice(b"Subject: bottom\r\n\r\nhi\r\n");
        raw
    }

    fn base64(data: &[u8]) -> String {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    const NORMAL: &[&str] = &[
        "From: a@example.org\r\nSubject: plain\r\n\r\nHallo\r\n",
        concat!(
            "From: a@example.org\r\nMIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n",
            "preamble\r\n--outer\r\n",
            "Content-Type: multipart/alternative; boundary=inner\r\n\r\n",
            "--inner\r\nContent-Type: text/plain\r\n\r\nText\r\n",
            "--inner\r\nContent-Type: text/html\r\n\r\n<p>Text</p>\r\n--inner--\r\n",
            "--outer\r\nContent-Type: application/pdf; name=a.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\n",
            "JVBERi0xLjQK\r\n",
            "--outer\r\nContent-Type: message/rfc822\r\n\r\n",
            "From: b@example.org\r\nSubject: forwarded\r\n",
            "Content-Type: multipart/mixed; boundary=deep\r\n\r\n",
            "--deep\r\nContent-Type: text/plain\r\n\r\nInside\r\n--deep--\r\n",
            "--outer--\r\nepilogue\r\n",
        ),
        concat!(
            "From: a@example.org\r\nContent-Type: multipart/digest; boundary=d\r\n\r\n",
            "--d\r\n\r\nFrom: c@example.org\r\nSubject: one\r\n\r\nEins\r\n",
            "--d\r\n\r\nFrom: c@example.org\r\nSubject: two\r\n\r\nZwei\r\n",
            "--d--\r\n",
        ),
        // Broken on purpose: a boundary that never comes, a base64 part that is not base64, and a
        // multipart without its end.
        concat!(
            "From: a@example.org\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n",
            "--x\r\nContent-Type: multipart/related; boundary=never\r\n\r\ntext\r\n",
            "--x\r\nContent-Type: image/png\r\nContent-Transfer-Encoding: base64\r\n\r\n!!==!!\r\n",
            "--x\r\nContent-Type: text/plain\r\n\r\nno end\r\n",
        ),
        "Subject: only headers\r\nContent-Type: message/rfc822\r\n",
        "no header at all",
    ];

    #[test]
    fn counts_what_mail_parser_builds() {
        let mut messages: Vec<Vec<u8>> = NORMAL.iter().map(|m| m.as_bytes().to_vec()).collect();
        let encoded = format!(
            "From: a@example.org\r\nContent-Type: multipart/mixed; boundary=e\r\n\r\n--e\r\n\
             Content-Type: message/rfc822\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--e--\r\n",
            base64(NORMAL[1].as_bytes())
        );
        messages.push(encoded.into_bytes());
        messages.push(nested_rfc822(10));
        for raw in &messages {
            let shape = shape(raw).unwrap();
            match MessageParser::default().parse(raw) {
                Some(message) => {
                    let (parts, depth) = parsed_shape(&message);
                    assert_eq!(shape.parts, parts, "{}", String::from_utf8_lossy(raw));
                    assert!(shape.depth >= depth, "{}", String::from_utf8_lossy(raw));
                }
                None => assert_eq!(shape.parts, 0),
            }
        }
    }

    #[test]
    fn deep_nesting_is_refused() {
        assert_eq!(check(&nested_rfc822(MAX_MIME_DEPTH)), Ok(()));
        assert_eq!(check(&nested_rfc822(MAX_MIME_DEPTH + 1)), Err(MimeFault::TooDeep));
        assert_eq!(check(&nested_rfc822(100_000)), Err(MimeFault::TooDeep));
        assert!(parse_message(&nested_rfc822(100_000)).is_none());

        let mut multipart = Vec::new();
        for level in 0..=MAX_MIME_DEPTH {
            multipart.extend_from_slice(
                format!("Content-Type: multipart/mixed; boundary=b{level}\r\n\r\n--b{level}\r\n").as_bytes(),
            );
        }
        multipart.extend_from_slice(b"\r\nbottom\r\n");
        assert_eq!(check(&multipart), Err(MimeFault::TooDeep));
    }

    /// Hidden inside an encoded message the chain does not show in the raw bytes, and
    /// `mail-parser` copies the decoded message recursively while it parses.
    #[test]
    fn deep_nesting_inside_an_encoded_message_is_refused() {
        let raw = format!(
            "Subject: outside\r\nContent-Type: message/rfc822\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            base64(&nested_rfc822(100_000))
        );
        assert_eq!(check(raw.as_bytes()), Err(MimeFault::TooDeep));
    }

    #[test]
    fn many_parts_are_refused() {
        let parts = |count: usize| {
            let mut raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n".to_vec();
            for _ in 0..count {
                raw.extend_from_slice(b"--b\r\n\r\n");
            }
            raw.extend_from_slice(b"--b--\r\n");
            raw
        };
        // The message itself and the multipart are parts too.
        assert_eq!(check(&parts(MAX_MIME_PARTS - 1)), Ok(()));
        assert_eq!(check(&parts(MAX_MIME_PARTS)), Err(MimeFault::TooManyParts));
        assert_eq!(check(&parts(1_000_000)), Err(MimeFault::TooManyParts));
    }

    #[test]
    fn many_header_fields_are_refused() {
        let fields = |count: usize| {
            let mut raw = Vec::new();
            for _ in 0..count {
                raw.extend_from_slice(b"a:\r\n");
            }
            raw.extend_from_slice(b"\r\nbody\r\n");
            raw
        };
        assert_eq!(check(&fields(MAX_HEADER_FIELDS)), Ok(()));
        assert_eq!(check(&fields(MAX_HEADER_FIELDS + 1)), Err(MimeFault::TooManyHeaderFields));
        // Lines that are no field at all count as well.
        let mut junk = Vec::new();
        for _ in 0..=MAX_HEADER_FIELDS {
            junk.extend_from_slice(b"no colon\r\n");
        }
        assert_eq!(check(&junk), Err(MimeFault::TooManyHeaderFields));
    }

    /// Every part announces a boundary of its own that never comes, so every one of them would
    /// be searched for to the end of the message.
    #[test]
    fn boundaries_that_never_come_are_refused() {
        let mut raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n".to_vec();
        let filler = vec![b'.'; 64 * 1024];
        raw.extend_from_slice(&filler);
        for n in 0..4_000 {
            raw.extend_from_slice(
                format!("\r\n--b\r\nContent-Type: multipart/mixed; boundary=never{n}\r\n\r\nx").as_bytes(),
            );
        }
        raw.extend_from_slice(b"\r\n--b--\r\n");
        assert_eq!(check(&raw), Err(MimeFault::TooComplex));
    }
}
