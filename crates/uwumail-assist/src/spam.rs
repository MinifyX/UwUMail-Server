//! The spam check: facts decide, the model explains.
//!
//! Small models judged mail by how it sounds. They called an ordinary invoice "90 % spam" next to a
//! passed DMARC check and a filter that rated it as wanted mail, and gave reasons that were not in
//! the mail at all. So the verdict is no longer theirs to make.
//!
//! What the server knows by itself — its spam filter's result, SPF, DKIM and DMARC, the reader's
//! history with the sender, and the deterministic phishing checks (`uwumail_smtp::phishing`) — adds
//! up to a score, and the score sets a band of verdicts the facts allow. The model reads the mail
//! and may choose within that band, and it has to say why: every reason cites a fact by its number
//! or quotes the mail. Reasons that cite nothing that is there, or that contradict the facts, are
//! dropped before anybody reads them. The confidence comes from how clear the facts are, nudged by
//! the model, never from the model alone.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;
use uwumail_smtp::phishing::Finding;

use crate::features::{AuthenticationSignals, SpamSignals};
use crate::mail::MailText;

/// The verdicts, from good to bad. `phishing` ranks with `spam` and needs phishing evidence.
pub const VERDICTS: [&str; 4] = ["legitimate", "suspicious", "spam", "phishing"];

fn rank(verdict: &str) -> u8 {
    match verdict {
        "legitimate" => 0,
        "suspicious" => 1,
        _ => 2,
    }
}

/// Whether a fact speaks for or against a mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    Good,
    Bad,
}

/// One fact that went into the score.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    /// A stable code the apps translate, e.g. `DMARC_PASS` or `LOOKALIKE_BRAND_FROM`.
    pub code: String,
    pub tone: Tone,
    /// How much it moved the score; positive is towards spam.
    pub weight: f64,
    /// What exactly was seen, e.g. the lookalike domain. Never mail text beyond a name or a domain.
    pub detail: Option<String>,
    /// Part of the phishing checks.
    pub phishing: bool,
}

/// The range of verdicts the facts allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Band {
    /// The facts clearly speak for the mail.
    Clean,
    LeaningClean,
    /// The facts are mixed or thin; the model's reading decides.
    Unclear,
    LeaningSpam,
    /// The facts clearly speak against it.
    Spam,
}

impl Band {
    fn of(score: f64) -> Band {
        if score <= -2.0 {
            Band::Clean
        } else if score < 1.5 {
            Band::LeaningClean
        } else if score < 4.0 {
            Band::Unclear
        } else if score < 7.0 {
            Band::LeaningSpam
        } else {
            Band::Spam
        }
    }

    /// The lowest and highest rank allowed.
    fn range(self) -> (u8, u8) {
        match self {
            Band::Clean => (0, 0),
            Band::LeaningClean => (0, 1),
            Band::Unclear => (0, 2),
            Band::LeaningSpam => (1, 2),
            Band::Spam => (2, 2),
        }
    }
}

/// What the facts say about a mail.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    /// The sum of the evidence; above 0 leans towards spam.
    pub score: f64,
    pub band: Band,
    pub evidence: Vec<Evidence>,
    /// The verdicts the model may choose from, in order.
    pub allowed: Vec<&'static str>,
    /// The verdict the facts alone give.
    pub default_verdict: &'static str,
}

impl Assessment {
    pub fn has_phishing_evidence(&self) -> bool {
        self.evidence.iter().any(|evidence| evidence.phishing && evidence.tone == Tone::Bad && evidence.weight >= 1.5)
    }
}

/// Phishing rules of the server's spam filter, as they show in `X-Spam-Status`.
const FILTER_PHISHING_RULES: &[&str] = &[
    "LOOKALIKE_LINK",
    "LINK_TO_IP",
    "HTML_ATTACHMENT",
    "MALWARE_LINK",
    "SPAMHAUS_DBL_MALICIOUS",
    "PHISHING_LINK_TEXT",
    "BRAND_LINK_TEXT",
    "FROM_NAME_SPOOFS_ADDRESS",
    "LOOKALIKE_BRAND_FROM",
    "LOOKALIKE_BRAND_LINK",
    "BRAND_IN_FROM_NAME",
    "CREDENTIAL_REQUEST",
];

/// Phrases that ask for money in a way scams do (lower case).
const PAYMENT_CUES: &[&str] = &[
    "gift card",
    "giftcard",
    "geschenkkarte",
    "gutscheinkarte",
    "itunes-karte",
    "google play card",
    "bitcoin",
    "btc wallet",
    "krypto-wallet",
    "western union",
    "moneygram",
    "wire transfer",
    "eilüberweisung",
    "dringende überweisung",
    "überweisung noch heute",
    "transfer the funds",
];

/// Phrases that press for haste (lower case).
const URGENCY_CUES: &[&str] = &[
    "within 24 hours",
    "within 48 hours",
    "immediately",
    "urgent",
    "final notice",
    "last warning",
    "innerhalb von 24 stunden",
    "innerhalb von 48 stunden",
    "umgehend",
    "sofort",
    "dringend",
    "letzte mahnung",
    "letzte warnung",
];

fn passed(result: &Option<String>) -> bool {
    result.as_deref() == Some("pass")
}

fn failed(result: &Option<String>) -> bool {
    matches!(result.as_deref(), Some("fail" | "permerror" | "softfail"))
}

fn add(evidence: &mut Vec<Evidence>, code: &str, weight: f64, detail: Option<String>, phishing: bool) {
    if weight == 0.0 || evidence.iter().any(|known| known.code == code) {
        return;
    }
    let tone = if weight > 0.0 { Tone::Bad } else { Tone::Good };
    evidence.push(Evidence { code: code.to_owned(), tone, weight: (weight * 10.0).round() / 10.0, detail, phishing });
}

/// Whether authentication backs the From domain, as the SMTP checks decide it: DMARC passed, or —
/// for a From domain without a DMARC policy — a DKIM signature or an SPF pass for the From domain,
/// a parent or a subdomain of it. A pass for some other domain the sender owns vouches for nothing,
/// and a DMARC failure is never outweighed (security review 0.22 R2-M1). The AI spam check and the
/// AI labels both ask this.
pub fn authentic(auth: &AuthenticationSignals) -> bool {
    match auth.dmarc.as_deref() {
        Some("pass") => true,
        None | Some("none") => auth.from_domain.as_deref().is_some_and(|from| {
            let related = |domain: &str| uwumail_smtp::related_domains(from, domain);
            (passed(&auth.dkim) && auth.dkim_pass_domains.iter().any(|domain| related(domain)))
                || (passed(&auth.spf) && auth.spf_pass_domain.as_deref().is_some_and(related))
        }),
        Some(_) => false,
    }
}

