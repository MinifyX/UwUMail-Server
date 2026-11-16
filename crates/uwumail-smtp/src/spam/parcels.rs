//! Fake shipping mails: a parcel that waits for a fee, a tracking number no carrier hands out, a
//! "track your parcel" mail whose links all lead somewhere else (docs/spam-filter.md, "Parcel mail").
//! The rules read what [`super::phishing::Input`] holds.

use super::links::site;
use super::phishing::{Finding, Input, LinkTarget};
use uwumail_labels::Carrier;

/// Words that say the mail is about a parcel, in lower case.
const PARCEL_WORDS: &[&str] = &[
    "paket",
    "päckchen",
    "sendung",
    "zustellung",
    "lieferung",
    "sendungsverfolgung",
    "package",
    "parcel",
    "shipment",
    "delivery",
    "tracking",
];

/// Carriers by name, in lower case; `ups` only as a word of its own.
const CARRIER_NAMES: &[&str] = &["dhl", "dpd", "hermes", "gls", "ups", "fedex", "deutsche post", "post"];

/// A parcel that waits for money or for the address to be confirmed: how fake shipping mails phish.
const FEE_CUES: &[&str] = &[
    "zollgebühr",
    "zollabgabe",
    "einfuhrabgabe",
    "nachgebühr",
    "zustellgebühr",
    "zustellungsgebühr",
    "lagergebühr",
    "versandgebühr",
    "ausstehende gebühr",
    "offene gebühr",
    "gebühr von",
    "gebühr bezahlen",
    "gebühr zahlen",
    "nachzahlung",
    "zahlung ausstehend",
    "adresse bestätigen",
    "adresse aktualisieren",
    "lieferadresse bestätigen",
    "lieferadresse aktualisieren",
    "zustellung bestätigen",
    "customs fee",
    "customs duty",
    "customs charge",
    "delivery fee",
    "redelivery fee",
    "shipping fee",
    "outstanding fee",
    "pay the fee",
    "confirm your address",
    "update your address",
    "confirm your delivery",
];

pub fn check(input: &Input<'_>) -> Vec<Finding> {
    let lower = format!("{}\n{}", input.subject, input.text).to_lowercase();
    let mut found = Vec::new();

    if let Some(number) = bad_number(&lower) {
        found.push(Finding {
            rule: "PARCEL_BAD_NUMBER",
            points: 2.5,
            detail: format!("{number} has a wrong check digit"),
        });
    }
    if input.from_authenticated || !PARCEL_WORDS.iter().any(|word| lower.contains(word)) {
        return found;
    }
    if let Some(cue) = FEE_CUES.iter().find(|cue| lower.contains(*cue)) {
        found.push(Finding { rule: "PARCEL_FEE_UNAUTH", points: 3.0, detail: format!("\"{cue}\"") });
    }
    // A carrier's mail links to the carrier, a shop's to the shop: one that names a carrier and
    // links only to a third site is no tracking mail.
    let from_site = input.from_address.and_then(super::phishing::domain_of).map(|domain| site(&domain));
    let hosts: Vec<&str> = input
        .links
        .iter()
        .filter_map(|link| match &link.target {
            LinkTarget::Host(host) => Some(host.as_str()),
            LinkTarget::Ip(_) => None,
        })
        .collect();
    if CARRIER_NAMES.iter().any(|name| has_word(&lower, name))
        && !hosts.is_empty()
        && hosts.len() == input.links.len()
        && hosts.iter().all(|host| Carrier::of_host(host).is_none() && Some(site(host)) != from_site)
    {
        let mut sites: Vec<String> = hosts.iter().map(|host| site(host)).collect();
        sites.sort();
        sites.dedup();
        sites.truncate(3);
        found.push(Finding { rule: "PARCEL_FOREIGN_LINK", points: 2.0, detail: sites.join(", ") });
    }
    found
}

