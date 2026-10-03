//! The built-in detectors: invoices, appointments, newsletters and shipments (docs/labels.md,
//! "Detectors"). Each looks for a few signals that together rarely occur by chance; when in doubt,
//! they say no.

use serde_json::{Value, json};

use crate::Mail;
use crate::facts::{Facts, SenderKind, any_in};
use crate::text::{boundary, find_any, find_word, fold, has_word, original_word, words};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Detector {
    Invoice,
    Appointment,
    Newsletter,
    Shipping,
    Account,
    Personal,
    Work,
    Advertising,
}

impl Detector {
    pub const ALL: [Detector; 8] = [
        Detector::Invoice,
        Detector::Appointment,
        Detector::Newsletter,
        Detector::Shipping,
        Detector::Account,
        Detector::Personal,
        Detector::Work,
        Detector::Advertising,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Detector::Invoice => "invoice",
            Detector::Appointment => "appointment",
            Detector::Newsletter => "newsletter",
            Detector::Shipping => "shipping",
            Detector::Account => "account",
            Detector::Personal => "personal",
            Detector::Work => "work",
            Detector::Advertising => "advertising",
        }
    }

    pub fn parse(name: &str) -> Option<Detector> {
        Detector::ALL.into_iter().find(|detector| detector.as_str() == name)
    }
}

/// What a detector found: its `params` for the log, the English reason, and how sure it is (0 to
/// 1; see [`crate::MAIN_THRESHOLD`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub params: Value,
    pub reason: String,
    pub confidence: f64,
}

pub(crate) const INVOICE_STEMS: &[&str] = &[
    "rechnung",
    "invoice",
    "faktura",
    "quittung",
    "zahlungsbeleg",
    "kassenbon",
    "kaufbeleg",
    "gutschrift",
    "zahlungsbestätigung",
    "receipt",
    "payment confirmation",
    "billing statement",
    "credit note",
    "mahnung",
    "zahlungserinnerung",
    "zahlungseingang",
    "payment received",
    "payment reminder",
    "your bill",
    "beleg für",
    "belege",
    "ihre zahlung",
    "deine zahlung",
    "your payment",
    "erstattung",
    "refund",
];
const AMOUNT_WORDS: &[&str] = &[
    "betrag",
    "summe",
    "gesamt",
    "total",
    "amount",
    "zu zahlen",
    "fällig",
    "due",
    "mwst",
    "ust",
    "vat",
    "netto",
    "brutto",
];
const CURRENCIES: &[&str] = &["€", "eur", "$", "usd", "£", "gbp", "chf"];

const APPOINTMENT_STEMS: &[&str] = &[
    "termin",
    "appointment",
    "einladung",
    "invitation",
    "meeting",
    "reservierung",
    "reservation",
    "buchung",
    "booking",
    "sprechstunde",
];
/// Words with `termin` in them that are about a delivery, not an appointment.
const NOT_APPOINTMENTS: &[&str] = &["liefertermin", "zustelltermin"];
const MONTHS: &[&str] = &[
    "januar",
    "februar",
    "märz",
    "april",
    "mai",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "dezember",
    "january",
    "february",
    "march",
    "may",
    "june",
    "july",
    "october",
    "december",
    "jan",
    "feb",
    "mär",
    "mrz",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "okt",
    "oct",
    "nov",
    "dez",
    "dec",
];

pub(crate) const SHIPPING_WORDS: &[&str] = &[
    "versand",
    "versendet",
    "versandt",
    "verschickt",
    "sendung",
    "paket",
    "zustellung",
    "lieferung",
    "unterwegs",
    "zugestellt",
    "shipped",
    "shipment",
    "shipping",
    "tracking",
    "package",
    "parcel",
    "delivery",
    "delivered",
    "dispatched",
];

/// Runs `detector` over `mail`.
pub fn detect(detector: Detector, mail: &Mail) -> Option<Finding> {
    View::new(mail).run(detector)
}

/// The folded parts of a mail and its facts, made once for all detectors.
pub(crate) struct View<'a> {
    pub(crate) mail: &'a Mail,
    subject: String,
    text: String,
    pub(crate) facts: Facts,
}

