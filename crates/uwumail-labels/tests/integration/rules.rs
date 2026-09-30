use serde_json::json;
use uwumail_labels::{Detector, Knowledge, Label, Rules, Source, decide};

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
fn the_first_source_that_matches_wins_once_per_label() {
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
        },
        Label {
            id: 2,
            keyword: "belege",
            rules: None,
            detector: Some(Detector::Invoice),
            learn_senders: true,
            classifier: true,
        },
        Label { id: 3, keyword: "stadtwerke", rules: None, detector: None, learn_senders: true, classifier: true },
        Label { id: 4, keyword: "gelernt-aus", rules: None, detector: None, learn_senders: false, classifier: false },
        Label {
            id: 5,
            keyword: "schon-da",
            rules: None,
            detector: Some(Detector::Invoice),
            learn_senders: true,
            classifier: true,
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
    let decisions = decide(&labels, &mail, &["schon-da".to_owned()], &knowledge, &[]);
    let summary: Vec<(i64, Source, &str)> = decisions.iter().map(|d| (d.label_id, d.source, d.code)).collect();
    assert_eq!(summary, [(1, Source::Rule, "rule"), (2, Source::Detector, "invoice"), (3, Source::Sender, "sender")]);
    assert_eq!(
        decisions[0].reason,
        "Matches the label's rules: sender is stadtwerke.example and subject contains \"RECHNUNG\""
    );
    assert_eq!(decisions[1].params, json!({ "word": "Rechnung", "amount": null }));
    assert_eq!(decisions[2].params, json!({ "address": "rechnung@mail.stadtwerke.example", "count": 2 }));
    assert_eq!(decisions[2].reason, "rechnung@mail.stadtwerke.example got this label by hand 2 times");

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
