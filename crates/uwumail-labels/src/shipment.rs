//! A shipment a mail tells of (docs/labels.md, "Shipments"): carrier, tracking numbers, status,
//! expected day, shop, order number and pickup place, by rules, without I/O. Links are read, never
//! opened: a redirect that names its target in the link is followed in the text of the link only.
//!
//! Rather no shipment than a wrong one: a number alone counts only beside a carrier and a shipping
//! word, a tracking link only on a carrier's own host.

use crate::detect::{ORDER_SUBJECT_WORDS, SHIPPING_WORDS, carrier_in, date, own_number, time, tracking};
use crate::facts::Facts;
use crate::mail::Mail;
use crate::text::{find_any, fold, has_word, words};

/// Tracking numbers of one mail that are kept, at most.
pub const MAX_NUMBERS: usize = 10;
/// Characters of an order number, a shop's name and a pickup place's name, at most.
pub const MAX_REF_CHARS: usize = 40;
pub const MAX_NAME_CHARS: usize = 100;
/// Characters after a word such as „voraussichtlich“ that are looked at for a day and times.
const WINDOW_CHARS: usize = 160;
/// Redirects in a link that are followed, at most.
const MAX_REDIRECTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Carrier {
    Dhl,
    DeutschePost,
    Dpd,
    Hermes,
    Gls,
    Ups,
    AmazonLogistics,
    Fedex,
    /// Another carrier, by its name if known.
    Other(String),
}

impl Carrier {
    /// The id in JMAP (docs/jmap-shipments.md): lower camelCase.
    pub fn id(&self) -> &'static str {
        match self {
            Carrier::Dhl => "dhl",
            Carrier::DeutschePost => "deutschePost",
            Carrier::Dpd => "dpd",
            Carrier::Hermes => "hermes",
            Carrier::Gls => "gls",
            Carrier::Ups => "ups",
            Carrier::AmazonLogistics => "amazonLogistics",
            Carrier::Fedex => "fedex",
            Carrier::Other(_) => "other",
        }
    }

    /// The carrier of an id; `None` for an id that is none of [`Carrier::id`].
    pub fn from_id(id: &str) -> Option<Carrier> {
        Some(match id {
            "dhl" => Carrier::Dhl,
            "deutschePost" => Carrier::DeutschePost,
            "dpd" => Carrier::Dpd,
            "hermes" => Carrier::Hermes,
            "gls" => Carrier::Gls,
            "ups" => Carrier::Ups,
            "amazonLogistics" => Carrier::AmazonLogistics,
            "fedex" => Carrier::Fedex,
            "other" => Carrier::Other(String::new()),
            _ => return None,
        })
    }

    /// The carrier [`crate::Facts`] names (`"DHL"`, `"Deutsche Post"` …).
    fn from_name(name: &str) -> Carrier {
        match name {
            "DHL" => Carrier::Dhl,
            "Deutsche Post" => Carrier::DeutschePost,
            "DPD" => Carrier::Dpd,
            "Hermes" => Carrier::Hermes,
            "GLS" => Carrier::Gls,
            "UPS" => Carrier::Ups,
            "Amazon" => Carrier::AmazonLogistics,
            "FedEx" => Carrier::Fedex,
            other => Carrier::Other(other.to_owned()),
        }
    }

    /// The carrier whose own host this is: by the name before the top-level domain
    /// (`nolp.dhl.de`, `www.ups.com`, `dpd.co.uk`), so `dhl.example.com` is not DHL's.
    pub fn of_host(host: &str) -> Option<Carrier> {
        let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
        let name = match labels.as_slice() {
            [.., name, second, top] if top.len() == 2 && ["co", "com", "org", "net"].contains(second) => name,
            [.., name, _] => name,
            _ => return None,
        };
        Some(match name.to_ascii_lowercase().as_str() {
            "dhl" => Carrier::Dhl,
            "deutschepost" | "deutsche-post" => Carrier::DeutschePost,
            "dpd" | "dpdgroup" => Carrier::Dpd,
            "myhermes" | "hermesworld" | "hermes" => Carrier::Hermes,
            "gls-pakete" | "gls-group" | "gls-germany" | "gls" => Carrier::Gls,
            "ups" => Carrier::Ups,
            "amazon" => Carrier::AmazonLogistics,
            "fedex" => Carrier::Fedex,
            _ => return None,
        })
    }

    /// The carrier's public tracking page for a number, `None` for [`Carrier::Other`] and a number
    /// that is not letters and digits.
    pub fn tracking_url(&self, number: &str) -> Option<String> {
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        let page = match self {
            Carrier::Dhl => "https://www.dhl.de/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=",
            Carrier::DeutschePost => "https://www.deutschepost.de/de/s/sendungsverfolgung.html?piececode=",
            Carrier::Dpd => "https://tracking.dpd.de/status/de_DE/parcel/",
            Carrier::Hermes => "https://www.myhermes.de/empfangen/sendungsverfolgung/sendungsinformation#",
            Carrier::Gls => "https://gls-group.eu/DE/de/paketverfolgung?match=",
            Carrier::Ups => "https://www.ups.com/track?tracknum=",
            Carrier::AmazonLogistics => "https://track.amazon.de/tracking/",
            Carrier::Fedex => "https://www.fedex.com/fedextrack/?trknbr=",
            Carrier::Other(_) => return None,
        };
        Some(format!("{page}{number}"))
    }
}