/// The first tracking number of a format with a check digit (DHL's SSCC `0034…`, UPS `1Z…`) whose
/// check digit is wrong: no carrier hands such a number out.
// shortcut: SSCC and UPS only, GLS/DPD check digits once real mails show their formats.
fn bad_number(lower: &str) -> Option<String> {
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| word.len() <= 20)
        .find(|word| (is_sscc(word) && !sscc_ok(word)) || (is_ups(word) && !ups_ok(word)))
        .map(str::to_ascii_uppercase)
}

/// A shipping label's SSCC with application identifier 00 and a German GS1 prefix (340–344).
fn is_sscc(word: &str) -> bool {
    word.len() == 20
        && word.bytes().all(|b| b.is_ascii_digit())
        && word.starts_with("0034")
        && (b'0'..=b'4').contains(&word.as_bytes()[4])
}

fn sscc_ok(word: &str) -> bool {
    let digits: Vec<u32> = word[2..].bytes().map(|b| u32::from(b - b'0')).collect();
    let (check, body) = digits.split_last().expect("18 digits");
    let sum: u32 = body.iter().rev().enumerate().map(|(i, d)| d * if i % 2 == 0 { 3 } else { 1 }).sum();
    (10 - sum % 10) % 10 == *check
}

fn is_ups(word: &str) -> bool {
    word.len() == 18 && word.starts_with("1z") && word.as_bytes()[17].is_ascii_digit()
}

fn ups_ok(word: &str) -> bool {
    let sum: u32 = word.as_bytes()[2..17]
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let value =
                if b.is_ascii_digit() { u32::from(b - b'0') } else { (u32::from(b.to_ascii_uppercase()) - 63) % 10 };
            value * if i % 2 == 1 { 2 } else { 1 }
        })
        .sum();
    (10 - sum % 10) % 10 == u32::from(word.as_bytes()[17] - b'0')
}

fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = text[at + word.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        before && after
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spam::phishing::SeenLink;

    fn rules(input: &Input<'_>) -> Vec<&'static str> {
        check(input).iter().map(|finding| finding.rule).collect()
    }

    fn link(host: &str) -> SeenLink {
        SeenLink { named: None, target: LinkTarget::Host(host.to_owned()) }
    }

    #[test]
    fn check_digits() {
        assert!(sscc_ok("00340433958441804804"));
        assert!(!sscc_ok("00340433958441804805"));
        assert!(ups_ok("1z999aa10123456784"));
        assert!(!ups_ok("1z999aa10123456785"));
        assert_eq!(bad_number("sendung 00340433958441804804"), None);
        assert_eq!(bad_number("sendung 00340433958441804805"), Some("00340433958441804805".into()));
        // An order number that only looks a little like one is no tracking number.
        assert_eq!(bad_number("bestellung 00123456789012345678"), None);
    }

    #[test]
    fn a_fake_parcel_mail_is_told_apart() {
        let fake = Input {
            from_address: Some("info@paket-service.example"),
            subject: "Ihr DHL Paket wartet",
            text: "Ihre Sendung konnte nicht zugestellt werden. Bitte zahlen Sie die Zustellgebühr von 1,99 €.",
            links: vec![link("pay.elsewhere.example")],
            ..Input::default()
        };
        assert_eq!(rules(&fake), ["PARCEL_FEE_UNAUTH", "PARCEL_FOREIGN_LINK"]);
        // The same words from a sender that authenticates are the shop's own business; links to
        // the carrier or the sender's own site are what real tracking mails have.
        assert!(rules(&Input { from_authenticated: true, ..fake.clone() }).is_empty());
        let real = Input {
            text: "Ihre Sendung ist unterwegs.",
            links: vec![link("www.dhl.example"), link("shop.paket-service.example")],
            ..fake.clone()
        };
        assert!(rules(&real).is_empty());
        // "Gruppe" holds no "ups" word.
        let no_carrier = Input { subject: "Ihre Gruppe", text: "Lieferung am Montag", ..real };
        assert!(rules(&no_carrier).is_empty());
    }
}
