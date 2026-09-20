//! Minimal header handling on raw messages, without re-encoding anything.

/// One header field: the name and the full raw bytes including folded lines and CRLF.
pub struct RawHeader<'x> {
    pub name: &'x str,
    pub raw: &'x [u8],
}

impl RawHeader<'_> {
    /// The value with folding removed and surrounding whitespace trimmed.
    pub fn value(&self) -> String {
        let text = String::from_utf8_lossy(self.raw);
        let value = text.split_once(':').map(|(_, v)| v).unwrap_or_default();
        value.split(['\r', '\n']).map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }
}

/// Splits the header block into fields. Returns the fields and the offset where the body starts.
pub fn split(raw: &[u8]) -> (Vec<RawHeader<'_>>, usize) {
    let mut headers = Vec::new();
    let mut pos = 0;
    while pos < raw.len() {
        let line_end = raw[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i + 1).unwrap_or(raw.len());
        let line = &raw[pos..line_end];
        if line == b"\r\n" || line == b"\n" {
            return (headers, line_end);
        }
        if matches!(line.first(), Some(b' ' | b'\t')) {
            // A continuation line without a preceding field: malformed, treat as body.
            if headers.is_empty() {
                return (headers, pos);
            }
            let last: &mut RawHeader<'_> = headers.last_mut().expect("checked above");
            let start = last.raw.as_ptr() as usize - raw.as_ptr() as usize;
            last.raw = &raw[start..line_end];
        } else {
            let Some(colon) = line.iter().position(|&b| b == b':') else {
                return (headers, pos);
            };
            let Ok(name) = std::str::from_utf8(&line[..colon]) else {
                return (headers, pos);
            };
            if name.is_empty() || name.contains([' ', '\t']) {
                return (headers, pos);
            }
            headers.push(RawHeader { name, raw: line });
        }
        pos = line_end;
    }
    (headers, raw.len())
}

pub fn count(raw: &[u8], name: &str) -> usize {
    split(raw).0.iter().filter(|h| h.name.eq_ignore_ascii_case(name)).count()
}

/// A fault in the header block that must stop a message before it is judged, expressed as the SMTP
/// reason (without the code). `None` means the block is fine to go on with.
///
/// Two shapes let a sender show one `From` to the recipient while a different one (or none) is what
/// SPF/DKIM/DMARC ran against:
/// - more than one `From` header (a policy domain placed first or last, judged as neither);
/// - a header block cut short by a line with no colon, which the authentication parser treats as
///   the end of the headers while the display parser reads past it to the real `From` below.
///
/// Both are refused. `split` already stops at the first line it cannot read as a field, so a block
/// that did not end at a blank line was cut short by exactly such a line.
pub fn header_block_fault(raw: &[u8]) -> Option<&'static str> {
    let (headers, body_start) = split(raw);
    let ended_cleanly = body_start == raw.len()
        || raw[..body_start].ends_with(b"\r\n\r\n")
        || raw[..body_start].ends_with(b"\n\n");
    if !ended_cleanly {
        return Some("the message has a malformed header block");
    }
    if headers.iter().filter(|h| h.name.eq_ignore_ascii_case("From")).count() > 1 {
        return Some("a message may have only one From header");
    }
    None
}

pub fn first_value(raw: &[u8], name: &str) -> Option<String> {
    split(raw).0.iter().find(|h| h.name.eq_ignore_ascii_case(name)).map(RawHeader::value)
}

/// The header block (for text/rfc822-headers in bounces).
pub fn header_block(raw: &[u8]) -> &[u8] {
    let (_, body_start) = split(raw);
    &raw[..body_start]
}

