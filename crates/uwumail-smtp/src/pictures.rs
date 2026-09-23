//! Sender pictures for company mail: the brand logo published with BIMI, or otherwise the icon of the
//! sender's website — fetched here, so neither the webmail nor an app asks the sender for it.
//!
//! Only the registrable domain is asked (`news.mail.shop.example` becomes `shop.example`), through the
//! egress, and never for addresses at mail providers, which belong to people. What was found is kept in
//! memory for a week and shared by everyone on the server, so a picture can't tell a sender who read
//! which mail or when; a logo that changed shows up a week later at the latest.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hickory_resolver::TokioResolver;
use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use url::Url;

use crate::egress::{Egress, EgressError};

const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// After a lookup nothing answered to, wait before asking the domain again.
const RETRY_UNREACHABLE_AFTER: Duration = Duration::from_secs(30 * 60);
const DNS_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_PAGE: usize = 512 * 1024;
const MAX_IMAGE: usize = 512 * 1024;
const PARALLEL_LOOKUPS: usize = 4;
/// All pictures together; the oldest go first.
const MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHED_DOMAINS: usize = 20_000;
/// Icons smaller than this look blurry in an avatar; they're only used when nothing better exists.
const SHARP_WIDTH: u32 = 48;
const IMAGE_ACCEPT: &str = "image/svg+xml,image/png,image/webp,image/*;q=0.8";

/// Mail providers where addresses belong to people, not to the company behind the domain.
const FREEMAIL: &[&str] = &[
    "163.com",
    "126.com",
    "1und1.de",
    "aim.com",
    "aol.com",
    "aol.de",
    "arcor.de",
    "bluewin.ch",
    "btinternet.com",
    "comcast.net",
    "disroot.org",
    "duck.com",
    "email.de",
    "fastmail.com",
    "fastmail.fm",
    "free.fr",
    "freenet.de",
    "gmail.com",
    "gmx.at",
    "gmx.ch",
    "gmx.com",
    "gmx.de",
    "gmx.fr",
    "gmx.net",
    "googlemail.com",
    "hey.com",
    "hotmail.co.uk",
    "hotmail.com",
    "hotmail.de",
    "hotmail.fr",
    "icloud.com",
    "kabelmail.de",
    "laposte.net",
    "libero.it",
    "live.com",
    "live.de",
    "live.fr",
    "mac.com",
    "mail.com",
    "mail.de",
    "mail.ru",
    "mailbox.org",
    "me.com",
    "msn.com",
    "naver.com",
    "o2.pl",
    "onet.pl",
    "online.de",
    "orange.fr",
    "outlook.com",
    "outlook.de",
    "outlook.fr",
    "pm.me",
    "posteo.de",
    "posteo.net",
    "proton.me",
    "protonmail.ch",
    "protonmail.com",
    "qq.com",
    "riseup.net",
    "rocketmail.com",
    "seznam.cz",
    "sfr.fr",
    "sky.com",
    "t-online.de",
    "tuta.com",
    "tuta.io",
    "tutanota.com",
    "tutanota.de",
    "vodafone.de",
    "web.de",
    "wp.pl",
    "yahoo.co.uk",
    "yahoo.com",
    "yahoo.de",
    "yahoo.fr",
    "yandex.com",
    "yandex.ru",
    "ymail.com",
    "zoho.com",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PictureKind {
    /// Made to fill a circle or square: BIMI logos and app icons.
    Logo,
    /// A small symbol that needs a plain background around it: favicons.
    Icon,
}

impl PictureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PictureKind::Logo => "logo",
            PictureKind::Icon => "icon",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SenderPicture {
    pub domain: String,
    pub kind: PictureKind,
    pub media_type: &'static str,
    pub bytes: Bytes,
}

/// The domain whose picture stands for this address, or `None` for mail providers and anything that
/// isn't a public domain name.
pub fn picture_domain(email: &str) -> Option<String> {
    let (_, host) = email.trim().trim_end_matches('>').rsplit_once('@')?;
    let host = match url::Host::parse(host.trim().trim_end_matches('.')).ok()? {
        url::Host::Domain(domain) => domain,
        _ => return None,
    };
    if !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
        return None;
    }
    if !psl::suffix(host.as_bytes()).is_some_and(|suffix| suffix.is_known()) {
        return None;
    }
    let domain = psl::domain_str(&host)?.to_string();
    (!FREEMAIL.contains(&domain.as_str())).then_some(domain)
}