/// Where a shipment is. Forward is the order here, see [`ShipmentStatus::moves_to`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ShipmentStatus {
    Ordered,
    Shipped,
    InTransit,
    OutForDelivery,
    Delivered,
    ReadyForPickup,
    DeliveryFailed,
    Returned,
    Cancelled,
}

impl ShipmentStatus {
    pub const ALL: [ShipmentStatus; 9] = [
        ShipmentStatus::Ordered,
        ShipmentStatus::Shipped,
        ShipmentStatus::InTransit,
        ShipmentStatus::OutForDelivery,
        ShipmentStatus::Delivered,
        ShipmentStatus::ReadyForPickup,
        ShipmentStatus::DeliveryFailed,
        ShipmentStatus::Returned,
        ShipmentStatus::Cancelled,
    ];

    /// The id in JMAP: camelCase.
    pub fn id(self) -> &'static str {
        match self {
            ShipmentStatus::Ordered => "ordered",
            ShipmentStatus::Shipped => "shipped",
            ShipmentStatus::InTransit => "inTransit",
            ShipmentStatus::OutForDelivery => "outForDelivery",
            ShipmentStatus::Delivered => "delivered",
            ShipmentStatus::ReadyForPickup => "readyForPickup",
            ShipmentStatus::DeliveryFailed => "deliveryFailed",
            ShipmentStatus::Returned => "returned",
            ShipmentStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_id(id: &str) -> Option<ShipmentStatus> {
        ShipmentStatus::ALL.into_iter().find(|status| status.id() == id)
    }

    /// Delivered, returned or cancelled: nothing more is coming.
    pub fn is_done(self) -> bool {
        matches!(self, ShipmentStatus::Delivered | ShipmentStatus::Returned | ShipmentStatus::Cancelled)
    }

    /// Whether a shipment at `self` takes `next` as its status: only forward, except that a failed
    /// delivery, a return and a cancellation always count, a failed delivery is tried again, and a
    /// shipment ready for pickup is then picked up (delivered).
    pub fn moves_to(self, next: ShipmentStatus) -> bool {
        next != self
            && (next > self
                || matches!(
                    next,
                    ShipmentStatus::DeliveryFailed | ShipmentStatus::Returned | ShipmentStatus::Cancelled
                )
                || (self == ShipmentStatus::DeliveryFailed && !next.is_done() && next != ShipmentStatus::Ordered)
                || (self == ShipmentStatus::ReadyForPickup && next == ShipmentStatus::Delivered))
    }
}

/// A day as a mail names it: a date (the year if written) or days after the mail's own `Date`
/// (`heute` 0, `morgen` 1). The caller knows the mail's date and the person's time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    On { year: Option<i32>, month: u32, day: u32 },
    InDays(u32),
}

