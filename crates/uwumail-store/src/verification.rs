//! Finding the one-time code or sign-in link in a mail (docs/verification-codes.md): what the
//! `Verification-Info` header of the OMXP draft says when the sender's signature vouches for it,
//! otherwise a careful guess from the subject and the text.
//!
//! Everything here is pure: bytes in, an answer or nothing out, no database and no clock. The SMTP
//! crate runs it once per incoming mail, the store keeps the answer (src/verification_codes.rs).
//!
//! The header is only read when a DKIM signature of the From domain covers it and DMARC passed:
//! anyone on the way could write one, and a code shown in a notification and offered for autofill
//! is exactly what a phisher wants to plant. A header that does not parse, names an origin outside
//! the sender's registrable domain or carries no expiry is not an error; the mail then goes through
//! the guess like every other.
//!
//! The guess is deliberately shy. A code in the notification that turns out to be an order number
//! is worse than no code at all, so it takes exactly one clear candidate next to a word like "code"
//! or "Bestätigungscode" and gives up on anything that looks like a date, an amount, a phone number,
//! a postal code or an order, invoice or customer number. Newsletters with discount codes stay
//! newsletters.

use std::sync::LazyLock;

use regex::Regex;

/// The longest header value read; a real one is well under 300 bytes.
const MAX_HEADER: usize = 1024;
/// At most this many `key=value` pairs in the header.
const MAX_PARAMS: usize = 8;
/// The longest link kept.
const MAX_URL: usize = 2048;
/// How much of subject and text the guess reads: codes sit at the top of a mail.
const MAX_TEXT: usize = 16 * 1024;
/// The largest message looked at at all (security-audit-0.26.0 VC-09). A code mail is a few
/// kilobytes; parsing a big one in full again only for this is not worth it.
pub const MAX_MESSAGE: usize = 1024 * 1024;
/// A code is never valid for longer, whatever the sender writes.
pub const MAX_VALIDITY: i64 = 24 * 3600;
/// How long a guessed code counts as valid when the text says nothing about it.
pub const DEFAULT_VALIDITY: i64 = 15 * 60;

/// A code to type or a link to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Otp,
    Link,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Otp => "otp",
            Kind::Link => "link",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "otp" => Some(Kind::Otp),
            "link" => Some(Kind::Link),
            _ => None,
        }
    }
}

/// What the code is for, as the OMXP draft names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Login,
    Signup,
    ConfirmEmail,
    ResetPassword,
    ConfirmAction,
    Other,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Login => "login",
            Purpose::Signup => "signup",
            Purpose::ConfirmEmail => "confirm-email",
            Purpose::ResetPassword => "reset-password",
            Purpose::ConfirmAction => "confirm-action",
            Purpose::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "login" => Purpose::Login,
            "signup" => Purpose::Signup,
            "confirm-email" => Purpose::ConfirmEmail,
            "reset-password" => Purpose::ResetPassword,
            "confirm-action" => Purpose::ConfirmAction,
            "other" => Purpose::Other,
            _ => return None,
        })
    }
}

/// Where an answer came from: the signed header, or the guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Header,
    Heuristic,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Header => "header",
            Source::Heuristic => "heuristic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "header" => Some(Source::Header),
            "heuristic" => Some(Source::Heuristic),
            _ => None,
        }
    }
}

/// A code or link found in a mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub kind: Kind,
    /// The host the code is used on. For a guess, the registrable From domain: the best there is.
    pub origin: String,
    /// The code, without the space or hyphen a guess may have found in the middle of it.
    pub code: Option<String>,
    pub url: Option<String>,
    pub purpose: Purpose,
    /// Unix seconds.
    pub expires_at: i64,
    pub source: Source,
    /// Whether clients may offer it for autofill on `origin`: only when DMARC passed, so the From
    /// domain the origin is bound to is really the sender's.
    pub autofill: bool,
}

