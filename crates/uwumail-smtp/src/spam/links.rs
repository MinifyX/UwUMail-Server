//! Links in a message and what they give away: a link whose text shows another site than the one
//! it leads to, a link to a bare IP address, and domains that only look like a familiar name.

use std::net::IpAddr;

use url::{Host, Url};

use super::html::Anchor;

/// Where a link leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Domain(String),
    Ip(IpAddr),
}

#[derive(Debug, Clone)]
pub(crate) struct Link {
    pub target: Target,
    /// For links in HTML: the text on them.
    pub text: Option<String>,
    /// The whole address, as lists of known links write it.
    pub url: Option<String>,
}

/// How many link domains are asked about on a blocklist per message.
const MAX_LOOKUPS: usize = 10;

/// Two-label endings under which anyone can register a name, so the site is the label before them.
/// Not the full public suffix list: the common country endings and hosting platforms that phishing
/// pages like to sit on, which is what telling one site from another in a link needs.
const SHARED_SUFFIXES: &[&str] = &[
    "co.uk",
    "org.uk",
    "me.uk",
    "ac.uk",
    "gov.uk",
    "ltd.uk",
    "plc.uk",
    "net.uk",
    "com.au",
    "net.au",
    "org.au",
    "edu.au",
    "gov.au",
    "co.nz",
    "org.nz",
    "net.nz",
    "co.jp",
    "ne.jp",
    "or.jp",
    "ac.jp",
    "go.jp",
    "com.br",
    "net.br",
    "org.br",
    "com.cn",
    "net.cn",
    "org.cn",
    "com.tr",
    "co.za",
    "co.in",
    "net.in",
    "org.in",
    "com.mx",
    "com.ar",
    "co.kr",
    "or.kr",
    "com.sg",
    "com.hk",
    "com.tw",
    "co.at",
    "or.at",
    "gv.at",
    "ac.at",
    "com.pl",
    "net.pl",
    "org.pl",
    "co.il",
    "com.ua",
    "github.io",
    "gitlab.io",
    "blogspot.com",
    "herokuapp.com",
    "netlify.app",
    "vercel.app",
    "pages.dev",
    "workers.dev",
    "web.app",
    "firebaseapp.com",
    "appspot.com",
    "azurewebsites.net",
    "cloudfront.net",
    "wixsite.com",
    "weebly.com",
    "godaddysites.com",
    "ngrok.io",
    "ngrok-free.app",
    "glitch.me",
    "onrender.com",
    "fly.dev",
    "r2.dev",
    "repl.co",
];

/// File endings that make a link text look like a domain name, e.g. "Rechnung.pdf".
const FILE_ENDINGS: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "txt", "rtf", "csv", "zip", "rar", "7z", "jpg",
    "jpeg", "png", "gif", "webp", "svg", "mp3", "mp4", "mov", "html", "htm", "php", "asp", "aspx", "jsp", "exe", "msi",
    "js", "ics", "vcf", "eml", "xml", "json",
];

/// Where an address in a link leads, for http and https links (and "www." without a scheme).
/// Mail, phone and in-page links have no target.
pub(crate) fn target(href: &str) -> Option<Target> {
    let href = href.trim();
    let lower = href.get(..8).unwrap_or(href).to_ascii_lowercase();
    let url = if lower.starts_with("http://") || lower.starts_with("https://") {
        Url::parse(href).ok()?
    } else if lower.starts_with("www.") {
        Url::parse(&format!("http://{href}")).ok()?
    } else {
        return None;
    };
    match url.host()? {
        Host::Domain(domain) => Some(Target::Domain(domain.trim_end_matches('.').to_ascii_lowercase())),
        Host::Ipv4(ip) => Some(Target::Ip(IpAddr::V4(ip))),
        Host::Ipv6(ip) => Some(Target::Ip(IpAddr::V6(ip))),
    }
}

/// A web address the way lists of known links write it: scheme and host in lower case, without a fragment.
pub(crate) fn normalized_url(href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.get(..8).unwrap_or(href).to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let mut url = Url::parse(href).ok()?;
    url.set_fragment(None);
    Some(url.to_string())
}

