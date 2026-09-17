//! Writing IMAP data: strings, flags, sequence sets, ENVELOPE, BODYSTRUCTURE and message sections.

use crate::command::{Section, SectionText};
use crate::mime::{self, Part};
use crate::mutf7;

/// Builds one response line (or several, with literals).
#[derive(Debug, Default)]
pub struct Out {
    pub bytes: Vec<u8>,
    /// Whether 8-bit text may go into quoted strings (UTF8=ACCEPT).
    pub utf8: bool,
}

impl Out {
    pub fn new(utf8: bool) -> Out {
        Out { bytes: Vec::new(), utf8 }
    }

    pub fn raw(&mut self, text: &str) -> &mut Self {
        self.bytes.extend_from_slice(text.as_bytes());
        self
    }

    pub fn number(&mut self, n: impl std::fmt::Display) -> &mut Self {
        self.raw(&n.to_string())
    }

    pub fn literal(&mut self, data: &[u8]) -> &mut Self {
        self.raw(&format!("{{{}}}\r\n", data.len()));
        self.bytes.extend_from_slice(data);
        self
    }

    /// A string as a quoted string when it can be one, as a literal otherwise.
    pub fn string(&mut self, data: &[u8]) -> &mut Self {
        let plain = data.iter().all(|&b| b != 0 && b != b'\r' && b != b'\n');
        let encoding_ok = data.is_ascii() || (self.utf8 && std::str::from_utf8(data).is_ok());
        if data.len() > 1024 || !plain || !encoding_ok {
            return self.literal(data);
        }
        self.bytes.push(b'"');
        for &b in data {
            if b == b'"' || b == b'\\' {
                self.bytes.push(b'\\');
            }
            self.bytes.push(b);
        }
        self.bytes.push(b'"');
        self
    }

    pub fn nstring(&mut self, data: Option<&[u8]>) -> &mut Self {
        match data {
            Some(data) => self.string(data),
            None => self.raw("NIL"),
        }
    }

    /// Text for a client: non-ASCII goes out as an encoded word unless the client takes UTF-8.
    pub fn text(&mut self, text: Option<&str>) -> &mut Self {
        match text {
            None => self.raw("NIL"),
            Some(text) if self.utf8 || text.is_ascii() => self.string(text.as_bytes()),
            Some(text) => self.string(encoded_word(text).as_bytes()),
        }
    }

    pub fn mailbox(&mut self, name: &str) -> &mut Self {
        if self.utf8 { self.string(name.as_bytes()) } else { self.string(mutf7::encode(name).as_bytes()) }
    }
}

/// RFC 2047 encoded word, base64, UTF-8.
fn encoded_word(text: &str) -> String {
    use base64::Engine as _;
    format!("=?utf-8?b?{}?=", base64::engine::general_purpose::STANDARD.encode(text))
}

/// `1:3,5,7:9` from sorted numbers.
pub fn sequence_set(numbers: &[u32]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < numbers.len() {
        let start = numbers[i];
        let mut end = start;
        while i + 1 < numbers.len() && numbers[i + 1] == end + 1 {
            i += 1;
            end = numbers[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}:{end}"));
        }
        i += 1;
    }
    out
}