impl Detected {
    /// The `verification` object of JMAP and Web Push (docs/jmap-verification.md).
    pub fn to_json(&self) -> serde_json::Value {
        let mut object = serde_json::json!({
            "kind": self.kind.as_str(),
            "origin": self.origin,
            "code": self.code,
            "purpose": self.purpose.as_str(),
            "expiresAt": utc(self.expires_at),
            "source": self.source.as_str(),
            "autofill": self.autofill,
        });
        if let Some(url) = &self.url {
            object["url"] = url.clone().into();
        }
        object
    }
}

/// `2026-11-01T17:05:00Z`.
fn utc(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// What the SMTP checks found out about the sender, which decides what is believed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Authenticity {
    /// A passing DKIM signature of the From domain covers `Verification-Info`.
    pub header_signed: bool,
    pub dmarc_passed: bool,
    /// The domain of the From address.
    pub from_domain: Option<String>,
}

/// The registrable domain of a host name by the Public Suffix List: `login.shop.example` and
/// `shop.example` both give `shop.example`. Nothing for what is not a plausible host name.
pub fn registrable_domain(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if !is_host(&host) {
        return None;
    }
    psl::domain_str(&host).map(str::to_owned)
}

/// Letters, digits, hyphens and dots, at least two labels, none empty or edged with a hyphen.
fn is_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Finds the code or link in `raw`, received at `received_at` (Unix seconds). The signed header
/// wins; otherwise the guess, and that only when DMARC passed (security-audit-0.26.0 VC-03): a
/// guess from mail anyone could have written would carry a domain it may not come from. A code
/// that has already expired is no code (VC-08), and a message over [`MAX_MESSAGE`] is not looked
/// at (VC-09). Never panics, whatever the bytes.
pub fn detect(raw: &[u8], auth: &Authenticity, received_at: i64) -> Option<Detected> {
    if raw.len() > MAX_MESSAGE {
        return None;
    }
    let found = find(raw, auth, received_at)?;
    (found.expires_at > received_at).then_some(found)
}

fn find(raw: &[u8], auth: &Authenticity, received_at: i64) -> Option<Detected> {
    let message = crate::mime_limits::parse_message(raw)?;
    let from_domain = auth.from_domain.as_deref().and_then(registrable_domain)?;

    if auth.header_signed && auth.dmarc_passed {
        let values: Vec<&str> = message
            .headers_raw()
            .filter(|(name, _)| name.eq_ignore_ascii_case("Verification-Info"))
            .map(|(_, value)| value)
            .collect();
        // Two headers that might say different things: neither is believed.
        if let [value] = values.as_slice() {
            let sent = message.date().map(|date| date.to_timestamp());
            if let Some(found) = from_header(value, &from_domain, sent, received_at) {
                return Some(found);
            }
        }
    }
    if !auth.dmarc_passed {
        return None;
    }

    let mut text = String::new();
    if let Some(subject) = message.subject() {
        text.push_str(subject);
        text.push('\n');
    }
    if let Some(body) = message.body_text(0) {
        text.push_str(&body);
    }
    let text = cut(&text, MAX_TEXT);
    let (code, purpose, validity) = guess(text)?;
    Some(Detected {
        kind: Kind::Otp,
        origin: from_domain,
        code: Some(code),
        url: None,
        purpose,
        expires_at: received_at.saturating_add(validity),
        source: Source::Heuristic,
        // Only ever reached with DMARC passed, see above.
        autofill: true,
    })
}

/// At most `max` bytes of `text`, cut at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

// --- The header ------------------------------------------------------------------------------

