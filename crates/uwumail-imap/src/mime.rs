//! The MIME structure of a raw message with exact byte ranges, as FETCH needs them: IMAP clients
//! download single parts by number and expect the bytes exactly as they are in the message.

use std::ops::Range;

/// Nesting deeper than this is treated as a leaf, so crafted messages cannot exhaust the stack.
const MAX_DEPTH: usize = 32;
const MAX_PARTS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The header, with the empty line that ends it.
    pub header: Range<usize>,
    pub body: Range<usize>,
    /// Lowercase.
    pub media_type: String,
    pub subtype: String,
    pub params: Vec<(String, String)>,
    pub id: Option<String>,
    pub description: Option<String>,
    pub encoding: Option<String>,
    pub md5: Option<String>,
    pub disposition: Option<(String, Vec<(String, String)>)>,
    pub language: Option<Vec<String>>,
    pub location: Option<String>,
    pub children: Vec<Part>,
    /// The message inside a message/rfc822 part.
    pub message: Option<Box<Part>>,
    /// Lines of a text body or an enclosed message.
    pub lines: usize,
}

impl Part {
    pub fn is_multipart(&self) -> bool {
        self.media_type == "multipart"
    }

    /// The part with the IMAP part number `path` (like `[1, 2]` for `1.2`).
    pub fn find(&self, path: &[u32]) -> Option<&Part> {
        let mut current = self;
        for (depth, number) in path.iter().enumerate() {
            let container = match (&current.message, depth) {
                (Some(message), d) if d > 0 => message,
                _ => current,
            };
            current = if container.is_multipart() {
                container.children.get(*number as usize - 1)?
            } else if *number == 1 {
                container
            } else {
                return None;
            };
        }
        Some(current)
    }
}

/// One header field: its name and the value with folding removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field<'a> {
    pub name: &'a str,
    pub value: String,
    /// The whole field as it is in the message, with its line break.
    pub raw: &'a [u8],
}

/// Where the header ends: after the first empty line, or at the end when there is none.
fn header_end(raw: &[u8], range: &Range<usize>) -> usize {
    let bytes = &raw[range.clone()];
    if bytes.starts_with(b"\r\n") {
        return range.start + 2;
    }
    if bytes.starts_with(b"\n") {
        return range.start + 1;
    }
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if bytes[i + 1..].starts_with(b"\r\n") {
                return range.start + i + 3;
            }
            if bytes[i + 1..].starts_with(b"\n") {
                return range.start + i + 2;
            }
        }
        i += 1;
    }
    range.end
}

/// The fields of a header.
pub fn fields<'a>(raw: &'a [u8], header: &Range<usize>) -> Vec<Field<'a>> {
    let bytes = &raw[header.clone()];
    let mut fields = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = start;
        // A field runs until a line that does not start with whitespace.
        loop {
            match bytes[end..].iter().position(|&b| b == b'\n') {
                Some(offset) => end += offset + 1,
                None => {
                    end = bytes.len();
                    break;
                }
            }
            if !matches!(bytes.get(end), Some(b' ' | b'\t')) {
                break;
            }
        }
        let field = &bytes[start..end];
        start = end;
        let Some(colon) = field.iter().position(|&b| b == b':') else { continue };
        let Ok(name) = std::str::from_utf8(&field[..colon]) else { continue };
        let name = name.trim_end();
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        let value = String::from_utf8_lossy(&field[colon + 1..]);
        let value = value.split(['\r', '\n']).map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        fields.push(Field { name, value, raw: field });
    }
    fields
}

fn field<'a>(fields: &'a [Field<'_>], name: &str) -> Option<&'a str> {
    fields.iter().find(|field| field.name.eq_ignore_ascii_case(name)).map(|field| field.value.as_str())
}

/// Splits `value; a=b; c="d"` into the value and its parameters.
fn value_and_params(value: &str) -> (String, Vec<(String, String)>) {
    let mut chars = value.char_indices().peekable();
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    while let Some((_, c)) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            '\\' if in_quotes => {
                current.push(c);
                if let Some((_, next)) = chars.next() {
                    current.push(next);
                }
            }
            ';' if !in_quotes => pieces.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    pieces.push(current);
    let mut pieces = pieces.into_iter();
    let head = pieces.next().unwrap_or_default().trim().to_owned();
    let params = pieces
        .filter_map(|piece| {
            let (name, value) = piece.split_once('=')?;
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            let value = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
                None => value.to_owned(),
            };
            (!name.is_empty()).then_some((name, value))
        })
        .collect();
    (head, params)
}