/// IMAP flags for stored keywords.
pub fn flags(keywords: &[String]) -> String {
    let flags: Vec<String> = keywords
        .iter()
        .map(|keyword| match keyword.as_str() {
            "$seen" => "\\Seen".to_owned(),
            "$answered" => "\\Answered".to_owned(),
            "$flagged" => "\\Flagged".to_owned(),
            "$draft" => "\\Draft".to_owned(),
            "$deleted" => "\\Deleted".to_owned(),
            other => other.to_owned(),
        })
        .collect();
    format!("({})", flags.join(" "))
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// INTERNALDATE: `"17-Sep-2026 08:30:00 +0000"`.
pub fn internal_date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let seconds = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "\"{day:02}-{}-{year:04} {:02}:{:02}:{:02} +0000\"",
        MONTHS[month as usize - 1],
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn params_list(out: &mut Out, params: &[(String, String)]) {
    if params.is_empty() {
        out.raw("NIL");
        return;
    }
    out.raw("(");
    for (i, (name, value)) in params.iter().enumerate() {
        if i > 0 {
            out.raw(" ");
        }
        out.string(name.to_ascii_uppercase().as_bytes()).raw(" ").text(Some(value));
    }
    out.raw(")");
}

fn addresses(out: &mut Out, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        out.raw("NIL");
        return;
    };
    let raw = format!("To: {value}\r\n\r\n");
    let parsed = mail_parser::MessageParser::default().parse_headers(raw.as_bytes());
    let list: Vec<(Option<String>, String)> = parsed
        .as_ref()
        .and_then(|message| message.to())
        .map(|address| {
            address
                .iter()
                .filter_map(|addr| {
                    let email = addr.address.as_deref()?.trim().to_owned();
                    let name = addr.name.as_deref().map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned);
                    Some((name, email))
                })
                .collect()
        })
        .unwrap_or_default();
    if list.is_empty() {
        out.raw("NIL");
        return;
    }
    out.raw("(");
    for (name, email) in list {
        let (local, host) = match email.rsplit_once('@') {
            Some((local, host)) => (local.to_owned(), Some(host.to_owned())),
            None => (email, None),
        };
        out.raw("(").text(name.as_deref()).raw(" NIL ").text(Some(&local)).raw(" ").text(host.as_deref()).raw(")");
    }
    out.raw(")");
}

/// ENVELOPE of a message header.
pub fn envelope(out: &mut Out, raw: &[u8], header: &std::ops::Range<usize>) {
    let fields = mime::fields(raw, header);
    let get = |name: &str| {
        fields.iter().find(|field| field.name.eq_ignore_ascii_case(name)).map(|field| field.value.as_str())
    };
    let from = get("From");
    out.raw("(");
    out.nstring(get("Date").map(str::as_bytes)).raw(" ");
    out.nstring(get("Subject").map(str::as_bytes)).raw(" ");
    addresses(out, from);
    out.raw(" ");
    addresses(out, get("Sender").or(from));
    out.raw(" ");
    addresses(out, get("Reply-To").or(from));
    for name in ["To", "Cc", "Bcc"] {
        out.raw(" ");
        addresses(out, get(name));
    }
    out.raw(" ");
    out.nstring(get("In-Reply-To").map(str::as_bytes)).raw(" ");
    out.nstring(get("Message-ID").map(str::as_bytes));
    out.raw(")");
}

/// BODY (without extension data) or BODYSTRUCTURE (with it).
pub fn body_structure(out: &mut Out, raw: &[u8], part: &Part, extended: bool) {
    out.raw("(");
    if part.is_multipart() {
        for child in &part.children {
            body_structure(out, raw, child, extended);
        }
        out.raw(" ").string(part.subtype.to_ascii_uppercase().as_bytes());
        if extended {
            out.raw(" ");
            params_list(out, &part.params);
            extension(out, part);
        }
        out.raw(")");
        return;
    }
    out.string(part.media_type.to_ascii_uppercase().as_bytes()).raw(" ");
    out.string(part.subtype.to_ascii_uppercase().as_bytes()).raw(" ");
    params_list(out, &part.params);
    out.raw(" ").text(part.id.as_deref());
    out.raw(" ").text(part.description.as_deref());
    out.raw(" ").string(part.encoding.as_deref().unwrap_or("7bit").to_ascii_uppercase().as_bytes());
    out.raw(" ").number(part.body.len());
    if let Some(message) = &part.message {
        out.raw(" ");
        envelope(out, raw, &message.header);
        out.raw(" ");
        body_structure(out, raw, message, extended);
        out.raw(" ").number(part.lines);
    } else if part.media_type == "text" {
        out.raw(" ").number(part.lines);
    }
    if extended {
        out.raw(" ").text(part.md5.as_deref());
        extension(out, part);
    }
    out.raw(")");
}