/// Reads `kind=otp; origin=…; code=…; purpose=…; expires=…`. Strict: anything off and the header
/// counts as absent.
fn from_header(value: &str, from_domain: &str, sent: Option<i64>, received_at: i64) -> Option<Detected> {
    if value.len() > MAX_HEADER {
        return None;
    }
    // Unfold: a header may continue on lines that start with white space.
    let value: String = value.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    let mut fields: Vec<(String, String)> = Vec::new();
    for part in value.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, field) = part.split_once('=')?;
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
            return None;
        }
        let field = field.trim();
        let field = field.strip_prefix('"').and_then(|f| f.strip_suffix('"')).unwrap_or(field);
        if fields.iter().any(|(known, _)| *known == key) || fields.len() == MAX_PARAMS {
            return None;
        }
        fields.push((key, field.to_owned()));
    }
    let get = |key: &str| fields.iter().find(|(known, _)| known == key).map(|(_, value)| value.as_str());

    let kind = Kind::parse(&get("kind")?.to_ascii_lowercase())?;
    let origin = get("origin")?.trim_end_matches('.').to_ascii_lowercase();
    if registrable_domain(&origin)? != from_domain {
        return None;
    }
    let purpose = match get("purpose") {
        Some(purpose) => Purpose::parse(&purpose.to_ascii_lowercase())?,
        None => Purpose::Other,
    };
    let code = match get("code") {
        Some(code) if is_code(code) => Some(code.to_owned()),
        Some(_) => return None,
        None => None,
    };
    let url = match get("url") {
        Some(url) => Some(https_below(url, &origin)?),
        None => None,
    };
    match kind {
        Kind::Otp if code.is_none() => return None,
        Kind::Link if url.is_none() => return None,
        _ => {}
    }
    let expires = chrono::DateTime::parse_from_rfc3339(get("expires")?).ok()?.timestamp();
    // Never longer than a day: not after the mail was written, not after it arrived.
    let latest = sent.unwrap_or(received_at).min(received_at).saturating_add(MAX_VALIDITY);
    Some(Detected {
        kind,
        origin,
        code,
        url,
        purpose,
        expires_at: expires.min(latest),
        source: Source::Header,
        autofill: true,
    })
}

/// 4 to 12 characters of `[0-9A-Z-]`, at least one of them not a hyphen (VC-11).
fn is_code(code: &str) -> bool {
    (4..=12).contains(&code.len())
        && code.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase() || b == b'-')
        && code.bytes().any(|b| b != b'-')
}

/// `url` when it is an `https` link to `origin` or a host below it, without user name or password.
fn https_below(url: &str, origin: &str) -> Option<String> {
    if url.len() > MAX_URL || url.bytes().any(|b| b <= b' ' || b == 0x7f || b == b'"' || b == b'<' || b == b'>') {
        return None;
    }
    let rest = url.get(..8).filter(|scheme| scheme.eq_ignore_ascii_case("https://")).and(url.get(8..))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') || authority.contains('\\') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) => return None,
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    (is_host(&host) && (host == origin || host.ends_with(&format!(".{origin}")))).then(|| url.to_owned())
}

// --- The guess -------------------------------------------------------------------------------

/// Validity in the text: "gültig für 10 Minuten", "valid for 2 hours", "expires in 15 min".
static VALIDITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:gültig|valid|expires?|läuft|verfällt)\D{0,30}?(\d{1,3})\s*(minuten|minutes|minute|min|stunden|stunde|hours|hour|std)\b",
    )
    .expect("a valid pattern")
});

/// Code words that are no code to sign in with.
const NOT_A_CODE: &[&str] = &[
    "rabatt",
    "gutschein",
    "promo",
    "discount",
    "coupon",
    "voucher",
    "aktions",
    "post",
    "zip",
    "bar",
    "qr",
    "quell",
    "source",
    "gift",
    "geschenk",
    "farb",
    "color",
    "colour",
    "fehler",
    "error",
    "status",
    "dress",
    "steuer",
    "area",
    "country",
    "länder",
    "vorwahl",
    "html",
];

/// Short words that are keywords on their own.
const SHORT_KEYWORDS: &[&str] = &["otp", "2fa", "pin", "tan", "mtan", "passcode", "totp", "einmalpasswort"];

/// Words just before or after a number that make it something else, also inside compounds such as
/// "Kundennummer" or "Sendungsnummer".
const NOT_NEAR: &[&str] = &[
    "bestell",
    "order",
    "rechnung",
    "invoice",
    "kunden",
    "customer",
    "auftrag",
    "sendung",
    "tracking",
    "artikel",
    "nummer",
    "number",
    "ticket",
    "referenz",
    "reference",
    "telefon",
    "phone",
    "hotline",
    "vertrag",
    "contract",
    "vorgang",
    "paket",
    "package",
    "lieferung",
    "shipment",
    "receipt",
    "quittung",
    "transaktion",
    "transaction",
    "postfach",
    "zimmer",
];