/// Adds up what the server knows. `phishing` are the findings of `uwumail_smtp::phishing` for this
/// mail, `text` the mail's subject and text for the content cues.
pub fn assess(signals: &SpamSignals, phishing: &[Finding], text: &str) -> Assessment {
    let mut evidence = Vec::new();
    let auth = &signals.authentication;
    let authentic = authentic(auth);

    // The spam filter: its whole verdict in one fact, scaled so that its limit weighs 3.
    if let Some(score) = signals.spam_score {
        let threshold = signals.spam_threshold.filter(|t| *t > 0.0).unwrap_or(5.0);
        let detail = Some(format!("{score:.1}/{threshold:.1}"));
        // Another account's provider may write no verdict of its own, and then the one counted could
        // be the sender's: a good word from a filter is only believed from this server's own.
        let own_filter = signals.sender.is_some();
        if score <= 0.0 {
            if own_filter {
                add(&mut evidence, "FILTER_WANTED", (score * 0.4).clamp(-2.0, -0.5), detail, false);
            }
        } else if score >= threshold {
            add(&mut evidence, "FILTER_OVER_LIMIT", 3.0 + ((score - threshold) * 0.2).min(1.5), detail, false);
        } else {
            add(&mut evidence, "FILTER_SOME_POINTS", 2.0 * score / threshold, detail, false);
        }
    }

    // Authentication: who really sent it.
    if passed(&auth.dmarc) {
        add(&mut evidence, "DMARC_PASS", -1.5, auth.from_domain.clone(), false);
    } else if failed(&auth.dmarc) {
        add(&mut evidence, "DMARC_FAIL", 2.0, auth.from_domain.clone(), false);
    } else if authentic {
        add(&mut evidence, "SPF_DKIM_PASS", -1.0, auth.from_domain.clone(), false);
    } else if failed(&auth.spf) && failed(&auth.dkim) {
        add(&mut evidence, "SPF_DKIM_FAIL", 1.5, auth.from_domain.clone(), false);
    } else if !passed(&auth.spf) && !passed(&auth.dkim) && (auth.spf.is_some() || auth.dkim.is_some()) {
        // Checked, and nothing vouches for the sender. No results at all means the mail did not
        // come from another server (or was fetched from elsewhere): not known, so not held against it.
        add(&mut evidence, "NO_AUTHENTICATION", 0.5, None, false);
    }

    // The reader's history with the address. A From address can be forged, so history only counts
    // in full when authentication backs the address.
    if let Some(sender) = &signals.sender {
        let trust = if authentic { 1.0 } else { 0.5 };
        if sender.written_to >= 1 {
            add(&mut evidence, "WRITTEN_TO", -2.5 * trust, Some(sender.written_to.to_string()), false);
        }
        if sender.in_contacts {
            add(&mut evidence, "IN_CONTACTS", -2.0 * trust, None, false);
        }
        if sender.earlier_messages >= 1 {
            let junk_share = sender.earlier_in_junk as f64 / sender.earlier_messages as f64;
            if junk_share >= 0.5 {
                add(&mut evidence, "EARLIER_IN_JUNK", 2.0, Some(sender.earlier_in_junk.to_string()), false);
            } else if sender.earlier_in_junk == 0 {
                let weight = (sender.earlier_messages as f64 * 0.5).min(1.5) * trust;
                add(&mut evidence, "KNOWN_SENDER", -weight, Some(sender.earlier_messages.to_string()), false);
            }
        } else if sender.written_to == 0 && !sender.in_contacts {
            add(&mut evidence, "FIRST_MAIL", 0.5, None, false);
        }
    }
    if signals.in_junk {
        add(&mut evidence, "IN_JUNK", 0.5, None, false);
    }

    // The phishing checks. The filter may have counted the same trick already; it is still the
    // strongest single fact there is, but not twice at full weight.
    for finding in phishing {
        if finding.points <= 0.0 {
            continue;
        }
        let counted = signals.tests.iter().any(|test| test == finding.rule);
        let weight = f64::from(finding.points) * if counted { 0.5 } else { 1.0 };
        add(&mut evidence, finding.rule, weight, Some(finding.detail.clone()), true);
    }
    for test in &signals.tests {
        if FILTER_PHISHING_RULES.contains(&test.as_str()) && !evidence.iter().any(|known| known.code == *test) {
            add(&mut evidence, test, 1.0, None, true);
        }
    }

    // Content cues, only for mail from somebody the reader does not know.
    let known = signals.sender.as_ref().is_some_and(|sender| sender.written_to >= 1 || sender.in_contacts);
    let lower = text.to_lowercase();
    let lower = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if !known {
        if let Some(cue) = PAYMENT_CUES.iter().find(|cue| lower.contains(*cue)) {
            add(&mut evidence, "PAYMENT_REQUEST", 1.5, Some((*cue).to_owned()), true);
        }
        if let Some(cue) = URGENCY_CUES.iter().find(|cue| lower.contains(*cue)) {
            add(&mut evidence, "URGENCY", 0.5, Some((*cue).to_owned()), false);
        }
    }

    let score = (evidence.iter().map(|evidence| evidence.weight).sum::<f64>() * 10.0).round() / 10.0;
    let band = Band::of(score);
    let mut assessment = Assessment { score, band, evidence, allowed: Vec::new(), default_verdict: "suspicious" };
    let phishing_possible = assessment.has_phishing_evidence();
    let (low, mut high) = band.range();
    // A clean-looking mail that still shows a phishing trick may at least be called suspicious, and
    // so may another account's mail, whose facts this server cannot verify: the model can always
    // raise a concern there (client review C-1).
    if high == 0 && (phishing_possible || signals.sender.is_none()) {
        high = 1;
    }
    assessment.allowed = VERDICTS
        .iter()
        .copied()
        .filter(|verdict| (low..=high).contains(&rank(verdict)) && (*verdict != "phishing" || phishing_possible))
        .collect();
    assessment.default_verdict = match band {
        Band::Clean | Band::LeaningClean => "legitimate",
        Band::Unclear => "suspicious",
        Band::LeaningSpam | Band::Spam if phishing_possible => "phishing",
        Band::LeaningSpam | Band::Spam => "spam",
    };
    assessment
}

