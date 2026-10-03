use serde_json::json;
use uwumail_labels::{Detector, Knowledge, Label, Rules, Source, candidates, decide};

use crate::{message, with_attachment};

#[test]
fn rules_are_checked_strictly() {
    let ok = Rules::check(&json!({ "match": "any", "conditions": [
        { "field": "from", "value": " @stadtwerke.example " },
        { "field": "hasAttachment", "value": "true" }
    ]}))
    .unwrap()
    .unwrap();
    assert_eq!(ok.conditions[0].value, "@stadtwerke.example");
    assert_eq!(ok.to_json()["match"], "any");
    assert_eq!(ok.to_json()["conditions"][1]["field"], "hasAttachment");

    assert_eq!(Rules::check(&json!(null)), Ok(None));
    assert_eq!(Rules::check(&json!({ "conditions": [] })), Ok(None));
    let bad = [
        json!({ "match": "some", "conditions": [] }),
        json!({ "conditions": [{ "field": "to", "value": "x" }] }),
        json!({ "conditions": [{ "field": "subject", "value": "  " }] }),
        json!({ "conditions": [{ "field": "subject", "value": "x".repeat(201) }] }),
        json!({ "conditions": [{ "field": "subject", "value": "a\u{7}b" }] }),
        json!({ "conditions": [{ "field": "hasAttachment", "value": "yes" }] }),
        json!({ "conditions": [{ "field": "subject", "value": "x", "extra": 1 }] }),
        json!({ "conditions": (0..11).map(|_| json!({ "field": "subject", "value": "x" })).collect::<Vec<_>>() }),
        json!({ "conditions": [], "also": true }),
        json!("from:x"),
    ];
    for rules in bad {
        assert!(Rules::check(&rules).is_err(), "{rules}");
    }
}

#[test]
fn the_surest_labels_win_at_most_two() {
    let rules = Rules::check(&json!({ "match": "all", "conditions": [
        { "field": "from", "value": "stadtwerke.example" },
        { "field": "subject", "value": "RECHNUNG" },
    ]}))
    .unwrap()
    .unwrap();
    let labels = [
        Label {
            id: 1,
            keyword: "rechnungen",
            rules: Some(&rules),
            detector: Some(Detector::Invoice),
            learn_senders: true,
            classifier: true,
            base: None,
            auto: true,
        },
        Label {
            id: 2,
            keyword: "belege",
            rules: None,
            detector: Some(Detector::Invoice),
            learn_senders: true,
            classifier: true,
            base: None,
            auto: true,
        },
        Label {
            id: 3,
            keyword: "stadtwerke",
            rules: None,
            detector: None,
            learn_senders: true,
            classifier: true,
            base: None,
            auto: true,
        },
        Label {
            id: 4,
            keyword: "gelernt-aus",
            rules: None,
            detector: None,
            learn_senders: false,
            classifier: false,
            base: None,
            auto: true,
        },
        Label {
            id: 5,
            keyword: "schon-da",
            rules: None,
            detector: Some(Detector::Invoice),
            learn_senders: true,
            classifier: true,
            base: None,
            auto: true,
        },
    ];
    let mail = with_attachment(
        &["From: Stadtwerke <Rechnung@Mail.Stadtwerke.example>", "Subject: Ihre   Rechnung September"],
        "Anbei Ihre Rechnung.",
        "application/pdf",
        "RE-4711.pdf",
    );
    let mut knowledge = Knowledge::default();
    knowledge.senders.insert(3, 2);
    knowledge.senders.insert(4, 5);
    // Each label once with its surest source; the rule first, then of the equally sure sender and
    // detector the sender (the person's own doing), and no third label.
    let found = candidates(&labels, &mail, &[], &knowledge, &[]);
    let summary: Vec<(i64, Source, &str)> = found.iter().map(|d| (d.label_id, d.source, d.code)).collect();
    assert_eq!(
        summary,
        [
            (1, Source::Rule, "rule"),
            (2, Source::Detector, "invoice"),
            (3, Source::Sender, "sender"),
            (5, Source::Detector, "invoice")
        ]
    );
    assert_eq!(found[1].params, json!({ "word": "Rechnung", "amount": null }));
    let decisions = decide(&labels, &mail, &[], &knowledge, &[]);
    let summary: Vec<(i64, Source)> = decisions.iter().map(|d| (d.label_id, d.source)).collect();
    assert_eq!(summary, [(1, Source::Rule), (3, Source::Sender)]);
    assert_eq!(
        decisions[0].reason,
        "Matches the label's rules: sender is stadtwerke.example and subject contains \"RECHNUNG\""
    );
    assert_eq!(decisions[1].params, json!({ "address": "rechnung@mail.stadtwerke.example", "count": 2 }));
    assert_eq!(decisions[1].reason, "rechnung@mail.stadtwerke.example got this label by hand 2 times");

    // With one label on the mail already, only one more comes.
    let decisions = decide(&labels, &mail, &["schon-da".to_owned()], &knowledge, &[]);
    assert_eq!(decisions.iter().map(|d| d.label_id).collect::<Vec<_>>(), [1]);

    // Taken off this sender's mail by hand, a label comes back only by its rules.
    let mut blocked = knowledge.clone();
    blocked.senders.insert(2, -1);
    blocked.senders.insert(1, -1);
    let found = candidates(&labels[..2], &mail, &[], &blocked, &[]);
    assert_eq!(found.iter().map(|d| d.label_id).collect::<Vec<_>>(), [1]);

    // Switched off, a label is not put on by itself at all.
    let off = [Label { auto: false, ..labels[1] }];
    assert!(decide(&off, &mail, &[], &knowledge, &[]).is_empty());

    // A From address nothing vouches for gets no learned sender's label (security audit 0.21.0
    // LABELS-L3): anyone can write a known address there.
    let mut forged = mail.clone();
    forged.from_trusted = false;
    assert!(decide(&labels[2..3], &forged, &[], &knowledge, &[]).is_empty());

    // One hand-labeling is not enough to learn a sender.
    knowledge.senders.insert(3, 1);
    assert!(decide(&labels[2..3], &mail, &[], &knowledge, &[]).is_empty());

    // Text conditions fold case and white space.
    let text_rules =
        Rules::check(&json!({ "conditions": [{ "field": "text", "value": "Kunden  NUMMER 42" }] })).unwrap().unwrap();
    let note = message(&["From: a@example.com", "Subject: x"], "Ihre\nKundennummer 42 bleibt.");
    assert!(text_rules.matches(&note).is_none());
    let note = message(&["From: a@example.com", "Subject: x"], "Ihre Kunden\n  nummer 42 bleibt.");
    assert!(text_rules.matches(&note).is_some());
}
