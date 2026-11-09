//! Finding the one-time code or sign-in link in a mail (docs/verification-codes.md): what the
//! `Verification-Info` header of the OMXP draft says when the sender's signature vouches for it,
//! otherwise a careful guess from the subject and the text.
//!
//! Everything here is pure: bytes in, an answer or nothing out, no database and no clock. The SMTP
//! crate runs it once per incoming mail, the store keeps the answer (src/verification_codes.rs).
//!
//! The header is only read when a DKIM signature of the From domain covers it and DMARC passed
//! (or the mail was submitted here by an account that may send as its From address):
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

use mail_parser::PartType;
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
    /// The code as the person types it: digits without the space or hyphen a guess found between
    /// them (`123 456`, `G-123456` give `123456`), a code with letters with its hyphens (`K7P-2QX`).
    pub code: Option<String>,
    pub url: Option<String>,
    pub purpose: Purpose,
    /// Unix seconds.
    pub expires_at: i64,
    pub source: Source,
    /// Whether clients may offer it for autofill on `origin`: only when DMARC passed or a DKIM
    /// signature of the From domain vouches for the mail, so the domain the origin is bound to is
    /// really the sender's. A guess on SPF alone is shown, never filled in.
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
    /// A passing DKIM signature whose `d=` has the registrable domain of From, DMARC record or not.
    pub dkim_aligned: bool,
    /// SPF passed for a MAIL FROM with the registrable domain of From.
    pub spf_aligned: bool,
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
/// wins; otherwise the guess, and that only when DMARC passed or DKIM or SPF of the From domain did
/// (security-audit-0.26.0 VC-03): a guess from mail anyone could have written would carry a domain
/// it may not come from. Only DMARC or DKIM make it one to fill in, see [`Detected::autofill`]. A code
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
    if !(auth.dmarc_passed || auth.dkim_aligned || auth.spf_aligned) {
        return None;
    }

    let mut text = String::new();
    if let Some(subject) = message.subject() {
        text.push_str(subject);
        text.push('\n');
    }
    // An HTML-only mail is read with its blocks on lines of their own: mail-parser only breaks at
    // `<br>` and `</p>`, so a code in a table cell or a heading ran into the next sentence.
    let html = message.html_part(0).and_then(|part| match &part.body {
        PartType::Html(html) => Some(read_html(html)),
        _ => None,
    });
    match (message.text_part(0).map(|part| &part.body), &html) {
        (Some(PartType::Html(_)), Some(html)) => text.push_str(&html.text),
        _ => {
            if let Some(body) = message.body_text(0) {
                text.push_str(&body);
            }
        }
    }
    let text = cut(&text, MAX_TEXT);
    let (kind, code, url, purpose, validity) = match guess(text) {
        Some((code, purpose, validity)) => (Kind::Otp, Some(code), None, purpose, validity),
        None if !is_list_mail(&message) => {
            let (url, purpose, validity) = guess_link(&html?.links, text, &from_domain)?;
            (Kind::Link, None, Some(url), purpose, validity)
        }
        None => return None,
    };
    Some(Detected {
        kind,
        origin: from_domain,
        code,
        url,
        purpose,
        expires_at: received_at.saturating_add(validity),
        source: Source::Heuristic,
        // SPF vouches for the server that sent it, not for what it says: shown, not filled in.
        autofill: auth.dmarc_passed || auth.dkim_aligned,
    })
}

/// Elements that stand on a line of their own in the text.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "center",
    "dd",
    "div",
    "dl",
    "dt",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "ul",
];

/// Elements whose content is no text.
const NOT_TEXT: &[&str] = &["head", "style", "script", "template"];

/// The visible text of an HTML body and its links.
#[derive(Default)]
struct Html {
    /// Each block on a line of its own.
    text: String,
    /// `href` (entities decoded) and the text of each link, in order.
    links: Vec<(String, String)>,
}

/// `href="…"`, `href='…'` or `href=…` inside a tag; `data-href` is no link.
static HREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\shref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#).expect("a valid pattern"));