/// The same, for words too short to look for inside others.
const NOT_NEAR_EXACTLY: &[&str] =
    &["nr", "no", "iban", "ref", "plz", "tel", "fax", "item", "case", "bon", "room", "seite", "page", "id"];

const CURRENCIES: &[&str] = &["€", "eur", "euro", "euros", "$", "usd", "dollar", "dollars", "£", "gbp", "chf", "fr"];

/// Words in a line that make its "code" a sales pitch.
const PROMOTION: &[&str] = &[
    "rabatt",
    "gutschein",
    "discount",
    "coupon",
    "voucher",
    "promo",
    "sale",
    "sparen",
    "spare",
    "save",
    "angebot",
    "offer",
    "gratis",
    "versandkostenfrei",
];

/// A word of a line, as written and folded for comparing.
struct Word<'a> {
    text: &'a str,
    lower: String,
}

fn words(line: &str) -> Vec<Word<'_>> {
    line.split(|c: char| c.is_whitespace() || c == ':' || c == '=')
        .map(|word| word.trim_matches(|c: char| ".,;!?()[]{}\"'<>*«»„“”‚‘’|".contains(c)))
        .filter(|word| !word.is_empty())
        .map(|text| Word { text, lower: text.to_lowercase() })
        .collect()
}

/// Whether a line asks for a code: "Bestätigungscode", "verification code", "Code:", "OTP", "2FA".
fn has_keyword(words: &[Word<'_>]) -> bool {
    let promoted = words.iter().any(|word| {
        word.text.contains('%') || PROMOTION.iter().any(|promotion| word.lower.trim_matches('-').starts_with(promotion))
    });
    if promoted {
        return false;
    }
    words.iter().enumerate().any(|(index, word)| {
        let lower = word.lower.as_str();
        if SHORT_KEYWORDS.contains(&lower.trim_matches('-')) {
            return true;
        }
        let Some(stem) = lower.strip_suffix("codes").or_else(|| lower.strip_suffix("code")) else {
            return false;
        };
        let stem = stem.trim_end_matches('-');
        let before = index.checked_sub(1).and_then(|i| words.get(i)).map(|word| word.lower.as_str()).unwrap_or("");
        !NOT_A_CODE.iter().any(|not| stem.ends_with(not) || before.starts_with(not))
    })
}

/// A number in the word, as far as its digits go: `+49`, `030`, `12.50`.
fn is_numeric(word: &Word<'_>) -> bool {
    word.text.bytes().filter(u8::is_ascii_digit).count() >= 2
        && word.text.bytes().all(|b| b.is_ascii_digit() || b".,-+/()".contains(&b))
}

fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// The code a word is, if it looks like one: 4 to 8 digits with at most one hyphen in the middle,
/// or 6 to 8 capitals and digits with at least one of each.
fn code_of(text: &str) -> Option<String> {
    if all_digits(text) {
        return (4..=8).contains(&text.len()).then(|| text.to_owned());
    }
    if let Some((left, right)) = text.split_once('-') {
        let joined = format!("{left}{right}");
        return (all_digits(left)
            && all_digits(right)
            && left.len() >= 2
            && right.len() >= 2
            && (4..=8).contains(&joined.len()))
        .then_some(joined);
    }
    let alphanumeric = text.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let digit = text.bytes().any(|b| b.is_ascii_digit());
    let letter = text.bytes().any(|b| b.is_ascii_uppercase());
    (alphanumeric && digit && letter && (6..=8).contains(&text.len())).then(|| text.to_owned())
}

/// Whether a four-digit number is rather a year.
fn is_year(code: &str) -> bool {
    code.len() == 4 && code.parse::<u32>().is_ok_and(|year| (1900..=2099).contains(&year))
}

/// The codes one line offers, without their space or hyphen.
fn candidates(words: &[Word<'_>]) -> Vec<String> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < words.len() {
        // "482 913": two short groups of digits read as one code.
        let pair = words.get(index + 1).filter(|next| {
            let first = words[index].text;
            all_digits(first)
                && all_digits(next.text)
                && (3..=4).contains(&first.len())
                && (3..=4).contains(&next.text.len())
        });
        let (code, last) = match pair {
            Some(next) => (Some(format!("{}{}", words[index].text, next.text)), index + 1),
            None => (code_of(words[index].text), index),
        };
        if let Some(code) = code
            && plausible(words, index, last, &code)
        {
            found.push(code);
        }
        index = last + 1;
    }
    found
}

/// Whether the words around a candidate leave it a code.
fn plausible(words: &[Word<'_>], first: usize, last: usize, code: &str) -> bool {
    if is_year(code) {
        return false;
    }
    let before = first.checked_sub(1).and_then(|i| words.get(i));
    let after = words.get(last + 1);
    // A number among numbers: a phone number, a date, an amount, an address.
    if before.is_some_and(is_numeric) || after.is_some_and(is_numeric) {
        return false;
    }
    let currency = |word: &Word<'_>| CURRENCIES.contains(&word.lower.as_str()) || word.text.contains(['€', '$', '£']);
    if before.is_some_and(currency) || after.is_some_and(currency) {
        return false;
    }
    let context = words[first.saturating_sub(3)..first].iter().chain(after);
    for word in context {
        let lower = word.lower.trim_start_matches('#');
        if word.text.starts_with('#')
            || NOT_NEAR.iter().any(|not| lower.contains(not))
            || NOT_NEAR_EXACTLY.contains(&lower)
        {
            return false;
        }
    }
    // "10115 Berlin": a postal code in front of a town.
    if code.len() == 5
        && first == last
        && after.is_some_and(|word| word.text.chars().next().is_some_and(char::is_uppercase))
    {
        return false;
    }
    true
}

/// The one code in the text, what it is for and how long it is valid, or nothing when there is no
/// code or more than one.
fn guess(text: &str) -> Option<(String, Purpose, i64)> {
    let lines: Vec<Vec<Word<'_>>> = text.lines().map(words).filter(|words| !words.is_empty()).collect();
    let keyword: Vec<bool> = lines.iter().map(|words| has_keyword(words)).collect();
    let mut found: Vec<String> = Vec::new();
    for (index, words) in lines.iter().enumerate() {
        let near = keyword[index]
            || (words.len() <= 3
                && (index.checked_sub(1).is_some_and(|i| keyword[i])
                    || index.checked_sub(2).is_some_and(|i| keyword[i])
                    || keyword.get(index + 1).copied().unwrap_or(false)));
        if !near {
            continue;
        }
        for code in candidates(words) {
            if !found.contains(&code) {
                found.push(code);
            }
        }
        if found.len() > 1 {
            return None;
        }
    }
    let code = found.pop()?;

    let lower = text.to_lowercase();
    let validity = VALIDITY
        .captures(&lower)
        .and_then(|captures| {
            let amount: i64 = captures.get(1)?.as_str().parse().ok()?;
            let unit = captures.get(2)?.as_str();
            let seconds = if unit.starts_with("min") { 60 } else { 3600 };
            Some(amount * seconds)
        })
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_VALIDITY)
        .min(MAX_VALIDITY);
    Some((code, purpose_of(&lower), validity))
}

/// What the wording says the code is for. Resetting a password says "sign in" too, so it goes
/// first.
fn purpose_of(lower: &str) -> Purpose {
    let any = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if any(&["zurücksetzen", "reset", "passwort vergessen", "kennwort vergessen", "forgot", "neues passwort"]) {
        Purpose::ResetPassword
    } else if any(&["registrier", "sign up", "signup", "konto erstellen", "create your account", "account erstellen"]) {
        Purpose::Signup
    } else if any(&[
        "e-mail-adresse bestätigen",
        "e-mail bestätigen",
        "email bestätigen",
        "confirm your email",
        "verify your email",
        "confirm your e-mail",
        "verify your e-mail",
    ]) {
        Purpose::ConfirmEmail
    } else if any(&["anmeld", "login", "log in", "sign in", "sign-in", "einlogg"]) {
        Purpose::Login
    } else {
        Purpose::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_552_400; // 2026-11-01T17:00:00Z

    fn mail(subject: &str, body: &str) -> Vec<u8> {
        format!(
            "From: Shop <no-reply@shop.example>\r\nTo: mini@example.org\r\nDate: Sun, 01 Nov 2026 17:00:00 +0000\r\n\
             Subject: {subject}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
        )
        .into_bytes()
    }

    fn with_header(header: &str) -> Vec<u8> {
        format!(
            "From: Shop <no-reply@shop.example>\r\nTo: mini@example.org\r\nDate: Sun, 01 Nov 2026 17:00:00 +0000\r\n\
             Subject: Dein Code\r\n{header}\r\n\r\nDein Code: 482913\r\n"
        )
        .into_bytes()
    }

    fn signed() -> Authenticity {
        Authenticity { header_signed: true, dmarc_passed: true, from_domain: Some("shop.example".into()) }
    }

    fn unsigned() -> Authenticity {
        Authenticity { header_signed: false, dmarc_passed: true, from_domain: Some("shop.example".into()) }
    }

    fn code(subject: &str, body: &str) -> Option<String> {
        detect(&mail(subject, body), &unsigned(), NOW).and_then(|found| found.code)
    }

    #[test]
    fn a_signed_header_is_believed() {
        let raw = with_header(
            "Verification-Info: kind=otp; origin=login.shop.example; code=AB-12CD;\r\n purpose=login; expires=2026-11-01T17:05:00Z",
        );
        let found = detect(&raw, &signed(), NOW).unwrap();
        assert_eq!(found.source, Source::Header);
        assert_eq!(found.origin, "login.shop.example");
        assert_eq!(found.code.as_deref(), Some("AB-12CD"));
        assert_eq!(found.purpose, Purpose::Login);
        assert_eq!(found.expires_at, NOW + 300);
        assert!(found.autofill);
        assert_eq!(found.to_json()["expiresAt"], "2026-11-01T17:05:00Z");
    }

    #[test]
    fn a_link_must_point_below_the_origin() {
        let header = |url: &str| {
            with_header(&format!(
                "Verification-Info: kind=link; origin=shop.example; url={url}; purpose=confirm-email; expires=2026-11-01T18:00:00Z"
            ))
        };
        let found = detect(&header("https://login.shop.example:443/c?t=1"), &signed(), NOW).unwrap();
        assert_eq!(found.kind, Kind::Link);
        assert_eq!(found.url.as_deref(), Some("https://login.shop.example:443/c?t=1"));
        for bad in [
            "http://shop.example/c",
            "https://shop.example@evil.example/c",
            "https://evil.example/c",
            "https://shopexample/c",
            "https://xshop.example/c",
            "https://shop.example:x/c",
        ] {
            // Without a usable header the guess still finds the code in the text.
            let found = detect(&header(bad), &signed(), NOW).unwrap();
            assert_eq!(found.source, Source::Heuristic, "{bad}");
        }
    }

    #[test]
    fn a_header_is_only_believed_when_signed_and_bound() {
        let good = "Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2026-11-01T17:05:00Z";
        let found = detect(&with_header(good), &unsigned(), NOW).unwrap();
        assert_eq!((found.source, found.code.as_deref()), (Source::Heuristic, Some("482913")));
        // Without DMARC neither the header nor a guess (VC-03).
        let no_dmarc = Authenticity { dmarc_passed: false, ..signed() };
        assert!(detect(&with_header(good), &no_dmarc, NOW).is_none());
        let no_dmarc = Authenticity { dmarc_passed: false, ..unsigned() };
        assert!(detect(&with_header(good), &no_dmarc, NOW).is_none());

        for bad in [
            "Verification-Info: kind=otp; origin=bank.example; code=777111; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=otp; origin=shop.example; code=777 111; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=otp; origin=shop.example; code=abc123; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=otp; origin=shop.example; code=----; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=otp; origin=shop.example; code=777111",
            "Verification-Info: kind=otp; origin=shop.example; code=777111; expires=morgen",
            "Verification-Info: kind=otp; origin=shop.example; code=777111; code=777112; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=otp; origin=shop.example; code=777111; purpose=pay; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=sms; origin=shop.example; code=777111; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=link; origin=shop.example; code=777111; expires=2026-11-01T17:05:00Z",
            "Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2026-11-01T17:05:00Z\r\n\
             Verification-Info: kind=otp; origin=shop.example; code=777112; expires=2026-11-01T17:05:00Z",
        ] {
            let found = detect(&with_header(bad), &signed(), NOW).unwrap();
            assert_eq!(found.source, Source::Heuristic, "{bad}");
        }
    }

    #[test]
    fn a_header_expiry_is_held_to_a_day() {
        let raw =
            with_header("Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2027-01-01T00:00:00Z");
        assert_eq!(detect(&raw, &signed(), NOW).unwrap().expires_at, NOW + MAX_VALIDITY);
        // Dated in the past: a day after the Date, not after arrival.
        let found = detect(&raw, &signed(), NOW + 3 * 3600).unwrap();
        assert_eq!(found.expires_at, NOW + MAX_VALIDITY);
    }

    #[test]
    fn expired_codes_and_big_messages_are_no_codes() {
        // VC-08: expired on arrival, also when the text has a code the guess would find.
        let raw =
            with_header("Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2026-11-01T16:00:00Z");
        assert!(detect(&raw, &signed(), NOW).is_none());
        let raw =
            with_header("Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2027-01-01T00:00:00Z");
        assert!(detect(&raw, &signed(), NOW + MAX_VALIDITY + 1).is_none(), "a day after the Date");
        // VC-09: too big to look at.
        let big = mail("Dein Code", &format!("Dein Code: 482913\r\n{}", "x".repeat(MAX_MESSAGE)));
        assert!(detect(&big, &unsigned(), NOW).is_none());
        assert!(detect(&mail("Dein Code", "Dein Code: 482913"), &unsigned(), NOW).is_some());
    }

    #[test]
    fn codes_are_found_in_german_and_english() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "Dein Bestätigungscode",
                "Hallo,\n\ndein Bestätigungscode lautet: 482913\n\nEr ist 10 Minuten gültig.",
                "482913",
            ),
            ("Anmeldung bei Shop", "Ihr Sicherheitscode:\n\n  482 913\n\nNicht weitergeben.", "482913"),
            ("482913 ist dein Einmalcode", "Gib ihn in der App ein.", "482913"),
            ("Your verification code", "Your verification code is 7391.\nIt expires in 10 minutes.", "7391"),
            (
                "Sign in to Shop",
                "Use this one-time code to sign in:\n\nQX7T2M\n\nIf this wasn't you, ignore it.",
                "QX7T2M",
            ),
            ("Security alert", "Your security code: 123-456", "123456"),
            ("Login", "Anmeldecode (gültig für 5 Minuten): 90817265", "90817265"),
            ("2FA", "Your 2FA code is 556677.", "556677"),
            ("PIN", "Ihre PIN: 4821", "4821"),
            ("Passwort zurücksetzen", "Code zum Zurücksetzen: 661100", "661100"),
        ];
        for (subject, body, expected) in cases {
            assert_eq!(code(subject, body).as_deref(), Some(*expected), "{subject}: {body}");
        }
        let found = detect(&mail("Login", "Anmeldecode (gültig für 5 Minuten): 90817265"), &unsigned(), NOW).unwrap();
        assert_eq!((found.purpose, found.expires_at), (Purpose::Login, NOW + 300));
        let found = detect(&mail("Reset", "Your code: 123456. Valid for 48 hours."), &unsigned(), NOW).unwrap();
        assert_eq!((found.purpose, found.expires_at), (Purpose::ResetPassword, NOW + MAX_VALIDITY));
        let found = detect(&mail("Willkommen", "Bestätigungscode: 123456"), &unsigned(), NOW).unwrap();
        assert_eq!(
            (found.purpose, found.expires_at, found.origin.as_str()),
            (Purpose::Other, NOW + DEFAULT_VALIDITY, "shop.example")
        );
    }

    #[test]
    fn other_numbers_are_no_codes() {
        let cases: &[(&str, &str)] = &[
            ("Deine Bestellung", "Bestellnummer: 4829131\nGesamt: 49,99 €\nDanke für deinen Einkauf!"),
            ("Order confirmation", "Thanks for your order #556677.\nTotal: $1299\nCode of conduct applies."),
            ("Deine Sendung ist unterwegs", "Sendungsnummer 12345678, Zustellung am 03.11.2026.\nCode: siehe App"),
            ("Newsletter", "Spare 20% mit dem Rabattcode SALE2026!\nNur bis 30.11."),
            ("Black Friday", "Use code SAVE20X at checkout for 20% off."),
            ("Gutschein", "Dein Gutscheincode: AB12CD34"),
            ("Rechnung", "Rechnungsnummer 2026117\nKundennummer 556677\nBetrag 129,00 EUR"),
            ("Kontakt", "Code? Ruf uns an: +49 30 1234567"),
            ("Adresse", "Postleitzahl und Code: 10115 Berlin"),
            ("Termin", "Dein Code für den Termin am 2026 ist bereit"),
            ("Two codes", "Code 123456 oder Code 654321"),
            ("Your code", "Your code is ready. Price: 1500 EUR"),
            ("Hallo", "Wie geht's? Wir sehen uns um 1830."),
        ];
        for (subject, body) in cases {
            assert_eq!(code(subject, body), None, "{subject}: {body}");
        }
    }

    #[test]
    fn html_mail_is_read_as_text() {
        let raw = "From: no-reply@shop.example\r\nSubject: Login\r\nContent-Type: text/html\r\n\r\n\
                   <p>Your login code</p><p><b>246810</b></p>\r\n";
        assert_eq!(detect(raw.as_bytes(), &unsigned(), NOW).unwrap().code.as_deref(), Some("246810"));
    }

    #[test]
    fn no_from_domain_no_code() {
        let auth = Authenticity { from_domain: None, ..unsigned() };
        assert_eq!(detect(&mail("Code", "Code: 482913"), &auth, NOW), None);
        let auth = Authenticity { from_domain: Some("localhost".into()), ..unsigned() };
        assert_eq!(detect(&mail("Code", "Code: 482913"), &auth, NOW), None);
    }

    #[test]
    fn garbage_never_panics() {
        // A small xorshift: the same bytes every run, no extra dependency.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet: &[u8] = b"0123456789 -:;=\r\n\t\"<>@#%ABCDEZaeiou\xc3\xa4\xe2\x82\xac\xff.CodeVerification-Info";
        let seeds = [
            with_header("Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2026-11-01T17:05:00Z"),
            mail("Code", "Code: 482913"),
        ];
        for round in 0..1500 {
            let mut raw = if round % 3 == 0 {
                (0..(next() % 600)).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect::<Vec<u8>>()
            } else {
                seeds[round % 2].clone()
            };
            if !raw.is_empty() {
                for _ in 0..(next() % 12) {
                    let at = (next() % raw.len() as u64) as usize;
                    raw[at] = alphabet[(next() % alphabet.len() as u64) as usize];
                }
            }
            let _ = detect(&raw, &signed(), (next() % (i64::MAX as u64)) as i64);
        }
        assert!(detect(b"", &signed(), i64::MAX).is_none());
        let long = format!("Verification-Info: kind=otp; {}", "a=b; ".repeat(400));
        let _ = detect(&with_header(&long), &signed(), NOW);
        let _ = detect(&mail("Code", &"Code 1 ".repeat(5000)), &signed(), i64::MAX);
    }
}