/// When a shipment is expected: a day, with a time window (hour, minute) if the mail names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateWindow {
    pub day: Day,
    pub from: Option<(u32, u32)>,
    pub to: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickupKind {
    Packstation,
    PostOffice,
    ParcelShop,
    AccessPoint,
}

impl PickupKind {
    pub fn id(self) -> &'static str {
        match self {
            PickupKind::Packstation => "packstation",
            PickupKind::PostOffice => "postOffice",
            PickupKind::ParcelShop => "parcelShop",
            PickupKind::AccessPoint => "accessPoint",
        }
    }
}

/// Where a shipment waits to be picked up, and until when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pickup {
    pub kind: PickupKind,
    pub name: Option<String>,
    pub address: Option<String>,
    pub until: Option<Day>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShipmentFacts {
    pub carrier: Option<Carrier>,
    /// In capitals; one mail can name several shipments of one carrier.
    pub numbers: Vec<String>,
    pub tracking_url: Option<String>,
    pub status: Option<ShipmentStatus>,
    pub expected: Option<DateWindow>,
    pub merchant: Option<String>,
    pub order_ref: Option<String>,
    pub pickup: Option<Pickup>,
    pub confidence: f32,
}

/// The shipment `mail` tells of, if any.
pub fn shipment(mail: &Mail) -> Option<ShipmentFacts> {
    let subject = fold(&mail.subject);
    let text = fold(&mail.text);
    let facts = Facts::of_folded(mail, &subject, &text);
    shipment_of(mail, &subject, &text, &facts)
}

pub(crate) fn shipment_of(mail: &Mail, subject: &str, text: &str, facts: &Facts) -> Option<ShipmentFacts> {
    // A shop's newsletter about free shipping.
    if facts.mass_mail() && facts.sales.len() >= 2 {
        return None;
    }
    let status = status_of(subject).or_else(|| status_of(text));
    // A label for a return the person still has to send is no shipment yet.
    if status != Some(ShipmentStatus::Returned)
        && (find_any(subject, RETURN_LABEL_WORDS).is_some() || find_any(text, RETURN_LABEL_WORDS).is_some())
    {
        return None;
    }

    let links: Vec<CarrierLink> = mail.links.iter().filter_map(|link| carrier_link(link)).collect();
    let mut numbers: Vec<(Carrier, String, f32)> = Vec::new();
    let add = |carrier: Carrier, number: String, confidence: f32, numbers: &mut Vec<(Carrier, String, f32)>| {
        if numbers.len() < MAX_NUMBERS && !numbers.iter().any(|(_, known, _)| *known == number) {
            numbers.push((carrier, number, confidence));
        }
    };
    for link in &links {
        if let Some(number) = &link.number {
            add(link.carrier.clone(), number.clone(), 0.95, &mut numbers);
        }
    }
    for (_, word) in words(subject).chain(words(text)) {
        if let Some((carrier, number)) = own_number(word) {
            add(Carrier::from_name(carrier), number, 0.9, &mut numbers);
        }
    }
    if numbers.is_empty()
        && !facts.mass_mail()
        && let Some((carrier, number)) = tracking(mail, subject, text)
    {
        add(Carrier::from_name(carrier), number.to_ascii_uppercase(), 0.85, &mut numbers);
    }

    let order_ref = amazon_order(subject)
        .or_else(|| amazon_order(text))
        .or_else(|| order_number(subject))
        .or_else(|| order_number(text));
    let order_subject = find_any(subject, ORDER_SUBJECT_WORDS).is_some() || find_any(subject, SHIPPING_WORDS).is_some();
    let mut confidence;
    if numbers.is_empty() {
        // An order confirmation or a shop's shipping notice without a number: by shop and order.
        if order_ref.is_none() || facts.mass_mail() || !order_subject {
            return None;
        }
        confidence = 0.8;
    } else {
        // An invoice that names the shipment in the small print.
        if !order_subject && status.is_none() && find_any(subject, INVOICE_WORDS).is_some() {
            return None;
        }
        confidence = numbers.iter().map(|(_, _, c)| *c).fold(0.0, f32::max);
    }
    if !facts.authenticated {
        confidence -= 0.1;
    }

    let carrier = numbers
        .first()
        .map(|(carrier, _, _)| carrier.clone())
        .or_else(|| links.iter().find(|link| link.carrier != Carrier::AmazonLogistics).map(|link| link.carrier.clone()))
        .or_else(|| carrier_in(mail, subject, text).filter(|name| *name != "Amazon").map(Carrier::from_name));
    let numbers: Vec<String> =
        numbers.into_iter().filter(|(of, _, _)| Some(of) == carrier.as_ref()).map(|(_, number, _)| number).collect();
    let tracking_url = links
        .iter()
        .find(|link| {
            Some(&link.carrier) == carrier.as_ref() && link.number.is_some() && link.number.as_ref() == numbers.first()
        })
        .map(|link| link.url.clone())
        .or_else(|| carrier.as_ref().zip(numbers.first()).and_then(|(carrier, number)| carrier.tracking_url(number)))
        .or_else(|| links.iter().find(|link| link.number.is_none()).map(|link| link.url.clone()));

    // Out for delivery means today unless the mail names another day.
    let today = (status == Some(ShipmentStatus::OutForDelivery)).then_some(Day::InDays(0));
    let expected = expected_of(subject, None)
        .or_else(|| expected_of(text, today))
        .or_else(|| today.map(|day| DateWindow { day, from: None, to: None }));
    let pickup = matches!(status, Some(ShipmentStatus::ReadyForPickup | ShipmentStatus::DeliveryFailed))
        .then(|| pickup_of(text))
        .flatten();
    Some(ShipmentFacts {
        merchant: merchant_of(mail),
        carrier,
        numbers,
        tracking_url,
        status,
        expected,
        order_ref,
        pickup,
        confidence,
    })
}

