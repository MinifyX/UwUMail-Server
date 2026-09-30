use std::collections::HashMap;

use uwumail_labels::{Knowledge, Label, Mail, Model, Source, decide, token_hash, tokens};

use crate::message;

fn hashes(mail: &Mail) -> Vec<i64> {
    tokens(mail).iter().map(|token| token_hash(token)).collect()
}

/// A model learned from `positives` and `negatives` the way the server counts: per token, in how
/// many examples with and without the label it occurs.
fn learn(positives: &[Mail], negatives: &[Mail]) -> Model {
    let mut counts: HashMap<i64, (i64, i64)> = HashMap::new();
    for mail in positives {
        for token in hashes(mail) {
            counts.entry(token).or_default().0 += 1;
        }
    }
    for mail in negatives {
        for token in hashes(mail) {
            counts.entry(token).or_default().1 += 1;
        }
    }
    Model { positives: positives.len() as i64, negatives: negatives.len() as i64, counts }
}

fn club(n: usize) -> Mail {
    message(
        &[
            &format!("From: Verein <vorstand{n}@sportverein.example>"),
            &format!("Subject: Training und Vereinsheim Woche {n}"),
        ],
        "Liebe Mitglieder, das Training der Jugendmannschaft findet im Vereinsheim statt. Beiträge bitte an den Kassenwart.",
    )
}

fn other(n: usize) -> Mail {
    let topics = [
        "Die Sitzung zum Projektplan verschiebt sich, bitte Unterlagen prüfen.",
        "Your cloud storage is almost full, upgrade your plan today.",
        "Hallo Leni, wie war der Urlaub? Lass uns bald telefonieren.",
        "Neue Angebote im Onlineshop: Schuhe und Jacken reduziert.",
    ];
    message(&[&format!("From: someone{n}@example.com"), &format!("Subject: Nachricht {n}")], topics[n % topics.len()])
}

#[test]
fn tokens_are_sender_subject_and_words() {
    let mail = message(
        &["From: Leni <Leni@Example.org>", "Subject: Hallo Welt 2026"],
        "Ein kurzer Text mit Wörtern, ab und zu 12345 Zahlen und einem sehr-langen-wort.",
    );
    let list = tokens(&mail);
    assert_eq!(&list[..4], ["from:leni@example.org", "domain:example.org", "subject:hallo", "subject:welt"]);
    assert!(list.contains(&"wörtern".to_owned()));
    assert!(!list.iter().any(|t| t == "ab" || t == "12345" || t == "subject:2026"));
    assert_eq!(list.iter().filter(|t| *t == "und").count(), 1);
}

#[test]
fn it_waits_for_examples_and_then_is_sure_only_of_similar_mail() {
    let positives: Vec<Mail> = (0..15).map(club).collect();
    let negatives: Vec<Mail> = (0..14).map(other).collect();
    let model = learn(&positives, &negatives);
    assert!(!model.ready(), "14 examples without the label are too few");
    assert_eq!(model.classify(&hashes(&club(99))), None);

    let negatives: Vec<Mail> = (0..30).map(other).collect();
    let model = learn(&positives, &negatives);
    let verdict = model.classify(&hashes(&club(99))).expect("a mail like the club's");
    assert!(verdict.probability >= 0.99);
    assert_eq!(verdict.examples, 15);
    assert_eq!(model.classify(&hashes(&other(7))), None);
    let (unrelated, _) =
        model.probability(&hashes(&message(&["From: x@example.net", "Subject: Hi"], "Ganz etwas anderes."))).unwrap();
    assert!(unrelated < 0.5, "{unrelated}");

    let labels =
        [Label { id: 9, keyword: "verein", rules: None, detector: None, learn_senders: true, classifier: true }];
    let mut knowledge = Knowledge::default();
    knowledge.models.insert(9, model.clone());
    let decisions = decide(&labels, &club(99), &[], &knowledge, &hashes(&club(99)));
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].source, Source::Classifier);
    assert_eq!(decisions[0].params["examples"], 15);
    assert!(decisions[0].reason.starts_with("Similar to the 15 mails with this label ("), "{}", decisions[0].reason);

    // Switched off for the label, the classifier says nothing.
    let off = [Label { classifier: false, ..labels[0] }];
    assert!(decide(&off, &club(99), &[], &knowledge, &hashes(&club(99))).is_empty());
}

/// Counts from a damaged store (near the largest number, or below zero) neither overflow nor make
/// the verdict NaN (client review C-D-4).
#[test]
fn damaged_counts_are_no_trouble() {
    let token = token_hash("vereinsheim");
    let mut model = Model { positives: 20, negatives: 20, counts: HashMap::new() };
    model.counts.insert(token, (i64::MAX, i64::MAX));
    let (probability, _) = model.probability(&[token]).unwrap();
    assert!(probability.is_finite());
    model.counts.insert(token, (-5, i64::MIN));
    let (probability, _) = model.probability(&[token]).unwrap();
    assert!(probability.is_finite());
}