fn count_lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Ranges of the parts of a multipart body, between the boundary lines.
fn split_multipart(raw: &[u8], body: &Range<usize>, boundary: &str) -> Vec<Range<usize>> {
    let delimiter = format!("--{boundary}");
    let bytes = &raw[body.clone()];
    let mut parts = Vec::new();
    let mut part_start: Option<usize> = None;
    let mut line_start = 0;
    while line_start <= bytes.len() {
        let line_end = bytes[line_start..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |p| line_start + p + 1);
        let line = &bytes[line_start..line_end];
        if let Some(rest) = line.strip_prefix(delimiter.as_bytes()) {
            let rest_text = String::from_utf8_lossy(rest);
            let rest_text = rest_text.trim_end();
            let closing = rest_text == "--";
            if closing || rest_text.is_empty() {
                if let Some(start) = part_start.take() {
                    // The line break before the boundary belongs to the boundary.
                    let mut end = line_start;
                    if end > start && bytes[end - 1] == b'\n' {
                        end -= 1;
                        if end > start && bytes[end - 1] == b'\r' {
                            end -= 1;
                        }
                    }
                    parts.push(body.start + start..body.start + end.max(start));
                }
                if closing {
                    return parts;
                }
                part_start = Some(line_end);
            }
        }
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end;
    }
    if let Some(start) = part_start {
        parts.push(body.start + start..body.end);
    }
    parts
}

struct Parser<'a> {
    raw: &'a [u8],
    parts: usize,
}

impl Parser<'_> {
    fn part(&mut self, range: Range<usize>, default_type: (&str, &str), depth: usize) -> Part {
        self.parts += 1;
        let header = range.start..header_end(self.raw, &range);
        let body = header.end..range.end;
        let fields = fields(self.raw, &header);

        let (media_type, subtype, params) = match field(&fields, "Content-Type") {
            Some(value) => {
                let (head, params) = value_and_params(value);
                match head.split_once('/') {
                    Some((t, s)) if !t.trim().is_empty() && !s.trim().is_empty() => {
                        (t.trim().to_ascii_lowercase(), s.trim().to_ascii_lowercase(), params)
                    }
                    _ => ("text".into(), "plain".into(), params),
                }
            }
            None => (default_type.0.to_owned(), default_type.1.to_owned(), Vec::new()),
        };
        let params = if media_type == "text" && !params.iter().any(|(name, _)| name == "charset") {
            let mut params = params;
            params.push(("charset".into(), "us-ascii".into()));
            params
        } else {
            params
        };
        let disposition = field(&fields, "Content-Disposition").map(|value| {
            let (head, params) = value_and_params(value);
            (head.to_ascii_lowercase(), params)
        });
        let language = field(&fields, "Content-Language")
            .map(|value| value.split(',').map(|tag| tag.trim().to_owned()).filter(|tag| !tag.is_empty()).collect());

        let mut part = Part {
            header,
            body: body.clone(),
            id: field(&fields, "Content-ID").map(str::to_owned),
            description: field(&fields, "Content-Description").map(str::to_owned),
            encoding: field(&fields, "Content-Transfer-Encoding").map(|value| value.to_ascii_lowercase()),
            md5: field(&fields, "Content-MD5").map(str::to_owned),
            location: field(&fields, "Content-Location").map(str::to_owned),
            disposition,
            language,
            children: Vec::new(),
            message: None,
            lines: 0,
            media_type,
            subtype,
            params,
        };
        let deep_enough = depth >= MAX_DEPTH || self.parts >= MAX_PARTS;
        if part.media_type == "multipart" && !deep_enough {
            let boundary = part.params.iter().find(|(name, _)| name == "boundary").map(|(_, value)| value.clone());
            if let Some(boundary) = boundary.filter(|b| !b.is_empty()) {
                let child_default = if part.subtype == "digest" { ("message", "rfc822") } else { ("text", "plain") };
                for range in split_multipart(self.raw, &body, &boundary) {
                    if self.parts >= MAX_PARTS {
                        break;
                    }
                    part.children.push(self.part(range, child_default, depth + 1));
                }
            }
            if part.children.is_empty() {
                // A multipart without parts: IMAP needs at least one, so show the body as text.
                part.children.push(Part {
                    header: body.start..body.start,
                    body: body.clone(),
                    media_type: "text".into(),
                    subtype: "plain".into(),
                    params: vec![("charset".into(), "us-ascii".into())],
                    id: None,
                    description: None,
                    encoding: None,
                    md5: None,
                    disposition: None,
                    language: None,
                    location: None,
                    children: Vec::new(),
                    message: None,
                    lines: count_lines(&self.raw[body.clone()]),
                });
            }
        } else if part.media_type == "message" && part.subtype == "rfc822" && !deep_enough {
            part.message = Some(Box::new(self.part(body.clone(), ("text", "plain"), depth + 1)));
            part.lines = count_lines(&self.raw[body]);
        } else if part.media_type == "text" {
            part.lines = count_lines(&self.raw[body]);
        }
        part
    }
}