/// Words of a status, the most telling first: a mail about a failed delivery also tells what was
/// out for delivery, one about a delivery what was shipped.
const STATUS_WORDS: &[(ShipmentStatus, &[&str])] = &[
    (
        ShipmentStatus::Cancelled,
        &[
            "wurde storniert",
            "ist storniert",
            "storniert:",
            "stornierungsbestätigung",
            "bestellung storniert",
            "has been cancelled",
            "has been canceled",
            "was cancelled",
            "was canceled",
            "order cancelled",
            "order canceled",
        ],
    ),
    (
        ShipmentStatus::Returned,
        &[
            "rücksendung ist unterwegs",
            "retoure ist unterwegs",
            "rücksendung ist auf dem weg",
            "rücksendung erhalten",
            "retoure erhalten",
            "rücksendung ist bei uns eingegangen",
            "retoure ist bei uns eingegangen",
            "zurück an den absender",
            "an den absender zurück",
            "return to sender",
            "returned to sender",
            "we received your return",
            "we've received your return",
            "your return is on its way",
            "your return has been received",
            "refund for your return",
        ],
    ),
    (
        ShipmentStatus::DeliveryFailed,
        &[
            "konnte nicht zugestellt",
            // „konnten wir Sie heute nicht antreffen“, „haben Sie leider nicht angetroffen“
            "nicht antreffen",
            "nicht angetroffen",
            "zustellversuch war nicht erfolgreich",
            "zustellung war nicht möglich",
            "nicht zustellbar",
            "could not be delivered",
            "delivery attempt was unsuccessful",
            "we missed you",
            "delivery failed",
        ],
    ),
    (
        ShipmentStatus::Delivered,
        &[
            "wurde zugestellt",
            "ist zugestellt",
            "erfolgreich zugestellt",
            "zugestellt:",
            "wurde geliefert",
            "ist angekommen",
            "wurde bei deinem nachbarn",
            "wurde bei ihrem nachbarn",
            "wurde beim nachbarn",
            "am ablageort abgelegt",
            "an deinem ablageort",
            "an ihrem ablageort",
            "has been delivered",
            "was delivered",
            "delivered:",
            "your package was delivered",
            "has arrived",
        ],
    ),
    (
        ShipmentStatus::ReadyForPickup,
        &[
            "zur abholung bereit",
            "liegt zur abholung",
            "bereit zur abholung",
            "abholbereit",
            "kann abgeholt werden",
            "kann ab sofort abgeholt",
            "liegt in der packstation",
            "liegt in ihrer packstation",
            "liegt in deiner packstation",
            "ready for pickup",
            "ready for collection",
            "ready to be picked up",
        ],
    ),
    (
        ShipmentStatus::OutForDelivery,
        &[
            "wird heute zugestellt",
            "wird heute geliefert",
            "kommt heute",
            "zustellung heute",
            "heute in zustellung",
            "ist in zustellung",
            "in zustellung",
            "in der zustellung",
            "zustellfahrzeug",
            "out for delivery",
            "arriving today",
            "will be delivered today",
            "zustellung erfolgt heute",
        ],
    ),
    (
        ShipmentStatus::InTransit,
        &[
            "in transit",
            "im paketzentrum",
            "im zielpaketzentrum",
            "im start-paketzentrum",
            "wird transportiert",
            "wurde sortiert",
            "im depot",
            "im paketverteilzentrum",
        ],
    ),
    (
        ShipmentStatus::Shipped,
        &[
            "wurde versandt",
            "wurde versendet",
            "ist versandt",
            "wurde verschickt",
            "versandbestätigung",
            "versendet:",
            "verschickt:",
            "versandt:",
            "ist unterwegs",
            "ist auf dem weg",
            "an den versanddienstleister übergeben",
            "an den paketdienst übergeben",
            "an dhl übergeben",
            "an dpd übergeben",
            "an hermes übergeben",
            "an gls übergeben",
            "an ups übergeben",
            "elektronisch angekündigt",
            "has shipped",
            "has been shipped",
            "was shipped",
            "shipped:",
            "is on its way",
            "dispatched",
            "shipping confirmation",
        ],
    ),
    (
        ShipmentStatus::Ordered,
        &[
            "bestellbestätigung",
            "vielen dank für deine bestellung",
            "vielen dank für ihre bestellung",
            "danke für deine bestellung",
            "danke für ihre bestellung",
            "bestellung ist eingegangen",
            "bestellung eingegangen",
            "bestellt:",
            "order confirmation",
            "thank you for your order",
            "thanks for your order",
            "we've received your order",
            "we have received your order",
        ],
    ),
];

