//! What a message itself gives away, read once: headers every mail program writes, how the message is
//! built, what its links claim, and its attachments. Plain functions over the bytes, so they run on a
//! blocking thread and are easy to test.

use mail_parser::{Address, Encoding, Message, MimeHeaders, PartType};
use md5::{Digest, Md5};
use sha2::Sha256;
use uwumail_store::mime_limits::parse_message;

use super::links::{self, Link, Target};
use super::{Hit, attachments, bayes, html, phishing};

/// Messages larger than this are not read for content; the rules about the sending server still apply.
pub(crate) const MAX_MESSAGE: usize = 25 * 1024 * 1024;
/// Hidden text only counts beyond this many characters: a hidden preview line is common and fine.
const HIDDEN_TEXT_CHARS: usize = 200;
const DAY: i64 = 24 * 3600;

#[derive(Debug, Default)]
pub(crate) struct Examination {
    pub hits: Vec<Hit>,
    /// Link domains worth asking a domain blocklist about.
    pub link_domains: Vec<String>,
    /// The message's Bayes tokens, hashed; empty without a key.
    pub tokens: Vec<i64>,
    /// The subject, and the text a reader sees (HTML turned into text), for word lists.
    pub subject: String,
    pub text: String,
    /// Link addresses and hosts, and each attachment's name with its MD5 and SHA-256, for built-in lists.
    pub urls: Vec<String>,
    pub link_hosts: Vec<String>,
    pub files: Vec<(String, [String; 2])>,
    pub from_domain: Option<String>,
    pub reply_to_domain: Option<String>,
}

fn add(hits: &mut Vec<Hit>, rule: &'static str, points: f32, detail: Option<String>) {
    if !hits.iter().any(|hit| hit.rule == rule) {
        hits.push(Hit { rule, points, detail });
    }
}

/// What the text and HTML parts of a message hold.
#[derive(Default)]
struct Body {
    links: Vec<Link>,
    text_parts: usize,
    html_parts: usize,
    hidden_chars: usize,
    /// A text part is base64-encoded although it is plain ASCII.
    needless_base64: bool,
}

fn read_body(message: &Message<'_>) -> Body {
    let mut ids: Vec<u32> = message.text_body.iter().chain(&message.html_body).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    let mut body = Body::default();
    for part in ids.iter().filter_map(|id| message.parts.get(*id as usize)) {
        let content = match &part.body {
            PartType::Text(text) => {
                body.text_parts += 1;
                body.links.extend(links::in_text(text));
                text
            }
            PartType::Html(html) => {
                body.html_parts += 1;
                let read = html::read(html);
                body.hidden_chars += read.hidden_chars;
                body.links.extend(links::in_anchors(&read.anchors));
                html
            }
            _ => continue,
        };
        // Base64 hides words from simple filters; plain ASCII text never needs it.
        if part.encoding == Encoding::Base64 && !content.trim().is_empty() && content.is_ascii() {
            body.needless_base64 = true;
        }
    }
    body
}

/// The sites a message links to, as the Bayes filter sees them.
fn link_sites(found: &[Link]) -> Vec<String> {
    found
        .iter()
        .map(|link| match &link.target {
            Target::Domain(domain) => links::site(domain),
            Target::Ip(_) => "ip".to_owned(),
        })
        .collect()
}

/// The hashed Bayes tokens of a stored message, for learning it.
pub(crate) fn learning_tokens(raw: &[u8], key: &[u8; 32]) -> Vec<i64> {
    if raw.len() > MAX_MESSAGE {
        return Vec::new();
    }
    let Some(message) = parse_message(raw) else { return Vec::new() };
    let body = read_body(&message);
    bayes::hashed(key, &bayes::tokens(&message, &link_sites(&body.links)))
}

