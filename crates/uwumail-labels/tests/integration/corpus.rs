//! How well the base labels are put on without a model, measured on the synthetic corpus
//! (`tests/corpus/README.md`). Precision matters more than recall: a wrong label is worse than
//! none. The numbers are printed (`cargo test -p uwumail-labels corpus -- --nocapture`).

use std::collections::BTreeMap;

use serde_json::Value;
use uwumail_labels::{Base, Knowledge, Label, Mail, decide};

/// A corpus mail as the deciding sees it, with the right labels.
pub struct Case {
    pub id: String,
    pub mail: Mail,
    pub truth: Vec<Base>,
}

/// A raw message from a corpus line.
fn raw(entry: &Value) -> Vec<u8> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or_default();
    let mut raw = String::new();
    let from = text("from");
    match entry.get("fromName").and_then(Value::as_str) {
        Some(name) => raw.push_str(&format!("From: \"{}\" <{from}>\r\n", name.replace('"', ""))),
        None => raw.push_str(&format!("From: {from}\r\n")),
    }
    let to: Vec<&str> = entry
        .get("to")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_else(|| vec!["max@mail.example"]);
    raw.push_str(&format!("To: {}\r\n", to.join(", ")));
    raw.push_str(&format!("Subject: {}\r\n", text("subject")));
    if let Some(headers) = entry.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            raw.push_str(&format!("{name}: {}\r\n", value.as_str().unwrap_or_default()));
        }
    }
    raw.push_str("MIME-Version: 1.0\r\n");
    let body = text("text").replace('\n', "\r\n");
    let attachments = entry.get("attachments").and_then(Value::as_array).cloned().unwrap_or_default();
    if attachments.is_empty() {
        raw.push_str("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
        raw.push_str(&body);
        return raw.into_bytes();
    }
    raw.push_str("Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n");
    raw.push_str("--b1\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
    raw.push_str(&body);
    for attachment in attachments {
        let name = attachment.get("name").and_then(Value::as_str).unwrap_or("file");
        let kind = attachment.get("type").and_then(Value::as_str).unwrap_or("application/octet-stream");
        let content =
            if kind == "text/calendar" { "QkVHSU46VkNBTEVOREFSDQpFTkQ6VkNBTEVOREFSDQo=" } else { "JVBERi0xLjQK" };
        raw.push_str(&format!(
            "\r\n--b1\r\nContent-Type: {kind}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{content}"
        ));
    }
    raw.push_str("\r\n--b1--\r\n");
    raw.into_bytes()
}

pub fn synthetic() -> Vec<Case> {
    let lines = include_str!("../corpus/mails.jsonl");
    lines
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let entry: Value = serde_json::from_str(line).expect("a corpus line is JSON");
            let mut mail = Mail::parse(&raw(&entry));
            mail.known_sender = entry.get("knownSender").and_then(Value::as_bool).unwrap_or(false);
            mail.from_trusted = entry.get("auth").and_then(Value::as_str).unwrap_or("pass") == "pass";
            let truth = entry["labels"]
                .as_array()
                .expect("labels")
                .iter()
                .map(|label| Base::parse(label.as_str().unwrap_or_default()).expect("a base label"))
                .collect();
            Case { id: entry["id"].as_str().unwrap_or_default().to_owned(), mail, truth }
        })
        .collect()
}

/// The eight base labels, as a person has them, with ids 1 to 8.
pub fn base_labels() -> Vec<Label<'static>> {
    Base::ALL
        .into_iter()
        .enumerate()
        .map(|(index, base)| Label {
            id: index as i64 + 1,
            keyword: base.as_str(),
            rules: None,
            detector: None,
            learn_senders: true,
            classifier: true,
            base: Some(base),
            auto: true,
        })
        .collect()
}

/// Per label: true positives, false positives, false negatives.
#[derive(Debug, Default, Clone, Copy)]
pub struct Counts {
    pub tp: usize,
    pub fp: usize,
    pub fn_: usize,
}

impl Counts {
    pub fn precision(&self) -> f64 {
        if self.tp + self.fp == 0 { 1.0 } else { self.tp as f64 / (self.tp + self.fp) as f64 }
    }
    pub fn recall(&self) -> f64 {
        if self.tp + self.fn_ == 0 { 1.0 } else { self.tp as f64 / (self.tp + self.fn_) as f64 }
    }
}