impl<'a> View<'a> {
    pub(crate) fn new(mail: &'a Mail) -> View<'a> {
        let subject = fold(&mail.subject);
        let text = fold(&mail.text);
        let facts = Facts::of_folded(mail, &subject, &text);
        View { mail, subject, text, facts }
    }

    /// With the facts already made by [`Facts::of`] for this same mail, so they are made once per
    /// mail (final client review X-1).
    pub(crate) fn with_facts(mail: &'a Mail, facts: Facts) -> View<'a> {
        View { mail, subject: fold(&mail.subject), text: fold(&mail.text), facts }
    }

    pub(crate) fn run(&self, detector: Detector) -> Option<Finding> {
        if self.facts.bounce {
            return None;
        }
        match detector {
            Detector::Invoice => invoice(self),
            Detector::Appointment => appointment(self),
            Detector::Newsletter => newsletter(self),
            Detector::Shipping => shipping(self),
            Detector::Account => account(self),
            Detector::Personal => personal(self),
            Detector::Work => work(self),
            Detector::Advertising => advertising(self),
        }
    }
}

pub(crate) fn is_pdf(name: &str, content_type: &str) -> bool {
    name.to_lowercase().ends_with(".pdf") || content_type == "application/pdf"
}

fn invoice(view: &View<'_>) -> Option<Finding> {
    let facts = &view.facts;
    // A shop's advertising names prices and sometimes "invoice" too: never an invoice.
    if facts.mass_mail() && facts.sales.len() >= 2 {
        return None;
    }
    for attachment in &view.mail.attachments {
        let name = fold(&attachment.name);
        if name.ends_with(".pdf") && find_any(&name, INVOICE_STEMS).is_some() {
            return Some(Finding {
                params: json!({ "attachment": attachment.name }),
                reason: format!("Looks like an invoice: PDF attachment \"{}\"", attachment.name),
                confidence: 0.95,
            });
        }
    }
    let amount = facts.amounts.first().cloned();
    if find_any(&view.subject, INVOICE_STEMS).is_none() {
        // Without the word in the subject: an invoice number and an amount, not from a person.
        let number = facts.invoice_numbers.first()?;
        let amount = amount?;
        if find_any(&view.subject, ORDER_SUBJECT_WORDS).is_some() {
            return None;
        }
        if facts.sender == SenderKind::Person || find_any(&view.text, INVOICE_STEMS).is_none() {
            return None;
        }
        return Some(Finding {
            reason: format!("Looks like an invoice: invoice number {number}, {amount}"),
            params: json!({ "number": number, "amount": amount }),
            confidence: 0.85,
        });
    }
    let amount_word = find_any(&view.text, AMOUNT_WORDS).is_some();
    let confidence = if facts.pdf || (amount.is_some() && amount_word) {
        0.9
    } else if amount.is_some() && facts.sender != SenderKind::Person {
        0.82
    } else {
        return None;
    };
    let word = original_word(&view.mail.subject, INVOICE_STEMS).unwrap_or_default();
    let reason = match &amount {
        Some(amount) => format!("Looks like an invoice: \"{word}\" in the subject, {amount}"),
        None => format!("Looks like an invoice: \"{word}\" in the subject and a PDF attachment"),
    };
    let confidence = if facts.authenticated { confidence } else { confidence - 0.1 };
    Some(Finding { params: json!({ "word": word, "amount": amount }), reason, confidence })
}

/// The first amount of money in folded text, as written: a number with two decimals right before or
/// after a currency.
pub fn amount(text: &str) -> Option<String> {
    amount_at(text).map(|(found, _)| found)
}

/// [`amount`] and the byte range of `text` it stands at, so a caller looking for the next one goes
/// on behind it instead of searching for its text, which could find an earlier, rejected copy
/// (final client review X-1).
pub fn amount_at(text: &str) -> Option<(String, std::ops::Range<usize>)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() || (index > 0 && (bytes[index - 1].is_ascii_alphanumeric())) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || matches!(bytes[index], b'.' | b',')) {
            index += 1;
        }
        let mut end = index;
        while end > start && !bytes[end - 1].is_ascii_digit() {
            end -= 1;
        }
        let number = &text[start..end];
        if !money_number(number) || bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
            continue;
        }
        let after = text[end..].strip_prefix(' ').unwrap_or(&text[end..]);
        if let Some(currency) = CURRENCIES.iter().find(|c| after.starts_with(**c)) {
            let rest = &after[currency.len()..];
            if !currency.chars().all(char::is_alphabetic) || boundary(rest.chars().next()) {
                let spaced = text[end..].starts_with(' ');
                let stop = end + usize::from(spaced) + currency.len();
                return Some((format!("{number}{}{currency}", if spaced { " " } else { "" }), start..stop));
            }
        }
        let before = text[..start].strip_suffix(' ').unwrap_or(&text[..start]);
        if let Some(currency) = CURRENCIES.iter().find(|c| before.ends_with(**c)) {
            let rest = &before[..before.len() - currency.len()];
            if !currency.chars().all(char::is_alphabetic) || boundary(rest.chars().next_back()) {
                let spaced = text[..start].ends_with(' ');
                let from = start - usize::from(spaced) - currency.len();
                return Some((format!("{currency}{}{number}", if spaced { " " } else { "" }), from..end));
            }
        }
    }
    None
}

