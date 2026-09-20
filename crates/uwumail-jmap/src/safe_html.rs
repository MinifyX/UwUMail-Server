//! Cleaning message HTML for the webmail.
//!
//! The app cleans mail in its own engine before it ever reaches a window. A browser has no engine
//! in front of it, so the server does the same work here and hands out the result as
//! `uwuSafeHtml` — the same rules, one place to fix if they are ever wrong. The webmail cleans
//! once more in the browser and shows the result in a frame without scripts, so a mistake here
//! would still have to get past two more doors.

use std::collections::HashSet;

/// Cleans message HTML: no scripts, no handlers, no frames, no forms — but the sender's layout
/// survives, because newsletters are built from tables and inline styles.
pub fn sanitize(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    builder
        .rm_clean_content_tags(&["style"])
        .add_tags(&["style", "center", "font", "u", "s", "strike"])
        .add_generic_attributes(&[
            "style",
            "align",
            "valign",
            "bgcolor",
            "width",
            "height",
            "border",
            "cellpadding",
            "cellspacing",
            "dir",
            "class",
            "id",
            "role",
        ])
        .add_tag_attributes("font", &["color", "face", "size"])
        .add_tag_attributes("img", &["src", "alt", "width", "height"])
        .add_tag_attributes("td", &["colspan", "rowspan", "background"])
        .add_tag_attributes("th", &["colspan", "rowspan", "background"])
        .add_tag_attributes("table", &["background"])
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid", "data"]))
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true);
    // The sanitizer drops <body>, and with it the background many newsletters set there. It moves
    // to a wrapper that goes through the sanitizer like the rest.
    let input = match body_background(html) {
        Some(style) => format!("<div style=\"{style}\">{html}</div>"),
        None => html.to_string(),
    };
    builder.clean(&input).to_string()
}

/// Reads `bgcolor` and `style` from the `<body>` tag as one inline style.
fn body_background(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<body")?;
    let end = start + lower[start..].find('>')?;
    let tag = &html[start..end];
    let attribute = |name: &str| {
        let tag_lower = tag.to_ascii_lowercase();
        let at = tag_lower.find(&format!("{name}="))? + name.len() + 1;
        let rest = &tag[at..];
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value = &rest[1..];
        Some(value[..value.find(quote)?].to_string())
    };
    let mut style = String::new();
    if let Some(color) = attribute("bgcolor").filter(|c| c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '#')) {
        style.push_str(&format!("background-color:{color};"));
    }
    if let Some(inline) = attribute("style") {
        style.push_str(&inline.replace('"', "'"));
    }
    (!style.is_empty()).then_some(style)
}

/// Whether the cleaned HTML would fetch something from another server when shown.
///
/// Asked the careful way round: everything that points somewhere counts, unless it points at the
/// message itself (`cid:`) or carries its content along (`data:`). Looking for `http` instead
/// would miss `src="//tracker.example/pixel.png"`, which a browser happily resolves against
/// https, and every `@import` that does not spell out `url(`.
///
/// The webmail only uses this to decide whether to offer "load the pictures in this mail"; what
/// is actually loaded is decided by the frame's own policy, which allows nothing remote until
/// someone asks. So an answer that is too careful costs a banner, never a leak.
pub fn has_remote_content(clean: &str) -> bool {
    let lower = clean.to_ascii_lowercase();
    // `srcset` and `poster` do not survive the cleaning above, but they are checked anyway: this
    // function should still be right if that rule set ever grows.
    for (marker, opener) in [
        ("src=", None),
        ("srcset=", None),
        ("poster=", None),
        ("background=", None),
        ("url(", Some(')')),
        ("@import", None),
    ] {
        let mut rest = lower.as_str();
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            if points_outwards(rest, opener) {
                return true;
            }
        }
    }
    false
}

/// Reads the value right after a marker and says whether it would leave this message.
fn points_outwards(rest: &str, closer: Option<char>) -> bool {
    let value = rest.trim_start();
    let value = match value.strip_prefix(['"', '\'']) {
        Some(quoted) => quoted.split(['"', '\'']).next().unwrap_or_default(),
        None => {
            let end = value.find(|c: char| c.is_whitespace() || Some(c) == closer || c == '>');
            &value[..end.unwrap_or(value.len())]
        }
    };
    let value = value.trim();
    // `srcset="a.png 1x, b.png 2x"` and friends: the first entry decides, the rest would be the
    // same kind of address anyway.
    let value = value.split([',', ' ']).next().unwrap_or_default();
    !value.is_empty() && !value.starts_with("data:") && !value.starts_with("cid:")
}

#[cfg(test)]
mod tests {
    use super::{has_remote_content, sanitize};

    #[test]
    fn drops_scripts_handlers_and_frames() {
        let clean = sanitize(
            "<p onclick=\"steal()\">Hi</p><script>bad()</script><iframe src=\"https://evil.example\"></iframe>\
             <form action=\"https://evil.example\"><input name=\"password\"></form>\
             <a href=\"javascript:bad()\">click</a><base href=\"https://evil.example\">",
        );
        assert!(!clean.contains("script"));
        assert!(!clean.contains("onclick"));
        assert!(!clean.contains("iframe"));
        assert!(!clean.contains("<form"));
        assert!(!clean.contains("<input"));
        assert!(!clean.contains("javascript:"));
        assert!(!clean.contains("<base"));
        assert!(clean.contains("Hi"));
    }

    #[test]
    fn newsletter_layouts_survive() {
        let clean = sanitize(
            "<html><head><style>#main > td { padding: 8px }</style></head>\
             <body bgcolor=\"#f4f4f4\" style=\"margin:0\" onload=\"x()\">\
             <table id=\"main\" width=\"600\" cellpadding=\"0\" align=\"center\"><tr><th colspan=\"2\">Hi</th></tr>\
             </table></body></html>",
        );
        assert!(clean.contains("padding: 8px"));
        assert!(clean.contains("background-color:#f4f4f4"));
        assert!(clean.contains("width=\"600\""));
        assert!(!clean.contains("onload"));
    }

    #[test]
    fn spots_content_from_other_servers() {
        assert!(has_remote_content("<img src=\"https://tracker.example/pixel.gif\">"));
        assert!(has_remote_content("<div style=\"background:url(http://tracker.example/a.png)\"></div>"));
        assert!(!has_remote_content("<img src=\"cid:part1\"><img src=\"data:image/png;base64,AAA\">"));
        assert!(!has_remote_content("<p>Just words.</p>"));
    }

    #[test]
    fn spots_the_addresses_that_do_not_say_https() {
        // A browser resolves these against https, so they count just as much.
        assert!(has_remote_content("<img src=\"//tracker.example/pixel.gif\">"));
        assert!(has_remote_content("<img src='//tracker.example/pixel.gif'>"));
        assert!(has_remote_content("<img src=//tracker.example/pixel.gif>"));
        assert!(has_remote_content("<style>@import \"https://tracker.example/a.css\";</style>"));
        assert!(has_remote_content("<style>@import url(//tracker.example/a.css);</style>"));
        assert!(has_remote_content("<div style=\"background:url('//tracker.example/a.png')\"></div>"));
        assert!(has_remote_content("<img srcset=\"//tracker.example/a.png 1x\">"));
    }
}