/// A label for a return still to be sent.
const RETURN_LABEL_WORDS: &[&str] =
    &["rücksendeetikett", "retourenlabel", "retourenschein", "rücksendelabel", "return label", "return shipping label"];

const INVOICE_WORDS: &[&str] = &["rechnung", "invoice", "receipt", "beleg", "quittung"];

/// The most telling status `folded` names.
fn status_of(folded: &str) -> Option<ShipmentStatus> {
    STATUS_WORDS
        .iter()
        .find(|(_, phrases)| phrases.iter().any(|phrase| folded.contains(phrase)))
        .map(|(status, _)| *status)
}

/// Words before an expected day or time window.
const EXPECTED_WORDS: &[&str] = &[
    "voraussichtlich",
    "zustellung am",
    "lieferung am",
    "liefertermin",
    "lieferdatum",
    "zustelltag",
    "zeitfenster",
    "wird am",
    "kommt am",
    "heute zwischen",
    "zwischen",
    "expected",
    "estimated delivery",
    "arriving",
    "delivery date",
    "scheduled delivery",
    "will arrive",
];

/// `text` from byte `at` (a char boundary) on, at most `chars` characters.
fn window(text: &str, at: usize, chars: usize) -> &str {
    let rest = &text[at..];
    rest.char_indices().nth(chars).map_or(rest, |(end, _)| &rest[..end])
}

/// The expected day and time window `folded` names; a window without a day falls back to `default`.
fn expected_of(folded: &str, default: Option<Day>) -> Option<DateWindow> {
    let (word, at) = find_any(folded, EXPECTED_WORDS)?;
    let after = window(folded, at + word.len(), WINDOW_CHARS);
    // „heute zwischen …“: the day may stand just before the word.
    let before_start = folded[..at].char_indices().rev().nth(20).map_or(0, |(i, _)| i);
    let near = &folded[before_start..at + word.len()];
    let relative = |text: &str| {
        if has_word(text, "übermorgen") {
            Some(Day::InDays(2))
        } else if has_word(text, "morgen") || has_word(text, "tomorrow") {
            Some(Day::InDays(1))
        } else if has_word(text, "heute") || has_word(text, "today") {
            Some(Day::InDays(0))
        } else {
            None
        }
    };
    let day = date(after)
        .and_then(|written| parse_date(&written, after))
        .or_else(|| relative(after))
        .or_else(|| relative(near));
    let (from, to) = times(after);
    let day = day.or(default)?;
    Some(DateWindow { day, from, to })
}