/// Disposition, language and location.
fn extension(out: &mut Out, part: &Part) {
    out.raw(" ");
    match &part.disposition {
        Some((kind, params)) => {
            out.raw("(").string(kind.to_ascii_uppercase().as_bytes()).raw(" ");
            params_list(out, params);
            out.raw(")");
        }
        None => {
            out.raw("NIL");
        }
    }
    out.raw(" ");
    match &part.language {
        Some(tags) if !tags.is_empty() => {
            out.raw("(");
            for (i, tag) in tags.iter().enumerate() {
                if i > 0 {
                    out.raw(" ");
                }
                out.string(tag.as_bytes());
            }
            out.raw(")");
        }
        _ => {
            out.raw("NIL");
        }
    }
    out.raw(" ").text(part.location.as_deref());
}

/// The bytes a section names, or `None` when the message has no such part (they are sent as empty).
pub fn section_bytes(raw: &[u8], root: &Part, section: &Section) -> Option<Vec<u8>> {
    let part = root.find(&section.part)?;
    let (header, body) = match (&section.text, section.part.is_empty()) {
        (None, true) => return Some(raw.to_vec()),
        (None, false) => return Some(raw[part.body.clone()].to_vec()),
        (Some(SectionText::Mime), _) => return Some(raw[part.header.clone()].to_vec()),
        (Some(_), true) => (&root.header, &root.body),
        (Some(_), false) => {
            let message = part.message.as_ref()?;
            (&message.header, &message.body)
        }
    };
    Some(match section.text.as_ref()? {
        SectionText::Header => raw[header.clone()].to_vec(),
        SectionText::Text => raw[body.clone()].to_vec(),
        SectionText::HeaderFields(names) | SectionText::HeaderFieldsNot(names) => {
            let keep = matches!(section.text, Some(SectionText::HeaderFields(_)));
            let mut out = Vec::new();
            for field in mime::fields(raw, header) {
                if names.iter().any(|name| name.eq_ignore_ascii_case(field.name)) == keep {
                    out.extend_from_slice(field.raw);
                    if !field.raw.ends_with(b"\n") {
                        out.extend_from_slice(b"\r\n");
                    }
                }
            }
            out.extend_from_slice(b"\r\n");
            out
        }
        SectionText::Mime => unreachable!("handled above"),
    })
}

/// How a section is named in the answer, like `BODY[1.2.HEADER.FIELDS (FROM)]<0>`.
pub fn section_label(section: &Section, origin: Option<u32>) -> String {
    let mut label = String::from("BODY[");
    let parts: Vec<String> = section.part.iter().map(u32::to_string).collect();
    label.push_str(&parts.join("."));
    if let Some(text) = &section.text {
        if !parts.is_empty() {
            label.push('.');
        }
        match text {
            SectionText::Header => label.push_str("HEADER"),
            SectionText::Text => label.push_str("TEXT"),
            SectionText::Mime => label.push_str("MIME"),
            SectionText::HeaderFields(names) | SectionText::HeaderFieldsNot(names) => {
                let kind =
                    if matches!(text, SectionText::HeaderFields(_)) { "HEADER.FIELDS" } else { "HEADER.FIELDS.NOT" };
                let names: Vec<String> = names.iter().map(|name| quote_field(name)).collect();
                label.push_str(&format!("{kind} ({})", names.join(" ")));
            }
        }
    }
    label.push(']');
    if let Some(origin) = origin {
        label.push_str(&format!("<{origin}>"));
    }
    label
}