/// The model's verdict held to the band, and the confidence the facts support. The third value is
/// the model's own verdict when it had to be moved.
pub fn settle(assessment: &Assessment, verdict: &str, model_confidence: f64) -> (String, f64, Option<String>) {
    let allowed = &assessment.allowed;
    let chosen = if allowed.contains(&verdict) {
        verdict
    } else if verdict == "phishing" && allowed.contains(&"spam") {
        "spam"
    } else {
        // The allowed verdict nearest to what the model said.
        let wanted = rank(verdict);
        allowed
            .iter()
            .copied()
            .min_by_key(|candidate| (rank(candidate).abs_diff(wanted), rank(candidate) != 1))
            .unwrap_or(assessment.default_verdict)
    };
    let moved = (chosen != verdict).then(|| verdict.to_owned());

    // How likely spam is by the facts, from 0 to 1.
    let spam_chance = 1.0 / (1.0 + (-(assessment.score - 2.75) / 1.5).exp());
    let by_facts = match rank(chosen) {
        0 => 1.0 - spam_chance,
        2 => spam_chance,
        // "Unclear" is most certain when the facts are evenly mixed.
        _ => 0.4 + 0.4 * (1.0 - (2.0 * spam_chance - 1.0).abs()),
    };
    let model = model_confidence.clamp(0.0, 1.0);
    let mut confidence = 0.7 * by_facts + 0.3 * model;
    if moved.is_some() {
        confidence = confidence.min(0.6);
    }
    (chosen.to_owned(), (confidence.clamp(0.3, 0.97) * 100.0).round() / 100.0, moved)
}

/// One numbered fact the model may cite.
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub id: String,
    pub text: String,
}

/// A reason as the model gave it, and what it cites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    pub text: String,
    /// The quote from the mail it rests on, as it stands in the mail.
    pub quote: Option<String>,
    /// The fact it rests on, by its number (`F3`).
    pub fact: Option<String>,
}

/// What the claim check knows about the mail beyond its text.
#[derive(Debug, Clone, Default)]
pub struct MailShape {
    pub has_links: bool,
    /// `None` when unknown (mail of another account).
    pub attachments: Option<usize>,
}

/// The reasons out of the model's answer: objects `{text, evidence}`; plain strings (older or
/// careless answers) come without evidence and are dropped by [`verify`].
pub fn parse_reasons(answer: &Value, max: usize, max_chars: usize) -> Vec<(String, String)> {
    answer
        .get("reasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(max * 2)
        .filter_map(|entry| {
            let (text, evidence) = match entry {
                Value::String(text) => (text.clone(), String::new()),
                Value::Object(object) => (
                    object.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    object.get("evidence").and_then(Value::as_str).unwrap_or_default().to_owned(),
                ),
                _ => return None,
            };
            let text = clip(&text.replace(['\n', '\r'], " "), max_chars);
            (!text.is_empty()).then(|| (text, clip(&evidence.replace(['\n', '\r'], " "), max_chars)))
        })
        .collect()
}

fn clip(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect::<String>().trim().to_owned()
}

/// Lower case, quotes and punctuation at the ends gone, whitespace collapsed: how a quote is
/// compared with the mail.
fn normalized(text: &str) -> String {
    let lower = text.to_lowercase();
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_matches(|c: char| c.is_whitespace() || "\"'„“”‚‘’«»…:;,.!?()[]".contains(c))
        .replace(['„', '“', '”', '‚', '‘', '’', '«', '»'], "\"")
}

/// Keeps the reasons that cite something real and contradict no fact. Answers the reasons kept and
/// how many were dropped.
pub fn verify(
    reasons: Vec<(String, String)>,
    mail: &MailText,
    shape: &MailShape,
    facts: &[Fact],
    signals: &SpamSignals,
    max: usize,
) -> (Vec<Reason>, usize) {
    let haystack = normalized(&format!(
        "{}\n{}\n{}\n{}\n{}",
        mail.subject,
        crate::mail::addresses(&mail.from),
        crate::mail::addresses(&mail.to),
        mail.text,
        mail.links.join("\n")
    ))
    .replace(['„', '“', '”'], "\"");
    let known_text = normalized(&facts.iter().map(|fact| fact.text.as_str()).collect::<Vec<_>>().join("\n"));
    let mail_numbers = digit_runs(&haystack);
    let mut kept: Vec<Reason> = Vec::new();
    let mut dropped = 0;
    for (text, evidence) in reasons {
        if kept.len() >= max {
            break;
        }
        // A fact counts only when the evidence field is nothing but its number and the reason is
        // about what that fact says; a fact number written into the reason's own text proves
        // nothing (security review 0.22 SPAM-6).
        let cited = cited_fact(&evidence, facts).filter(|fact| relates(&text, fact));
        let quote = if cited.is_none() { quoted(&evidence, &haystack) } else { None };
        let grounded = cited.map(|fact| fact.id.clone());
        if (grounded.is_none() && quote.is_none())
            || contradicts(&text, mail, shape, signals)
            || brings_contacts(&text, &haystack, &known_text, &mail_numbers)
            || kept.iter().any(|known| known.text == text)
        {
            dropped += 1;
            continue;
        }
        kept.push(Reason { text, quote, fact: grounded });
    }
    (kept, dropped)
}

/// The fact an evidence field names, when the field is only that: `F3`, `[F3]`, `Fakt F3`, `fact
/// F3.` — and that fact exists.
fn cited_fact<'a>(evidence: &str, facts: &'a [Fact]) -> Option<&'a Fact> {
    let lower = evidence.trim().to_lowercase();
    let lower = lower.trim_matches(|c: char| c.is_whitespace() || "[]().:;,\"'".contains(c));
    let lower = ["fakt", "fact"].iter().find_map(|word| lower.strip_prefix(word)).unwrap_or(lower).trim_start();
    let digits = lower.strip_prefix('f')?.trim_end_matches(|c: char| ".:;,)]".contains(c));
    if digits.is_empty() || digits.len() > 3 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let id = format!("F{digits}");
    facts.iter().find(|fact| fact.id == id)
}

