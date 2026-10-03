//! Whether a label someone creates or changes overlaps with one they have (docs/labels.md,
//! "Overlapping labels"): the same name, the meaning of a base label, or largely the same words.
//! Overlapping labels are what makes a mail get two labels for one thing, or the wrong one of two.

use serde::Serialize;

use crate::Base;
use crate::text::{fold, words};

/// A label the new one is compared with.
#[derive(Debug, Clone, Copy)]
pub struct OverlapLabel<'a> {
    pub id: i64,
    pub name: &'a str,
    pub description: &'a str,
    pub base: Option<Base>,
}

/// Where the new label overlaps with one already there.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overlap {
    pub id: i64,
    /// `name` (the same name, or a name for the same base label), `meaning` (words that are what a
    /// base label is about), or `words` (largely the same words).
    pub kind: &'static str,
    /// The words both have, as stems, at most 8.
    pub words: Vec<String>,
}

const STOPWORDS: &[&str] = &[
    "und",
    "oder",
    "der",
    "die",
    "das",
    "den",
    "dem",
    "des",
    "ein",
    "eine",
    "einer",
    "eines",
    "einem",
    "mit",
    "von",
    "für",
    "fuer",
    "auf",
    "aus",
    "bei",
    "zum",
    "zur",
    "alle",
    "allem",
    "alles",
    "mails",
    "mail",
    "e-mail",
    "emails",
    "nachrichten",
    "sowie",
    "auch",
    "nicht",
    "nur",
    "wie",
    "was",
    "wenn",
    "the",
    "and",
    "for",
    "from",
    "with",
    "all",
    "any",
    "that",
    "this",
    "are",
    "not",
    "about",
    "like",
    "messages",
    "message",
    "etc",
    "z.b",
    "usw",
    "meine",
    "mein",
    "my",
    "your",
    "deine",
    "dein",
    "ihre",
];

/// Words of each base label's subject (stemmed when compared).
fn concept(base: Base) -> &'static [&'static str] {
    match base {
        Base::Invoice => &[
            "rechnung",
            "invoice",
            "quittung",
            "beleg",
            "zahlung",
            "receipt",
            "payment",
            "bill",
            "billing",
            "mahnung",
            "gutschrift",
            "betrag",
            "abrechnung",
            "abbuchung",
            "lastschrift",
        ],
        Base::Shipping => &[
            "versand",
            "paket",
            "päckchen",
            "lieferung",
            "sendung",
            "bestellung",
            "order",
            "shipping",
            "shipment",
            "delivery",
            "package",
            "parcel",
            "tracking",
            "zustellung",
            "retoure",
        ],
        Base::Appointment => &[
            "termin",
            "einladung",
            "meeting",
            "reservierung",
            "buchung",
            "booking",
            "appointment",
            "besprechung",
            "kalender",
            "calendar",
            "invitation",
            "reservation",
        ],
        Base::Newsletter => &["newsletter", "digest", "blog", "news", "abo", "rundbrief", "subscription", "podcast"],
        Base::Account => &[
            "konto",
            "account",
            "passwort",
            "password",
            "login",
            "anmeldung",
            "sicherheit",
            "security",
            "registrierung",
            "verifizierung",
            "verification",
            "zugang",
            "sign-in",
            "2fa",
        ],
        Base::Personal => &["freund", "familie", "privat", "private", "personal", "persönlich", "friend", "family"],
        Base::Work => &[
            "arbeit",
            "kollege",
            "kollegin",
            "kunde",
            "kundin",
            "büro",
            "projekt",
            "work",
            "business",
            "geschäft",
            "geschäftlich",
            "firma",
            "colleague",
            "client",
            "customer",
            "office",
        ],
        Base::Advertising => &[
            "werbung",
            "angebot",
            "rabatt",
            "gutschein",
            "sale",
            "promotion",
            "deal",
            "marketing",
            "offer",
            "coupon",
            "discount",
            "reklame",
        ],
    }
}

/// A rough stem: folded, common German and English endings off, at least four characters left.
fn stem(word: &str) -> String {
    let word = fold(word).replace('ß', "ss").replace('ä', "a").replace('ö', "o").replace('ü', "u");
    for ending in ["ungen", "ung", "ies", "en", "er", "es", "e", "s", "n"] {
        if let Some(rest) = word.strip_suffix(ending)
            && rest.chars().count() >= 4
        {
            return rest.to_owned();
        }
    }
    word
}