/// Reads an HTML body as text, each block on a line of its own, and collects its links on the way,
/// both from the same visible content: comments, `head`, `style`, `script` and `template` are
/// skipped. mail-parser's own conversion only breaks at `<br>` and `</p>`, and after a break it
/// drops the space behind an inline element: `<td><b>9063</b> ist der Code` came out as
/// "9063ist der Code". Stops once the text is past [`MAX_TEXT`], the most the guess reads.
fn read_html(html: &str) -> Html {
    let html = cut(html, MAX_MESSAGE);
    // Lower case for finding tags; ASCII only, so every offset stays the same.
    let lower = html.to_ascii_lowercase();
    let mut read = Html::default();
    // The link being read: its `href` and where its text starts.
    let mut link: Option<(String, usize)> = None;
    let mut at = 0;
    while at < html.len() && read.text.len() <= MAX_TEXT {
        let text = &mut read.text;
        let Some(open) = lower[at..].find('<').map(|open| at + open) else {
            push_html_text(text, &html[at..]);
            break;
        };
        push_html_text(text, &html[at..open]);
        if lower[open..].starts_with("<!--") {
            at = lower[open..].find("-->").map_or(html.len(), |end| open + end + 3);
            continue;
        }
        let end = lower[open..].find('>').map_or(html.len(), |end| open + end + 1);
        let closing = lower[open + 1..].starts_with('/');
        let name: String =
            lower[open + 1..end].trim_start_matches('/').chars().take_while(char::is_ascii_alphanumeric).collect();
        at = end;
        if NOT_TEXT.contains(&name.as_str()) && !closing {
            let close = format!("</{name}");
            at = lower[end..].find(&close).map_or(html.len(), |close| end + close);
        } else if BLOCKS.contains(&name.as_str()) && !text.ends_with('\n') {
            let trimmed = text.trim_end_matches(' ').len();
            text.truncate(trimmed);
            text.push('\n');
        } else if name == "a" {
            if let Some((href, start)) = link.take() {
                let label = text.get(start.min(text.len())..).unwrap_or_default();
                read.links.push((href, label.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
            if !closing {
                link = HREF.captures(&html[open..end]).and_then(|href| {
                    let href = href.get(1).or_else(|| href.get(2)).or_else(|| href.get(3))?.as_str();
                    // Entities decoded the way a browser does before following it: `&amp;`, `&#38;`.
                    let href = mail_parser::decoders::html::html_to_text(href);
                    Some((href.trim().to_owned(), text.len()))
                });
            }
        }
    }
    read
}

/// A piece of text between tags: entities decoded, white space folded into one space.
fn push_html_text(text: &mut String, piece: &str) {
    if piece.is_empty() {
        return;
    }
    let space_before = piece.starts_with(|c: char| c.is_ascii_whitespace());
    let space_after = piece.ends_with(|c: char| c.is_ascii_whitespace());
    let decoded = mail_parser::decoders::html::html_to_text(piece);
    let decoded = decoded.trim();
    let at_start = text.is_empty() || text.ends_with([' ', '\n']);
    if (space_before || decoded.is_empty() && space_after) && !at_start {
        text.push(' ');
    }
    if !decoded.is_empty() {
        text.push_str(decoded);
        if space_after {
            text.push(' ');
        }
    }
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

/// A number of minutes or hours, in digits or as a word, and its unit, in German, English, French,
/// Spanish, Italian and Dutch.
const AMOUNT: &str = concat!(
    r"\b(\d{1,3}|einer|eine|ein|zwei|drei|vier|fünfzehn|fünf|zehn|zwanzig|dreißig|sechzig",
    r"|an|a|one|two|three|four|fifteen|five|ten|twenty|thirty|sixty",
    r"|une|un|deux|trois|quatre|quinze|cinq|dix|vingt|trente|soixante",
    r"|una|uno|dos|tres|cuatro|quince|cinco|diez|veinte|treinta|sesenta",
    r"|due|tre|quattro|quindici|cinque|dieci|venti|trenta|sessanta",
    r"|een|twee|drie|vier|vijftien|vijf|tien|twintig|dertig|zestig)",
    r"\s*(minuten|minutes|minute|minutos|minuto|minuti|min|stunden|stunde|hours|hour|std|heures|heure",
    r"|horas|hora|ore|ora|uur)\b",
);

/// Words that say a code is valid or expires: "gültig für 10 Minuten", "gilt 10 Minuten", "valid for
/// 2 hours", "expires in 15 min", "valable 10 minutes", "expira en 10 minutos", "scade tra 10
/// minuti", "geldig voor 10 minuten".
const VALID: &str = "gültig|gilt|valid|expires?|läuft|verfällt|valable|valide|expire|expira|caduca|válido|vence\
                     |scade|valido|vale|geldig|verloopt|vervalt";

/// Validity in the text, the words first or the other way round, "5 Minuten gültig".
static VALIDITY: [LazyLock<Regex>; 2] = [
    LazyLock::new(|| Regex::new(&format!(r"(?i)(?:{VALID})\D{{0,30}}?{AMOUNT}")).expect("a valid pattern")),
    LazyLock::new(|| Regex::new(&format!(r"(?i){AMOUNT}\s+(?:lang\s+|long\s+)?(?:{VALID})")).expect("a valid pattern")),
];

/// A number of minutes or hours, in digits or as a word.
fn amount(word: &str) -> Option<i64> {
    if let Ok(amount) = word.parse() {
        return Some(amount);
    }
    Some(match word {
        "ein" | "eine" | "einer" | "an" | "a" | "one" | "un" | "une" | "una" | "uno" | "een" => 1,
        "zwei" | "two" | "deux" | "dos" | "due" | "twee" => 2,
        "drei" | "three" | "trois" | "tres" | "tre" | "drie" => 3,
        "vier" | "four" | "quatre" | "cuatro" | "quattro" => 4,
        "fünf" | "five" | "cinq" | "cinco" | "cinque" | "vijf" => 5,
        "zehn" | "ten" | "dix" | "diez" | "dieci" | "tien" => 10,
        "fünfzehn" | "fifteen" | "quinze" | "quince" | "quindici" | "vijftien" => 15,
        "zwanzig" | "twenty" | "vingt" | "veinte" | "venti" | "twintig" => 20,
        "dreißig" | "thirty" | "trente" | "treinta" | "trenta" | "dertig" => 30,
        "sechzig" | "sixty" | "soixante" | "sesenta" | "sessanta" | "zestig" => 60,
        _ => return None,
    })
}

/// How long the text says the code or link is valid, [`DEFAULT_VALIDITY`] when it doesn't say.
fn validity_of(lower: &str) -> i64 {
    VALIDITY
        .iter()
        .find_map(|pattern| pattern.captures(lower))
        .and_then(|captures| {
            let amount = amount(captures.get(1)?.as_str())?;
            let unit = captures.get(2)?.as_str();
            let seconds = if unit.starts_with("min") { 60 } else { 3600 };
            Some(amount * seconds)
        })
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_VALIDITY)
        .min(MAX_VALIDITY)
}

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
    "réduction",
    "reduc",
    "descuento",
    "cupón",
    "sconto",
    "buono",
    "korting",
    "kortings",
    "cadeau",
    "regalo",
    "postal",
    "postale",
    "fiscal",
    "fiscale",
    "avviamento",
];

/// Short words that are keywords on their own.
const SHORT_KEYWORDS: &[&str] = &["otp", "2fa", "pin", "tan", "mtan", "passcode", "totp", "einmalpasswort"];

/// How a word for "code" ends, in German, English, French and Dutch ("Anmeldecode",
/// "verificatiecode"), Spanish and Italian.
const CODE_WORDS: &[&str] = &["codes", "code", "códigos", "código", "codigo", "codice", "codici"];

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
    "commande",
    "facture",
    "client",
    "numéro",
    "número",
    "numero",
    "suivi",
    "colis",
    "référence",
    "téléphone",
    "pedido",
    "factura",
    "seguimiento",
    "envío",
    "referencia",
    "teléfono",
    "ordine",
    "fattura",
    "spedizione",
    "riferimento",
    "bestelling",
    "factuur",
    "klant",
    "zending",
    "referentie",
    "telefoon",
];

/// The same, for words too short to look for inside others.
const NOT_NEAR_EXACTLY: &[&str] = &[
    "nr", "no", "iban", "ref", "plz", "tel", "fax", "item", "case", "bon", "room", "seite", "page", "id", "n°", "nº",
    "núm", "tél", "cap",
];

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
    "réduction",
    "offre",
    "soldes",
    "descuento",
    "oferta",
    "rebajas",
    "ahorra",
    "sconto",
    "offerta",
    "saldi",
    "risparmia",
    "korting",
    "aanbieding",
    "bespaar",
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
        let Some(stem) = CODE_WORDS.iter().find_map(|code| lower.strip_suffix(code)) else {
            return false;
        };
        let stem = stem.trim_end_matches('-');
        let near = |at: Option<usize>| at.and_then(|i| words.get(i)).map(|word| word.lower.as_str()).unwrap_or("");
        // "Rabattcode", "promo code", and the other way round in French, Spanish and Italian:
        // "code promo", "código postal", "codice sconto".
        let (before, after) = (near(index.checked_sub(1)), near(index.checked_add(1)));
        !NOT_A_CODE.iter().any(|not| stem.ends_with(not) || before.starts_with(not) || after.starts_with(not))
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

fn mixed(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && text.bytes().any(|b| b.is_ascii_digit())
        && text.bytes().any(|b| b.is_ascii_uppercase())
}

/// The code a word is, if it looks like one, the way it is typed in (docs/verification-codes.md):
/// - 4 to 8 digits, also with one hyphen in the middle, "123-456", typed without it;
/// - a letter, a hyphen and 4 to 8 digits, "G-123456": the letter is the sender's mark and the
///   form asks for the digits only;
/// - 6 to 8 capitals and digits with at least one of each, "AB12CD";
/// - two or three such groups of 2 to 5 joined by hyphens, "K7P-2QX", kept with the hyphens as the
///   sender shows them. A group of three or more letters alone reads as a word ("COVID-19",
///   "SALE-2026"), so none may be one.
fn code_of(text: &str) -> Option<String> {
    if all_digits(text) {
        return (4..=8).contains(&text.len()).then(|| text.to_owned());
    }
    let groups: Vec<&str> = text.split('-').collect();
    match groups.as_slice() {
        [] | [_] => (mixed(text) && (6..=8).contains(&text.len())).then(|| text.to_owned()),
        [left, right] if all_digits(left) && all_digits(right) => {
            let joined = format!("{left}{right}");
            (left.len() >= 2 && right.len() >= 2 && (4..=8).contains(&joined.len())).then_some(joined)
        }
        [mark, digits] if mark.len() == 1 && mark.bytes().all(|b| b.is_ascii_uppercase()) && all_digits(digits) => {
            (4..=8).contains(&digits.len()).then(|| (*digits).to_owned())
        }
        _ => {
            let word = |group: &&str| group.len() >= 3 && group.bytes().all(|b| b.is_ascii_uppercase());
            let length: usize = groups.iter().map(|group| group.len()).sum();
            (groups.len() <= 3
                && groups.iter().all(|group| (2..=5).contains(&group.len()))
                && !groups.iter().any(word)
                && (5..=10).contains(&length)
                && mixed(&groups.concat()))
            .then(|| text.to_owned())
        }
    }
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
    let context =
        words[first.saturating_sub(3)..first].iter().map(|word| (word, true)).chain(after.map(|word| (word, false)));
    for (word, in_front) in context {
        let lower = word.lower.trim_start_matches('#');
        // "Nr. 4711" but not "4711. No lo compartas.": the short ones only count in front.
        if word.text.starts_with('#')
            || NOT_NEAR.iter().any(|not| lower.contains(not))
            || in_front && NOT_NEAR_EXACTLY.contains(&lower)
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
    let lines = lines_of(text);
    let lines: Vec<Vec<Word<'_>>> = lines.iter().map(|line| words(line)).filter(|words| !words.is_empty()).collect();
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
    Some((code, purpose_of(&lower), validity_of(&lower)))
}

/// The lines of the text, with a code spread over boxes of one digit or capital each put back
/// together: a cell per digit gives "4", "8", "2", … on lines of their own, spaced out it is
/// "4 8 2 9 1 3". Only a run of 4 to 8 is joined; others stay as they are.
fn lines_of(text: &str) -> Vec<String> {
    // One digit or capital, maybe ending the sentence.
    let single = |piece: &str| {
        let piece = piece.trim_end_matches(['.', ',', ';', '!']);
        piece.len() == 1 && piece.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
    };
    // Joins the run when it has the length of a code, otherwise gives its pieces back.
    let flush = |run: &mut Vec<&str>, out: &mut Vec<String>, on_lines: bool| {
        if (4..=8).contains(&run.len()) {
            out.push(run.concat());
        } else if on_lines {
            out.extend(run.iter().map(|piece| (*piece).to_owned()));
        } else if !run.is_empty() {
            out.push(run.join(" "));
        }
        run.clear();
    };
    let mut lines = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if single(trimmed) {
            run.push(trimmed);
            continue;
        }
        flush(&mut run, &mut lines, true);
        if line.split_whitespace().filter(|piece| single(piece)).nth(3).is_none() {
            lines.push(line.to_owned());
            continue;
        }
        let mut pieces = Vec::new();
        for piece in line.split_whitespace() {
            if single(piece) {
                run.push(piece.trim_end_matches(['.', ',', ';', '!']));
            } else {
                flush(&mut run, &mut pieces, false);
                pieces.push(piece.to_owned());
            }
        }
        flush(&mut run, &mut pieces, false);
        lines.push(pieces.join(" "));
    }
    flush(&mut run, &mut lines, true);
    lines
}

/// What the button of a sign-in or confirmation link says, as a whole or at its start.
const LINK_BUTTONS: &[&str] = &[
    "sign in",
    "sign-in",
    "log in",
    "login",
    "anmelden",
    "jetzt anmelden",
    "einloggen",
    "jetzt einloggen",
    "verify",
    "confirm",
    "bestätigen",
    "jetzt bestätigen",
    "verifizieren",
    "e-mail bestätigen",
    "e-mail-adresse bestätigen",
    "email bestätigen",
    "e-mail verifizieren",
    "e-mail-adresse verifizieren",
    "reset password",
    "reset your password",
    "passwort zurücksetzen",
    "kennwort zurücksetzen",
];

/// What a mail with a link someone asked for says around it.
const LINK_SIGNS: &[&str] = &[
    "didn't request",
    "didn’t request",
    "did not request",
    "didn't ask",
    "didn’t ask",
    "did not ask",
    "wasn't you",
    "wasn’t you",
    "link expires",
    "link will expire",
    "link is valid for",
    "nicht angefordert",
    "nicht angefragt",
    "nicht beantragt",
    "nicht du warst",
    "nicht von ihnen",
    "link ist nur",
    "link ist gültig",
    "link läuft",
    "link verfällt",
];

/// Whether a mail is list or notification traffic: what a platform's notification carries may be
/// written by anyone with an account there, and a "Sign in" link planted in a comment would come
/// with the platform's DMARC pass (security review of 0.26.2, F1).
fn is_list_mail(message: &mail_parser::Message<'_>) -> bool {
    let header = |name: &str| message.headers_raw().find(|(found, _)| found.eq_ignore_ascii_case(name));
    header("List-Id").is_some()
        || header("In-Reply-To").is_some()
        || header("References").is_some()
        || header("Precedence")
            .is_some_and(|(_, value)| ["list", "bulk", "junk"].contains(&value.trim().to_ascii_lowercase().as_str()))
}

/// Whether a link carries another address in its query, the way a redirect does: the link to the
/// From domain then takes the reader elsewhere (F2).
fn redirects(url: &str) -> bool {
    let query = url.split_once('?').map(|(_, query)| query.to_ascii_lowercase()).unwrap_or_default();
    ["=http", "=//", "%3a%2f%2f", "%2f%2f"].iter().any(|sign| query.contains(sign))
}

/// The one sign-in or confirmation link of an HTML mail, what it is for and how long it is valid.
/// As shy as the code: exactly one link behind a button such as "Sign in" or "E-Mail bestätigen",
/// an `https` link to the From domain or below it that leads nowhere else, and a text that says it
/// was asked for or expires. A newsletter's "Jetzt anmelden" for an event says neither.
fn guess_link(links: &[(String, String)], text: &str, from_domain: &str) -> Option<(String, Purpose, i64)> {
    let lower = text.to_lowercase();
    if !LINK_SIGNS.iter().any(|sign| lower.contains(sign)) {
        return None;
    }
    let mut found: Vec<String> = Vec::new();
    for (href, label) in links {
        let label = label.to_lowercase();
        let label = label.trim_matches(|c: char| !c.is_alphanumeric());
        let button = LINK_BUTTONS.iter().any(|button| {
            label == *button || (label.starts_with(&format!("{button} ")) && label.split(' ').count() <= 6)
        });
        if !button {
            continue;
        }
        let Some(url) = https_below(href, from_domain).filter(|url| !redirects(url)) else { continue };
        if !found.contains(&url) {
            found.push(url);
        }
        if found.len() > 1 {
            return None;
        }
    }
    let url = found.pop()?;
    Some((url, purpose_of(&lower), validity_of(&lower)))
}

/// What the wording says the code is for. Resetting a password says "sign in" too, so it goes
/// first.
fn purpose_of(lower: &str) -> Purpose {
    let any = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if any(&[
        "zurücksetzen",
        "reset",
        "passwort vergessen",
        "kennwort vergessen",
        "forgot",
        "neues passwort",
        "réinitialis",
        "restablecer",
        "reimpost",
        "wachtwoord vergeten",
        "wachtwoord opnieuw",
    ]) {
        Purpose::ResetPassword
    } else if any(&[
        "registrier",
        "sign up",
        "signup",
        "konto erstellen",
        "create your account",
        "account erstellen",
        "inscription",
        "registro",
        "registrazione",
        "registratie",
    ]) {
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
    } else if any(&[
        "anmeld",
        "login",
        "log in",
        "sign in",
        "sign-in",
        "einlogg",
        "connexion",
        "iniciar sesión",
        "inicio de sesión",
        "accesso",
        "accedi",
        "inloggen",
    ]) {
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
        Authenticity { header_signed: true, ..unsigned() }
    }

    fn unsigned() -> Authenticity {
        Authenticity { dmarc_passed: true, dkim_aligned: true, ..unvouched() }
    }

    /// Neither DMARC nor DKIM nor SPF of the From domain passed.
    fn unvouched() -> Authenticity {
        Authenticity { from_domain: Some("shop.example".into()), ..Default::default() }
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
        // Without DMARC no header; without DMARC, DKIM or SPF of the From domain no guess (VC-03).
        let no_dmarc = Authenticity { dmarc_passed: false, ..signed() };
        let found = detect(&with_header(good), &no_dmarc, NOW).unwrap();
        assert_eq!((found.source, found.autofill), (Source::Heuristic, true));
        assert!(detect(&with_header(good), &Authenticity { header_signed: true, ..unvouched() }, NOW).is_none());
        assert!(detect(&with_header(good), &unvouched(), NOW).is_none());

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

    fn html_mail(subject: &str, html: &str) -> Vec<u8> {
        format!(
            "From: Shop <no.reply@shop.example>\r\nTo: mini@example.org\r\nSubject: {subject}\r\n\
             Content-Type: text/html; charset=utf-8\r\n\r\n<html><head><style>td {{ color: #000 }}</style></head>\
             <body><table><tr><td>Shop</td></tr><tr><td><img alt=\"Shop\" src=\"cid:logo\"></td></tr>{html}</table></body></html>\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn a_code_in_its_own_cell_or_heading_is_found() {
        // The code in a heading of its own cell, right before the next sentence (IKEA, 2026-11-03).
        let raw = html_mail(
            "Dein Einmalcode",
            "<tr><td><h1>Hej,</h1></td></tr><tr><td><p>bitte gib die sechs Ziffern unten in die Ansicht ein, \
             in der du den Einmalcode angefordert hast.</p></td></tr><tr><td><h1>842526</h1></td></tr>\
             <tr><td><p>Dieser Einmalcode ist nur 5 Minuten g&uuml;ltig.</p></td></tr>",
        );
        let found = detect(&raw, &unsigned(), NOW).unwrap();
        assert_eq!((found.kind, found.code.as_deref(), found.expires_at), (Kind::Otp, Some("842526"), NOW + 300));
        let raw = html_mail(
            "Verifizieren Sie Ihre E-Mail-Adresse",
            "<tr><td><div><b>9063</b> ist der Code zur Verifizierung Ihrer registrierten E-Mail-Adresse.</div>\
             <div>Der Code verfällt nach zwei Minuten.</div></td></tr>",
        );
        let found = detect(&raw, &unsigned(), NOW).unwrap();
        assert_eq!((found.code.as_deref(), found.expires_at), (Some("9063"), NOW + 120));
    }

    #[test]
    fn validity_may_be_written_out() {
        for (text, seconds) in [
            ("Der Code verfällt nach zwei Minuten.", 120),
            ("Er ist eine Stunde gültig.", 3600),
            ("This code expires in ten minutes.", 600),
            ("Valid for an hour.", 3600),
            ("Gültig für fünfzehn Minuten.", 900),
            ("Gültig, solange du willst.", DEFAULT_VALIDITY),
            ("Er gilt 10 Minuten.", 600),
            ("Il est valable une heure.", 3600),
            ("Expira en quince minutos.", 900),
            ("Scade tra dieci minuti.", 600),
            ("Deze code is 30 minuten geldig.", 1800),
            ("Valable 2 heures.", 7200),
        ] {
            assert_eq!(validity_of(&text.to_lowercase()), seconds, "{text}");
        }
    }

    fn sign_in(href: &str, button: &str, note: &str) -> Vec<u8> {
        html_mail(
            "Your secure link to Shop is here",
            &format!(
                "<tr><td><h2>Sign in to Shop</h2><p>Click the button below to finish signing in. This link expires \
                 in 10 minutes.</p><a class=\"btn\" href=\"{href}\"><span>{button}</span></a><p>{note}</p>\
                 <p><a href=\"https://shop.example/help\">Help</a> &middot; <a href=\"https://shop.example/privacy\">Privacy</a></p></td></tr>"
            ),
        )
    }

    #[test]
    fn a_sign_in_link_is_found() {
        let note = "If you didn't request this email, you can safely ignore it.";
        let found =
            detect(&sign_in("https://login.shop.example/magic?t=abc&amp;u=1", "Sign in", note), &unsigned(), NOW)
                .unwrap();
        assert_eq!(found.kind, Kind::Link);
        assert_eq!(found.url.as_deref(), Some("https://login.shop.example/magic?t=abc&u=1"));
        assert_eq!(found.code, None);
        assert_eq!((found.origin.as_str(), found.purpose), ("shop.example", Purpose::Login));
        assert_eq!((found.source, found.expires_at), (Source::Heuristic, NOW + 600));
        let raw = html_mail(
            "Bitte bestätige deine E-Mail-Adresse",
            "<tr><td><p>Du hast dich bei Shop registriert.</p>\
             <a href='https://shop.example/confirm/xyz'>E-Mail-Adresse bestätigen</a>\
             <p>Falls du das nicht angefordert hast, ignoriere diese Mail.</p></td></tr>",
        );
        let found = detect(&raw, &unsigned(), NOW).unwrap();
        assert_eq!((found.url.as_deref(), found.purpose), (Some("https://shop.example/confirm/xyz"), Purpose::Signup));
    }

    #[test]
    fn other_links_are_no_sign_in_links() {
        let note = "If you didn't request this email, you can safely ignore it.";
        for (href, button) in [
            ("https://tracker.example/r?u=shop.example", "Sign in"),
            ("http://shop.example/magic", "Sign in"),
            ("https://shop.example@evil.example/magic", "Sign in"),
            ("https://shop.example/sale", "Jetzt shoppen"),
        ] {
            assert_eq!(detect(&sign_in(href, button, note), &unsigned(), NOW), None, "{href} {button}");
        }
        // Without a word that it was asked for or expires, and with two different sign-in links.
        let raw = html_mail(
            "Unser Herbstfest",
            "<tr><td><p>Feiere mit uns am Samstag!</p><a href=\"https://shop.example/fest\">Jetzt anmelden</a></td></tr>",
        );
        assert_eq!(detect(&raw, &unsigned(), NOW), None);
        let raw = html_mail(
            "Sign in",
            "<tr><td><p>This link expires soon.</p><a href=\"https://shop.example/a\">Sign in</a>\
             <a href=\"https://shop.example/b\">Log in</a></td></tr>",
        );
        assert_eq!(detect(&raw, &unsigned(), NOW), None);
        // Not from a sender nothing vouches for (VC-03).
        let raw = sign_in("https://shop.example/magic", "Sign in", note);
        assert_eq!(detect(&raw, &unvouched(), NOW), None);
    }

    #[test]
    fn a_sign_in_link_is_only_believed_in_the_html_itself() {
        let note = "If you didn't request this email, you can safely ignore it.";
        // A link that leads on to another address (F2).
        for href in [
            "https://shop.example/out?to=https%3A%2F%2Fevil.example%2F",
            "https://shop.example/out?to=https://evil.example/",
            "https://shop.example/out?to=//evil.example/",
        ] {
            assert_eq!(detect(&sign_in(href, "Sign in", note), &unsigned(), NOW), None, "{href}");
        }
        // Not in a list, a bulk mail or a reply: what someone else wrote on a platform (F1).
        let raw = sign_in("https://shop.example/magic", "Sign in", note);
        for header in ["List-Id: <news.shop.example>", "Precedence: bulk", "In-Reply-To: <a@shop.example>"] {
            let raw = [format!("{header}\r\n").as_bytes(), &raw].concat();
            assert_eq!(detect(&raw, &unsigned(), NOW), None, "{header}");
        }
        // Neither a `data-href` nor a link in a comment or a template is one (F4); an unquoted
        // `href` with an encoded `&` is (F5).
        for html in [
            "<a data-href=\"https://shop.example/magic\" href=\"#\">Sign in</a>",
            "<!-- <a href=\"https://shop.example/magic\">Sign in</a> -->",
            "<template><a href=\"https://shop.example/magic\">Sign in</a></template>",
        ] {
            let raw = html_mail("Sign in", &format!("<tr><td><p>This link expires in 10 minutes.</p>{html}</td></tr>"));
            assert_eq!(detect(&raw, &unsigned(), NOW), None, "{html}");
        }
        let raw = html_mail(
            "Sign in",
            "<tr><td><p>This link expires in 10 minutes.</p><a href=https://shop.example/m?a=1&#38;b=2>Sign in</a></td></tr>",
        );
        assert_eq!(detect(&raw, &unsigned(), NOW).unwrap().url.as_deref(), Some("https://shop.example/m?a=1&b=2"));
    }

    /// The two mails that 0.30.0 fixes, anonymised: apps on this server sending a code to a local
    /// account. A text part and an HTML part, the code in the subject and on a line of its own.
    #[test]
    fn codes_from_apps_on_this_server() {
        let raw = "From: Lock <no-reply@lock.example.com>\r\nTo: mini@example.net\r\nSubject: Dein Anmeldecode: 133900\r\n\
                   MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\n\
                   Content-Type: text/plain; charset=utf-8\r\n\r\nHallo,\r\n\r\nHier ist dein Code f\u{fc}r die Anmeldung bei Lock:\r\n\r\n\
                   133900\r\n\r\nEr gilt 10 Minuten.\r\n--b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
                   <p>Hier ist dein Code f&uuml;r die Anmeldung bei Lock:</p><p style=\"font-size:28px\"><b>133900</b></p>\
                   <p>Er gilt 10 Minuten.</p>\r\n--b--\r\n";
        let auth = Authenticity { from_domain: Some("lock.example.com".into()), ..unsigned() };
        let found = detect(raw.as_bytes(), &auth, NOW).unwrap();
        assert_eq!(
            (found.code.as_deref(), found.origin.as_str(), found.purpose, found.expires_at, found.autofill),
            (Some("133900"), "example.com", Purpose::Login, NOW + 600, true)
        );
        let raw = "From: Team <team@chat.example.net>\r\nTo: mini@example.net\r\nSubject: Dein Team-Code: 527521\r\n\
                   Content-Type: text/plain; charset=utf-8\r\n\r\nHallo,\r\n\r\ndein Code f\u{fc}r den Team-Bereich von Chat ist \
                   527521. Er gilt 10 Minuten.\r\n";
        let auth = Authenticity { from_domain: Some("chat.example.net".into()), ..unsigned() };
        let found = detect(raw.as_bytes(), &auth, NOW).unwrap();
        assert_eq!((found.code.as_deref(), found.expires_at), (Some("527521"), NOW + 600));
    }

    #[test]
    fn how_far_the_sender_is_vouched_for() {
        let raw = mail("Dein Code", "Dein Code: 482913");
        // DKIM of the From domain without a DMARC record: a code to fill in.
        let dkim = Authenticity { dkim_aligned: true, ..unvouched() };
        assert!(detect(&raw, &dkim, NOW).unwrap().autofill);
        // SPF alone: shown, not filled in.
        let spf = Authenticity { spf_aligned: true, ..unvouched() };
        let found = detect(&raw, &spf, NOW).unwrap();
        assert_eq!((found.code.as_deref(), found.autofill), (Some("482913"), false));
        // DMARC passed on SPF: as before.
        assert!(
            detect(&raw, &Authenticity { dmarc_passed: true, spf_aligned: true, ..unvouched() }, NOW).unwrap().autofill
        );
        assert!(detect(&raw, &unvouched(), NOW).is_none());
        // A header is only believed with DMARC.
        let header =
            with_header("Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2026-11-01T17:05:00Z");
        let found =
            detect(&header, &Authenticity { header_signed: true, dkim_aligned: true, ..unvouched() }, NOW).unwrap();
        assert_eq!(found.source, Source::Heuristic);
    }

    #[test]
    fn a_corpus_of_codes() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "Votre code de vérification",
                "Votre code de vérification est 482913. Il est valable 10 minutes.",
                "482913",
            ),
            ("Code de connexion", "Bonjour,\n\nCode de connexion : 7391\n\nNe le partagez pas.", "7391"),
            (
                "Tu código de verificación",
                "Tu código de verificación es 482913. Expira en 5 minutos. No lo compartas.",
                "482913",
            ),
            ("Acceso", "Código: 9081", "9081"),
            ("Il tuo codice", "Il tuo codice di accesso è 482913. Scade tra 10 minuti.", "482913"),
            ("Accesso", "Codice OTP: 112233", "112233"),
            ("Je verificatiecode", "Je verificatiecode is 482913. Deze code is 15 minuten geldig.", "482913"),
            ("Inloggen", "Uw inlogcode: 667788", "667788"),
            ("Sign in", "Your sign-in code is K7P-2QX", "K7P-2QX"),
            ("Anmeldung", "Bestätigungscode: AB12CD", "AB12CD"),
            ("Your code", "Your code: 123 456", "123456"),
            ("Your code", "Your code: 123-456", "123456"),
            ("Verification", "G-123456 is your verification code.", "123456"),
            ("Passcode", "Your one-time passcode is 8812 3344.", "88123344"),
            ("Einmalpasswort", "Ihr Einmalpasswort lautet 55443322", "55443322"),
            ("Dein Code: 482913", "", "482913"),
            ("482913 is your code", "Thanks.", "482913"),
            ("Code", "Hier dein Code für die App:\n\nX7Q-K2P-9MZ\n\nGilt eine Stunde.", "X7Q-K2P-9MZ"),
            ("Sicherheit", "Dein Sicherheitscode ist 4 8 2 9 1 3.", "482913"),
            ("Login", "Use this code to log in:\n4\n8\n2\n9\n1\n3", "482913"),
        ];
        for (subject, body, expected) in cases {
            assert_eq!(code(subject, body).as_deref(), Some(*expected), "{subject}: {body}");
        }
        for (subject, html, expected) in [
            ("Dein Code", "<tr><td>Code</td><td>482913</td></tr>", "482913"),
            (
                "Your code",
                "<tr><td>Your verification code</td></tr><tr><td>4</td><td>8</td><td>2</td><td>9</td><td>1</td><td>3</td></tr>",
                "482913",
            ),
            (
                "Votre code",
                "<tr><td><p>Votre code</p><p><span>4</span> <span>8</span> <span>2</span> <span>9</span> <span>1</span> <span>3</span></p></td></tr>",
                "482913",
            ),
            (
                "Tu código",
                "<tr><td><p>Tu código:</p><div style=\"font-size:40px\"><span>4</span><span>8</span><span>2</span><span>9</span></div></td></tr>",
                "4829",
            ),
            ("Dein Code: 771100", "<tr><td><img src=\"cid:code\" alt=\"\"></td></tr>", "771100"),
        ] {
            let found = detect(&html_mail(subject, html), &unsigned(), NOW);
            assert_eq!(found.and_then(|found| found.code).as_deref(), Some(expected), "{html}");
        }
        let expiry = |subject: &str, body: &str| {
            detect(&mail(subject, body), &unsigned(), NOW).map(|found| (found.expires_at - NOW, found.purpose))
        };
        assert_eq!(
            expiry("Votre code", "Votre code de connexion : 482913. Valable 10 minutes."),
            Some((600, Purpose::Login))
        );
        assert_eq!(
            expiry("Código", "Tu código para restablecer la contraseña: 482913"),
            Some((DEFAULT_VALIDITY, Purpose::ResetPassword))
        );
        assert_eq!(
            expiry("Codice", "Codice di registrazione: 482913, valido per 2 ore"),
            Some((7200, Purpose::Signup))
        );
        assert_eq!(
            expiry("Code", "Je code om in te loggen: 482913. Verloopt over vijf minuten."),
            Some((300, Purpose::Other))
        );
    }

    #[test]
    fn a_corpus_of_other_numbers() {
        let cases: &[(&str, &str)] = &[
            ("Profitez de l'été", "Profitez de -20% avec le code promo ETE2026 !"),
            ("Ofertas", "Usa el código descuento AB12CD en tu próxima compra."),
            ("Saldi", "Codice sconto: SC1234X valido fino al 30.11."),
            ("Zomeractie", "Kortingscode: ZOMER24"),
            ("Votre facture", "Facture n° 482913\nCode client 556677\nMontant 49,90 €"),
            ("Tu pedido está en camino", "Número de seguimiento: 48291355.\nCódigo postal 28013"),
            ("Einladung: Team-Termin", "Am 03.11.2026 um 14:00. Code für den Raum: siehe Kalender"),
            ("Meeting", "Join the meeting with code 482 913 7781"),
            ("Ordine confermato", "Il codice ordine 482913 è confermato. Totale 49,99 €"),
            ("Contact", "Un code ? Appelez-nous au 01 23 45 67 89"),
            ("Tu código", "Tu código de descuento vale 1500 €"),
            ("Adresse", "Code: 10115 Berlin"),
            ("Adres", "Postcode 1012 AB Amsterdam"),
            ("Gültigkeit", "Dein Code gilt bis 31.12.2026"),
            ("Health", "Your COVID-19 code is ready"),
            ("Code", "Dein Code: ABC-DEF"),
            ("Codes", "Code 123456 ou code 654321"),
            ("Referentie", "Referentie: 482913, uw code volgt"),
            ("Dirección", "Calle Mayor 1, 28013 Madrid. Código: ver app"),
            ("Schritte", "Schritte 1 2 3 4 5 und fertig"),
            ("Invoice INV-2026-0042", "Your invoice INV-2026-0042 and our code of conduct"),
            ("Tracking", "Tracking code: 1Z999AA10123456784"),
            ("A gift for you", "Your gift code: K7P-2QX"),
            ("Black Friday", "Use code SALE-2026 today"),
            ("Newsletter", "Ton code du mois : -30% sur tout avec MOIS30"),
        ];
        for (subject, body) in cases {
            assert_eq!(code(subject, body), None, "{subject}: {body}");
        }
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
        let alphabet: &[u8] =
            b"0123456789 -:;=\r\n\t\"<>/!&@#%ABCDEZaeiou\xc3\xa4\xe2\x82\xac\xff.CodeVerification-Info";
        let seeds = [
            with_header("Verification-Info: kind=otp; origin=shop.example; code=777111; expires=2026-11-01T17:05:00Z"),
            mail("Code", "Code: 482913"),
            html_mail(
                "Sign in",
                "<tr><td><!-- x --><h1>Code</h1><b>482913</b> ist &uuml;g&#252;ltig <a href=\"https://shop.example/m\">Sign in</a> \
                 didn't request<script>x</script></td></tr>",
            ),
        ];
        for round in 0..1500 {
            let mut raw = if round % 3 == 0 {
                (0..(next() % 600)).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect::<Vec<u8>>()
            } else {
                seeds[round % seeds.len()].clone()
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
