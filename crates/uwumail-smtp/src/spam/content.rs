//! What a message itself gives away, read once: headers every mail program writes, how the message is
//! built, what its links claim, and its attachments. Plain functions over the bytes, so they run on a
//! blocking thread and are easy to test.

use mail_parser::{Encoding, Message, MessageParser, PartType};

use super::links::{self, Target};
use super::{Hit, attachments, html};

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
}

fn add(hits: &mut Vec<Hit>, rule: &'static str, points: f32, detail: Option<String>) {
    if !hits.iter().any(|hit| hit.rule == rule) {
        hits.push(Hit { rule, points, detail });
    }
}

/// Reads a message for the content rules. `dmarc_passed` softens the rules that newsletters from real
/// senders trip over: tracking links whose text shows the shop's own address, and hidden preview text.
pub(crate) fn examine(raw: &[u8], now: i64, dmarc_passed: bool) -> Examination {
    if raw.len() > MAX_MESSAGE {
        return Examination::default();
    }
    let Some(message) = MessageParser::default().parse(raw) else { return Examination::default() };
    let mut hits = Vec::new();
    headers(&message, now, &mut hits);
    from_name(&message, &mut hits);

    let mut body: Vec<u32> = message.text_body.iter().chain(&message.html_body).copied().collect();
    body.sort_unstable();
    body.dedup();
    let (mut text_parts, mut html_parts, mut hidden_chars) = (0, 0, 0);
    let mut found = Vec::new();
    for part in body.iter().filter_map(|id| message.parts.get(*id as usize)) {
        let content = match &part.body {
            PartType::Text(text) => {
                text_parts += 1;
                found.extend(links::in_text(text));
                text
            }
            PartType::Html(html) => {
                html_parts += 1;
                let read = html::read(html);
                hidden_chars += read.hidden_chars;
                found.extend(links::in_anchors(&read.anchors));
                html
            }
            _ => continue,
        };
        // Base64 hides words from simple filters; plain ASCII text never needs it.
        if part.encoding == Encoding::Base64 && !content.trim().is_empty() && content.is_ascii() {
            add(&mut hits, "BASE64_TEXT", 1.0, None);
        }
    }
    if html_parts > 0 && text_parts == 0 {
        add(&mut hits, "HTML_ONLY", 0.5, None);
    }
    if !dmarc_passed && hidden_chars > HIDDEN_TEXT_CHARS {
        add(&mut hits, "HIDDEN_TEXT", 1.0, Some(format!("{hidden_chars} characters")));
    }

    for link in &found {
        match (&link.target, link.text.as_deref().and_then(links::named_in_text)) {
            (Target::Domain(domain), Some(named)) if !dmarc_passed && !links::same_site(&named, domain) => {
                add(&mut hits, "PHISHING_LINK_TEXT", 3.0, Some(format!("{named} -> {domain}")));
            }
            (Target::Ip(ip), Some(named)) if !dmarc_passed => {
                add(&mut hits, "PHISHING_LINK_TEXT", 3.0, Some(format!("{named} -> {ip}")));
            }
            _ => {}
        }
        match &link.target {
            Target::Ip(ip) => add(&mut hits, "LINK_TO_IP", 1.5, Some(ip.to_string())),
            Target::Domain(domain) if links::is_lookalike(domain) => {
                add(&mut hits, "LOOKALIKE_LINK", 3.0, Some(domain.clone()));
            }
            Target::Domain(_) => {}
        }
    }

    for hit in attachments::judge(&message) {
        add(&mut hits, hit.rule, hit.points, hit.detail);
    }
    Examination { hits, link_domains: links::domains_to_look_up(&found) }
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

/// A display name showing an address of another site than the one the mail comes from, like
/// "service@bank.example" <someone@elsewhere.example>.
fn from_name(message: &Message<'_>, hits: &mut Vec<Hit>) {
    let Some(from) = message.from().and_then(|from| from.first()) else { return };
    let (Some(name), Some(address)) = (from.name.as_deref(), from.address.as_deref()) else { return };
    let Some((_, actual)) = address.rsplit_once('@') else { return };
    let shown = name
        .split(|c: char| c.is_whitespace() || "<>()[]\"',;:".contains(c))
        .filter_map(|word| word.rsplit_once('@').map(|(_, domain)| domain.trim_end_matches('.')))
        .find(|domain| domain.contains('.') && !links::same_site(domain, actual));
    if shown.is_some() {
        add(hits, "FROM_NAME_SPOOFS_ADDRESS", 3.0, Some(format!("{name} <{address}>")));
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;

    const DATE: &str = "Wed, 16 Sep 2026 10:00:00 +0000";

    fn now() -> i64 {
        let raw = format!("Date: {DATE}\r\n\r\n");
        MessageParser::default().parse(raw.as_bytes()).unwrap().date().unwrap().to_timestamp()
    }

    fn rules(raw: &str, dmarc_passed: bool) -> Vec<&'static str> {
        let mut rules: Vec<&'static str> =
            examine(raw.as_bytes(), now(), dmarc_passed).hits.into_iter().map(|hit| hit.rule).collect();
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
        assert_eq!(rules(&newsletter(), false), ["PHISHING_LINK_TEXT"]);
        let found = examine(newsletter().as_bytes(), now(), true);
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
                "FROM_NAME_SPOOFS_ADDRESS",
                "HTML_ONLY",
                "LINK_TO_IP",
                "LOOKALIKE_LINK",
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
        assert!(examine(b"\x00\xff not mail at all", now(), false).hits.len() <= 3);
        let huge = vec![b'a'; MAX_MESSAGE + 1];
        assert!(examine(&huge, now(), false).hits.is_empty());
    }
}