/// Word stems by topic, English and German: a reason and a fact that both touch one topic are
/// about the same thing.
const TOPICS: &[&[&str]] = &[
    &[
        "spf",
        "dkim",
        "dmarc",
        "authent",
        "verif",
        "signed",
        "signiert",
        "signatur",
        "genuine",
        "echt",
        "spoof",
        "forged",
        "fälsch",
        "gefälscht",
        "really comes",
        "wirklich",
        "domain",
    ],
    &["filter", "score", "punkte", "points", "bayes", "rule", "regel", "limit", "grenze"],
    &[
        "earlier",
        "früher",
        "bisher",
        "before",
        "zuvor",
        "wrote",
        "written",
        "geschrieben",
        "sent to",
        "contact",
        "kontakt",
        "address book",
        "adressbuch",
        "known",
        "bekannt",
        "unknown",
        "unbekannt",
        "first",
        "erste",
        "erstmals",
        "history",
        "verlauf",
    ],
    &["junk", "spam folder", "spam-ordner", "spamordner"],
    &[
        "link",
        "url",
        "address",
        "adresse",
        "site",
        "seite",
        "domain",
        "lookalike",
        "imitat",
        "ähnlich",
        "looks like",
        "brand",
        "marke",
        "display name",
        "anzeigename",
        "reply",
        "antwort",
    ],
    &["attachment", "anhang", "anhänge", "file", "datei"],
    &["payment", "zahlung", "bezahl", "geld", "money", "gift card", "gutschein", "urgent", "dringend", "sofort"],
    &["login", "log in", "sign in", "anmeld", "password", "passwort", "konto", "account", "verify", "bestätig"],
];

/// Whether a reason is about what its cited fact says: a topic or a word they share.
fn relates(reason: &str, fact: &Fact) -> bool {
    let reason = reason.to_lowercase();
    let fact_text = fact.text.to_lowercase();
    if TOPICS.iter().any(|stems| {
        stems.iter().any(|stem| reason.contains(stem)) && stems.iter().any(|stem| fact_text.contains(stem))
    }) {
        return true;
    }
    let words = |text: &str| -> HashSet<String> {
        text.split(|c: char| !c.is_alphanumeric()).filter(|word| word.chars().count() >= 5).map(str::to_owned).collect()
    };
    !words(&reason).is_disjoint(&words(&fact_text))
}

/// Whether a reason brings a web address, mail address or phone number that neither the mail nor
/// the facts contain: the model's own invention, or one a prompt injection put there — shown as a
/// checked reason it would send the reader somewhere (security review 0.22 SPAM-6).
fn brings_contacts(text: &str, mail: &str, facts: &str, mail_numbers: &[String]) -> bool {
    let address = text.split_whitespace().map(|word| normalized(word.trim_matches(['<', '>']))).any(|word| {
        let looks = word.contains("://")
            || word.starts_with("www.")
            || (word.contains('@') && word.contains('.'))
            || looks_like_host(&word);
        looks && !mail.contains(&word) && !facts.contains(&word)
    });
    if address {
        return true;
    }
    // Phone-like numbers of seven digits or more, each of which has to stand within one number of
    // the mail: the end of one number and the start of the next run together are no number of it
    // (security review 0.22 R2, I-2).
    digit_runs(text)
        .iter()
        .filter(|number| number.len() >= 7)
        .any(|number| !mail_numbers.iter().any(|known| known.contains(number.as_str())))
}

/// The numbers of a text as runs of digits, joined across the usual phone separators (`+49 (30)
/// 123-45 67` is one).
fn digit_runs(text: &str) -> Vec<String> {
    let mut run = String::new();
    let mut numbers = Vec::new();
    for c in text.chars().chain(std::iter::once('x')) {
        if c.is_ascii_digit() {
            run.push(c);
        } else if !(c == ' ' || c == '+' || c == '-' || c == '/' || c == '(' || c == ')') && !run.is_empty() {
            numbers.push(std::mem::take(&mut run));
        }
    }
    numbers
}

/// A word with a dot that reads like a host name (`hotline.example`), not like an abbreviation
/// (`z.b.`) or a number (`12.00`).
fn looks_like_host(word: &str) -> bool {
    let labels: Vec<&str> = word.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| !label.is_empty() && label.chars().all(|c| c.is_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|end| end.chars().count() >= 2 && end.chars().all(char::is_alphabetic))
        && labels.iter().map(|label| label.chars().count()).max().unwrap_or(0) >= 3
}

/// The quote when it stands in the mail: at least four characters, compared without case and
/// with whitespace collapsed.
fn quoted(evidence: &str, haystack: &str) -> Option<String> {
    let wanted = normalized(evidence);
    (wanted.chars().count() >= 4 && haystack.contains(&wanted)).then(|| evidence.trim().to_owned())
}

/// Calling a sender verified or genuine (security review 0.22 R2 I-2).
const AUTH_SUCCESS_PHRASES: &[&str] = &[
    "verified sender",
    "sender is verified",
    "verifizierter absender",
    "verifizierten absender",
    "absender ist verifiziert",
    "authenticated sender",
    "is authenticated",
    "authentifizierter absender",
    "ist authentifiziert",
    "passed authentication",
    "authentication passed",
    "spf pass",
    "dkim pass",
    "dmarc pass",
    "genuine sender",
    "echter absender",
    "really comes from",
    "kommt wirklich von",
    "can trust",
    "trustworthy",
    "vertrauenswürdig",
];

/// Words that turn the phrase after them around: "not trustworthy", "kein verifizierter Absender",
/// "doesn't seem trustworthy". Contractions also without their apostrophe, as models write them.
const NEGATIONS: &[&str] = &[
    "not",
    "no",
    "never",
    "nothing",
    "nobody",
    "neither",
    "nor",
    "without",
    "hardly",
    "barely",
    "less",
    "unlikely",
    "cannot",
    "can't",
    "cant",
    "isn't",
    "isnt",
    "aren't",
    "arent",
    "wasn't",
    "wasnt",
    "weren't",
    "werent",
    "doesn't",
    "doesnt",
    "don't",
    "dont",
    "didn't",
    "didnt",
    "won't",
    "wont",
    "wouldn't",
    "wouldnt",
    "shouldn't",
    "shouldnt",
    "couldn't",
    "couldnt",
    "mustn't",
    "mustnt",
    "hasn't",
    "hasnt",
    "haven't",
    "havent",
    "hadn't",
    "hadnt",
    "ain't",
    "nicht",
    "kein",
    "keine",
    "keinen",
    "keiner",
    "keinem",
    "keines",
    "nie",
    "niemals",
    "nichts",
    "ohne",
    "weder",
    "kaum",
    "unwahrscheinlich",
];

