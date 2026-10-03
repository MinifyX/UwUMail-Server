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

use serde::Serialize;
use serde_json::Value;
use uwumail_smtp::phishing::Finding;

use crate::features::SpamSignals;
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

/// Adds up what the server knows. `phishing` are the findings of `uwumail_smtp::phishing` for this
/// mail, `text` the mail's subject and text for the content cues.
pub fn assess(signals: &SpamSignals, phishing: &[Finding], text: &str) -> Assessment {
    let mut evidence = Vec::new();
    let auth = &signals.authentication;
    let authentic = match auth.dmarc.as_deref() {
        Some("pass") => true,
        None | Some("none") => passed(&auth.dkim) && passed(&auth.spf),
        Some(_) => false,
    };

    // The spam filter: its whole verdict in one fact, scaled so that its limit weighs 3.
    if let Some(score) = signals.spam_score {
        let threshold = signals.spam_threshold.filter(|t| *t > 0.0).unwrap_or(5.0);
        let detail = Some(format!("{score:.1}/{threshold:.1}"));
        if score <= 0.0 {
            add(&mut evidence, "FILTER_WANTED", (score * 0.4).clamp(-2.0, -0.5), detail, false);
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
    // A clean-looking mail that still shows a phishing trick may at least be called suspicious.
    if high == 0 && phishing_possible {
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
    let mut kept: Vec<Reason> = Vec::new();
    let mut dropped = 0;
    for (text, evidence) in reasons {
        if kept.len() >= max {
            break;
        }
        let cited = cited_fact(&evidence, facts).or_else(|| cited_fact(&text, facts));
        let quote = if cited.is_none() { quoted(&evidence, &haystack) } else { None };
        let grounded = cited.is_some() || quote.is_some();
        if !grounded || contradicts(&text, mail, shape, signals) || kept.iter().any(|known| known.text == text) {
            dropped += 1;
            continue;
        }
        kept.push(Reason { text, quote, fact: cited });
    }
    (kept, dropped)
}

/// The fact id a citation names (`F3`, `[F3]`, `Fakt F3`), when that fact exists.
fn cited_fact(text: &str, facts: &[Fact]) -> Option<String> {
    let upper = text.to_uppercase();
    let bytes = upper.as_bytes();
    let mut at = 0;
    while let Some(found) = upper[at..].find('F') {
        let start = at + found;
        let digits: String = upper[start + 1..].chars().take_while(char::is_ascii_digit).collect();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if before_ok && !digits.is_empty() {
            let id = format!("F{digits}");
            if facts.iter().any(|fact| fact.id == id) {
                return Some(id);
            }
        }
        at = start + 1;
    }
    None
}

/// The quote when it stands in the mail: at least four characters, compared without case and
/// with whitespace collapsed.
fn quoted(evidence: &str, haystack: &str) -> Option<String> {
    let wanted = normalized(evidence);
    (wanted.chars().count() >= 4 && haystack.contains(&wanted)).then(|| evidence.trim().to_owned())
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
    use crate::features::{AuthenticationSignals, SenderSignals};

    fn invoice() -> SpamSignals {
        SpamSignals {
            authentication: AuthenticationSignals {
                spf: Some("pass".into()),
                dkim: Some("pass".into()),
                dmarc: Some("pass".into()),
                from_domain: Some("hoster.example".into()),
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

    fn stranger() -> SpamSignals {
        SpamSignals {
            authentication: AuthenticationSignals {
                spf: Some("pass".into()),
                dkim: Some("pass".into()),
                dmarc: Some("pass".into()),
                from_domain: Some("konto-hilfe.example".into()),
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
            &[Fact { id: "F1".into(), text: String::new() }],
            &signals,
            6,
        );
        assert_eq!(kept.len(), 1, "saying there is none is fine");
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