/// `raw` without the headers `unwanted` picks.
fn without(raw: &[u8], unwanted: impl Fn(&RawHeader<'_>) -> bool) -> Vec<u8> {
    let (headers, _) = split(raw);
    let removed: Vec<&RawHeader<'_>> = headers.iter().filter(|header| unwanted(header)).collect();
    if removed.is_empty() {
        return raw.to_vec();
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut pos = 0;
    for header in removed {
        let start = header.raw.as_ptr() as usize - raw.as_ptr() as usize;
        out.extend_from_slice(&raw[pos..start]);
        pos = start + header.raw.len();
    }
    out.extend_from_slice(&raw[pos..]);
    out
}

/// Removes Authentication-Results headers that claim to come from `hostname`, so senders
/// cannot fake our own verdicts (RFC 8601, section 5).
pub fn strip_forged_auth_results(raw: &[u8], hostname: &str) -> Vec<u8> {
    without(raw, |h| h.name.eq_ignore_ascii_case("Authentication-Results") && claims_to_be(&h.value(), hostname))
}

/// Removes spam verdicts that came with the message, so a sender cannot vouch for itself with
/// `X-Spam-Status: No` and the only verdict left is the one this server adds.
pub fn strip_spam_verdicts(raw: &[u8]) -> Vec<u8> {
    without(raw, |h| h.name.eq_ignore_ascii_case("X-Spam-Score") || h.name.eq_ignore_ascii_case("X-Spam-Status"))
}

/// Removes virus verdicts that came with the message. Only done when our own scanner looked (or
/// tried to), so a sender cannot claim `X-Virus-Scanned: yes` for itself.
pub fn strip_virus_verdicts(raw: &[u8]) -> Vec<u8> {
    without(raw, |h| h.name.eq_ignore_ascii_case("X-Virus-Scanned"))
}

fn claims_to_be(value: &str, hostname: &str) -> bool {
    // The authserv-id may carry a leading comment, surrounding quotes (RFC 8601 §2.2) or a trailing
    // dot; a conformant reader ignores those, so a forgery that hides behind them must still be
    // recognised as ours and stripped (security-audit-0.5.2 S-18).
    let id = value.split(';').next().unwrap_or_default().trim_start();
    let id = match id.strip_prefix('(') {
        Some(rest) => rest.split_once(')').map_or(rest, |(_, after)| after),
        None => id,
    };
    id.split_whitespace()
        .next()
        .map(|authserv| authserv.trim_matches('"').trim_end_matches('.'))
        .is_some_and(|authserv| authserv.eq_ignore_ascii_case(hostname))
}

/// Converts bare LF line endings to CRLF.
pub fn normalize_line_endings(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 50);
    let mut previous = 0u8;
    for &byte in raw {
        if byte == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(byte);
        previous = byte;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &[u8] = b"Received: from a\r\n by b\r\nAuthentication-Results: mx.example.de; dkim=pass\r\nAuthentication-Results: other.example; spf=pass\r\nSubject: Hi\r\n\r\nbody\r\n";

    #[test]
    fn splits_folded_headers() {
        let (headers, body) = split(MESSAGE);
        assert_eq!(headers.len(), 4);
        assert_eq!(headers[0].value(), "from a by b");
        assert_eq!(&MESSAGE[body..], b"body\r\n");
        assert_eq!(count(MESSAGE, "authentication-results"), 2);
        assert_eq!(first_value(MESSAGE, "subject").as_deref(), Some("Hi"));
    }

    #[test]
    fn strips_spam_verdicts_the_sender_brought_along() {
        let raw = b"X-Spam-Status: No, score=-10\r\nSubject: Hi\r\nx-spam-score: -10.0\r\nX-Rspamd-Score: 1.2\r\n\r\nX-Spam-Status: body\r\n";
        let stripped = String::from_utf8(strip_spam_verdicts(raw)).unwrap();
        assert_eq!(stripped, "Subject: Hi\r\nX-Rspamd-Score: 1.2\r\n\r\nX-Spam-Status: body\r\n");
    }

    #[test]
    fn strips_virus_verdicts_the_sender_brought_along() {
        let raw = b"x-virus-scanned: yes (trust me)\r\nSubject: Hi\r\nX-Virus-Scanned: also me\r\n\r\nbody\r\n";
        let stripped = String::from_utf8(strip_virus_verdicts(raw)).unwrap();
        assert_eq!(stripped, "Subject: Hi\r\n\r\nbody\r\n");
    }

    #[test]
    fn strips_only_our_auth_results() {
        let stripped = strip_forged_auth_results(MESSAGE, "MX.example.de");
        let text = String::from_utf8(stripped).unwrap();
        assert!(!text.contains("dkim=pass"));
        assert!(text.contains("other.example; spf=pass"));
        assert!(text.starts_with("Received: from a\r\n by b\r\n"));
    }

    #[test]
    fn strips_our_results_behind_quotes_a_comment_or_a_trailing_dot() {
        // A conformant reader ignores a quoted authserv-id, a leading comment or a trailing dot, so
        // a forgery hiding behind them is still ours to strip (security-audit-0.5.2 S-18).
        for id in ["\"mx.example.de\"", "(by our filter) mx.example.de", "mx.example.de."] {
            let message = format!("Authentication-Results: {id}; dkim=pass header.d=evil.example\r\nSubject: Hi\r\n\r\nbody\r\n");
            let stripped = String::from_utf8(strip_forged_auth_results(message.as_bytes(), "mx.example.de")).unwrap();
            assert!(!stripped.contains("dkim=pass"), "forgery behind {id} is removed: {stripped}");
        }
    }

    #[test]
    fn strips_our_results_even_with_a_version_number() {
        // RFC 8601 allows "authserv-id version"; a forged header must not slip through by adding one.
        let message = b"Authentication-Results: mx.example.de 1; dkim=pass header.d=evil.example\r\n\
Authentication-Results: mx.example.de; spf=pass\r\nSubject: Hi\r\n\r\nbody\r\n";
        let stripped = String::from_utf8(strip_forged_auth_results(message, "mx.example.de")).unwrap();
        assert!(!stripped.contains("dkim=pass"), "the versioned forgery is removed: {stripped}");
        assert!(!stripped.contains("spf=pass"), "the plain forgery is removed too");
        assert!(stripped.starts_with("Subject: Hi\r\n"));
        // A genuinely different authserv-id is kept.
        assert!(claims_to_be("mx.example.de 1; dkim=pass", "mx.example.de"));
        assert!(!claims_to_be("other.example; dkim=pass", "mx.example.de"));
    }

    #[test]
    fn normalizes_line_endings() {
        assert_eq!(normalize_line_endings(b"a\nb\r\nc\n"), b"a\r\nb\r\nc\r\n");
    }
}