/// Words that may stand between a negation and the phrase it turns around: "is not a verified
/// sender", "isn't really trustworthy", "unlikely to be genuine", "ist kein wirklich echter".
const BETWEEN: &[&str] = &[
    "a",
    "an",
    "the",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "seem",
    "seems",
    "look",
    "looks",
    "appear",
    "appears",
    "to",
    "very",
    "really",
    "truly",
    "fully",
    "entirely",
    "completely",
    "particularly",
    "so",
    "ein",
    "eine",
    "einen",
    "einem",
    "einer",
    "der",
    "die",
    "das",
    "ist",
    "sind",
    "war",
    "sein",
    "scheint",
    "wirkt",
    "sehr",
    "wirklich",
    "ganz",
    "völlig",
    "besonders",
];

/// What ends a clause: a negation before it belongs to another statement ("No red flags: verified
/// sender", "No doubt, a trustworthy sender").
const CLAUSE_ENDS: &[char] = &['.', ',', ':', ';', '!', '?', '—', '–', '(', ')', '"', '\n'];

/// Whether `lower` uses one of `phrases` as a claim: as a word of its own (not inside "unverified"
/// or "untrustworthy") and not turned around by a negation of its own clause (security review 0.22
/// R3-L3, R4-L1). The negation has to come right before the phrase, with at most three of the
/// [`BETWEEN`] words in between ("is not a verified sender"), so a negation of another statement
/// never cancels praise: not across punctuation ("No red flags: verified sender"), not before
/// another word ("not only trustworthy", "nicht nur ein verifizierter Absender", "no doubt
/// trustworthy", "ohne Zweifel echter Absender", "never seen a more trustworthy mail").
fn claims(lower: &str, phrases: &[&str]) -> bool {
    let lower = lower.replace(['\u{2019}', '\u{2018}', '\u{02bc}', '`', '\u{00b4}'], "'");
    phrases.iter().any(|phrase| {
        lower.match_indices(phrase).any(|(at, _)| {
            let before = &lower[..at];
            if before.chars().next_back().is_some_and(char::is_alphanumeric) {
                return false;
            }
            let clause = before.rsplit(CLAUSE_ENDS).next().unwrap_or_default();
            // A dash between words ends a clause too, one inside a word ("e-mail") does not.
            let clause = clause.rsplit(" - ").next().unwrap_or_default();
            let mut words =
                clause.split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter(|word| !word.is_empty()).rev();
            let is_negated = words
                .by_ref()
                .take(4)
                .find(|word| !BETWEEN.contains(word))
                .is_some_and(|word| NEGATIONS.contains(&word.trim_matches('\'')));
            !is_negated
        })
    })
}

/// Claims a reason may make only when the facts back them.
fn contradicts(text: &str, mail: &MailText, shape: &MailShape, signals: &SpamSignals) -> bool {
    let lower = text.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    let denies = says(&[
        "no link",
        "keine link",
        "keinen link",
        "ohne link",
        "no attachment",
        "kein anhang",
        "keine anhänge",
        "keinen anhang",
        "ohne anhang",
    ]);

    if !denies
        && !shape.has_links
        && mail.links.is_empty()
        && !mail.text.contains("http")
        && says(&["link", "url", "klick", "click"])
    {
        return true;
    }
    if !denies && shape.attachments == Some(0) && says(&["attachment", "anhang", "anhänge", "angehängt", "attached"])
    {
        return true;
    }
    let auth = &signals.authentication;
    let claims_auth_failure = says(&[
        "spf fail",
        "dkim fail",
        "dmarc fail",
        "spf-prüfung fehlgeschlagen",
        "dkim-prüfung fehlgeschlagen",
        "dmarc-prüfung fehlgeschlagen",
        "not authenticated",
        "nicht authentifiziert",
        "authentication failed",
        "authentifizierung fehlgeschlagen",
        "forged sender",
        "gefälschter absender",
        "spoofed",
    ]);
    if claims_auth_failure && passed(&auth.dmarc) {
        return true;
    }
    // Calling a sender verified or genuine needs authentication that backs its From domain: the
    // topic match alone would let "verified sender, you can trust it" lean on a failed DMARC fact
    // (security review 0.22 R2, I-2).
    let claims_auth_success = claims(&lower, AUTH_SUCCESS_PHRASES);
    if claims_auth_success && !claims_auth_failure && !authentic(auth) {
        return true;
    }
    if let Some(sender) = &signals.sender {
        let known = sender.earlier_messages >= 1 || sender.written_to >= 1 || sender.in_contacts;
        let claims_unknown = says(&[
            "unknown sender",
            "unbekannter absender",
            "unbekannten absender",
            "first mail",
            "first time",
            "erste mail",
            "erstmals",
            "zum ersten mal",
            "never written",
            "noch nie",
            "not in your contacts",
            "nicht in deinen kontakten",
            "nicht im adressbuch",
        ]);
        if claims_unknown && known {
            return true;
        }
        let claims_known = says(&[
            "known sender",
            "bekannter absender",
            "bekannten absender",
            "in your contacts",
            "in deinen kontakten",
            "im adressbuch",
            "wrote to before",
            "schon geschrieben",
        ]) && !claims_unknown;
        if claims_known && !known {
            return true;
        }
    }
    if let (Some(score), Some(threshold)) = (signals.spam_score, signals.spam_threshold) {
        let mentions_filter = says(&["filter", "spam score", "spam-score", "punkte", "points"]);
        let says_high = says(&[
            "high",
            "hoch",
            "hohe",
            "viele punkte",
            "many points",
            "over the limit",
            "über dem grenzwert",
            "über der grenze",
            "überschritten",
        ]);
        let says_low = says(&[
            "low",
            "niedrig",
            "wenige punkte",
            "few points",
            "under the limit",
            "unter dem grenzwert",
            "unter der grenze",
            "as wanted",
            "als erwünscht",
        ]);
        if mentions_filter && says_high && !says_low && score < threshold {
            return true;
        }
        if mentions_filter && says_low && !says_high && score >= threshold {
            return true;
        }
    }
    false
}