/// The logo URL of a `default._bimi` TXT record, if it publishes one.
pub fn bimi_logo(record: &str) -> Option<Url> {
    let mut tags = record.split(';').map(str::trim).filter(|tag| !tag.is_empty());
    let (key, version) = tags.next()?.split_once('=')?;
    if !key.trim().eq_ignore_ascii_case("v") || !version.trim().eq_ignore_ascii_case("BIMI1") {
        return None;
    }
    let location =
        tags.filter_map(|tag| tag.split_once('=')).find(|(key, _)| key.trim().eq_ignore_ascii_case("l"))?.1.trim();
    Url::parse(location).ok().filter(|url| url.scheme() == "https")
}

/// Icon links of a web page, best first, resolved against `base`.
pub fn icon_links(html: &str, base: &Url) -> Vec<(Url, PictureKind)> {
    let lower = html.to_ascii_lowercase();
    let end = lower.find("</head").unwrap_or(lower.len());
    let mut found: Vec<(i32, Url, PictureKind)> = Vec::new();
    let mut offset = 0;
    while let Some(start) = lower[offset..end].find("<link") {
        let tag_start = offset + start + "<link".len();
        let attributes = attributes(&html[tag_start..end]);
        offset = tag_start;
        let get = |name: &str| attributes.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
        let (Some(rel), Some(href)) = (get("rel"), get("href")) else { continue };
        let Some(url) = base.join(&href.replace("&amp;", "&")).ok().filter(|url| url.scheme() == "https") else {
            continue;
        };
        let rel = rel.to_ascii_lowercase();
        let rel: Vec<&str> = rel.split_ascii_whitespace().collect();
        let largest = get("sizes").map(|sizes| {
            sizes
                .split_ascii_whitespace()
                .map(|size| {
                    if size.eq_ignore_ascii_case("any") {
                        512
                    } else {
                        size.split(['x', 'X']).next().and_then(|n| n.parse::<i32>().ok()).unwrap_or(0)
                    }
                })
                .max()
                .unwrap_or(0)
        });
        let svg = get("type").is_some_and(|t| t.contains("svg")) || url.path().ends_with(".svg");
        let (score, kind) = if rel.iter().any(|r| r.starts_with("apple-touch-icon")) {
            (1000 + largest.unwrap_or(180).min(512), PictureKind::Logo)
        } else if rel.contains(&"icon") {
            match (svg, largest) {
                (true, _) => (900, PictureKind::Icon),
                (false, Some(size)) => (300 + size.min(512), PictureKind::Icon),
                (false, None) if url.path().ends_with(".ico") => (100, PictureKind::Icon),
                (false, None) => (200, PictureKind::Icon),
            }
        } else if rel.contains(&"fluid-icon") {
            (500, PictureKind::Logo)
        } else {
            continue;
        };
        if !found.iter().any(|(_, known, _)| *known == url) {
            found.push((score, url, kind));
        }
    }
    found.sort_by_key(|(score, _, _)| std::cmp::Reverse(*score));
    found.into_iter().map(|(_, url, kind)| (url, kind)).collect()
}

/// `name="value"` pairs of a tag, up to its closing `>`. Names are lowercase.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0;
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' {
            return pairs;
        }
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let name = tag[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            pairs.push((name, String::new()));
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = match bytes.get(i) {
            Some(quote @ (b'"' | b'\'')) => {
                let start = i + 1;
                let end = tag[start..].find(*quote as char).map_or(tag.len(), |n| start + n);
                i = (end + 1).min(tag.len());
                &tag[start..end]
            }
            _ => {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                &tag[start..i]
            }
        };
        pairs.push((name, value.trim().to_string()));
    }
}

/// The picture's type by its first bytes. Web servers often send HTML error pages for picture addresses.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(&[0, 0, 1, 0]) && bytes.len() > 6 {
        return Some("image/x-icon");
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    let looks_like_svg = (head.starts_with("<?xml") || head.starts_with("<svg") || head.starts_with("<!--"))
        && head.contains("<svg")
        && !head.contains("<html");
    looks_like_svg.then_some("image/svg+xml")
}