/// `49,90`, `1.249,00`, `12.50`, `1,249.00`: digits, the last separator two digits from the end.
fn money_number(number: &str) -> bool {
    let bytes = number.as_bytes();
    bytes.len() >= 4
        && matches!(bytes[bytes.len() - 3], b'.' | b',')
        && bytes[bytes.len() - 2..].iter().all(u8::is_ascii_digit)
        && bytes[0].is_ascii_digit()
        && !number[..number.len() - 3].contains(bytes[bytes.len() - 3] as char)
}

fn appointment(view: &View<'_>) -> Option<Finding> {
    let facts = &view.facts;
    // An advertised event or webinar is advertising, even with an invitation attached.
    if facts.mass_mail() && !facts.sales.is_empty() {
        return None;
    }
    if view.mail.calendar {
        return Some(Finding {
            params: json!({ "calendar": true }),
            reason: "Looks like an appointment: a calendar invitation".into(),
            confidence: 0.95,
        });
    }
    // The stems are single words, so a stem is always inside one word of the subject: one pass over
    // the words, never back and forth around each find (that was quadratic in a long word, security
    // audit 0.21.0 LABELS-H1).
    let stem = APPOINTMENT_STEMS.iter().find(|stem| {
        words(&view.subject)
            .any(|(_, word)| word.contains(**stem) && !NOT_APPOINTMENTS.iter().any(|not| word.contains(not)))
    })?;
    let date = facts.dates.first()?;
    let time = facts.times.first()?;
    let word = original_word(&view.mail.subject, &[stem]).unwrap_or_else(|| (*stem).to_owned());
    Some(Finding {
        reason: format!("Looks like an appointment: \"{word}\" on {date} at {time}"),
        params: json!({ "word": word, "date": date, "time": time }),
        confidence: if facts.mass_mail() { 0.8 } else { 0.88 },
    })
}

/// A number as a day of the month: 1 to 31, an optional dot after it.
fn day(word: &str) -> bool {
    let digits = word.strip_suffix('.').unwrap_or(word);
    digits.len() <= 2 && digits.parse::<u32>().is_ok_and(|day| (1..=31).contains(&day))
}

/// The first date in folded text, as written.
pub fn date(text: &str) -> Option<String> {
    // Numeric: 31.12.2026, 31.12.26, "31.12. ", 2026-12-31.
    for (start, run) in runs(text, |c| c.is_ascii_digit() || c == '.' || c == '-', false) {
        let parts: Vec<&str> = run.split('.').collect();
        let valid_day_month = |d: &str, m: &str| {
            d.len() <= 2
                && m.len() <= 2
                && d.parse::<u32>().is_ok_and(|d| (1..=31).contains(&d))
                && m.parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))
        };
        match parts.as_slice() {
            [d, m, y]
                if valid_day_month(d, m) && (y.len() == 2 || y.len() == 4) && y.bytes().all(|b| b.is_ascii_digit()) =>
            {
                return Some(run.to_owned());
            }
            [d, m, ""] if valid_day_month(d, m) && text[start + run.len()..].starts_with(' ') => {
                return Some(run.to_owned());
            }
            _ => {}
        }
        let parts: Vec<&str> = run.split('-').collect();
        if let [y, m, d] = parts.as_slice()
            && y.len() == 4
            && y.bytes().all(|b| b.is_ascii_digit())
            && m.len() == 2
            && d.len() == 2
            && m.parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))
            && d.parse::<u32>().is_ok_and(|d| (1..=31).contains(&d))
        {
            return Some(run.to_owned());
        }
    }
    // A day next to a month's name: "6. oktober", "october 6", "6 oct".
    let list: Vec<(usize, &str)> = words(text).collect();
    for (index, (start, word)) in list.iter().enumerate() {
        if !MONTHS.contains(word) {
            continue;
        }
        let end = start + word.len();
        if let Some((before, number)) = index.checked_sub(1).map(|i| list[i]) {
            let with_dot = if text[before + number.len()..].starts_with('.') {
                &text[before..before + number.len() + 1]
            } else {
                number
            };
            if day(with_dot) && start - (before + with_dot.len()) <= 1 {
                return Some(text[before..end].to_owned());
            }
        }
        if let Some(&(after, number)) = list.get(index + 1)
            && day(number)
            && after - end <= 1
        {
            return Some(text[*start..after + number.len()].to_owned());
        }
    }
    None
}

