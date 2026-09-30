//! Microsoft Defender "Safe Links": Exchange Online rewrites every link of incoming mail to
//! `https://<region>.safelinks.protection.outlook.com/?url=<the link>&data=…`. For showing a link
//! and for lists of links (previews, prompts), the original is what matters; the mail itself is
//! never changed.

/// Hosts that wrap links: `safelinks.protection.outlook.com` and its regional names
/// (`nam12.…`, `eur01.…`), and the US government clouds.
const SUFFIXES: &[&str] =
    &["safelinks.protection.outlook.com", "safelinks.protection.office365.us", "safelinks.protection.outlook.us"];

/// Links are unwrapped this many times at most (a wrapped link wrapped again).
const MAX_LAYERS: usize = 3;
const MAX_URL: usize = 8192;

fn is_safelinks_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    SUFFIXES.iter().any(|suffix| host == *suffix || host.strip_suffix(suffix).is_some_and(|rest| rest.ends_with('.')))
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (bytes.get(i + 1).and_then(|b| hex(*b)), bytes.get(i + 2).and_then(|b| hex(*b)))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// The link a Safe Links URL wraps, or `None` when `url` is no Safe Links URL (or wraps
/// something that is not an `http`, `https` or `mailto` link).
pub fn unwrap(url: &str) -> Option<String> {
    let mut current = url.trim().to_owned();
    let mut unwrapped = false;
    for _ in 0..MAX_LAYERS {
        match unwrap_once(&current) {
            Some(inner) => {
                current = inner;
                unwrapped = true;
            }
            None => break,
        }
    }
    unwrapped.then_some(current)
}

fn unwrap_once(url: &str) -> Option<String> {
    if url.len() > MAX_URL {
        return None;
    }
    let rest = url.get(..8).filter(|s| s.eq_ignore_ascii_case("https://")).map(|_| &url[8..])?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit('@').next()?.split(':').next()?;
    if !is_safelinks_host(host) {
        return None;
    }
    let query = rest[end..].split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    let value = query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        key.eq_ignore_ascii_case("url").then_some(value)
    })?;
    let target = percent_decode(value)?;
    let target = target.trim();
    let lower = target.to_ascii_lowercase();
    let allowed = lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:");
    (allowed && !target.chars().any(|c| c.is_control() || c.is_whitespace())).then(|| target.to_owned())
}

/// `url` itself when it is no Safe Links URL.
pub fn original(url: &str) -> std::borrow::Cow<'_, str> {
    match unwrap(url) {
        Some(inner) => std::borrow::Cow::Owned(inner),
        None => std::borrow::Cow::Borrowed(url),
    }
}

/// Text with every Safe Links URL in it replaced by the link it wraps.
pub fn unwrap_in_text(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.to_ascii_lowercase().contains("safelinks.protection.") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = find_https(rest) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']'))
            .unwrap_or(tail.len());
        let candidate = &tail[..end];
        match unwrap(candidate) {
            Some(inner) => out.push_str(&inner),
            None => out.push_str(candidate),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

fn find_https(text: &str) -> Option<usize> {
    text.char_indices()
        .find(|(i, _)| text.get(*i..*i + 8).is_some_and(|s| s.eq_ignore_ascii_case("https://")))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps() {
        let wrapped = "https://nam12.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1%26c%3D2&data=05%7C01&sdata=x&reserved=0";
        assert_eq!(unwrap(wrapped).as_deref(), Some("https://example.com/a?b=1&c=2"));
        let plain = "https://safelinks.protection.outlook.com/?url=http%3A%2F%2Fexample.org";
        assert_eq!(unwrap(plain).as_deref(), Some("http://example.org"));
        let path = "https://eur01.safelinks.protection.outlook.com/ap/w-59584e83/?url=https%3A%2F%2Fexample.net%2F&data=1";
        assert_eq!(unwrap(path).as_deref(), Some("https://example.net/"));
        let twice = format!(
            "https://eur02.safelinks.protection.outlook.com/?URL={}&data=1",
            wrapped.replace('%', "%25").replace(':', "%3A").replace('/', "%2F").replace('?', "%3F").replace('&', "%26")
        );
        assert_eq!(unwrap(&twice).as_deref(), Some("https://example.com/a?b=1&c=2"));
        assert_eq!(unwrap("https://evil.example/?url=https%3A%2F%2Fexample.com"), None);
        assert_eq!(unwrap("https://safelinks.protection.outlook.com.evil.example/?url=https%3A%2F%2Fx.example"), None);
        assert_eq!(unwrap("https://xsafelinks.protection.outlook.com/?url=https%3A%2F%2Fx.example"), None);
        assert_eq!(unwrap("https://nam12.safelinks.protection.outlook.com/?url=javascript%3Aalert(1)"), None);
        assert_eq!(unwrap("https://nam12.safelinks.protection.outlook.com/?data=1"), None);
        assert_eq!(original("https://example.com/"), "https://example.com/");
        let text = format!("Siehe <{wrapped}> und https://example.org/x.");
        assert_eq!(unwrap_in_text(&text), "Siehe <https://example.com/a?b=1&c=2> und https://example.org/x.");
        assert!(matches!(unwrap_in_text("no links"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(unwrap_in_text("ä https://nam12.safelinks.protection.outlook.com"), "ä https://nam12.safelinks.protection.outlook.com");
    }
}