/// Reads a message for the content rules. `dmarc_passed` softens the rules that newsletters from real
/// senders trip over: tracking links whose text shows the shop's own address, and hidden preview text.
/// With a `key`, the message's Bayes tokens come along.
pub(crate) fn examine(raw: &[u8], now: i64, dmarc_passed: bool, key: Option<&[u8; 32]>) -> Examination {
    if raw.len() > MAX_MESSAGE {
        return Examination::default();
    }
    let Some(message) = parse_message(raw) else { return Examination::default() };
    let mut hits = Vec::new();
    headers(&message, now, &mut hits);

    let body = read_body(&message);
    if body.needless_base64 {
        add(&mut hits, "BASE64_TEXT", 1.0, None);
    }
    if body.html_parts > 0 && body.text_parts == 0 {
        add(&mut hits, "HTML_ONLY", 0.5, None);
    }
    if !dmarc_passed && body.hidden_chars > HIDDEN_TEXT_CHARS {
        add(&mut hits, "HIDDEN_TEXT", 1.0, Some(format!("{} characters", body.hidden_chars)));
    }

    let text = visible_text(&message);
    let from = message.from().and_then(|from| from.first());
    let seen = phishing::Read {
        from_name: from.and_then(|from| from.name.as_deref()).map(str::to_owned),
        from_address: from.and_then(|from| from.address.as_deref()).map(str::to_owned),
        reply_to: message
            .reply_to()
            .and_then(|reply| reply.first())
            .and_then(|reply| reply.address.as_deref())
            .map(str::to_owned),
        subject: message.subject().unwrap_or_default().chars().take(MAX_SUBJECT).collect(),
        text: text.chars().take(64 * 1024).collect(),
        links: body.links.iter().take(200).map(phishing::SeenLink::of).collect(),
        mailing_list: message.header("List-Id").is_some() || message.header("List-Post").is_some(),
    };
    for finding in phishing::check(&seen.input()) {
        // A link text naming some third site is what newsletters with tracking links do all the
        // time; from a sender DMARC vouches for it is no trick. A brand's address on a link to
        // somewhere else always is.
        let softened = dmarc_passed && finding.rule == "PHISHING_LINK_TEXT";
        if finding.points > 0.0 && !softened {
            add(&mut hits, finding.rule, finding.points, Some(finding.detail));
        }
    }
    for link in &body.links {
        match &link.target {
            Target::Ip(ip) => add(&mut hits, "LINK_TO_IP", 1.5, Some(ip.to_string())),
            // The same trick, said more precisely by the brand check.
            Target::Domain(domain)
                if links::is_lookalike(domain) && !hits.iter().any(|hit| hit.rule == "LOOKALIKE_BRAND_LINK") =>
            {
                add(&mut hits, "LOOKALIKE_LINK", 3.0, Some(domain.clone()));
            }
            Target::Domain(_) => {}
        }
    }

    for hit in attachments::judge(&message) {
        add(&mut hits, hit.rule, hit.points, hit.detail);
    }
    let tokens =
        key.map(|key| bayes::hashed(key, &bayes::tokens(&message, &link_sites(&body.links)))).unwrap_or_default();
    let subject = message.subject().unwrap_or_default().chars().take(MAX_SUBJECT).collect();
    let mut urls: Vec<String> = body.links.iter().filter_map(|link| link.url.clone()).collect();
    urls.sort();
    urls.dedup();
    let mut link_hosts: Vec<String> = body
        .links
        .iter()
        .filter_map(|link| match &link.target {
            Target::Domain(domain) => Some(domain.clone()),
            Target::Ip(_) => None,
        })
        .collect();
    link_hosts.sort();
    link_hosts.dedup();
    let files = message
        .attachments()
        .take(MAX_FILES)
        .map(|part| {
            let name = part.attachment_name().unwrap_or("attachment").to_owned();
            let contents = part.contents();
            (name, [hex::encode(Md5::digest(contents)), hex::encode(Sha256::digest(contents))])
        })
        .collect();
    Examination {
        hits,
        link_domains: links::domains_to_look_up(&body.links),
        tokens,
        subject,
        text,
        urls,
        link_hosts,
        files,
        from_domain: address_domain(message.from()),
        reply_to_domain: address_domain(message.reply_to()),
    }
}

/// Attachments looked at for known malware; more are rarely more than a padded spam.
const MAX_FILES: usize = 25;

fn address_domain(address: Option<&Address<'_>>) -> Option<String> {
    let address = address?.first()?.address()?;
    address.rsplit_once('@').map(|(_, domain)| domain.trim().trim_end_matches('.').to_ascii_lowercase())
}

