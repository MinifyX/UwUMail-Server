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

fn claims_to_be(value: &str, hostname: &str) -> bool {
    value
        .split(';')
        .next()
        .and_then(|id| id.split_whitespace().next())
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
    fn strips_only_our_auth_results() {
        let stripped = strip_forged_auth_results(MESSAGE, "MX.example.de");
        let text = String::from_utf8(stripped).unwrap();
        assert!(!text.contains("dkim=pass"));
        assert!(text.contains("other.example; spf=pass"));
        assert!(text.starts_with("Received: from a\r\n by b\r\n"));
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