/// An hour and minute.
type Clock = (u32, u32);

/// The first time in `text`, and a second one just after it (a window: „10:15 und 12:15“).
fn times(text: &str) -> (Option<Clock>, Option<Clock>) {
    let Some(first) = time(text) else { return (None, None) };
    let Some(at) = text.find(&first) else { return (None, None) };
    let rest = window(text, at + first.len(), 25);
    let from = parse_time(&first, rest);
    let to = time(rest)
        .and_then(|second| {
            let after = rest.find(&second).map_or("", |at| &rest[at + second.len()..]);
            parse_time(&second, after)
        })
        .filter(|to| from.is_some_and(|from| *to > from));
    (from, to)
}

/// A time as [`crate::time`] finds it: `9:30`, `09:30 uhr`, `9 uhr`, `9am`, `2 pm`, `14h`.
/// `after` is the text that follows, for `2:00 pm`.
fn parse_time(written: &str, after: &str) -> Option<(u32, u32)> {
    let digits: String = written.chars().take_while(|c| c.is_ascii_digit() || *c == ':').collect();
    let (hours, minutes) = digits.split_once(':').unwrap_or((&digits, "0"));
    let mut hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    let mut unit = written[digits.len()..].trim();
    if unit.is_empty() {
        let next = after.trim_start();
        unit = ["am", "pm"]
            .into_iter()
            .find(|u| next.starts_with(u) && !next[u.len()..].starts_with(char::is_alphanumeric))
            .unwrap_or("");
    }
    if unit == "pm" && hours < 12 {
        hours += 12;
    } else if unit == "am" && hours == 12 {
        hours = 0;
    }
    (hours <= 23 && minutes <= 59).then_some((hours, minutes))
}

/// A date as [`crate::date`] finds it, with a year written just after it in `context`.
fn parse_date(written: &str, context: &str) -> Option<Day> {
    let number = |s: &str| s.trim_end_matches('.').parse::<u32>().ok();
    let (day, month, year) = if let [y, m, d] = written.split('-').collect::<Vec<_>>().as_slice()
        && y.len() == 4
    {
        (number(d)?, number(m)?, Some(number(y)? as i32))
    } else if written.contains('.')
        && written.split('.').nth(1).is_some_and(|m| !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()))
    {
        let mut parts = written.split('.');
        let day = number(parts.next()?)?;
        let month = number(parts.next()?)?;
        let year = parts
            .next()
            .filter(|y| !y.is_empty())
            .and_then(number)
            .map(|y| if y < 100 { 2000 + y as i32 } else { y as i32 });
        (day, month, year)
    } else {
        // „6. oktober“, „october 6“: a day and a month's name.
        let mut day = None;
        let mut month = None;
        for (_, word) in words(written) {
            if let Some(n) = number(word) {
                day = Some(n);
            } else {
                month = month_number(word);
            }
        }
        let rest = context.find(written).map_or("", |at| &context[at + written.len()..]);
        let year = words(rest)
            .next()
            .filter(|(at, word)| *at <= 2 && word.len() == 4)
            .and_then(|(_, word)| word.parse::<i32>().ok())
            .filter(|year| (2000..=2100).contains(year));
        (day?, month?, year)
    };
    ((1..=31).contains(&day) && (1..=12).contains(&month)).then_some(Day::On { year, month, day })
}

fn month_number(word: &str) -> Option<u32> {
    let prefix: String = word.chars().take(3).collect();
    Some(match prefix.as_str() {
        "jan" => 1,
        "feb" => 2,
        "mär" | "mrz" | "mar" => 3,
        "apr" => 4,
        "mai" | "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "okt" | "oct" => 10,
        "nov" => 11,
        "dez" | "dec" => 12,
        _ => return None,
    })
}