/// Pixel width of PNG and ICO files (the largest image of an ICO), for picking sharp icons.
fn pixel_width(bytes: &[u8], media_type: &str) -> Option<u32> {
    match media_type {
        "image/png" if bytes.len() >= 24 => Some(u32::from_be_bytes(bytes[16..20].try_into().ok()?)),
        "image/x-icon" => {
            let count = u16::from_le_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
            (0..count)
                .filter_map(|index| bytes.get(6 + index * 16).map(|&w| if w == 0 { 256 } else { u32::from(w) }))
                .max()
        }
        _ => None,
    }
}

enum Lookup {
    Found(SenderPicture),
    Nothing,
    /// Nothing answered at all. Not remembered as "no picture".
    Unreachable,
}

struct Cached {
    at: Instant,
    picture: Option<SenderPicture>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<String, Cached>,
    bytes: usize,
}

impl Cache {
    fn insert(&mut self, domain: String, picture: Option<SenderPicture>) {
        self.remove(&domain);
        self.bytes += picture.as_ref().map_or(0, |picture| picture.bytes.len());
        self.entries.insert(domain, Cached { at: Instant::now(), picture });
        while self.bytes > MAX_CACHED_BYTES || self.entries.len() > MAX_CACHED_DOMAINS {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, cached)| cached.at).map(|(d, _)| d.clone()) else {
                break;
            };
            self.remove(&oldest);
        }
    }

    fn remove(&mut self, domain: &str) {
        if let Some(old) = self.entries.remove(domain) {
            self.bytes -= old.picture.map_or(0, |picture| picture.bytes.len());
        }
    }
}

pub struct SenderPictures {
    egress: Egress,
    resolver: Option<TokioResolver>,
    cache: Mutex<Cache>,
    unreachable: Mutex<HashMap<String, Instant>>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    permits: Semaphore,
}

impl SenderPictures {
    pub fn new(egress: Egress) -> SenderPictures {
        let resolver = mail_auth::MessageAuthenticator::new_system_conf().ok().map(|auth| auth.resolver().clone());
        SenderPictures::with_resolver(egress, resolver)
    }

    fn with_resolver(egress: Egress, resolver: Option<TokioResolver>) -> SenderPictures {
        SenderPictures {
            egress,
            resolver,
            cache: Mutex::default(),
            unreachable: Mutex::default(),
            locks: Mutex::default(),
            permits: Semaphore::new(PARALLEL_LOOKUPS),
        }
    }

