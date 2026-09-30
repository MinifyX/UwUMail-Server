//! The built-in detectors: invoices, appointments, newsletters and shipments (docs/labels.md,
//! "Detectors"). Each looks for a few signals that together rarely occur by chance; when in doubt,
//! they say no.

use serde_json::{Value, json};

use crate::Mail;
use crate::text::{boundary, find_any, find_word, fold, has_word, original_word, words};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Detector {
    Invoice,
    Appointment,
    Newsletter,
    Shipping,
}

impl Detector {
    pub const ALL: [Detector; 4] = [Detector::Invoice, Detector::Appointment, Detector::Newsletter, Detector::Shipping];

    pub fn as_str(self) -> &'static str {
        match self {
            Detector::Invoice => "invoice",
            Detector::Appointment => "appointment",
            Detector::Newsletter => "newsletter",
            Detector::Shipping => "shipping",
        }
    }

    pub fn parse(name: &str) -> Option<Detector> {
        Detector::ALL.into_iter().find(|detector| detector.as_str() == name)
    }
}

/// What a detector found: its `params` for the log, and the English reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub params: Value,
    pub reason: String,
}

const INVOICE_STEMS: &[&str] = &[
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

const SHIPPING_WORDS: &[&str] = &[
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
    let view = View::new(mail);
    match detector {
        Detector::Invoice => invoice(&view),
        Detector::Appointment => appointment(&view),
        Detector::Newsletter => newsletter(&view),
        Detector::Shipping => shipping(&view),
    }
}

/// The folded parts of a mail, made once.
struct View<'a> {
    mail: &'a Mail,
    subject: String,
    text: String,
}