/// Words before the last day of a pickup.
const UNTIL_WORDS: &[&str] =
    &["abholfrist", "bis spätestens", "bis zum", "abholen bis", "until", "pick it up by", "collect it by"];

/// Where `folded` says the shipment waits.
fn pickup_of(folded: &str) -> Option<Pickup> {
    let kinds: [(PickupKind, &[&str]); 4] = [
        (PickupKind::Packstation, &["packstation"]),
        (PickupKind::PostOffice, &["postfiliale", "filiale", "post office", "postamt"]),
        (
            PickupKind::ParcelShop,
            &["paketshop", "parcelshop", "parcel shop", "pickup point", "paketladen", "annahmestelle"],
        ),
        (PickupKind::AccessPoint, &["access point"]),
    ];
    let (kind, word, at) = kinds
        .iter()
        .filter_map(|(kind, stems)| find_any(folded, stems).map(|(word, at)| (*kind, word, at)))
        .min_by_key(|(_, _, at)| *at)?;
    let name = (kind == PickupKind::Packstation)
        .then(|| words(&folded[at + word.len()..]).next())
        .flatten()
        .filter(|(offset, number)| *offset <= 1 && number.len() <= 5 && number.bytes().all(|b| b.is_ascii_digit()))
        .map(|(_, number)| format!("Packstation {number}"));
    let until = find_any(folded, UNTIL_WORDS).and_then(|(word, at)| {
        let after = window(folded, at + word.len(), 60);
        date(after).and_then(|written| parse_date(&written, after))
    });
    Some(Pickup { kind, name, address: None, until })
}

/// An Amazon order number, `123-1234567-1234567`, in `text`.
fn amazon_order(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let digits =
        |from: usize, count: usize| bytes.get(from..from + count).is_some_and(|run| run.iter().all(u8::is_ascii_digit));
    (0..bytes.len().saturating_sub(18)).find_map(|start| {
        let fits = (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
            && digits(start, 3)
            && bytes[start + 3] == b'-'
            && digits(start + 4, 7)
            && bytes[start + 11] == b'-'
            && digits(start + 12, 7)
            && bytes.get(start + 19).is_none_or(|b| !b.is_ascii_alphanumeric());
        fits.then(|| text[start..start + 19].to_owned())
    })
}

/// Words before an order number.
const ORDER_NUMBER_WORDS: &[&str] = &[
    "bestellnummer",
    "bestell-nr",
    "bestellnr",
    "auftragsnummer",
    "auftrags-nr",
    "order number",
    "order no",
    "order #",
    "bestellung nr",
    "bestellung #",
    "bestellung",
    "order",
];

/// The order number after a word such as „Bestellnummer“: letters, digits and dashes with a digit.
fn order_number(folded: &str) -> Option<String> {
    for stem in ORDER_NUMBER_WORDS {
        for (at, _) in folded.match_indices(stem).take(5) {
            let rest = folded[at + stem.len()..].trim_start_matches(|c: char| " :#.".contains(c) || c == '\u{a0}');
            let rest = rest.strip_prefix("nr").map_or(rest, |r| r.trim_start_matches(|c: char| " :#.".contains(c)));
            let candidate: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .take(MAX_REF_CHARS + 1)
                .collect();
            let candidate = candidate.trim_matches('-');
            if (3..=MAX_REF_CHARS).contains(&candidate.len()) && candidate.bytes().any(|b| b.is_ascii_digit()) {
                return Some(candidate.to_ascii_uppercase());
            }
        }
    }
    None
}

/// The shop: Amazon by its domain, else the sender's name, unless a carrier sends.
fn merchant_of(mail: &Mail) -> Option<String> {
    match Carrier::of_host(mail.from_domain()) {
        Some(Carrier::AmazonLogistics) => return Some("Amazon".to_owned()),
        Some(_) => return None,
        None => {}
    }
    let name = mail.from_name.trim();
    let name = if name.is_empty() {
        let labels: Vec<&str> = mail.from_domain().split('.').collect();
        labels.len().checked_sub(2).map(|i| labels[i]).unwrap_or_default()
    } else {
        name
    };
    let name: String = name.chars().take(MAX_NAME_CHARS).collect();
    (!name.is_empty()).then_some(name)
}

/// A link on a carrier's own host, with the number it names.
struct CarrierLink {
    carrier: Carrier,
    number: Option<String>,
    url: String,
}

/// Query keys and path words that hold a tracking number.
const NUMBER_KEYS: &[&str] = &[
    "piececode",
    "idc",
    "tracking-id",
    "trackingid",
    "trackingnumber",
    "tracknum",
    "tracknums",
    "tracknumbers",
    "inquirynumber1",
    "parcelnumber",
    "parcelno",
    "parcelnr",
    "match",
    "trknbr",
    "sendungsnummer",
];
const NUMBER_PATH_WORDS: &[&str] = &["parcel", "sendungsinformation", "tracking", "track", "sendungsverfolgung"];
/// Query keys of redirects that name their target.
const REDIRECT_KEYS: &[&str] =
    &["url", "u", "target", "redirect", "redirect_url", "redirecturl", "dest", "destination", "link", "goto", "r"];

fn carrier_link(link: &str) -> Option<CarrierLink> {
    let mut url = link.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let parts = Url::split(&url)?;
        if let Some(carrier) = Carrier::of_host(&parts.host) {
            let number = number_in(&parts);
            // Only https links of the carrier itself are kept to open later.
            if !url.starts_with("https://") {
                let number = number?;
                return Some(CarrierLink { url: carrier.tracking_url(&number)?, carrier, number: Some(number) });
            }
            let carrier_link = CarrierLink { carrier, number, url: url.clone() };
            if carrier_link.carrier == Carrier::AmazonLogistics
                && carrier_link.number.is_none()
                && !parts.path.contains("progress-tracker")
                && !parts.path.contains("ship-track")
            {
                return None;
            }
            return Some(carrier_link);
        }
        let target = parts
            .query_pairs()
            .find(|(key, _)| REDIRECT_KEYS.contains(&key.to_ascii_lowercase().as_str()))
            .and_then(|(_, value)| percent_decode(value))
            .filter(|target| target.starts_with("https://") || target.starts_with("http://"))?;
        url = target;
    }
    None
}

