//! The company footer an admin can make mandatory for a domain (docs/signatures.md): appended by
//! the server to every message sent from the domain, through JMAP and SMTP submission alike, before
//! the message is DKIM-signed.
//!
//! Only the message's own text is touched: the plain text and the HTML part of the body, both
//! parts of a `multipart/alternative`, the first part of a `multipart/mixed` or `related` (the
//! body before the attachments). A changed part is written anew as UTF-8, `7bit` when it is plain
//! ASCII with short lines, else quoted-printable; every other byte of the message stays as it was.
//! Signed or encrypted mail (S/MIME, PGP/MIME, inline PGP) is left alone: a footer would break the
//! signature or end up outside the encryption. A part that already carries the footer gets it no
//! second time.

use mail_parser::decoders::base64::base64_decode;
use mail_parser::decoders::charsets::map::charset_decoder;
use mail_parser::decoders::html::html_to_text;
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{Encoding, Message, MessageParser, MimeHeaders, PartType};
use uwumail_store::{SignatureText, escape_html};

use crate::headers;

/// What became of a message.
#[derive(Debug, PartialEq, Eq)]
pub enum Footer {
    /// The footer went in; the message as it is to be sent.
    Added(Vec<u8>),
    /// Every text part has it already.
    AlreadyThere,
    /// Left alone, and why.
    Skipped(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Html,
}

struct Target {
    part: usize,
    kind: Kind,
}

/// MIME types that are a signature or encryption over the message.
fn is_secured(ctype: &str, subtype: &str) -> bool {
    matches!(
        (ctype, subtype),
        ("multipart", "signed")
            | ("multipart", "encrypted")
            | ("application", "pkcs7-mime")
            | ("application", "x-pkcs7-mime")
            | ("application", "pkcs7-signature")
            | ("application", "pgp-encrypted")
            | ("application", "pgp-signature")
    )
}

fn content_type(message: &Message<'_>, part: usize) -> (String, String) {
    message.parts[part]
        .content_type()
        .map(|ct| (ct.ctype().to_ascii_lowercase(), ct.subtype().unwrap_or_default().to_ascii_lowercase()))
        .unwrap_or_else(|| ("text".into(), "plain".into()))
}

/// The body parts the footer goes into, or why none can take it.
fn find_targets(message: &Message<'_>, part: usize, depth: usize, out: &mut Vec<Target>) -> Result<(), &'static str> {
    if depth > 8 {
        return Ok(());
    }
    let (ctype, subtype) = content_type(message, part);
    if is_secured(&ctype, &subtype) {
        return Err("signed or encrypted");
    }
    let mime = &message.parts[part];
    if mime.content_disposition().is_some_and(|d| d.is_attachment()) {
        return Ok(());
    }
    match &mime.body {
        PartType::Text(_) if ctype == "text" && subtype == "plain" => out.push(Target { part, kind: Kind::Plain }),
        PartType::Html(_) => out.push(Target { part, kind: Kind::Html }),
        PartType::Multipart(children) => {
            let children: Vec<usize> = children.iter().map(|&id| id as usize).collect();
            // Secured mail anywhere is left alone as a whole.
            for &child in &children {
                let (ctype, subtype) = content_type(message, child);
                if is_secured(&ctype, &subtype) {
                    return Err("signed or encrypted");
                }
            }
            if subtype == "alternative" {
                for child in children {
                    let mut found = Vec::new();
                    find_targets(message, child, depth + 1, &mut found)?;
                    for target in found {
                        if !out.iter().any(|known| known.kind == target.kind) {
                            out.push(target);
                        }
                    }
                }
            } else if let Some(&first) = children.first() {
                find_targets(message, first, depth + 1, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The decoded text of a part, or `None` when it cannot be read back faithfully.
fn decode(message: &Message<'_>, part: usize, raw: &[u8]) -> Option<String> {
    let mime = &message.parts[part];
    let body = raw.get(mime.offset_body as usize..mime.offset_end as usize)?;
    let bytes = match mime.encoding {
        Encoding::QuotedPrintable => quoted_printable_decode(body)?,
        Encoding::Base64 => base64_decode(body)?,
        Encoding::None => body.to_vec(),
    };
    let charset = mime
        .content_type()
        .and_then(|ct| ct.attribute("charset"))
        .map(|c| c.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match charset.as_str() {
        "" | "us-ascii" | "ascii" | "utf-8" | "utf8" => String::from_utf8(bytes).ok(),
        other => charset_decoder(other.as_bytes()).map(|decode| decode(&bytes)),
    }
}

/// Collapses whitespace, so a footer re-wrapped by a mail program is still recognised.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n")
}

/// The plain text with the footer below it.
fn plain_with_footer(body: &str, footer: &str) -> String {
    let body = crlf(body);
    let mut out = body.trim_end_matches(['\r', '\n']).to_owned();
    out.push_str("\r\n\r\n");
    out.push_str(crlf(footer).trim_end_matches(['\r', '\n']));
    out.push_str("\r\n");
    out
}

/// The HTML with the footer at the end of its body.
fn html_with_footer(body: &str, footer: &str) -> String {
    let block = format!("<div class=\"uwumail-footer\">{footer}</div>");
    // ASCII lower-casing keeps every byte where it is, so positions carry over.
    let lower = body.to_ascii_lowercase();
    let at = lower.rfind("</body").or_else(|| lower.rfind("</html"));
    let mut out = String::with_capacity(body.len() + block.len() + 2);
    match at {
        Some(at) => {
            out.push_str(&body[..at]);
            out.push_str(&block);
            out.push_str(&body[at..]);
        }
        None => {
            out.push_str(body.trim_end_matches(['\r', '\n']));
            out.push_str("\r\n");
            out.push_str(&block);
            out.push_str("\r\n");
        }
    }
    crlf(&out)
}

/// Quoted-printable (RFC 2045, 6.7) with CRLF line breaks and lines of at most 76 characters.
fn quoted_printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for (index, line) in text.split("\r\n").enumerate() {
        if index > 0 {
            out.push_str("\r\n");
        }
        let bytes = line.as_bytes();
        let mut width = 0;
        for (i, &byte) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let plain = (byte == b' ' || byte == b'\t') && !last || (33..=126).contains(&byte) && byte != b'=';
            let piece = if plain { (byte as char).to_string() } else { format!("={byte:02X}") };
            if width + piece.len() > 75 {
                out.push_str("=\r\n");
                width = 0;
            }
            out.push_str(&piece);
            width += piece.len();
        }
    }
    out
}

/// The part's new header block: everything it had but its type and encoding, then those anew.
fn part_headers(original: &[u8], kind: Kind, format: Option<&str>, encoding: &str, top: bool) -> Vec<u8> {
    let (fields, _) = headers::split(original);
    let mut out = Vec::with_capacity(original.len() + 80);
    let mut mime_version = false;
    for field in &fields {
        if field.name.eq_ignore_ascii_case("Content-Type")
            || field.name.eq_ignore_ascii_case("Content-Transfer-Encoding")
        {
            continue;
        }
        mime_version |= field.name.eq_ignore_ascii_case("MIME-Version");
        out.extend_from_slice(field.raw);
        if !field.raw.ends_with(b"\n") {
            out.extend_from_slice(b"\r\n");
        }
    }
    if top && !mime_version {
        out.extend_from_slice(b"MIME-Version: 1.0\r\n");
    }
    let subtype = if kind == Kind::Plain { "plain" } else { "html" };
    out.extend_from_slice(format!("Content-Type: text/{subtype}; charset=\"utf-8\"").as_bytes());
    if let Some(format) = format {
        out.extend_from_slice(format!("; format={format}").as_bytes());
    }
    out.extend_from_slice(format!("\r\nContent-Transfer-Encoding: {encoding}\r\n\r\n").as_bytes());
    out
}

/// Appends `footer` (placeholders already filled) to the message `raw` (CRLF line endings).
pub fn append(raw: &[u8], footer: &SignatureText) -> Footer {
    if footer.is_empty() {
        return Footer::AlreadyThere;
    }
    let Some(message) = MessageParser::new().parse(raw) else {
        return Footer::Skipped("not a message");
    };
    if message.parts.is_empty() {
        return Footer::Skipped("not a message");
    }
    let mut targets = Vec::new();
    if let Err(reason) = find_targets(&message, 0, 0, &mut targets) {
        return Footer::Skipped(reason);
    }
    if targets.is_empty() {
        return Footer::Skipped("no text to add it to");
    }
    let text_footer = if footer.text.trim().is_empty() { html_to_text(&footer.html) } else { footer.text.clone() };
    let html_footer = if footer.html.trim().is_empty() {
        footer.text.trim_end().lines().map(escape_html).collect::<Vec<_>>().join("<br>")
    } else {
        footer.html.clone()
    };

    let mut replacements: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    let mut already = 0;
    for target in &targets {
        let Some(text) = decode(&message, target.part, raw) else {
            return Footer::Skipped("a charset or encoding it cannot write back");
        };
        if text.contains("-----BEGIN PGP SIGNED MESSAGE-----") || text.contains("-----BEGIN PGP MESSAGE-----") {
            return Footer::Skipped("signed or encrypted");
        }
        let has_it = match target.kind {
            Kind::Plain => squash(&text).contains(&squash(&text_footer)),
            Kind::Html => squash(&text).contains(&squash(&html_footer)),
        };
        if has_it {
            already += 1;
            continue;
        }
        let body = match target.kind {
            Kind::Plain => plain_with_footer(&text, &text_footer),
            Kind::Html => html_with_footer(&text, &html_footer),
        };
        let short_ascii = body.is_ascii() && body.split("\r\n").all(|line| line.len() <= 998);
        let (encoding, mut encoded) =
            if short_ascii { ("7bit", body) } else { ("quoted-printable", quoted_printable(&body)) };
        let mime = &message.parts[target.part];
        let (start, body_start, end) =
            (mime.offset_header as usize, mime.offset_body as usize, mime.offset_end as usize);
        let original_body = &raw[body_start..end];
        // The line break before the next boundary belongs to whichever side it was on.
        if !original_body.ends_with(b"\n") {
            while encoded.ends_with("\r\n") {
                encoded.truncate(encoded.len() - 2);
            }
        } else if !encoded.ends_with("\r\n") {
            encoded.push_str("\r\n");
        }
        let format = mime
            .content_type()
            .and_then(|ct| ct.attribute("format"))
            .filter(|f| target.kind == Kind::Plain && f.eq_ignore_ascii_case("flowed"))
            .map(|_| "flowed");
        let mut part = part_headers(&raw[start..body_start], target.kind, format, encoding, target.part == 0);
        part.extend_from_slice(encoded.as_bytes());
        replacements.push((start, end, part));
    }
    if replacements.is_empty() {
        return if already > 0 { Footer::AlreadyThere } else { Footer::Skipped("no text to add it to") };
    }
    replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut out = raw.to_vec();
    for (start, end, part) in replacements {
        out.splice(start..end, part);
    }
    Footer::Added(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footer() -> SignatureText {
        SignatureText::new("Mustermann GmbH\nAmtsgericht Beispiel HRB 1", "<p><b>Mustermann GmbH</b><br>HRB 1</p>")
    }

    #[test]
    fn a_multi_line_html_footer_is_recognised_after_sending() {
        let footer = SignatureText::new("", "<p>\n  Mustermann GmbH\n</p>");
        let raw = "From: mini@example.org\r\nContent-Type: text/html\r\n\r\n<p>Hi</p>\r\n";
        let Footer::Added(out) = append(raw.as_bytes(), &footer) else { panic!() };
        assert_eq!(append(&out, &footer), Footer::AlreadyThere);
    }

    fn added(raw: &str) -> String {
        match append(raw.replace('\n', "\r\n").as_bytes(), &footer()) {
            Footer::Added(out) => String::from_utf8(out).unwrap(),
            other => panic!("expected the footer, got {other:?}"),
        }
    }

    fn texts(raw: &str) -> (Option<String>, Option<String>) {
        let message = MessageParser::new().parse(raw.as_bytes()).unwrap();
        (message.body_text(0).map(|t| t.into_owned()), message.body_html(0).map(|t| t.into_owned()))
    }

    #[test]
    fn plain_text_gets_it_below_and_the_headers_stay() {
        let out = added("From: mini@example.org\nTo: nyu@example.net\nSubject: Hallo\n\nHallo Nyu,\nbis bald.\n");
        assert!(out.starts_with("From: mini@example.org\r\nTo: nyu@example.net\r\nSubject: Hallo\r\n"), "{out}");
        assert!(out.contains("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=\"utf-8\"\r\nContent-Transfer-Encoding: 7bit\r\n\r\n"));
        assert!(out.ends_with("bis bald.\r\n\r\nMustermann GmbH\r\nAmtsgericht Beispiel HRB 1\r\n"), "{out}");
        // Sending it once more (a retry, a resent message) adds nothing.
        assert_eq!(append(out.as_bytes(), &footer()), Footer::AlreadyThere);
    }

    #[test]
    fn alternative_gets_it_in_both_parts() {
        let raw = "From: mini@example.org\nMIME-Version: 1.0\nContent-Type: multipart/alternative; boundary=\"b1\"\n\n--b1\nContent-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: quoted-printable\n\nGr=C3=BC=C3=9Fe\n--b1\nContent-Type: text/html; charset=utf-8\n\n<html><body><p>Grüße</p></body></html>\n--b1--\n";
        let out = added(raw);
        let (text, html) = texts(&out);
        assert_eq!(text.unwrap().trim_end(), "Grüße\r\n\r\nMustermann GmbH\r\nAmtsgericht Beispiel HRB 1");
        let html = html.unwrap();
        assert!(
            html.contains(
                "<p>Grüße</p><div class=\"uwumail-footer\"><p><b>Mustermann GmbH</b><br>HRB 1</p></div></body>"
            ),
            "{html}"
        );
        assert!(out.contains("--b1--"));
        assert_eq!(append(out.as_bytes(), &footer()), Footer::AlreadyThere);
    }

    #[test]
    fn mixed_with_attachments_only_touches_the_body() {
        let attachment = "JVBERi0xLjQKJcfsj6IKNSAwIG9iago8PC9MZW5ndGggNiAwIFI+PgpzdHJlYW0K";
        let raw = format!(
            "From: mini@example.org\nMIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"m\"\n\n--m\nContent-Type: multipart/alternative; boundary=\"a\"\n\n--a\nContent-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: base64\n\nSGFsbG8gV2VsdA==\n--a\nContent-Type: text/html; charset=utf-8\nContent-Transfer-Encoding: base64\n\nPHA+SGFsbG8gV2VsdDwvcD4=\n--a--\n\n--m\nContent-Type: text/plain; name=\"notes.txt\"\nContent-Disposition: attachment; filename=\"notes.txt\"\n\nnot the body\n--m\nContent-Type: application/pdf; name=\"a.pdf\"\nContent-Disposition: attachment; filename=\"a.pdf\"\nContent-Transfer-Encoding: base64\n\n{attachment}\n--m--\n"
        );
        let out = added(&raw);
        let (text, html) = texts(&out);
        assert!(text.unwrap().starts_with("Hallo Welt\r\n\r\nMustermann GmbH"));
        assert!(html.unwrap().trim_end().ends_with(
            "<p>Hallo Welt</p>\r\n<div class=\"uwumail-footer\"><p><b>Mustermann GmbH</b><br>HRB 1</p></div>"
        ));
        assert!(out.contains("\r\nnot the body\r\n--m\r\n"), "the text attachment is untouched");
        assert!(out.contains(&format!("\r\n\r\n{attachment}\r\n--m--")), "the PDF is byte for byte the same");
        let message = MessageParser::new().parse(out.as_bytes()).unwrap();
        assert_eq!(message.attachment_count(), 2);
    }

    #[test]
    fn html_only_and_non_utf8_charsets_are_written_back_as_utf8() {
        let raw = "From: mini@example.org\nContent-Type: text/html; charset=iso-8859-1\nContent-Transfer-Encoding: 8bit\n\n<p>Gr\u{fc}\u{df}e</p>\n";
        // As the bytes of Latin-1, not UTF-8.
        let latin1: Vec<u8> = raw.replace('\n', "\r\n").chars().map(|c| c as u32 as u8).collect();
        let Footer::Added(out) = append(&latin1, &footer()) else { panic!("expected the footer") };
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("Content-Type: text/html; charset=\"utf-8\"\r\nContent-Transfer-Encoding: quoted-printable")
        );
        let (_, html) = texts(&out);
        assert!(html.unwrap().starts_with("<p>Grüße</p>\r\n<div class=\"uwumail-footer\">"));

        let raw = "From: mini@example.org\nContent-Type: text/plain; charset=x-unknown-thing\n\nHallo\n";
        assert!(matches!(append(raw.replace('\n', "\r\n").as_bytes(), &footer()), Footer::Skipped(_)));
    }

    #[test]
    fn long_lines_and_8bit_text_become_quoted_printable() {
        let long = "x".repeat(1200);
        let out = added(&format!(
            "From: mini@example.org\nContent-Type: text/plain; charset=utf-8; format=flowed\nContent-Transfer-Encoding: 8bit\n\nÄpfel {long}\n"
        ));
        assert!(out.contains(
            "Content-Type: text/plain; charset=\"utf-8\"; format=flowed\r\nContent-Transfer-Encoding: quoted-printable"
        ));
        assert!(out.split("\r\n").all(|line| line.len() <= 998));
        let (text, _) = texts(&out);
        assert!(text.unwrap().starts_with(&format!("Äpfel {long}\r\n\r\nMustermann GmbH")));
    }

    #[test]
    fn wrapped_base64_in_windows_1252_is_read_and_written_back() {
        use base64::Engine;
        // "Grüße aus Köln" in Windows-1252, wrapped as mail programs do.
        let latin: Vec<u8> = "Grüße aus Köln, ".repeat(8).chars().map(|c| c as u32 as u8).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&latin);
        let wrapped: Vec<&str> = encoded.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect();
        let raw = format!(
            "From: mini@example.org\nContent-Type: text/plain; charset=windows-1252\nContent-Transfer-Encoding: base64\n\n{}\n",
            wrapped.join("\n")
        );
        let out = added(&raw);
        assert!(out.contains("Content-Transfer-Encoding: quoted-printable"));
        let (text, _) = texts(&out);
        let text = text.unwrap();
        assert!(text.starts_with("Grüße aus Köln, Grüße"), "{text}");
        assert!(text.trim_end().ends_with("Amtsgericht Beispiel HRB 1"));
    }

    #[test]
    fn signed_and_encrypted_mail_is_left_alone() {
        for ctype in [
            "multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=\"s\"",
            "multipart/signed; protocol=\"application/pgp-signature\"; boundary=\"s\"",
            "multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"s\"",
            "application/pkcs7-mime; smime-type=enveloped-data",
        ] {
            let raw = format!(
                "From: mini@example.org\nContent-Type: {ctype}\n\n--s\nContent-Type: text/plain\n\nHallo\n--s\nContent-Type: application/pkcs7-signature\n\nAAAA\n--s--\n"
            );
            assert_eq!(
                append(raw.replace('\n', "\r\n").as_bytes(), &footer()),
                Footer::Skipped("signed or encrypted"),
                "{ctype}"
            );
        }
        let inline = "From: mini@example.org\n\n-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nHallo\n";
        assert_eq!(append(inline.replace('\n', "\r\n").as_bytes(), &footer()), Footer::Skipped("signed or encrypted"));
        // Signed mail inside a mixed one, too.
        let nested = "From: mini@example.org\nContent-Type: multipart/mixed; boundary=\"m\"\n\n--m\nContent-Type: multipart/signed; boundary=\"s\"\n\n--s\nContent-Type: text/plain\n\nHallo\n--s--\n--m--\n";
        assert_eq!(append(nested.replace('\n', "\r\n").as_bytes(), &footer()), Footer::Skipped("signed or encrypted"));
    }

    #[test]
    fn a_text_only_footer_is_escaped_into_html() {
        let footer = SignatureText::new("A & <B> GmbH", "");
        let raw = "From: mini@example.org\r\nContent-Type: text/html\r\n\r\n<p>Hi</p>";
        let Footer::Added(out) = append(raw.as_bytes(), &footer) else { panic!() };
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("<div class=\"uwumail-footer\">A &amp; &lt;B&gt; GmbH</div>"), "{out}");
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for raw in [
            "",
            "\r\n",
            "From: x\r\n",
            "Content-Type: multipart/mixed; boundary=\"\"\r\n\r\n--\r\n",
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n%%%%",
        ] {
            let _ = append(raw.as_bytes(), &footer());
        }
        let mut deep = String::from("From: x\r\n");
        for i in 0..40 {
            deep.push_str(&format!("Content-Type: multipart/mixed; boundary=\"b{i}\"\r\n\r\n--b{i}\r\n"));
        }
        deep.push_str("Content-Type: text/plain\r\n\r\nHi\r\n");
        let _ = append(deep.as_bytes(), &footer());
    }

    #[test]
    fn quoted_printable_round_trips() {
        let text = "Grüße = schön \t\r\nzweite Zeile mit Leerzeichen am Ende \r\n".to_owned() + &"ö".repeat(100);
        let encoded = quoted_printable(&text);
        assert!(encoded.is_ascii() && encoded.split("\r\n").all(|line| line.len() <= 76));
        assert_eq!(String::from_utf8(quoted_printable_decode(encoded.as_bytes()).unwrap()).unwrap(), text);
    }
}