/// The first time of day in folded text, as written: `9:30`, `09:30 uhr`, `9 uhr`, `9am`, `14h`.
pub fn time(text: &str) -> Option<String> {
    // A unit may follow without a space (`9am`, `14h`).
    for (start, run) in runs(text, |c| c.is_ascii_digit() || c == ':', true) {
        if let Some((hours, minutes)) = run.split_once(':')
            && (1..=2).contains(&hours.len())
            && minutes.len() == 2
            && hours.parse::<u32>().is_ok_and(|h| h <= 23)
            && minutes.parse::<u32>().is_ok_and(|m| m <= 59)
        {
            return Some(run.to_owned());
        }
        if (1..=2).contains(&run.len()) && run.parse::<u32>().is_ok_and(|h| h <= 23) {
            let after = &text[start + run.len()..];
            let spaced = after.strip_prefix(' ').unwrap_or(after);
            for unit in ["uhr", "am", "pm", "h"] {
                if spaced.starts_with(unit) && boundary(spaced[unit.len()..].chars().next()) {
                    let used = after.len() - spaced.len() + unit.len();
                    return Some(text[start..start + run.len() + used].to_owned());
                }
            }
        }
    }
    None
}

/// Maximal runs of characters `keep` says yes to that stand apart from letters and digits around
/// them (a letter may follow with `letter_after`), with their byte offsets.
fn runs(text: &str, keep: impl Fn(char) -> bool, letter_after: bool) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (index, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (keep(c) && index < text.len(), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                let run = text[from..index].trim_end_matches(['-', ':']);
                let next = text[index..].chars().next();
                let clean_edges = boundary(text[..from].chars().next_back())
                    && (boundary(next) || (letter_after && next.is_some_and(char::is_alphabetic)));
                if clean_edges && run.starts_with(|c: char| c.is_ascii_digit()) {
                    out.push((from, run));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

fn newsletter(view: &View<'_>) -> Option<Finding> {
    let mail = view.mail;
    let facts = &view.facts;
    // Notifications about activity on an account (followers, posts, recaps) are no editions.
    if !facts.list_unsubscribe || facts.discussion_list || facts.notification {
        return None;
    }
    // Shops send their invoices, shipments, invitations and account mail with List-Unsubscribe
    // too, and mail mainly selling is advertising.
    if [invoice, shipping, appointment, account, advertising].iter().any(|detector| detector(view).is_some()) {
        return None;
    }
    let header = if mail.header("list-id").is_some() {
        "List-Id"
    } else if mail.header("precedence").is_some_and(|value| matches!(fold(value).as_str(), "bulk" | "list")) {
        "Precedence"
    } else if mail.header("list-unsubscribe-post").is_some() {
        "List-Unsubscribe-Post"
    } else if facts.sender == SenderKind::Marketing {
        "marketing sender"
    } else {
        return None;
    };
    // Only the subject and the beginning count: every advertising mail's footer says how to leave
    // the "newsletter".
    let head: String = view.text.chars().take(600).collect();
    let both = format!("{} {head}", view.subject);
    let editorial = find_any(&both, EDITORIAL_WORDS).map(|(word, _)| word);
    // Sent in bulk, but neither editorial nor selling: say nothing rather than guess.
    let (confidence, reason) = match editorial {
        Some(word) => (0.88, format!("Looks like a newsletter: it has List-Unsubscribe and {header}, and \"{word}\"")),
        None if facts.sales.is_empty() => {
            (0.75, format!("Looks like a newsletter: it has List-Unsubscribe and {header}"))
        }
        None => return None,
    };
    Some(Finding { params: json!({ "header": header, "word": editorial }), reason, confidence })
}

const EDITORIAL_WORDS: &[&str] = &[
    "newsletter",
    "weekly",
    "wöchentlich",
    "wochenrückblick",
    "monatsrückblick",
    "rückblick",
    "ausgabe",
    "issue no",
    "digest",
    "neuigkeiten",
    "news",
    "nachrichten",
    "blog",
    "podcast",
    "artikel",
    "article",
    "this week",
    "diese woche",
    "dieser woche",
    "jobs für dich",
    "neue jobs",
    "job alert",
    "jobs for you",
    "new jobs",
    "stellenangebote",
    "updates",
    "changelog",
    "release notes",
    "vereinsnachrichten",
    "mitgliederinfo",
    "rundbrief",
    "roundup",
    "recap",
];

/// A tracking number that names its carrier by itself, as a word of folded text: carrier and the
/// number in capitals.
fn own_tracking(text: &str) -> Option<(&'static str, String)> {
    for (_, word) in words(text) {
        let digits_after = |prefix: &str, lengths: std::ops::RangeInclusive<usize>| {
            word.strip_prefix(prefix)
                .is_some_and(|rest| lengths.contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_digit()))
        };
        if word.len() == 18 && word.starts_with("1z") && word.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Some(("UPS", word.to_ascii_uppercase()));
        }
        if digits_after("tba", 12..=12) {
            return Some(("Amazon", word.to_ascii_uppercase()));
        }
        if digits_after("jjd", 18..=20) || digits_after("00340434", 12..=12) {
            return Some(("DHL", word.to_ascii_uppercase()));
        }
    }
    None
}

/// The first carrier a mail names: in folded subject and text, the From domain, or `UPS` in
/// capitals in the original.
pub(crate) fn carrier_in(mail: &Mail, subject: &str, text: &str) -> Option<&'static str> {
    let named = |word: &str| has_word(subject, word) || has_word(text, word);
    let domain_labels: Vec<&str> = mail.from_domain().split('.').collect();
    let from = |prefix: &str| domain_labels.iter().any(|label| label.starts_with(prefix));
    if named("dhl") || text.contains("deutsche post") || subject.contains("deutsche post") || from("dhl") {
        return Some("DHL");
    }
    if named("dpd") || from("dpd") {
        return Some("DPD");
    }
    if named("hermes") || from("hermes") || from("myhermes") {
        return Some("Hermes");
    }
    if named("gls") || from("gls") {
        return Some("GLS");
    }
    if find_word(&mail.subject, "UPS").is_some()
        || find_word(&mail.text, "UPS").is_some()
        || domain_labels.contains(&"ups")
    {
        return Some("UPS");
    }
    if named("amazon") || from("amazon") {
        return Some("Amazon");
    }
    None
}