    /// The picture for an address, `None` when it has none or gets none.
    pub async fn get(&self, email: &str) -> Option<SenderPicture> {
        let domain = picture_domain(email)?;
        let lock = self.locks.lock().unwrap_or_else(|e| e.into_inner()).entry(domain.clone()).or_default().clone();
        let _guard = lock.lock().await;
        let stale = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            match cache.entries.get(&domain) {
                Some(cached) if cached.at.elapsed() < FRESH_FOR => return cached.picture.clone(),
                Some(cached) => cached.picture.clone(),
                None => None,
            }
        };
        let unreachable = self.unreachable.lock().unwrap_or_else(|e| e.into_inner()).get(&domain).copied();
        if unreachable.is_some_and(|at| at.elapsed() < RETRY_UNREACHABLE_AFTER) {
            return stale;
        }
        let lookup = {
            let _permit = self.permits.acquire().await.ok()?;
            self.lookup(&domain).await
        };
        let picture = match lookup {
            Lookup::Unreachable => {
                self.unreachable.lock().unwrap_or_else(|e| e.into_inner()).insert(domain, Instant::now());
                return stale;
            }
            Lookup::Nothing => None,
            Lookup::Found(picture) => Some(picture),
        };
        self.unreachable.lock().unwrap_or_else(|e| e.into_inner()).remove(&domain);
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(domain.clone(), picture.clone());
        self.locks.lock().unwrap_or_else(|e| e.into_inner()).remove(&domain);
        picture
    }

    async fn lookup(&self, domain: &str) -> Lookup {
        let mut answered = false;
        let (logo, dns_answered) = self.bimi(domain).await;
        answered |= dns_answered;
        if let Some(url) = logo
            && let Ok(Some((bytes, _))) = self.download(url.as_str(), MAX_IMAGE, IMAGE_ACCEPT).await
            && sniff_image(&bytes) == Some("image/svg+xml")
        {
            return Lookup::Found(SenderPicture {
                domain: domain.to_owned(),
                kind: PictureKind::Logo,
                media_type: "image/svg+xml",
                bytes,
            });
        }

        let mut candidates = Vec::new();
        for start in [format!("https://{domain}/"), format!("https://www.{domain}/")] {
            match self.download(&start, MAX_PAGE, "text/html").await {
                Ok(Some((page, final_url))) => {
                    answered = true;
                    candidates = icon_links(&String::from_utf8_lossy(&page), &final_url);
                    if let Ok(favicon) = final_url.join("/favicon.ico") {
                        candidates.push((favicon, PictureKind::Icon));
                    }
                    break;
                }
                Ok(None) => answered = true,
                Err(()) => {}
            }
        }
        if candidates.is_empty()
            && let Ok(favicon) = Url::parse(&format!("https://{domain}/favicon.ico"))
        {
            candidates.push((favicon, PictureKind::Icon));
        }

        let mut blurry = None;
        let mut seen = Vec::new();
        for (url, kind) in candidates.into_iter().take(6) {
            if seen.contains(&url) {
                continue;
            }
            seen.push(url.clone());
            let Ok(response) = self.download(url.as_str(), MAX_IMAGE, IMAGE_ACCEPT).await else { continue };
            answered = true;
            let Some((bytes, _)) = response else { continue };
            let Some(media_type) = sniff_image(&bytes) else { continue };
            let picture = SenderPicture { domain: domain.to_owned(), kind, media_type, bytes };
            if pixel_width(&picture.bytes, media_type).is_some_and(|width| width < SHARP_WIDTH) {
                blurry.get_or_insert(picture);
                continue;
            }
            return Lookup::Found(picture);
        }
        match (blurry, answered) {
            (Some(picture), _) => Lookup::Found(SenderPicture { kind: PictureKind::Icon, ..picture }),
            (None, true) => Lookup::Nothing,
            (None, false) => Lookup::Unreachable,
        }
    }

    /// The BIMI logo address and whether DNS answered at all. DNS says nothing about who reads what, so it
    /// is asked directly.
    async fn bimi(&self, domain: &str) -> (Option<Url>, bool) {
        let Some(resolver) = &self.resolver else { return (None, false) };
        match tokio::time::timeout(DNS_TIMEOUT, resolver.txt_lookup(format!("default._bimi.{domain}."))).await {
            Ok(Ok(lookup)) => {
                let logo = lookup.answers().iter().find_map(|record| match &record.data {
                    hickory_resolver::proto::rr::RData::TXT(txt) => {
                        let text: String = txt.txt_data.iter().map(|part| String::from_utf8_lossy(part)).collect();
                        bimi_logo(&text)
                    }
                    _ => None,
                });
                (logo, true)
            }
            Ok(Err(err)) => (None, err.is_no_records_found() || err.is_nx_domain()),
            Err(_) => (None, false),
        }
    }

    /// `Ok(Some)` with the body and the final address, `Ok(None)` when the server answered with an error or
    /// too much, `Err` when nothing answered.
    async fn download(&self, url: &str, limit: usize, accept: &str) -> Result<Option<(Bytes, Url)>, ()> {
        match self.egress.get(url, accept, limit).await {
            Ok(fetched) => Ok(Some((fetched.body, fetched.url))),
            Err(EgressError::Unreachable | EgressError::Timeout) => Err(()),
            Err(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn only_company_domains_get_a_picture() {
        assert_eq!(picture_domain("news@mail.shop.example.co.uk"), Some("example.co.uk".into()));
        assert_eq!(picture_domain("Shop <news@news.shop.de>"), Some("shop.de".into()));
        assert_eq!(picture_domain("someone@gmail.com"), None);
        assert_eq!(picture_domain("someone@mail.gmx.net"), None);
        assert_eq!(picture_domain("root@localhost"), None);
        assert_eq!(picture_domain("x@[127.0.0.1]"), None);
        assert_eq!(picture_domain("x@intranet.corp"), None);
    }

    #[test]
    fn bimi_records_and_icon_links_are_read() {
        let logo = bimi_logo("v=BIMI1; l=https://shop.example/logo.svg; a=;").unwrap();
        assert_eq!(logo.as_str(), "https://shop.example/logo.svg");
        assert!(bimi_logo("v=BIMI1; l=http://shop.example/logo.svg").is_none());
        assert!(bimi_logo("v=spf1 -all").is_none());

        let base = Url::parse("https://www.shop.example/").unwrap();
        let html = r#"<head><link rel="icon" href="/favicon.ico"><link rel="apple-touch-icon" sizes="180x180" href="/touch.png"><link rel="icon" type="image/svg+xml" href="http://cdn.shop.example/i.svg"></head>"#;
        let links = icon_links(html, &base);
        assert_eq!(links[0].0.as_str(), "https://www.shop.example/touch.png");
        assert_eq!(links[0].1, PictureKind::Logo);
        assert!(links.iter().all(|(url, _)| url.scheme() == "https"), "no plain http");
    }

    #[test]
    fn pictures_are_told_by_their_bytes() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(b"<?xml version=\"1.0\"?><svg></svg>"), Some("image/svg+xml"));
        assert_eq!(sniff_image(b"<!doctype html><html><svg></svg></html>"), None);
    }

    #[test]
    fn the_cache_forgets_the_oldest_past_its_size() {
        let mut cache = Cache::default();
        let big = |domain: &str| SenderPicture {
            domain: domain.into(),
            kind: PictureKind::Icon,
            media_type: "image/png",
            bytes: Bytes::from(vec![0; MAX_CACHED_BYTES / 2]),
        };
        cache.insert("a.example".into(), Some(big("a.example")));
        cache.insert("b.example".into(), Some(big("b.example")));
        cache.insert("c.example".into(), Some(big("c.example")));
        assert!(!cache.entries.contains_key("a.example"));
        assert!(cache.entries.contains_key("c.example"));
        assert!(cache.bytes <= MAX_CACHED_BYTES);
    }

    /// A website over TLS with an apple-touch-icon; remembers the paths it was asked for.
    async fn website() -> (Egress, Arc<Mutex<Vec<String>>>) {
        let generated = rcgen::generate_simple_self_signed(vec!["shop.de".into(), "www.shop.de".into()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let log = log.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(socket).await else { return };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() == 1 {
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8(head).unwrap();
                    let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                    log.lock().unwrap().push(path.clone());
                    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
                    png.extend_from_slice(&180u32.to_be_bytes());
                    png.extend_from_slice(&180u32.to_be_bytes());
                    let (status, body) = match path.as_str() {
                        "/" => ("200 OK", br#"<head><link rel="apple-touch-icon" href="/touch.png"></head>"#.to_vec()),
                        "/touch.png" => ("200 OK", png),
                        _ => ("404 Not Found", Vec::new()),
                    };
                    let answer = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n", body.len());
                    stream.write_all(answer.as_bytes()).await.unwrap();
                    stream.write_all(&body).await.unwrap();
                    let _ = stream.shutdown().await;
                });
            }
        });
        (Egress::pinned_trusting(address, generated.cert.der().clone()), seen)
    }

    #[tokio::test]
    async fn a_website_icon_is_fetched_once_and_then_remembered() {
        let (egress, seen) = website().await;
        let pictures = SenderPictures::with_resolver(egress, None);
        let picture = pictures.get("news@mail.shop.de").await.unwrap();
        assert_eq!(
            (picture.kind, picture.media_type, picture.domain.as_str()),
            (PictureKind::Logo, "image/png", "shop.de")
        );
        let asked = seen.lock().unwrap().len();
        assert!(pictures.get("other@shop.de").await.is_some());
        assert_eq!(seen.lock().unwrap().len(), asked, "the second address of the same company asks nobody");
        assert!(pictures.get("friend@gmail.com").await.is_none());
    }
}