fn number_in(parts: &Url<'_>) -> Option<String> {
    let valid = |value: &str| {
        let value = value.split(',').next().unwrap_or_default().trim();
        ((8..=35).contains(&value.len())
            && value.bytes().all(|b| b.is_ascii_alphanumeric())
            && value.bytes().filter(u8::is_ascii_digit).count() >= 4)
            .then(|| value.to_ascii_uppercase())
    };
    parts
        .query_pairs()
        .filter(|(key, _)| NUMBER_KEYS.contains(&key.to_ascii_lowercase().as_str()))
        .find_map(|(_, value)| percent_decode(value).as_deref().and_then(valid))
        .or_else(|| {
            let segments: Vec<&str> = parts.path.split('/').filter(|s| !s.is_empty()).collect();
            segments
                .windows(2)
                .filter(|pair| NUMBER_PATH_WORDS.contains(&pair[0].to_ascii_lowercase().as_str()))
                .find_map(|pair| valid(pair[1]))
        })
        .or_else(|| valid(parts.fragment))
}

/// The parts of an `http(s)` link that are read; enough for links, not a URL parser.
struct Url<'a> {
    host: String,
    path: &'a str,
    query: &'a str,
    fragment: &'a str,
}

impl<'a> Url<'a> {
    fn split(url: &'a str) -> Option<Url<'a>> {
        let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        // `https://dhl.de@elsewhere.example/` goes elsewhere.
        let host = authority.rsplit('@').next().unwrap_or_default();
        let host = host.split(':').next().unwrap_or_default().to_ascii_lowercase();
        let rest = &rest[end..];
        let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        (!host.is_empty()).then_some(Url { host, path, query, fragment })
    }

    fn query_pairs(&self) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.query.split('&').filter_map(|pair| pair.split_once('='))
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}