/// A tracking number and its carrier: one that names its carrier, or a run of 10 to 20 digits
/// beside a named carrier and a shipping word.
pub(crate) fn tracking(mail: &Mail, subject: &str, text: &str) -> Option<(&'static str, String)> {
    if let Some(found) = own_tracking(subject).or_else(|| own_tracking(text)) {
        return Some(found);
    }
    let carrier = carrier_in(mail, subject, text)?;
    if find_any(subject, SHIPPING_WORDS).is_none() && find_any(text, SHIPPING_WORDS).is_none() {
        return None;
    }
    let number = words(subject)
        .chain(words(text))
        .map(|(_, word)| word)
        .find(|word| (10..=20).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_digit()))?;
    Some((carrier, number.to_owned()))
}

/// Words of an order on its way, in the subject: with them, shipping needs no carrier.
pub(crate) const ORDER_SUBJECT_WORDS: &[&str] = &[
    "bestellbestätigung",
    "bestellung",
    "versandbestätigung",
    "versendet",
    "versandt",
    "verschickt",
    "ist unterwegs",
    "in zustellung",
    "zugestellt",
    "paket",
    "päckchen",
    "sendung",
    "lieferung",
    "bestellt:",
    "geliefert",
    "abholbereit",
    "zur abholung",
    "rücksendung",
    "retoure",
    "order confirmation",
    "your order",
    "order #",
    "order no",
    "has shipped",
    "shipped",
    "on its way",
    "out for delivery",
    "delivered",
    "package",
    "parcel",
    "ready for pickup",
    "return label",
    "refund for your return",
];

