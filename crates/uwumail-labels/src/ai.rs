//! The model as the last word (docs/labels.md, "Asking the model"): asked only about the labels the
//! cheap ways leave in doubt, held to the facts, and counted no surer than they are. What the model
//! says is an [`AiAnswer`] per label; "unsure" counts as no.

use std::collections::HashMap;

use serde_json::json;

use crate::{Base, Decision, Facts, Label, MAIN_THRESHOLD, MAX_LABELS, SECOND_THRESHOLD, Source};

/// How sure a "yes" of the model is: enough for a main label, not for a second one.
pub const AI_YES: f64 = 0.85;
/// A "yes" that a weaker hint of another way supports (a detector, similar mails, a sender …).
pub const AI_SUPPORTED: f64 = 0.92;
/// A hint at least this sure supports a "yes".
pub const HINT: f64 = 0.5;

/// What the model said of one label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAnswer {
    Yes,
    No,
    Unsure,
}

impl AiAnswer {
    /// `"yes"`, `"no"`, `"unsure"`, or `true`/`false` as older prompts had it.
    pub fn parse(value: &serde_json::Value) -> Option<AiAnswer> {
        match value {
            serde_json::Value::Bool(true) => Some(AiAnswer::Yes),
            serde_json::Value::Bool(false) => Some(AiAnswer::No),
            serde_json::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
                "yes" | "true" | "ja" => Some(AiAnswer::Yes),
                "no" | "false" | "nein" => Some(AiAnswer::No),
                "unsure" | "maybe" | "unknown" => Some(AiAnswer::Unsure),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The model's verdict on one label, with its reason.
#[derive(Debug, Clone, PartialEq)]
pub struct AiVerdict {
    pub label_id: i64,
    pub verdict: AiAnswer,
    pub reason: String,
}

/// The labels the model should be asked about, given what the cheap ways found (`candidates`, all
/// of them, the unsure ones too) and the keywords on the mail (`present`): none when two labels are
/// sure already. With a main label sure (or one on the mail), only the person's own labels are
/// asked about, as the base labels' detectors have had their say; never one that excludes a label
/// on the mail or a sure one.
pub fn ask_about(labels: &[Label<'_>], present: &[String], candidates: &[Decision]) -> Vec<i64> {
    let sure = crate::choose(labels, present, candidates.to_vec());
    let on: Vec<&Label<'_>> = labels
        .iter()
        .filter(|label| present.iter().any(|keyword| keyword.eq_ignore_ascii_case(label.keyword)))
        .collect();
    if on.len() + sure.len() >= MAX_LABELS {
        return Vec::new();
    }
    let taken: Vec<Option<Base>> = on
        .iter()
        .map(|label| label.meaning())
        .chain(sure.iter().map(|decision| labels.iter().find(|l| l.id == decision.label_id).and_then(Label::meaning)))
        .collect();
    let only_own = !taken.is_empty();
    labels
        .iter()
        .filter(|label| label.auto)
        .filter(|label| !on.iter().any(|on| on.id == label.id) && !sure.iter().any(|d| d.label_id == label.id))
        .filter(|label| !only_own || label.base.is_none())
        .filter(|label| {
            let its = label.meaning();
            !taken.iter().any(|known| matches!((known, its), (Some(a), Some(b)) if a.excludes(b)))
        })
        .map(|label| label.id)
        .collect()
}

/// Whether the facts rule out that a mail is what `base` stands for, whatever the model says: a
/// mass mail is never personal, an automatic notice never work, a mail sent to one person never a
/// newsletter, a device's notice never account mail (the mistakes small models make most).
pub fn ruled_out(base: Base, facts: &Facts) -> bool {
    if facts.bounce || facts.test_mail {
        return true;
    }
    match base {
        Base::Personal => !facts.written_by_person() || facts.same_domain || facts.to_self,
        Base::Work => {
            facts.to_self
                || facts.mass_mail()
                || facts.automatic
                || matches!(facts.sender, crate::SenderKind::NoReply | crate::SenderKind::Marketing)
                || (facts.sender == crate::SenderKind::Role && !facts.known_sender)
        }
        // An app's notification about activity on the account is neither an edition nor an ad,
        // unless it is plainly selling.
        Base::Newsletter => !facts.mass_mail() || facts.discussion_list || facts.notification,
        Base::Advertising => {
            !facts.mass_mail() || facts.discussion_list || (facts.notification && facts.sales.len() < 2)
        }
        // Notices of apps and devices (monitoring, smart home, reminders) are no account mail: it
        // names the account in the subject or brings a code.
        Base::Account => facts.written_by_person() || (facts.account.is_empty() && facts.code.is_none()),
        // A neighbour writing about a parcel is no shipment, nor is a mail naming no tracking
        // number, carrier, shipping or pickup word.
        Base::Shipping => (facts.written_by_person() && facts.freemail) || !facts.shipment_evidence,
        // An order confirmation is no invoice unless it says it is one, and an invoice names an
        // invoice, a number, an amount or brings a PDF.
        Base::Invoice => {
            (facts.order && !facts.invoice_word && facts.invoice_numbers.is_empty())
                || (!facts.invoice_word && facts.invoice_numbers.is_empty() && facts.amounts.is_empty() && !facts.pdf)
        }
        Base::Appointment => false,
    }
}

/// The model's verdicts on the labels it was asked about (`asked`) as candidates: a "yes" counts
/// [`AI_YES`], [`AI_SUPPORTED`] with a hint of the cheap ways (`candidates`); "no" and "unsure"
/// count nothing. A model that says yes to more than [`MAX_LABELS`] labels, or to two that exclude
/// each other, is not sure at all: none of its yeses count.
pub fn ai_candidates(
    labels: &[Label<'_>],
    facts: &Facts,
    asked: &[i64],
    verdicts: &[AiVerdict],
    candidates: &[Decision],
) -> Vec<Decision> {
    let mut seen = Vec::new();
    let yes: Vec<(&Label<'_>, &AiVerdict)> = verdicts
        .iter()
        .filter(|verdict| asked.contains(&verdict.label_id))
        .filter(|verdict| {
            let first = !seen.contains(&verdict.label_id);
            seen.push(verdict.label_id);
            first && verdict.verdict == AiAnswer::Yes
        })
        .filter_map(|verdict| Some((labels.iter().find(|label| label.id == verdict.label_id)?, verdict)))
        .collect();
    if yes.len() > MAX_LABELS {
        return Vec::new();
    }
    if let [(a, _), (b, _)] = yes.as_slice()
        && matches!((a.meaning(), b.meaning()), (Some(x), Some(y)) if x.excludes(y))
    {
        return Vec::new();
    }
    yes.into_iter()
        .filter(|(label, _)| label.meaning().is_none_or(|base| !ruled_out(base, facts)))
        .map(|(label, verdict)| {
            let hint = candidates
                .iter()
                .filter(|candidate| candidate.label_id == label.id)
                .map(|candidate| candidate.confidence)
                .fold(0.0, f64::max);
            Decision {
                label_id: label.id,
                keyword: label.keyword.to_owned(),
                source: Source::Ai,
                code: "ai",
                params: json!({ "supported": hint >= HINT }),
                reason: verdict.reason.clone(),
                confidence: if hint >= HINT { AI_SUPPORTED } else { AI_YES },
            }
        })
        .collect()
}

/// The surest candidate per label of both lists, for [`crate::choose`].
pub fn merge(candidates: Vec<Decision>, more: Vec<Decision>) -> Vec<Decision> {
    let mut best: HashMap<i64, Decision> = HashMap::new();
    for candidate in candidates.into_iter().chain(more) {
        match best.get(&candidate.label_id) {
            Some(known) if known.confidence >= candidate.confidence => {}
            _ => {
                best.insert(candidate.label_id, candidate);
            }
        }
    }
    let mut out: Vec<Decision> = best.into_values().collect();
    out.sort_by_key(|decision| decision.label_id);
    out
}

// Keep the thresholds in step: a lone yes is a main label, never a second one.
const _: () = assert!(AI_YES >= MAIN_THRESHOLD && AI_YES < SECOND_THRESHOLD && AI_SUPPORTED >= SECOND_THRESHOLD);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mail, SenderKind};

    fn labels() -> Vec<Label<'static>> {
        let base = |id: i64, keyword: &'static str, base: Base| Label {
            id,
            keyword,
            rules: None,
            detector: None,
            learn_senders: false,
            classifier: false,
            base: Some(base),
            auto: true,
        };
        vec![
            base(1, "rechnung", Base::Invoice),
            base(2, "newsletter", Base::Newsletter),
            base(3, "persönlich", Base::Personal),
            Label { base: None, ..base(4, "reisen", Base::Invoice) },
        ]
    }

    fn sure(label_id: i64, confidence: f64) -> Decision {
        Decision {
            label_id,
            keyword: String::new(),
            source: Source::Detector,
            code: "invoice",
            params: json!({}),
            reason: String::new(),
            confidence,
        }
    }

    fn yes(label_id: i64) -> AiVerdict {
        AiVerdict { label_id, verdict: AiAnswer::Yes, reason: "fits".into() }
    }

    #[test]
    fn asks_only_what_is_in_doubt() {
        let labels = labels();
        assert_eq!(ask_about(&labels, &[], &[sure(1, 0.6)]), [1, 2, 3, 4]);
        // A sure invoice: only own labels, and never one excluded by it.
        assert_eq!(ask_about(&labels, &[], &[sure(1, 0.9)]), [4]);
        assert_eq!(ask_about(&labels, &["newsletter".into()], &[]), [4]);
        assert!(ask_about(&labels, &["newsletter".into(), "reisen".into()], &[]).is_empty());
    }

    #[test]
    fn notifications_tests_and_mails_without_evidence_are_ruled_out() {
        let recap = Mail::new(
            "stories-recap@mail.social.example",
            "lea und 3 weitere Personen haben vor Kurzem etwas gepostet",
            "Sieh dir an, was los ist",
            vec![],
            false,
            vec![],
        );
        let mut facts = Facts::of(&recap);
        facts.list_unsubscribe = true;
        assert!(facts.notification);
        assert!(ruled_out(Base::Newsletter, &facts));
        assert!(ruled_out(Base::Advertising, &facts));
        let weekly = Mail::new("news@weekly.example", "Self-Host Weekly #42", "Die Neuigkeiten", vec![], false, vec![]);
        let mut facts = Facts::of(&weekly);
        facts.list_unsubscribe = true;
        assert!(!facts.notification);
        assert!(!ruled_out(Base::Newsletter, &facts));
        // A plain test mail gets nothing.
        let test = Mail::new("mia@firma.example", "Test", "test", vec![], false, vec![]);
        assert!(Base::ALL.iter().all(|base| ruled_out(*base, &Facts::of(&test))));
        // An invoice names something an invoice has; a shipment something a shipment has.
        let iban =
            Mail::new("no-reply@bank.example", "Deine IBAN wartet auf dich", "Jetzt loslegen", vec![], false, vec![]);
        let facts = Facts::of(&iban);
        assert!(ruled_out(Base::Invoice, &facts));
        assert!(ruled_out(Base::Shipping, &facts));
        let parcel = Mail::new("noreply@shop.example", "Dein Paket ist unterwegs", "Bald da", vec![], false, vec![]);
        assert!(!ruled_out(Base::Shipping, &Facts::of(&parcel)));
    }

    #[test]
    fn the_model_is_held_to_the_facts() {
        let labels = labels();
        let mail =
            Mail::new("noreply@shop.example", "Hallo Mia", "Liebe Mia, schön dass du da bist", vec![], false, vec![]);
        let mut facts = Facts::of(&mail);
        facts.list_unsubscribe = true;
        assert!(ruled_out(Base::Personal, &facts));
        assert!(!ruled_out(Base::Newsletter, &facts));
        // A device's notice is no account mail; a sign-in code is.
        assert!(ruled_out(Base::Account, &facts));
        let code = Mail::new("noreply@shop.example", "Dein Code", "Dein Code: 482913", vec![], false, vec![]);
        assert!(!ruled_out(Base::Account, &Facts::of(&code)));
        let found = ai_candidates(&labels, &facts, &[1, 2, 3, 4], &[yes(3)], &[]);
        assert!(found.is_empty(), "{found:?}");
        let found = ai_candidates(&labels, &facts, &[1, 2, 3, 4], &[yes(2)], &[sure(2, 0.6)]);
        assert_eq!((found[0].label_id, found[0].confidence), (2, AI_SUPPORTED));
        // Not asked, or too many yeses, or two that exclude each other: nothing.
        assert!(ai_candidates(&labels, &facts, &[1], &[yes(2)], &[]).is_empty());
        assert!(ai_candidates(&labels, &facts, &[1, 2, 3, 4], &[yes(1), yes(2), yes(4)], &[]).is_empty());
        assert!(ai_candidates(&labels, &facts, &[1, 2, 3, 4], &[yes(1), yes(2)], &[]).is_empty());
        facts.list_unsubscribe = false;
        facts.sender = SenderKind::Person;
        facts.freemail = true;
        assert!(!ruled_out(Base::Personal, &facts));
        assert!(ruled_out(Base::Newsletter, &facts));
        let found = ai_candidates(&labels, &facts, &[3], &[AiVerdict { verdict: AiAnswer::Unsure, ..yes(3) }], &[]);
        assert!(found.is_empty());
        assert_eq!(AiAnswer::parse(&json!(true)), Some(AiAnswer::Yes));
        assert_eq!(AiAnswer::parse(&json!("Unsure")), Some(AiAnswer::Unsure));
        assert_eq!(AiAnswer::parse(&json!(3)), None);
    }

    #[test]
    fn merging_keeps_the_surest() {
        let merged = merge(vec![sure(1, 0.6), sure(2, 0.9)], vec![sure(1, 0.85)]);
        assert_eq!(merged.iter().map(|d| (d.label_id, d.confidence)).collect::<Vec<_>>(), [(1, 0.85), (2, 0.9)]);
    }
}
