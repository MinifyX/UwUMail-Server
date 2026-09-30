//! Small HTML helpers: escaping, the charset a document names, and its text.

/// Text made safe to put into HTML, attribute values included.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The charset a document's `<meta>` names in its first kilobytes.
pub fn meta_charset(html: &[u8]) -> Option<String> {
    let head = &html[..html.len().min(4096)];
    let lower: Vec<u8> = head.iter().map(u8::to_ascii_lowercase).collect();
    let at = lower.windows(8).position(|w| w == b"charset=")?;
    let rest = &head[at + 8..];
    let rest = rest.strip_prefix(b"\"").or_else(|| rest.strip_prefix(b"'")).unwrap_or(rest);
    let end = rest.iter().position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')))?;
    let name = std::str::from_utf8(&rest[..end]).ok()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// The text of an HTML document, roughly as a reader sees it: without tags, style and script,
/// with line breaks for blocks and entities decoded.
pub fn to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_text(&mut out, &rest[..lt]);
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = &after[..gt];
        let name: String =
            tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>();
        let name = name.to_ascii_lowercase();
        rest = &after[gt + 1..];
        if !tag.starts_with('/') && matches!(name.as_str(), "style" | "script" | "head" | "title") {
            let close = format!("</{name}");
            // Searched without lower-casing the rest: that copied all of it once per tag.
            match find_ignore_case(rest, &close) {
                Some(end) => {
                    let skip = rest[end..].find('>').map_or(rest.len(), |g| end + g + 1);
                    rest = &rest[skip..];
                }
                None => rest = "",
            }
            continue;
        }
        if tag.starts_with("!--") && !tag.ends_with("--") {
            match rest.find("-->") {
                Some(end) => rest = &rest[end + 3..],
                None => rest = "",
            }
            continue;
        }
        let block = matches!(name.as_str(), "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
        if name == "br" || (block && !out.is_empty() && !out.ends_with('\n')) {
            out.push('\n');
        }
    }
    push_text(&mut out, rest);
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    let mut text = String::new();
    let mut blank = 0;
    for line in lines {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 || text.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(line);
        text.push('\n');
    }
    text.trim_end().to_owned()
}

/// Where `needle` (ASCII, lower case) first occurs in `haystack`, ignoring ASCII case.
fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let (hay, needle) = (haystack.as_bytes(), needle.as_bytes());
    let first = *needle.first()?;
    (0..hay.len().checked_sub(needle.len())? + 1)
        .find(|&at| hay[at].to_ascii_lowercase() == first && hay[at..at + needle.len()].eq_ignore_ascii_case(needle))
}

fn push_text(out: &mut String, raw: &str) {
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        push_spaces(out, &rest[..amp]);
        let after = &rest[amp + 1..];
        // Only the next few bytes: an entity is short, and looking further for every `&` made
        // text of many of them take quadratic time.
        let end = after.bytes().take(11).position(|b| b == b';');
        let decoded = end.and_then(|e| entity(&after[..e]));
        match (end, decoded) {
            (Some(e), Some(c)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            _ => {
                out.push('&');
                rest = after;
            }
        }
    }
    push_spaces(out, rest);
}

/// Text with the line breaks of the source as spaces, as HTML shows it.
fn push_spaces(out: &mut String, text: &str) {
    for c in text.chars() {
        if c == '\r' || c == '\n' || c == '\t' {
            if !out.ends_with(' ') && !out.ends_with('\n') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
}

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
        assert_eq!(
            meta_charset(br#"<meta http-equiv="Content-Type" content="text/html; charset=windows-1252">"#),
            Some("windows-1252".into())
        );
        assert_eq!(meta_charset(br#"<meta charset="utf-8">"#), Some("utf-8".into()));
        assert_eq!(meta_charset(b"<p>no</p>"), None);
        let html = "<html><head><style>p{}</style></head><body><p>Hallo&nbsp;Welt &amp; &#x263A;</p>\r\n\
                    <div>zweite\r\nZeile</div><script>x()</script><!-- c --><br>Ende &bogus; & <";
        assert_eq!(to_text(html), "Hallo Welt & \u{263A}\nzweite Zeile\n\nEnde &bogus; &");
    }
}