fn shipping(view: &View<'_>) -> Option<Finding> {
    let facts = &view.facts;
    let finding = |carrier: Option<&str>, tracking: Option<&str>, confidence: f64| {
        let mut reason = "Looks like a shipment".to_owned();
        let parts: Vec<String> = [carrier.map(str::to_owned), tracking.map(|t| format!("tracking number {t}"))]
            .into_iter()
            .flatten()
            .collect();
        if !parts.is_empty() {
            reason.push_str(": ");
            reason.push_str(&parts.join(", "));
        }
        Some(Finding { params: json!({ "carrier": carrier, "tracking": tracking }), reason, confidence })
    };
    // A shop's newsletter about free shipping names a carrier and a shipping word too.
    let advertising = facts.mass_mail() && facts.sales.len() >= 2;
    if let Some((carrier, number)) = &facts.tracking
        && !advertising
    {
        return finding(Some(carrier), Some(number), if facts.authenticated { 0.95 } else { 0.85 });
    }
    if advertising {
        return None;
    }
    let order_word = find_any(&view.subject, ORDER_SUBJECT_WORDS).map(|(word, _)| word);
    if let Some(carrier) = facts.carrier.as_deref()
        && find_any(&view.subject, SHIPPING_WORDS).is_some()
        && !facts.list_unsubscribe
    {
        return finding(Some(carrier), None, if facts.authenticated { 0.85 } else { 0.7 });
    }
    // An order or shipment named in the subject, by a company, not to a list or with order words
    // in the text too.
    let word = order_word?;
    if facts.sender == SenderKind::Person || facts.discussion_list || !facts.sales.is_empty() {
        return None;
    }
    // Goods are sent: an order of a subscription or a download has nothing on its way.
    let goods = find_any(&view.text, SHIPPING_WORDS).is_some()
        || find_any(&view.text, &["liefer", "päckchen", "abhol"]).is_some();
    let ordered =
        find_any(&view.text, &["bestell", "order", "sendung", "paket", "package", "parcel", "artikel", "item"])
            .is_some();
    if !goods || !ordered {
        return None;
    }
    Some(Finding {
        params: json!({ "carrier": facts.carrier, "tracking": null, "word": word }),
        reason: format!("Looks like a shipment: \"{word}\" in the subject"),
        confidence: if facts.list_unsubscribe { 0.8 } else { 0.85 } - if facts.authenticated { 0.0 } else { 0.15 },
    })
}

fn account(view: &View<'_>) -> Option<Finding> {
    let facts = &view.facts;
    if facts.sender == SenderKind::Person && !facts.automatic || facts.discussion_list {
        return None;
    }
    // The same service's ads and newsletters say "your account" too.
    if facts.sales.len() >= 2 {
        return None;
    }
    if let Some(word) = facts.account.first() {
        let strong = !matches!(
            word.as_str(),
            "dein konto"
                | "ihr konto"
                | "deinem konto"
                | "ihrem konto"
                | "your account"
                | "konto wurde"
                | "welcome to"
                | "willkommen bei"
                | "verbunden"
                | "mitgliedschaft"
                | "membership"
                | "richtlinie"
                | "vertrag"
                | "trial"
        );
        // "Your account was credited 50,00 €" is about money.
        if !strong && !facts.amounts.is_empty() {
            return None;
        }
        let confidence = match (strong, facts.code.is_some() || facts.account.len() >= 2) {
            (true, _) => 0.9,
            (false, true) => 0.85,
            (false, false) if !facts.mass_mail() && facts.sender == SenderKind::NoReply => 0.8,
            (false, false) => return None,
        };
        let confidence = if facts.list_unsubscribe && !facts.sales.is_empty() { confidence - 0.1 } else { confidence };
        // Phishing copies exactly these mails; one nothing vouches for is no sure account mail.
        let confidence = if facts.authenticated { confidence } else { confidence - 0.15 };
        return Some(Finding {
            params: json!({ "word": word, "code": facts.code.is_some() }),
            reason: format!("Looks like a message about your account: \"{word}\" in the subject"),
            confidence,
        });
    }
    // "Your code is 482913" from a system, without a word in the subject.
    facts.code.as_ref()?;
    if facts.mass_mail() || !matches!(facts.sender, SenderKind::NoReply | SenderKind::Role) {
        return None;
    }
    let both = format!("{} {}", view.subject, view.text);
    if !any_in(
        &both,
        &["anmeld", "login", "sign in", "verif", "bestätig", "confirm", "sicherheit", "security", "passwor"],
    ) {
        return None;
    }
    Some(Finding {
        confidence: if facts.authenticated { 0.85 } else { 0.7 },
        params: json!({ "word": null, "code": true }),
        reason: "Looks like a message about your account: a one-time code".into(),
    })
}