/// The facts as numbered lines for the prompt, and the list the reasons are checked against.
pub fn facts(
    signals: &SpamSignals,
    assessment: &Assessment,
    describe: impl Fn(&str) -> Option<&'static str>,
) -> Vec<Fact> {
    let mut facts: Vec<String> = Vec::new();
    let auth = &signals.authentication;
    let or_none = |value: &Option<String>| value.clone().unwrap_or_else(|| "not checked".into());
    if signals.sender.is_none() {
        facts.push(
            "The mail is from another account of the reader: the checks below are what that account's mail provider \
wrote into the mail, not checks of this server."
                .into(),
        );
    }
    facts.push(format!(
        "SPF: {}; DKIM: {}; DMARC: {}; domain of the From address: {}",
        or_none(&auth.spf),
        or_none(&auth.dkim),
        or_none(&auth.dmarc),
        auth.from_domain.clone().unwrap_or_else(|| "none".into())
    ));
    if passed(&auth.dmarc) {
        let domain = auth.from_domain.as_deref().unwrap_or("the From address");
        facts.push(format!("DMARC passed for {domain}: the mail really comes from the domain in its From address."));
    }
    match (signals.spam_score, signals.spam_threshold) {
        (Some(score), Some(threshold)) => facts.push(format!(
            "Spam filter: {score:.1} points, Junk from {threshold:.1}; {} (fewer points mean more likely wanted \
mail; 0 or less means the filter rates it as wanted)",
            if score >= threshold { "this mail is over the limit" } else { "this mail is under the limit" }
        )),
        (Some(score), None) => facts.push(format!("Spam filter: {score:.1} points")),
        _ => facts.push("Spam filter: did not look at this mail".into()),
    }
    for test in &signals.tests {
        match describe(test) {
            Some(meaning) => facts.push(format!("Spam filter rule {test}: {meaning}")),
            None => facts.push(format!("Spam filter rule {test}")),
        }
    }
    facts.push(format!("In the Junk folder now: {}", if signals.in_junk { "yes" } else { "no" }));
    match &signals.sender {
        Some(sender) => facts.push(format!(
            "Earlier mails from this address: {} ({} of them in Junk); mails the reader sent to it: {}; in the reader's \
address book: {}",
            sender.earlier_messages,
            sender.earlier_in_junk,
            sender.written_to,
            if sender.in_contacts { "yes" } else { "no" }
        )),
        None => facts.push("Earlier mails from this address: not known".into()),
    }
    for evidence in assessment.evidence.iter().filter(|evidence| evidence.phishing) {
        let meaning = describe(&evidence.code).unwrap_or("a phishing check found something");
        match &evidence.detail {
            Some(detail) => facts.push(format!("Phishing check {}: {meaning} ({detail})", evidence.code)),
            None => facts.push(format!("Phishing check {}: {meaning}", evidence.code)),
        }
    }
    let towards = if assessment.has_phishing_evidence() { "spam or phishing" } else { "spam" };
    let lean = match assessment.band {
        Band::Clean => "they clearly speak for the mail".to_owned(),
        Band::LeaningClean => "they lean towards a normal mail".to_owned(),
        Band::Unclear => "they are mixed, so the mail itself decides".to_owned(),
        Band::LeaningSpam => format!(
            "they point towards {towards}; \"suspicious\" fits only when the mail itself clearly speaks against them"
        ),
        Band::Spam => format!("they clearly speak against the mail: {towards}"),
    };
    facts.push(format!("The server's weighing of all this: {:+.1} points, {lean}", assessment.score));
    facts.into_iter().enumerate().map(|(index, text)| Fact { id: format!("F{}", index + 1), text }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::SenderSignals;

    fn invoice() -> SpamSignals {
        SpamSignals {
            authentication: AuthenticationSignals {
                spf: Some("pass".into()),
                dkim: Some("pass".into()),
                dmarc: Some("pass".into()),
                from_domain: Some("hoster.example".into()),
                ..AuthenticationSignals::default()
            },
            spam_score: Some(-5.5),
            spam_threshold: Some(5.0),
            tests: vec!["BAYES_HAM".into(), "KNOWN_GOOD_SENDER".into()],
            in_junk: false,
            sender: Some(SenderSignals {
                address: Some("billing@hoster.example".into()),
                earlier_messages: 12,
                ..SenderSignals::default()
            }),
        }
    }

    /// Client review C-1 on the server: another account's mail gets no good word from a filter
    /// verdict this server cannot verify, and the model may always call it suspicious.
    #[test]
    fn a_foreign_filter_cannot_vouch_for_a_mail() {
        let foreign = SpamSignals { sender: None, spam_score: Some(-50.0), ..invoice() };
        let assessment = assess(&foreign, &[], "");
        assert!(!assessment.evidence.iter().any(|evidence| evidence.code == "FILTER_WANTED"));
        assert!(assessment.allowed.contains(&"suspicious"), "{:?}", assessment.allowed);
        // Own mail keeps both: the verdict is this server's.
        let own = assess(&invoice(), &[], "");
        assert!(own.evidence.iter().any(|evidence| evidence.code == "FILTER_WANTED"));
    }

    fn stranger() -> SpamSignals {
        SpamSignals {
            authentication: AuthenticationSignals {
                spf: Some("pass".into()),
                dkim: Some("pass".into()),
                dmarc: Some("pass".into()),
                from_domain: Some("konto-hilfe.example".into()),
                ..AuthenticationSignals::default()
            },
            spam_score: Some(3.0),
            spam_threshold: Some(5.0),
            tests: vec!["BRAND_IN_FROM_NAME".into()],
            in_junk: false,
            sender: Some(SenderSignals::default()),
        }
    }

    fn finding(rule: &'static str, points: f32) -> Finding {
        Finding { rule, points, detail: "x".into() }
    }

    #[test]
    fn a_known_authenticated_invoice_can_only_be_legitimate() {
        let assessment = assess(&invoice(), &[], "Rechnung, dankend erhalten");
        assert_eq!(assessment.band, Band::Clean, "{assessment:?}");
        assert_eq!(assessment.allowed, ["legitimate"]);
        let (verdict, confidence, moved) = settle(&assessment, "spam", 0.9);
        assert_eq!((verdict.as_str(), moved.as_deref()), ("legitimate", Some("spam")));
        assert!(confidence <= 0.6, "a moved verdict is never sure: {confidence}");
        let (verdict, confidence, moved) = settle(&assessment, "legitimate", 0.8);
        assert_eq!((verdict.as_str(), moved), ("legitimate", None));
        assert!(confidence >= 0.9, "{confidence}");
    }

    #[test]
    fn a_brand_imitation_is_phishing_whatever_the_model_says() {
        let findings = [
            finding("LOOKALIKE_BRAND_FROM", 4.0),
            finding("CREDENTIAL_REQUEST", 1.5),
            finding("BRAND_IN_FROM_NAME", 2.5),
        ];
        let assessment = assess(&stranger(), &findings, "Ihr Konto wurde gesperrt. Bitte sofort bestätigen.");
        assert!(matches!(assessment.band, Band::LeaningSpam | Band::Spam), "{assessment:?}");
        assert_eq!(assessment.default_verdict, "phishing");
        assert!(!assessment.allowed.contains(&"legitimate"));
        let (verdict, _, moved) = settle(&assessment, "legitimate", 0.9);
        assert_ne!(verdict, "legitimate");
        assert_eq!(moved.as_deref(), Some("legitimate"));
        let (verdict, confidence, _) = settle(&assessment, "phishing", 0.8);
        assert_eq!(verdict, "phishing");
        assert!(confidence >= 0.8, "{confidence}");
    }

    #[test]
    fn phishing_needs_phishing_evidence() {
        let mut signals = stranger();
        signals.tests.clear();
        signals.spam_score = Some(9.0);
        let assessment = assess(&signals, &[], "Cheap pills");
        assert!(!assessment.allowed.contains(&"phishing"));
        assert_eq!(settle(&assessment, "phishing", 0.9).0, "spam");
    }

    #[test]
    fn history_counts_less_when_nothing_backs_the_address() {
        let mut forged = invoice();
        forged.authentication.dmarc = Some("fail".into());
        forged.authentication.spf = Some("fail".into());
        let honest = assess(&invoice(), &[], "");
        let forged = assess(&forged, &[], "");
        assert!(forged.score > honest.score + 3.0, "{} vs {}", forged.score, honest.score);
        assert_ne!(forged.band, Band::Clean);
    }

    #[test]
    fn thin_facts_leave_the_model_free() {
        let signals = SpamSignals { sender: Some(SenderSignals::default()), ..SpamSignals::default() };
        let assessment = assess(&signals, &[], "Hallo, kurze Frage zu deinem Angebot.");
        assert_eq!(assessment.band, Band::LeaningClean, "{assessment:?}");
        assert_eq!(assessment.allowed, ["legitimate", "suspicious"]);
    }

    #[test]
    fn missing_authentication_results_are_unknown_not_bad() {
        // Fetched or imported mail of a stranger: no results, a few filter points.
        let fetched = SpamSignals {
            spam_score: Some(1.5),
            spam_threshold: Some(5.0),
            sender: Some(SenderSignals::default()),
            ..SpamSignals::default()
        };
        let assessment = assess(&fetched, &[], "");
        assert!(!assessment.evidence.iter().any(|e| e.code == "NO_AUTHENTICATION"), "{assessment:?}");
        assert_eq!(assessment.band, Band::LeaningClean, "{assessment:?}");
        // Checked, with nothing passing, it counts.
        let mut unchecked = fetched;
        unchecked.authentication.spf = Some("none".into());
        unchecked.authentication.dkim = Some("none".into());
        assert!(assess(&unchecked, &[], "").evidence.iter().any(|e| e.code == "NO_AUTHENTICATION"));
    }

    fn mail() -> MailText {
        MailText {
            subject: "Ihre Rechnung 4711".into(),
            text: "Vielen Dank, wir haben Ihre Zahlung dankend erhalten.\nBetrag: 12,00 EUR".into(),
            ..MailText::default()
        }
    }

    #[test]
    fn reasons_must_cite_something_real() {
        let signals = invoice();
        let assessment = assess(&signals, &[], "");
        let facts = facts(&signals, &assessment, |_| None);
        let reasons = vec![
            ("DMARC passed, so it really comes from the hoster.".to_owned(), "F2".to_owned()),
            ("It confirms a payment that was made.".to_owned(), "„Zahlung dankend erhalten“".to_owned()),
            ("It demands payment within 24 hours.".to_owned(), "zahlen Sie innerhalb von 24 Stunden".to_owned()),
            ("It contains a suspicious link.".to_owned(), "Betrag: 12,00 EUR".to_owned()),
            ("Unknown sender.".to_owned(), "F1".to_owned()),
            ("A fact that does not exist.".to_owned(), "F99".to_owned()),
            ("Nothing cited.".to_owned(), String::new()),
        ];
        let (kept, dropped) = verify(reasons, &mail(), &MailShape::default(), &facts, &signals, 6);
        let texts: Vec<&str> = kept.iter().map(|reason| reason.text.as_str()).collect();
        assert_eq!(
            texts,
            ["DMARC passed, so it really comes from the hoster.", "It confirms a payment that was made."]
        );
        assert_eq!(dropped, 5);
        assert_eq!(kept[0].fact.as_deref(), Some("F2"));
        assert_eq!(kept[1].quote.as_deref(), Some("„Zahlung dankend erhalten“"));
    }

    #[test]
    fn claims_against_the_facts_are_dropped() {
        let signals = invoice();
        let shape = MailShape { has_links: false, attachments: Some(0) };
        for claim in [
            "The SPF fail shows a forged sender.",
            "Der Spamfilter hat hohe Punkte vergeben.",
            "Ein verdächtiger Anhang ist angehängt.",
            "Der Absender schreibt zum ersten Mal.",
        ] {
            let (kept, _) = verify(
                vec![(claim.to_owned(), "F1".to_owned())],
                &mail(),
                &shape,
                &[Fact { id: "F1".into(), text: String::new() }],
                &signals,
                6,
            );
            assert!(kept.is_empty(), "{claim}");
        }
        let (kept, _) = verify(
            vec![("Es hat keinen Anhang und keine Links.".to_owned(), "F1".to_owned())],
            &mail(),
            &shape,
            &[Fact { id: "F1".into(), text: "Links: none; attachments: none".into() }],
            &signals,
            6,
        );
        assert_eq!(kept.len(), 1, "saying there is none is fine");
    }

    /// Security review 0.22 SPAM-6: a fact number in the reason's text, a citation of an unrelated
    /// fact, or a phone number or address the mail does not contain is no grounding.
    #[test]
    fn citations_must_be_structured_and_about_the_fact() {
        let signals = invoice();
        let assessment = assess(&signals, &[], "");
        let facts = facts(&signals, &assessment, |_| None);
        let dmarc = facts.iter().find(|fact| fact.text.starts_with("DMARC passed")).unwrap().id.clone();
        let filter = facts.iter().find(|fact| fact.text.starts_with("Spam filter")).unwrap().id.clone();
        let reasons = vec![
            // The id only in the text, with a free-form evidence: not a citation.
            (format!("Verified sender ({dmarc}), safe to reply."), "trust me".to_owned()),
            // The evidence is prose that happens to contain an id.
            ("Verified sender.".to_owned(), format!("see {dmarc} and the hotline")),
            // A citation of a fact about something else.
            ("The invoice amount is correct.".to_owned(), filter.clone()),
            // A related citation that brings a phone number and a site the mail never had.
            (
                "DMARC passed; call the hotline at +49 30 1234567 or visit hotline.example".to_owned(),
                format!("[{dmarc}]"),
            ),
            // Fine: structured, related.
            ("DMARC vouches for the sender's domain.".to_owned(), format!("Fakt {dmarc}.")),
            ("The spam filter gave it few points.".to_owned(), filter.clone()),
        ];
        let (kept, dropped) = verify(reasons, &mail(), &MailShape::default(), &facts, &signals, 6);
        let texts: Vec<&str> = kept.iter().map(|reason| reason.text.as_str()).collect();
        assert_eq!(texts, ["DMARC vouches for the sender's domain.", "The spam filter gave it few points."]);
        assert_eq!(dropped, 4);
        assert_eq!(kept[0].fact.as_deref(), Some(dmarc.as_str()));

        // A number or address that does stand in the mail may be named.
        let mut with_number = mail();
        with_number.text.push_str("\nRückfragen: 030 1234567");
        let (kept, _) = verify(
            vec![("It names the number 030 1234567 for questions.".to_owned(), "Rückfragen: 030 1234567".to_owned())],
            &with_number,
            &MailShape::default(),
            &facts,
            &signals,
            6,
        );
        assert_eq!(kept.len(), 1);

        // The end of one number and the start of the next are no number of the mail (R2, I-2).
        let mut two_numbers = mail();
        two_numbers.text.push_str("\nKunde 4711, Rechnung 2026-0815");
        let (kept, _) = verify(
            vec![("Call 4711 2026 now.".to_owned(), "Kunde 4711, Rechnung".to_owned())],
            &two_numbers,
            &MailShape::default(),
            &facts,
            &signals,
            6,
        );
        assert!(kept.is_empty(), "{kept:?}");
    }

    /// Security review 0.22 R2, I-2: calling a sender verified needs authentication that backs it,
    /// whatever fact the reason cites.
    #[test]
    fn a_verified_sender_needs_authentication_behind_it() {
        let mut signals = invoice();
        signals.authentication.dmarc = Some("fail".into());
        let assessment = assess(&signals, &[], "");
        let facts = facts(&signals, &assessment, |_| None);
        let dmarc = facts.iter().find(|fact| fact.text.contains("DMARC")).unwrap().id.clone();
        for claim in ["Verified sender, you can trust it.", "Der Absender ist vertrauenswürdig und verifiziert."] {
            let (kept, _) =
                verify(vec![(claim.to_owned(), dmarc.clone())], &mail(), &MailShape::default(), &facts, &signals, 6);
            assert!(kept.is_empty(), "{claim}");
        }
        // Warnings that negate the praise stay (security review 0.22 R3-L3).
        for warning in [
            "The sender is unverified and not trustworthy.",
            "Unverified sender: the domain is new.",
            "DMARC failed, so this sender is untrustworthy.",
            "The sender is not verified.",
            "DMARC failed: you cannot trust this sender.",
            "Kein verifizierter Absender, die Domain ist neu.",
            "DMARC fehlgeschlagen: der Absender ist nicht vertrauenswürdig.",
            "Absender unbestätigt und unverifiziert.",
            "DMARC fehlgeschlagen, der Absender ist niemals vertrauenswürdig.",
        ] {
            let (kept, _) =
                verify(vec![(warning.to_owned(), dmarc.clone())], &mail(), &MailShape::default(), &facts, &signals, 6);
            assert_eq!(kept.len(), 1, "{warning}");
        }
        // Contractions and curly apostrophes negate too (R4 I-1).
        for warning in [
            "The sender isn\u{2019}t trustworthy.",
            "The sender doesn't seem trustworthy at all.",
            "The sender doesn\u{2019}t really seem trustworthy.",
            "This wouldn't be a genuine sender.",
            "These aren't authenticated sender details.",
            "The sender is unlikely to be trustworthy.",
            "Der Absender ist kein wirklich echter Absender.",
            "The sender is not a verified sender, the domain is new.",
        ] {
            assert!(!claims(&warning.to_lowercase(), AUTH_SUCCESS_PHRASES), "{warning}");
            // Tied to the DMARC fact without saying it failed, so only the negation keeps it.
            let reason = format!("DMARC: {warning}");
            let (kept, _) =
                verify(vec![(reason.clone(), dmarc.clone())], &mail(), &MailShape::default(), &facts, &signals, 6);
            assert_eq!(kept.len(), 1, "{reason}");
        }
        // Praise after a negation of another clause or another word is still praise (R4-L1).
        for praise in [
            "No DMARC problem here, the sender is trustworthy.",
            "No red flags: verified sender.",
            "No red flags - verified sender.",
            "No red flags \u{2014} verified sender.",
            "No doubt, a trustworthy sender.",
            "No doubt a trustworthy sender.",
            "Without doubt genuine sender.",
            "Nothing suspicious, without doubt genuine sender.",
            "Never seen a more trustworthy mail.",
            "Not only trustworthy but also polite.",
            "Nicht nur ein verifizierter Absender, sondern auch freundlich.",
            "Zweifellos vertrauenswürdig.",
            "Ohne Zweifel ein echter Absender.",
            "Nothing wrong. Trustworthy.",
            "Is it a scam? No! A verified sender.",
        ] {
            assert!(claims(&praise.to_lowercase(), AUTH_SUCCESS_PHRASES), "{praise}");
            let reason = format!("DMARC: {praise}");
            let (kept, _) =
                verify(vec![(reason.clone(), dmarc.clone())], &mail(), &MailShape::default(), &facts, &signals, 6);
            assert!(kept.is_empty(), "{reason}: {kept:?}");
        }
        // The same reason without praise is kept, so the drops above are the claim's doing.
        let (kept, _) = verify(
            vec![("DMARC: no red flags.".to_owned(), dmarc.clone())],
            &mail(),
            &MailShape::default(),
            &facts,
            &signals,
            6,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        // Saying it failed is fine.
        let (kept, _) = verify(
            vec![("DMARC failed: the sender is not authenticated.".to_owned(), dmarc.clone())],
            &mail(),
            &MailShape::default(),
            &facts,
            &signals,
            6,
        );
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn reasons_come_as_objects_or_strings() {
        let answer: Value = serde_json::json!({
            "reasons": [{"text": "A\nB", "evidence": "F1"}, "plain", 7, {"text": ""}]
        });
        assert_eq!(
            parse_reasons(&answer, 6, 300),
            [("A B".to_owned(), "F1".to_owned()), ("plain".to_owned(), String::new())]
        );
    }
}