impl<'a> View<'a> {
    fn new(mail: &'a Mail) -> View<'a> {
        View { mail, subject: fold(&mail.subject), text: fold(&mail.text) }
    }
}

fn is_pdf(name: &str, content_type: &str) -> bool {
    name.to_lowercase().ends_with(".pdf") || content_type == "application/pdf"
}

fn invoice(view: &View<'_>) -> Option<Finding> {
    for attachment in &view.mail.attachments {
        let name = fold(&attachment.name);
        if name.ends_with(".pdf") && find_any(&name, INVOICE_STEMS).is_some() {
            return Some(Finding {
                params: json!({ "attachment": attachment.name }),
                reason: format!("Looks like an invoice: PDF attachment \"{}\"", attachment.name),
            });
        }
    }
    find_any(&view.subject, INVOICE_STEMS)?;
    let pdf = view.mail.attachments.iter().any(|a| is_pdf(&a.name, &a.content_type));
    let amount = amount(&view.text);
    if !pdf && !(amount.is_some() && find_any(&view.text, AMOUNT_WORDS).is_some()) {
        return None;
    }
    let word = original_word(&view.mail.subject, INVOICE_STEMS).unwrap_or_default();
    let reason = match &amount {
        Some(amount) => format!("Looks like an invoice: \"{word}\" in the subject, {amount}"),
        None => format!("Looks like an invoice: \"{word}\" in the subject and a PDF attachment"),
    };
    Some(Finding { params: json!({ "word": word, "amount": amount }), reason })
}

/// The first amount of money in folded text, as written: a number with two decimals right before or
/// after a currency.
pub fn amount(text: &str) -> Option<String> {
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
                return Some(format!("{number}{}{currency}", if spaced { " " } else { "" }));
            }
        }
        let before = text[..start].strip_suffix(' ').unwrap_or(&text[..start]);
        if let Some(currency) = CURRENCIES.iter().find(|c| before.ends_with(**c)) {
            let rest = &before[..before.len() - currency.len()];
            if !currency.chars().all(char::is_alphabetic) || boundary(rest.chars().next_back()) {
                let spaced = text[..start].ends_with(' ');
                return Some(format!("{currency}{}{number}", if spaced { " " } else { "" }));
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
    if view.mail.calendar {
        return Some(Finding {
            params: json!({ "calendar": true }),
            reason: "Looks like an appointment: a calendar invitation".into(),
        });
    }
    let stem = APPOINTMENT_STEMS.iter().find(|stem| {
        let mut from = 0;
        while let Some(found) = view.subject[from..].find(**stem) {
            let at = from + found;
            let word = crate::text::word_at(&view.subject, at);
            if !NOT_APPOINTMENTS.iter().any(|not| word.contains(not)) {
                return true;
            }
            from = at + stem.len();
        }
        false
    })?;
    let both = format!("{} {}", view.subject, view.text);
    let date = date(&both)?;
    let time = time(&both)?;
    let word = original_word(&view.mail.subject, &[stem]).unwrap_or_else(|| (*stem).to_owned());
    Some(Finding {
        reason: format!("Looks like an appointment: \"{word}\" on {date} at {time}"),
        params: json!({ "word": word, "date": date, "time": time }),
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
    mail.header("list-unsubscribe")?;
    if mail.header("list-post").is_some()
        || invoice(view).is_some()
        || shipping(view).is_some()
        || appointment(view).is_some()
    {
        return None;
    }
    let bulk = mail.header("precedence").is_some_and(|value| matches!(fold(value).as_str(), "bulk" | "list"));
    let header = if mail.header("list-id").is_some() {
        "List-Id"
    } else if bulk {
        "Precedence"
    } else if mail.header("list-unsubscribe-post").is_some() {
        "List-Unsubscribe-Post"
    } else {
        return None;
    };
    Some(Finding {
        params: json!({ "header": header }),
        reason: format!("Looks like a newsletter: it has List-Unsubscribe and {header}"),
    })
}

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

/// The first carrier the mail names: in folded subject and text, the From domain, or `UPS` in
/// capitals in the original.
fn carrier(view: &View<'_>) -> Option<&'static str> {
    let named = |word: &str| has_word(&view.subject, word) || has_word(&view.text, word);
    let domain_labels: Vec<&str> = view.mail.from_domain().split('.').collect();
    let from = |prefix: &str| domain_labels.iter().any(|label| label.starts_with(prefix));
    if named("dhl") || view.text.contains("deutsche post") || view.subject.contains("deutsche post") || from("dhl") {
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
    if find_word(&view.mail.subject, "UPS").is_some()
        || find_word(&view.mail.text, "UPS").is_some()
        || domain_labels.contains(&"ups")
    {
        return Some("UPS");
    }
    if named("amazon") || from("amazon") {
        return Some("Amazon");
    }
    None
}

fn shipping(view: &View<'_>) -> Option<Finding> {
    let finding = |carrier: Option<&str>, tracking: Option<String>| {
        let mut reason = "Looks like a shipment".to_owned();
        let parts: Vec<String> =
            [carrier.map(str::to_owned), tracking.as_ref().map(|t| format!("tracking number {t}"))]
                .into_iter()
                .flatten()
                .collect();
        if !parts.is_empty() {
            reason.push_str(": ");
            reason.push_str(&parts.join(", "));
        }
        Finding { params: json!({ "carrier": carrier, "tracking": tracking }), reason }
    };
    if let Some((carrier, tracking)) = own_tracking(&view.subject).or_else(|| own_tracking(&view.text)) {
        return Some(finding(Some(carrier), Some(tracking)));
    }
    let carrier = carrier(view)?;
    let word_in_subject = find_any(&view.subject, SHIPPING_WORDS).is_some();
    if word_in_subject || find_any(&view.text, SHIPPING_WORDS).is_some() {
        let number = words(&view.subject)
            .chain(words(&view.text))
            .map(|(_, word)| word)
            .find(|word| (10..=20).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_digit()));
        if let Some(number) = number {
            return Some(finding(Some(carrier), Some(number.to_owned())));
        }
    }
    // Without a number, only mail that is not sent to a list: a shop's newsletter about free
    // shipping names a carrier and a shipping word too.
    (word_in_subject && view.mail.header("list-unsubscribe").is_none()).then(|| finding(Some(carrier), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_dates_and_times() {
        assert_eq!(amount("gesamt 49,90 € inkl. mwst").as_deref(), Some("49,90 €"));
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