/// Scores predictions against the truth, prints a table and the wrong labels, and answers the
/// counts per label and over all.
pub fn score(title: &str, results: &[(String, Vec<Base>, Vec<Base>)]) -> (BTreeMap<&'static str, Counts>, Counts) {
    let mut per: BTreeMap<&'static str, Counts> = BTreeMap::new();
    let mut all = Counts::default();
    let mut wrong = Vec::new();
    for (id, truth, predicted) in results {
        for base in Base::ALL {
            let entry = per.entry(base.as_str()).or_default();
            match (truth.contains(&base), predicted.contains(&base)) {
                (true, true) => {
                    entry.tp += 1;
                    all.tp += 1;
                }
                (false, true) => {
                    entry.fp += 1;
                    all.fp += 1;
                    wrong.push(format!("{id}: {} (truth {truth:?})", base.as_str()));
                }
                (true, false) => {
                    entry.fn_ += 1;
                    all.fn_ += 1;
                    if std::env::var_os("UWUMAIL_SHOW_MISSED").is_some() {
                        println!("  missed: {id}: {}", base.as_str());
                    }
                }
                (false, false) => {}
            }
        }
    }
    println!("\n{title}: {} mails", results.len());
    println!("{:<12} {:>4} {:>4} {:>4} {:>9} {:>7}", "label", "tp", "fp", "fn", "precision", "recall");
    for (name, counts) in &per {
        println!(
            "{name:<12} {:>4} {:>4} {:>4} {:>8.1}% {:>6.1}%",
            counts.tp,
            counts.fp,
            counts.fn_,
            counts.precision() * 100.0,
            counts.recall() * 100.0
        );
    }
    println!(
        "{:<12} {:>4} {:>4} {:>4} {:>8.1}% {:>6.1}%",
        "all",
        all.tp,
        all.fp,
        all.fn_,
        all.precision() * 100.0,
        all.recall() * 100.0
    );
    for line in wrong {
        println!("  wrong: {line}");
    }
    (per, all)
}

/// The base labels decided for each case without a model and without anything learned.
pub fn without_model(cases: &[Case]) -> Vec<(String, Vec<Base>, Vec<Base>)> {
    let labels = base_labels();
    cases
        .iter()
        .map(|case| {
            let decisions = decide(&labels, &case.mail, &[], &Knowledge::default(), &[]);
            let predicted = decisions.iter().map(|d| Base::ALL[(d.label_id - 1) as usize]).collect::<Vec<Base>>();
            (case.id.clone(), case.truth.clone(), predicted)
        })
        .collect()
}

#[test]
fn base_labels_without_a_model_are_precise() {
    let cases = synthetic();
    assert!(cases.len() >= 150, "the corpus has {} mails", cases.len());
    let results = without_model(&cases);
    let (per, all) = score("Synthetic corpus, no model, nothing learned", &results);
    // Rather no label than a wrong one: the bar is precision, recall only must not collapse.
    assert!(all.precision() >= 0.9, "precision {:.3}", all.precision());
    assert!(all.recall() >= 0.5, "recall {:.3}", all.recall());
    for (name, counts) in per {
        assert!(counts.precision() >= 0.8, "{name}: precision {:.3}", counts.precision());
    }
    // No mail gets more than two labels, nor two that exclude each other.
    for (id, _, predicted) in &results {
        assert!(predicted.len() <= 2, "{id}: {predicted:?}");
        if let [a, b] = predicted.as_slice() {
            assert!(!a.excludes(*b), "{id}: {predicted:?}");
        }
    }
}

/// A local corpus of real mails (never in the repository): `$UWUMAIL_REAL_CORPUS` is a directory
/// with `meta.json` (the mails' files and spam log) and `truth-labels.json` (`{ "mails": { file: {
/// "labels": [...], "confidence": "sure" | "unsure" } } }`). Answers the cases and whether each is
/// sure.
pub fn real() -> Option<Vec<(Case, bool)>> {
    let dir = std::path::PathBuf::from(std::env::var_os("UWUMAIL_REAL_CORPUS")?);
    let meta: Value = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
    let truth: Value = serde_json::from_slice(&std::fs::read(dir.join("truth-labels.json")).ok()?).ok()?;
    let mut out = Vec::new();
    for entry in meta["mails"].as_array()? {
        let file = entry["file"].as_str()?;
        let Some(judged) = truth["mails"].get(file) else { continue };
        let raw = std::fs::read(dir.join(file)).ok()?;
        let mut mail = Mail::parse(&raw);
        let auth = entry["spam_log"]["auth"].as_str().unwrap_or("dmarc=pass");
        mail.from_trusted = auth.contains("dmarc=pass") || auth.contains("dkim=pass") || auth.contains("spf=pass");
        let labels = judged["labels"].as_array()?.iter().filter_map(|l| Base::parse(l.as_str()?)).collect();
        let sure = judged["confidence"].as_str() != Some("unsure");
        out.push((Case { id: file.to_owned(), mail, truth: labels }, sure));
    }
    Some(out)
}

#[test]
#[ignore = "needs a local corpus of real mails in $UWUMAIL_REAL_CORPUS"]
fn real_mails_without_a_model() {
    let cases = real().expect("UWUMAIL_REAL_CORPUS with meta.json and truth-labels.json");
    let all: Vec<Case> = cases
        .iter()
        .map(|(case, _)| Case { id: case.id.clone(), mail: case.mail.clone(), truth: case.truth.clone() })
        .collect();
    score("Real corpus, no model, nothing learned", &without_model(&all));
    let sure: Vec<Case> = cases
        .iter()
        .filter(|(_, sure)| *sure)
        .map(|(case, _)| Case { id: case.id.clone(), mail: case.mail.clone(), truth: case.truth.clone() })
        .collect();
    score("Real corpus, sure judgments only", &without_model(&sure));
}