fn quote_field(name: &str) -> String {
    if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        name.to_ascii_uppercase()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(f: impl FnOnce(&mut Out)) -> String {
        let mut out = Out::new(false);
        f(&mut out);
        String::from_utf8(out.bytes).unwrap()
    }

    #[test]
    fn strings_quote_or_become_literals() {
        assert_eq!(
            written(|o| {
                o.string(b"Hallo \"Nyu\"");
            }),
            "\"Hallo \\\"Nyu\\\"\""
        );
        assert_eq!(
            written(|o| {
                o.string(b"zwei\r\nZeilen");
            }),
            "{12}\r\nzwei\r\nZeilen"
        );
        assert_eq!(
            written(|o| {
                o.text(Some("Grüße"));
            }),
            "\"=?utf-8?b?R3LDvMOfZQ==?=\""
        );
        assert_eq!(
            written(|o| {
                o.mailbox("Entwürfe");
            }),
            "\"Entw&APw-rfe\""
        );
        let mut utf8 = Out::new(true);
        utf8.mailbox("Entwürfe");
        assert_eq!(String::from_utf8(utf8.bytes).unwrap(), "\"Entwürfe\"");
    }

    #[test]
    fn numbers_flags_and_dates() {
        assert_eq!(sequence_set(&[1, 2, 3, 5, 7, 8]), "1:3,5,7:8");
        assert_eq!(flags(&["$seen".into(), "$forwarded".into()]), "(\\Seen $forwarded)");
        assert_eq!(internal_date(0), "\"01-Jan-1970 00:00:00 +0000\"");
        assert_eq!(internal_date(1_789_634_096), "\"17-Sep-2026 08:34:56 +0000\"");
        assert_eq!(civil_from_days(crate::parser::days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    const MESSAGE: &str = "Date: Thu, 17 Sep 2026 10:00:00 +0200\r\n\
From: Nyu <nyu@example.org>\r\n\
To: Mini <mini@example.de>, leni@example.de\r\n\
Subject: Bilder\r\n\
Message-ID: <1@example.org>\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hallo\r\n\
--b\r\n\
Content-Type: image/png; name=nyu.png\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=nyu.png\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--b--\r\n";

    #[test]
    fn envelope_and_body_structure() {
        let raw = MESSAGE.as_bytes();
        let root = mime::parse(raw);
        assert_eq!(
            written(|o| envelope(o, raw, &root.header)),
            "(\"Thu, 17 Sep 2026 10:00:00 +0200\" \"Bilder\" ((\"Nyu\" NIL \"nyu\" \"example.org\")) \
((\"Nyu\" NIL \"nyu\" \"example.org\")) ((\"Nyu\" NIL \"nyu\" \"example.org\")) \
((\"Mini\" NIL \"mini\" \"example.de\")(NIL NIL \"leni\" \"example.de\")) NIL NIL NIL \"<1@example.org>\")"
        );
        assert_eq!(
            written(|o| body_structure(o, raw, &root, true)),
            "((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 5 0 NIL NIL NIL NIL)\
(\"IMAGE\" \"PNG\" (\"NAME\" \"nyu.png\") NIL NIL \"BASE64\" 12 NIL (\"ATTACHMENT\" (\"FILENAME\" \"nyu.png\")) NIL NIL) \
\"MIXED\" (\"BOUNDARY\" \"b\") NIL NIL NIL)"
        );
        assert_eq!(
            written(|o| body_structure(o, raw, &root, false)),
            "((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 5 0)(\"IMAGE\" \"PNG\" (\"NAME\" \"nyu.png\") NIL NIL \"BASE64\" 12) \"MIXED\")"
        );
    }

    #[test]
    fn sections() {
        let raw = MESSAGE.as_bytes();
        let root = mime::parse(raw);
        let section = |part: Vec<u32>, text: Option<SectionText>| Section { part, text };
        let get = |s: Section| String::from_utf8(section_bytes(raw, &root, &s).unwrap()).unwrap();
        assert_eq!(get(section(vec![1], None)), "Hallo");
        assert_eq!(get(section(vec![2], Some(SectionText::Mime))).lines().count(), 4);
        assert_eq!(
            get(section(vec![], Some(SectionText::HeaderFields(vec!["subject".into(), "FROM".into()])))),
            "From: Nyu <nyu@example.org>\r\nSubject: Bilder\r\n\r\n"
        );
        assert!(get(section(vec![], Some(SectionText::Text))).starts_with("--b\r\n"));
        assert!(section_bytes(raw, &root, &section(vec![3], None)).is_none());
        assert_eq!(
            section_label(
                &section(vec![], Some(SectionText::HeaderFields(vec!["From".into(), "X Odd".into()]))),
                Some(0)
            ),
            "BODY[HEADER.FIELDS (FROM \"X Odd\")]<0>"
        );
    }
}