/// The structure of a whole message.
pub fn parse(raw: &[u8]) -> Part {
    Parser { raw, parts: 0 }.part(0..raw.len(), ("text", "plain"), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = "From: Nyu <nyu@example.org>\r\n\
Subject: Katzenbilder\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
Preamble\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=inner\r\n\
\r\n\
--inner\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hallo Mini!\r\n\
Zweite Zeile\r\n\
--inner\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>Hallo</p>\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: message/rfc822\r\n\
\r\n\
Subject: Weitergeleitet\r\n\
\r\n\
Innen\r\n\
--outer\r\n\
Content-Type: image/png; name=\"nyu.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=\"nyu.png\"\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--outer--\r\n\
Epilog\r\n";

    fn text<'a>(raw: &'a [u8], range: &Range<usize>) -> &'a str {
        std::str::from_utf8(&raw[range.clone()]).unwrap()
    }

    #[test]
    fn nested_parts_have_exact_ranges() {
        let raw = MIXED.as_bytes();
        let root = parse(raw);
        assert_eq!((root.media_type.as_str(), root.subtype.as_str()), ("multipart", "mixed"));
        assert_eq!(root.children.len(), 3);
        assert!(text(raw, &root.header).ends_with("boundary=\"outer\"\r\n\r\n"));

        let plain = root.find(&[1, 1]).unwrap();
        assert_eq!(text(raw, &plain.body), "Hallo Mini!\r\nZweite Zeile");
        assert_eq!(text(raw, &plain.header), "Content-Type: text/plain; charset=utf-8\r\n\r\n");
        assert_eq!(plain.params, vec![("charset".to_owned(), "utf-8".to_owned())]);
        assert_eq!(plain.lines, 1);

        let html = root.find(&[1, 2]).unwrap();
        assert_eq!(text(raw, &html.body), "<p>Hallo</p>");
        assert_eq!(
            html.params,
            vec![("charset".to_owned(), "us-ascii".to_owned())],
            "text parts get a default charset"
        );

        let alternative = root.find(&[1]).unwrap();
        assert!(
            text(raw, &alternative.body).starts_with("--inner\r\n")
                && text(raw, &alternative.body).ends_with("--inner--")
        );

        let forwarded = root.find(&[2]).unwrap();
        let inner = forwarded.message.as_ref().unwrap();
        assert_eq!(text(raw, &inner.header), "Subject: Weitergeleitet\r\n\r\n");
        assert_eq!(text(raw, &inner.body), "Innen");
        assert_eq!(root.find(&[2, 1]).unwrap().body, inner.body, "2.1 is the body of the enclosed message");

        let image = root.find(&[3]).unwrap();
        assert_eq!(image.encoding.as_deref(), Some("base64"));
        assert_eq!(image.disposition, Some(("attachment".into(), vec![("filename".into(), "nyu.png".into())])));
        assert_eq!(text(raw, &image.body), "iVBORw0KGgo=");
        assert!(root.find(&[4]).is_none());
    }

    #[test]
    fn single_part_messages_are_part_one() {
        let raw = b"Subject: Hi\r\n\r\nNur Text\r\n";
        let root = parse(raw);
        assert_eq!(root.find(&[1]), Some(&root));
        assert_eq!(text(raw, &root.body), "Nur Text\r\n");
        assert_eq!((root.media_type.as_str(), root.lines), ("text", 1));
    }

    #[test]
    fn headers_unfold_and_keep_their_raw_form() {
        let raw = b"Subject: lange\r\n Zeile\r\nX-Leer:\r\n\r\nText";
        let root = parse(raw);
        let fields = fields(raw, &root.header);
        assert_eq!(fields[0].value, "lange Zeile");
        assert_eq!(fields[0].raw, b"Subject: lange\r\n Zeile\r\n");
        assert_eq!(fields[1].value, "");
    }

    #[test]
    fn broken_messages_still_have_a_structure() {
        let root = parse(b"Content-Type: multipart/mixed\r\n\r\nkein boundary");
        assert_eq!(root.children.len(), 1);
        let headers_only = parse(b"Subject: nur Kopf");
        assert!(headers_only.body.is_empty());
        let mut deep = String::new();
        for i in 0..100 {
            deep.push_str(&format!("Content-Type: multipart/mixed; boundary=b{i}\r\n\r\n--b{i}\r\n"));
        }
        parse(deep.as_bytes());
    }
}
