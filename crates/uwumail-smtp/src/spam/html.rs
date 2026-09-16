//! Just enough HTML reading for the spam rules: the links with the text a reader sees on them, and
//! how much text is kept out of sight. HTML in mail is often broken, so this never fails; it reads
//! what it can, and only the first part of very long HTML.

/// HTML beyond this is not read. A trick needs to sit early enough to matter to a reader anyway.
const MAX_HTML: usize = 2 * 1024 * 1024;
const MAX_ANCHORS: usize = 200;

/// Elements that have no end tag, so they never hold text.
const VOID: &[&str] =
    &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub href: String,
    /// The text a reader sees on the link, with whitespace collapsed.
    pub text: String,
}

#[derive(Debug, Default)]
pub(crate) struct Html {
    pub anchors: Vec<Anchor>,
    /// Letters, digits and other visible characters inside elements a reader cannot see.
    pub hidden_chars: usize,
}

struct Open {
    name: String,
    hidden: bool,
}

pub(crate) fn read(html: &str) -> Html {
    let mut end = html.len().min(MAX_HTML);
    while !html.is_char_boundary(end) {
        end -= 1;
    }
    let html = &html[..end];
    let bytes = html.as_bytes();

    let mut out = Html::default();
    let mut stack: Vec<Open> = Vec::new();
    let mut hidden_open = 0usize;
    let mut anchor: Option<usize> = None;
    let mut pos = 0;

    while pos < bytes.len() {
        let Some(offset) = html[pos..].find('<') else {
            text(&html[pos..], hidden_open > 0, anchor, &mut out);
            break;
        };
        text(&html[pos..pos + offset], hidden_open > 0, anchor, &mut out);
        pos += offset;
        let rest = &html[pos..];

        if rest.starts_with("<!--") {
            pos += rest.find("-->").map_or(rest.len(), |at| at + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            pos += rest.find('>').map_or(rest.len(), |at| at + 1);
            continue;
        }
        let closing = rest.starts_with("</");
        let name_start = if closing { 2 } else { 1 };
        let name_len = rest[name_start..].bytes().take_while(u8::is_ascii_alphanumeric).count();
        if name_len == 0 {
            // A lone "<" in text.
            text("<", hidden_open > 0, anchor, &mut out);
            pos += 1;
            continue;
        }
        let name = rest[name_start..name_start + name_len].to_ascii_lowercase();
        let (attributes, tag_len, self_closing) = attributes(&rest[name_start + name_len..]);
        pos += name_start + name_len + tag_len;

        if closing {
            if let Some(at) = stack.iter().rposition(|open| open.name == name) {
                for open in stack.drain(at..) {
                    if open.hidden {
                        hidden_open -= 1;
                    }
                    if open.name == "a" {
                        anchor = None;
                    }
                }
            }
            continue;
        }

        if name == "script" || name == "style" {
            // Their content is not text anyone reads.
            let close = format!("</{name}");
            let lower = html[pos..].to_ascii_lowercase();
            pos += lower.find(&close).unwrap_or(html.len() - pos);
            continue;
        }
        if name == "a"
            && let Some((_, href)) = attributes.iter().find(|(key, _)| key == "href")
            && out.anchors.len() < MAX_ANCHORS
        {
            out.anchors.push(Anchor { href: decode_entities(href.trim()), text: String::new() });
            anchor = Some(out.anchors.len() - 1);
        }
        if self_closing || VOID.contains(&name.as_str()) {
            continue;
        }
        let hidden = is_hidden(&attributes);
        if hidden {
            hidden_open += 1;
        }
        stack.push(Open { name, hidden });
    }

    for anchor in &mut out.anchors {
        anchor.text = anchor.text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    out
}

fn text(raw: &str, hidden: bool, anchor: Option<usize>, out: &mut Html) {
    if raw.is_empty() {
        return;
    }
    let decoded = decode_entities(raw);
    if hidden {
        out.hidden_chars += decoded.chars().filter(|c| !c.is_whitespace()).count();
    }
    if let Some(index) = anchor {
        let text = &mut out.anchors[index].text;
        if text.len() < 1000 {
            text.push_str(&decoded);
            text.push(' ');
        }
    }
}

/// The attributes of a tag after its name, how many bytes the rest of the tag takes up to and
/// including the `>`, and whether it ends with `/>`.
fn attributes(rest: &str) -> (Vec<(String, String)>, usize, bool) {
    let bytes = rest.as_bytes();
    let mut found = Vec::new();
    let mut pos = 0;
    let mut self_closing = false;
    loop {
        while pos < bytes.len() && (bytes[pos].is_ascii_whitespace() || bytes[pos] == b'/') {
            self_closing = bytes[pos] == b'/';
            pos += 1;
        }
        if pos >= bytes.len() {
            return (found, pos, self_closing);
        }
        if bytes[pos] == b'>' {
            return (found, pos + 1, self_closing);
        }
        self_closing = false;
        let name_start = pos;
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && !matches!(bytes[pos], b'=' | b'>' | b'/') {
            pos += 1;
        }
        let name = rest[name_start..pos].to_ascii_lowercase();
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let mut value = String::new();
        if pos < bytes.len() && bytes[pos] == b'=' {
            pos += 1;
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos < bytes.len() && matches!(bytes[pos], b'"' | b'\'') {
                let quote = bytes[pos];
                let start = pos + 1;
                let end = rest[start..].bytes().position(|b| b == quote).map_or(bytes.len(), |at| start + at);
                value = rest[start..end].to_owned();
                pos = (end + 1).min(bytes.len());
            } else {
                let start = pos;
                while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'>' {
                    pos += 1;
                }
                value = rest[start..pos].to_owned();
            }
        }
        if !name.is_empty() {
            found.push((name, value));
        }
    }
}

/// Whether an element keeps its content out of sight: the `hidden` attribute, or a style that
/// shows nothing (`display: none`, `visibility: hidden`, a zero font size or opacity, or a zero
/// height with the overflow cut off, the usual way to hide a preview line).
fn is_hidden(attributes: &[(String, String)]) -> bool {
    if attributes.iter().any(|(key, _)| key == "hidden") {
        return true;
    }
    let Some((_, style)) = attributes.iter().find(|(key, _)| key == "style") else { return false };
    let style: String = style.to_ascii_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let value =
        |property: &str| style.split(';').find_map(|declaration| declaration.strip_prefix(property)?.strip_prefix(':'));
    let zero = |property: &str| {
        value(property).is_some_and(|value| {
            let number: String = value.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            number.parse::<f32>().is_ok_and(|n| n == 0.0)
        })
    };
    value("display").is_some_and(|v| v.starts_with("none"))
        || value("visibility").is_some_and(|v| v.starts_with("hidden"))
        || zero("font-size")
        || zero("opacity")
        || (zero("max-height") && value("overflow").is_some_and(|v| v.starts_with("hidden")))
}

/// Decodes the character references mail HTML uses; anything unknown stays as it is.
pub(crate) fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest[1..].find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = &rest[1..1 + end];
            let character = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let number = name.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code)
                }
            }?;
            Some((character, end + 2))
        });
        match decoded {
            Some((character, len)) => {
                out.push(character);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchors(html: &str) -> Vec<(String, String)> {
        read(html).anchors.into_iter().map(|anchor| (anchor.href, anchor.text)).collect()
    }

    #[test]
    fn links_come_with_the_text_a_reader_sees() {
        let html = r#"<p>Hallo <A HREF="https://evil.example/login?a=1&amp;b=2"><b>www.paypal.com</b></A> und
            <a href=https://shop.example/>Zum&nbsp;Shop</a></p>"#;
        assert_eq!(
            anchors(html),
            vec![
                ("https://evil.example/login?a=1&b=2".into(), "www.paypal.com".into()),
                ("https://shop.example/".into(), "Zum Shop".into()),
            ]
        );
    }

    #[test]
    fn broken_html_is_read_as_far_as_it_goes() {
        let found = anchors(r#"</div><a href="https://a.example">eins <a href='https://b.example'>zwei</div> drei"#);
        assert_eq!(found[0], ("https://a.example".into(), "eins".into()));
        assert_eq!(found[1].0, "https://b.example");
        assert!(found[1].1.starts_with("zwei"), "{found:?}");
        // Cut off in the middle of a tag, or angle brackets that are no tags: no panic, no link.
        assert!(read("<a href=\"https://c.example").anchors.len() <= 1);
        assert!(read("1 < 2 > 0 & plain & text <").anchors.is_empty());
    }

    #[test]
    fn scripts_styles_and_comments_are_not_text() {
        let html = r#"<style>a { color: red }</style><script>document.write('<a href="https://x.example">x</a>')</script>
            <!-- <a href="https://y.example">y</a> --><a href="https://z.example">z</a>"#;
        assert_eq!(anchors(html), vec![("https://z.example".into(), "z".into())]);
    }

    #[test]
    fn hidden_text_is_counted() {
        let visible = read(r#"<div style="font-size: 0.8em">sichtbar</div><p>auch</p>"#);
        assert_eq!(visible.hidden_chars, 0);
        let hidden = read(concat!(
            r#"<div style="display:none">zehn Zeichen</div>"#,
            r#"<span style="FONT-SIZE: 0px">abc</span>"#,
            r#"<p hidden>de</p>"#,
            r#"<div style="max-height:0; overflow: hidden"><b>fg</b></div>"#,
            r#"<div style="opacity:0">h</div><br><div>sichtbar</div>"#,
        ));
        assert_eq!(hidden.hidden_chars, "zehnZeichen".len() + 3 + 2 + 2 + 1);
    }

    #[test]
    fn entities_decode_and_unknown_ones_stay() {
        assert_eq!(decode_entities("a &amp; b &#x41;&#66; &unknown; &"), "a & b AB &unknown; &");
    }
}