/// Addresses written out in plain text.
pub(crate) fn in_text(text: &str) -> Vec<Link> {
    let mut found = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut pos = 0;
    while found.len() < 200 {
        let next = ["https://", "http://", "www."].iter().filter_map(|start| lower[pos..].find(start)).min();
        let Some(offset) = next else { break };
        let start = pos + offset;
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || "<>\"'()[]{}".contains(c))
            .map_or(text.len(), |len| start + len);
        let candidate = text[start..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if let Some(target) = target(candidate) {
            found.push(Link { target, text: None, url: normalized_url(candidate) });
        }
        pos = end.max(start + 1);
    }
    found
}

pub(crate) fn in_anchors(anchors: &[Anchor]) -> Vec<Link> {
    anchors
        .iter()
        .filter_map(|anchor| {
            Some(Link {
                target: target(&anchor.href)?,
                text: Some(anchor.text.clone()),
                url: normalized_url(&anchor.href),
            })
        })
        .collect()
}

/// The site a link text names, when the text is an address or a bare domain name like
/// "www.bank.example" or "bank.example/login". Ordinary words ("Hier klicken") and file names
/// ("Rechnung.pdf") name none.
pub(crate) fn named_in_text(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches(['.', ',', ';', ':', '!', '?']);
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    if let Some(Target::Domain(domain)) = target(text) {
        return Some(domain);
    }
    let name = text.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    let (_, ending) = name.rsplit_once('.')?;
    let looks_like_domain =
        name.split('.').all(|label| !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            && ending.len() >= 2
            && ending.chars().all(|c| c.is_ascii_alphabetic())
            && !FILE_ENDINGS.contains(&ending);
    looks_like_domain.then_some(name)
}

/// The part of a host name that stands for one site: the last two labels, or three where the last
/// two are a shared ending like "co.uk" or "github.io".
pub(crate) fn site(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let take = if labels.len() >= 3 && SHARED_SUFFIXES.contains(&labels[labels.len() - 2..].join(".").as_str()) {
        3
    } else {
        2
    };
    labels[labels.len().saturating_sub(take)..].join(".")
}

pub(crate) fn same_site(a: &str, b: &str) -> bool {
    site(a) == site(b)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Other,
}

fn script(c: char) -> Option<Script> {
    if c.is_ascii_alphabetic() || ('\u{C0}'..='\u{24F}').contains(&c) {
        Some(Script::Latin)
    } else if ('\u{400}'..='\u{52F}').contains(&c) {
        Some(Script::Cyrillic)
    } else if ('\u{370}'..='\u{3FF}').contains(&c) {
        Some(Script::Greek)
    } else if c.is_alphabetic() {
        Some(Script::Other)
    } else {
        None
    }
}

/// Cyrillic and Greek letters that look like Latin ones.
const LOOKALIKES: &str = "аеорсухіјѕԁԛԝһӏьαικνορυχε";

/// Whether a domain only looks like a familiar name: a label that mixes Latin letters with Cyrillic or
/// Greek ones, or one made only of Cyrillic or Greek letters that look Latin under a Latin ending.
/// Real names in other scripts, like "müller.de" or a Cyrillic name under ".рф", are fine.
pub(crate) fn is_lookalike(domain: &str) -> bool {
    if !domain.split('.').any(|label| label.starts_with("xn--")) {
        return false;
    }
    let (unicode, result) = idna::domain_to_unicode(domain);
    if result.is_err() {
        return false;
    }
    let labels: Vec<&str> = unicode.split('.').collect();
    let latin_ending = labels.last().is_some_and(|ending| ending.chars().all(|c| c.is_ascii_alphanumeric()));
    labels.iter().any(|label| {
        let scripts: Vec<Script> = label.chars().filter_map(script).collect();
        let latin = scripts.contains(&Script::Latin);
        let foreign = scripts.iter().any(|s| matches!(s, Script::Cyrillic | Script::Greek));
        let only_lookalikes = !label.is_empty()
            && label.chars().filter(|c| c.is_alphabetic()).all(|c| LOOKALIKES.contains(c))
            && label.chars().any(|c| LOOKALIKES.contains(c));
        (latin && foreign) || (latin_ending && only_lookalikes)
    })
}

/// The link domains worth asking a domain blocklist about: each once, only real host names, and
/// not too many per message.
pub(crate) fn domains_to_look_up(links: &[Link]) -> Vec<String> {
    let mut domains: Vec<String> = Vec::new();
    for link in links {
        let Target::Domain(domain) = &link.target else { continue };
        let valid = domain.len() <= 200
            && domain.contains('.')
            && domain.split('.').all(|label| {
                (1..=63).contains(&label.len()) && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if valid && !domains.contains(domain) {
            domains.push(domain.clone());
            if domains.len() == MAX_LOOKUPS {
                break;
            }
        }
    }
    domains
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(target: Option<Target>) -> Option<String> {
        match target? {
            Target::Domain(domain) => Some(domain),
            Target::Ip(_) => None,
        }
    }

    #[test]
    fn link_targets_are_read_like_a_browser_would() {
        assert_eq!(domain(target("https://Login.Bank.Example./konto")).as_deref(), Some("login.bank.example"));
        assert_eq!(domain(target("www.shop.example/angebot")).as_deref(), Some("www.shop.example"));
        assert_eq!(target("http://192.0.2.1/x"), Some(Target::Ip("192.0.2.1".parse().unwrap())));
        // A number is an address in disguise; browsers go there.
        assert_eq!(target("http://3221225985/"), Some(Target::Ip("192.0.2.1".parse().unwrap())));
        assert_eq!(target("http://[2001:db8::1]/"), Some(Target::Ip("2001:db8::1".parse().unwrap())));
        for other in ["mailto:info@shop.example", "tel:+49301234", "#oben", "/relative", "javascript:void(0)"] {
            assert_eq!(target(other), None, "{other}");
        }
    }

    #[test]
    fn addresses_are_found_in_plain_text() {
        let found = in_text("Besuche https://a.example/x, (www.b.example). Oder HTTP://C.example!");
        let domains: Vec<String> = found.into_iter().filter_map(|link| domain(Some(link.target))).collect();
        assert_eq!(domains, ["a.example", "www.b.example", "c.example"]);
    }

    #[test]
    fn a_link_text_names_a_site_only_when_it_looks_like_one() {
        assert_eq!(named_in_text("www.bank.example").as_deref(), Some("www.bank.example"));
        assert_eq!(named_in_text(" https://bank.example/login ").as_deref(), Some("bank.example"));
        assert_eq!(named_in_text("Bank.Example/konto").as_deref(), Some("bank.example"));
        for words in ["Hier klicken", "Rechnung.pdf", "Version 2.0", "", "sale!"] {
            assert_eq!(named_in_text(words), None, "{words}");
        }
    }

    #[test]
    fn sites_are_told_apart_without_the_public_suffix_list() {
        assert!(same_site("www.bank.example", "bank.example"));
        assert!(same_site("email.bank.example", "www.bank.example"));
        assert!(same_site("bank.co.uk", "www.bank.co.uk"));
        assert!(!same_site("bank.co.uk", "evil.co.uk"));
        assert!(!same_site("bank.github.io", "evil.github.io"));
        assert!(!same_site("bank.example", "bank.example.evil.example"));
    }

    #[test]
    fn lookalike_domains_are_caught_and_real_names_are_not() {
        let ascii = |unicode: &str| idna::domain_to_ascii(unicode).unwrap();
        // "pаypal" with a Cyrillic "а", and "сосо" made only of Cyrillic letters that look Latin.
        assert!(is_lookalike(&ascii("p\u{430}ypal.example")));
        assert!(is_lookalike(&ascii("\u{441}\u{43E}\u{441}\u{43E}.com")));
        // A German umlaut, a real Cyrillic word, and Cyrillic under a Cyrillic ending.
        assert!(!is_lookalike(&ascii("müller.de")));
        assert!(!is_lookalike(&ascii("\u{43F}\u{440}\u{438}\u{43C}\u{435}\u{440}.com")));
        assert!(!is_lookalike(&ascii("\u{441}\u{43E}\u{441}\u{43E}.\u{440}\u{444}")));
        assert!(!is_lookalike("bank.example"));
    }

    #[test]
    fn each_link_domain_is_looked_up_once() {
        let links = in_text("https://a.example https://a.example http://192.0.2.1 https://b.example/x");
        assert_eq!(domains_to_look_up(&links), ["a.example", "b.example"]);
        let many: String = (0..30).map(|n| format!("https://d{n}.example ")).collect();
        assert_eq!(domains_to_look_up(&in_text(&many)).len(), MAX_LOOKUPS);
    }
}