/// The stems of the meaningful words of `text`, each once, in order.
fn stems(text: &str) -> Vec<String> {
    let folded = fold(text);
    let mut out: Vec<String> = Vec::new();
    for (_, word) in words(&folded) {
        if word.chars().count() < 3 || STOPWORDS.contains(&word) || word.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let stem = stem(word);
        if !out.contains(&stem) {
            out.push(stem);
        }
    }
    out
}

/// Stems of `stems` that are (or start or end with) one of the base label's subject words: German
/// compounds (`Handyrechnung`) count.
fn concept_hits(stems: &[String], base: Base) -> Vec<String> {
    let subject: Vec<String> = concept(base).iter().map(|word| stem(word)).collect();
    stems
        .iter()
        .filter(|stem| {
            subject.iter().any(|word| stem == &word || stem.ends_with(word.as_str()) || stem.starts_with(word.as_str()))
        })
        .cloned()
        .collect()
}

/// The labels of `others` that a label called `name` with `description` overlaps with; `others`
/// should not hold the label itself.
pub fn overlaps(name: &str, description: &str, others: &[OverlapLabel<'_>]) -> Vec<Overlap> {
    let named = Base::named(name);
    let name_stems = stems(name);
    let description_stems = stems(description);
    let mut all = name_stems.clone();
    for stem in &description_stems {
        if !all.contains(stem) {
            all.push(stem.clone());
        }
    }
    let mut out = Vec::new();
    for other in others {
        let other_base = other.base.or_else(|| Base::named(other.name));
        let other_name = stems(other.name);
        if (named.is_some() && named == other_base) || (!name_stems.is_empty() && name_stems == other_name) {
            out.push(Overlap { id: other.id, kind: "name", words: name_stems.iter().take(8).cloned().collect() });
            continue;
        }
        if let Some(base) = other_base {
            let in_name = concept_hits(&name_stems, base);
            let in_description = concept_hits(&description_stems, base);
            if !in_name.is_empty() || in_description.len() >= 2 {
                let mut hits = in_name;
                hits.extend(in_description.into_iter().filter(|stem| !name_stems.contains(stem)));
                hits.truncate(8);
                out.push(Overlap { id: other.id, kind: "meaning", words: hits });
                continue;
            }
        }
        let mut theirs = other_name;
        for stem in stems(other.description) {
            if !theirs.contains(&stem) {
                theirs.push(stem);
            }
        }
        let shared: Vec<String> = all.iter().filter(|stem| theirs.contains(stem)).cloned().collect();
        let union = all.len() + theirs.len() - shared.len();
        if shared.len() >= 2 && union > 0 && shared.len() as f64 / union as f64 >= 0.25 {
            out.push(Overlap { id: other.id, kind: "words", words: shared.into_iter().take(8).collect() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label<'a>(id: i64, name: &'a str, description: &'a str, base: Option<Base>) -> OverlapLabel<'a> {
        OverlapLabel { id, name, description, base }
    }

    #[test]
    fn finds_overlaps() {
        let others = [
            label(1, "Rechnung", "Rechnungen, Quittungen", Some(Base::Invoice)),
            label(2, "Coding", "GitHub, CI und Code-Reviews", None),
            label(3, "Werbung", "Angebote und Rabatte", Some(Base::Advertising)),
        ];
        let found = overlaps("Rechnungen", "", &others);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].id, found[0].kind), (1, "name"));
        let found = overlaps("Handyrechnungen", "Mobilfunk", &others);
        assert_eq!((found[0].id, found[0].kind), (1, "meaning"));
        let found = overlaps("Programmieren", "Mails von GitHub zu CI-Läufen und Reviews", &others);
        assert_eq!((found[0].id, found[0].kind), (2, "words"), "{found:?}");
        assert!(overlaps("Reisen", "Flüge, Hotels und Bahntickets", &others).is_empty());
        let found = overlaps("Shopping-Deals", "Gutscheine und Rabattcodes von Läden", &others);
        assert_eq!(found.iter().map(|o| o.id).collect::<Vec<_>>(), [3]);
    }
}