/// Enough of a message's text for word lists: spammers put their words up front.
const MAX_TEXT: usize = 200 * 1024;
const MAX_SUBJECT: usize = 1_000;

/// The text parts, and HTML parts turned into text, as a reader sees them.
fn visible_text(message: &Message<'_>) -> String {
    let mut text = String::new();
    for index in 0..message.text_body.len() {
        if text.len() >= MAX_TEXT {
            break;
        }
        if let Some(body) = message.body_text(index) {
            text.push_str(&body);
            text.push('\n');
        }
    }
    text
}

/// Headers every mail program writes. Missing ones, or a date days away from now, are typical for
/// tools that send spam.
fn headers(message: &Message<'_>, now: i64, hits: &mut Vec<Hit>) {
    match message.date().map(|date| date.to_timestamp()) {
        None => add(hits, "MISSING_DATE", 1.0, None),
        Some(at) if at > now + DAY => {
            add(hits, "DATE_IN_FUTURE", 2.0, Some(format!("{} days ahead", (at - now) / DAY)))
        }
        Some(at) if at < now - 30 * DAY => {
            add(hits, "DATE_IN_PAST", 1.0, Some(format!("{} days old", (now - at) / DAY)))
        }
        Some(_) => {}
    }
    if message.message_id().is_none() {
        add(hits, "MISSING_MESSAGE_ID", 1.0, None);
    }
    if let Some(subject) = message.subject() {
        let letters = subject.chars().filter(|c| c.is_alphabetic()).count();
        if letters >= 10 && !subject.chars().any(char::is_lowercase) {
            add(hits, "SUBJECT_ALL_CAPS", 1.5, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;

    const DATE: &str = "Wed, 16 Sep 2026 10:00:00 +0000";

    #[test]
    fn links_attachments_and_addresses_are_read_for_built_in_lists() {
        let raw = "From: Shop <news@Shop.example>
Reply-To: kasse@freemail.example
Subject: Rechnung
\
            MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"m\"

\
            --m
Content-Type: text/html

<a href=\"https://Files.Example/x.exe#a\">x</a>
\
            --m
Content-Type: application/octet-stream
Content-Disposition: attachment; filename=\"x.exe\"
\
            Content-Transfer-Encoding: base64

TVo=
--m--
";
        let examination = examine(raw.as_bytes(), now(), false, None);
        assert_eq!(examination.urls, ["https://files.example/x.exe"]);
        assert_eq!(examination.link_hosts, ["files.example"]);
        assert_eq!(examination.from_domain.as_deref(), Some("shop.example"));
        assert_eq!(examination.reply_to_domain.as_deref(), Some("freemail.example"));
        let (name, [md5, sha256]) = &examination.files[0];
        assert_eq!(name, "x.exe");
        assert_eq!(md5, "ac6ad5d9b99757c3a878f2d275ace198");
        assert_eq!(sha256, "9b8db510ef42b8ed54a3712636fda55a4f8cfcd5493e20b74ab00cd4f3979f2d");
    }

    fn now() -> i64 {
        let raw = format!("Date: {DATE}\r\n\r\n");
        mail_parser::MessageParser::default().parse(raw.as_bytes()).unwrap().date().unwrap().to_timestamp()
    }

    fn rules(raw: &str, dmarc_passed: bool) -> Vec<&'static str> {
        let mut rules: Vec<&'static str> =
            examine(raw.as_bytes(), now(), dmarc_passed, None).hits.into_iter().map(|hit| hit.rule).collect();
        rules.sort_unstable();
        rules
    }

    fn newsletter() -> String {
        format!(
            "From: Shop <news@shop.example>\r\nDate: {DATE}\r\nMessage-ID: <1@shop.example>\r\nSubject: Neue Angebote\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"a\"\r\n\r\n\
             --a\r\nContent-Type: text/plain\r\n\r\nHallo! https://shop.example/angebote\r\n\
             --a\r\nContent-Type: text/html\r\n\r\n<p style=\"display:none\">Vorschau</p>\
             <a href=\"https://click.mailer.example/t/1\">https://shop.example/angebote</a>\r\n--a--\r\n"
        )
    }

    #[test]
    fn a_real_newsletter_trips_nothing_and_its_tracking_links_only_count_without_dmarc() {
        assert!(rules(&newsletter(), true).is_empty(), "{:?}", rules(&newsletter(), true));
        // Its tracking links show the shop's own address, which is no trick even without DMARC.
        assert!(rules(&newsletter(), false).is_empty(), "{:?}", rules(&newsletter(), false));
        let found = examine(newsletter().as_bytes(), now(), true, None);
        assert_eq!(found.link_domains, ["shop.example", "click.mailer.example"]);
    }

    #[test]
    fn a_phishing_mail_shows_its_tricks() {
        let lookalike = idna::domain_to_ascii("p\u{430}ypal.example").unwrap();
        let raw = format!(
            "From: \"service@bank.example\" <alert@evil.example>\r\nSubject: KONTO GESPERRT SOFORT\r\n\
             Content-Type: text/html\r\n\r\n<a href=\"http://192.0.2.7/login\">www.bank.example</a> \
             <a href=\"https://{lookalike}/\">Konto</a>\r\n"
        );
        assert_eq!(
            rules(&raw, false),
            [
                "CREDENTIAL_REQUEST",
                "FROM_NAME_SPOOFS_ADDRESS",
                "HTML_ONLY",
                "LINK_TO_IP",
                "LOOKALIKE_BRAND_LINK",
                "MISSING_DATE",
                "MISSING_MESSAGE_ID",
                "PHISHING_LINK_TEXT",
                "SUBJECT_ALL_CAPS"
            ]
        );
    }

    #[test]
    fn dates_far_away_and_names_that_match_the_address() {
        let with = |from: &str, date: &str| {
            format!("From: {from}\r\nDate: {date}\r\nMessage-ID: <2@x.example>\r\nSubject: Hi\r\n\r\nHallo\r\n")
        };
        assert_eq!(rules(&with("a@x.example", "Tue, 22 Sep 2026 10:00:00 +0000"), false), ["DATE_IN_FUTURE"]);
        assert_eq!(rules(&with("a@x.example", "Sat, 11 Jul 2026 10:00:00 +0000"), false), ["DATE_IN_PAST"]);
        // A name that is the address itself, or an address of the same site, is no trick.
        assert!(rules(&with("\"a@x.example\" <a@x.example>", DATE), false).is_empty());
        assert!(rules(&with("\"Support (help@x.example)\" <noreply@mail.x.example>", DATE), false).is_empty());
        // Short shouting is fine.
        assert!(
            rules(
                &format!("From: a@x.example\r\nDate: {DATE}\r\nMessage-ID: <3@x>\r\nSubject: USA OK\r\n\r\nx\r\n"),
                false
            )
            .is_empty()
        );
    }

    #[test]
    fn base64_for_plain_ascii_text_and_hidden_paragraphs() {
        let encode = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
        let with_body = |body: String| {
            format!(
                "From: a@x.example\r\nDate: {DATE}\r\nMessage-ID: <4@x>\r\nSubject: Hi\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}\r\n"
            )
        };
        assert_eq!(rules(&with_body(encode("Cheap pills, best price")), false), ["BASE64_TEXT"]);
        assert!(rules(&with_body(encode("Viele Grüße aus Köln")), false).is_empty());

        let hidden = "x".repeat(250);
        let raw = format!(
            "From: a@x.example\r\nDate: {DATE}\r\nMessage-ID: <5@x>\r\nSubject: Hi\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/alternative; boundary=\"a\"\r\n\r\n--a\r\nContent-Type: text/plain\r\n\r\nHallo\r\n\
             --a\r\nContent-Type: text/html\r\n\r\n<p>Hallo</p><div style=\"display:none\">{hidden}</div>\r\n--a--\r\n"
        );
        assert_eq!(rules(&raw, false), ["HIDDEN_TEXT"]);
        assert!(rules(&raw, true).is_empty());
    }

    #[test]
    fn broken_or_huge_messages_give_nothing() {
        assert!(examine(b"\x00\xff not mail at all", now(), false, None).hits.len() <= 3);
        let huge = vec![b'a'; MAX_MESSAGE + 1];
        assert!(examine(&huge, now(), false, None).hits.is_empty());
    }
}