fn personal(view: &View<'_>) -> Option<Finding> {
    let facts = &view.facts;
    if !facts.written_by_person() || facts.same_domain || facts.formal {
        return None;
    }
    if [invoice, shipping, account].iter().any(|detector| detector(view).is_some()) || !facts.sales.is_empty() {
        return None;
    }
    // A person at a company's domain may as well write on business: only freemail is sure.
    if !facts.freemail {
        return None;
    }
    let confidence = match (facts.known_sender, facts.casual) {
        (true, true) => 0.9,
        (false, true) => 0.85,
        (true, false) => 0.82,
        (false, false) => return None,
    };
    let confidence = if facts.authenticated { confidence } else { confidence - 0.1 };
    let why = if facts.known_sender { "a sender you know" } else { "a private address" };
    Some(Finding {
        params: json!({ "known": facts.known_sender, "freemail": facts.freemail }),
        reason: format!("Looks personal: written by a person from {why}"),
        confidence,
    })
}

fn work(view: &View<'_>) -> Option<Finding> {
    let facts = &view.facts;
    // Mail to oneself is no colleague's.
    if view.facts.to_self {
        return None;
    }
    if !facts.written_by_person() || facts.freemail || !facts.sales.is_empty() {
        return None;
    }
    let (confidence, why) = if facts.same_domain {
        (0.9, "a colleague from your own domain")
    } else if facts.formal && facts.known_sender {
        (0.85, "a business contact you know")
    } else {
        return None;
    };
    let confidence = if facts.authenticated { confidence } else { confidence - 0.1 };
    Some(Finding {
        params: json!({ "colleague": facts.same_domain, "known": facts.known_sender }),
        reason: format!("Looks like work: written by {why}"),
        confidence,
    })
}

fn advertising(view: &View<'_>) -> Option<Finding> {
    let facts = &view.facts;
    if !facts.mass_mail() {
        return None;
    }
    if facts.discussion_list || facts.tracking.is_some() {
        return None;
    }
    let in_subject = facts
        .sales
        .iter()
        .filter(|word| view.subject.contains(word.trim_start_matches('-').trim_end_matches(" %")))
        .count();
    let score = facts.sales.len() + in_subject;
    let confidence = match score {
        4.. => 0.92,
        3 => 0.86,
        2 if in_subject >= 1 => 0.82,
        _ => return None,
    };
    Some(Finding {
        params: json!({ "words": facts.sales }),
        reason: format!("Looks like advertising: {}", facts.sales.join(", ")),
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_dates_and_times() {
        assert_eq!(amount("gesamt 49,90 € inkl. mwst").as_deref(), Some("49,90 €"));
        for text in ["gesamt 49,90 € inkl. mwst", "total: $1,249.00 due", "eur 12.50", "x49,90 € und 5,00€!"] {
            let (found, at) = amount_at(text).unwrap();
            assert_eq!(&text[at], found, "{text}");
        }
        assert_eq!(amount("total: $1,249.00 due").as_deref(), Some("$1,249.00"));
        assert_eq!(amount("eur 12.50").as_deref(), Some("eur 12.50"));
        assert_eq!(amount("version 1.2.34 euro"), None);
        assert_eq!(amount("call 0800 123 456"), None);
        assert_eq!(date("am 06.10.2026 um").as_deref(), Some("06.10.2026"));
        assert_eq!(date("am 6.10. um 9").as_deref(), Some("6.10."));
        assert_eq!(date("on 2026-10-06").as_deref(), Some("2026-10-06"));
        assert_eq!(date("dienstag, 6. oktober").as_deref(), Some("6. oktober"));
        assert_eq!(date("on october 6, 2026").as_deref(), Some("october 6"));
        assert_eq!(date("version 1.2.3"), None);
        assert_eq!(time("um 9:30 uhr").as_deref(), Some("9:30"));
        assert_eq!(time("um 9 uhr").as_deref(), Some("9 uhr"));
        assert_eq!(time("at 3pm").as_deref(), Some("3pm"));
        assert_eq!(time("ratio 12:345"), None);
    }
}
