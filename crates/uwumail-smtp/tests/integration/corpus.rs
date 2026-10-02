//! How well the spam filter tells spam and phishing from wanted mail, on the synthetic corpus in
//! `tests/corpus` (see its README).
//!
//! This measures what the filter can tell without the network: the authentication results the corpus
//! states and the message itself ([`uwumail_smtp::score_offline`]). Blocklists, the Bayes filter and
//! downloaded lists come on top in real delivery and can only move these numbers, so the bounds here
//! are about the rules themselves: above all, how much wanted mail they would put into Junk.
//!
//! Run with `--nocapture` to see the table and which rules fire on wanted mail.

use std::collections::BTreeMap;
use std::path::Path;

use uwumail_smtp::{Authentication, score_offline};

/// Wed, 16 Sep 2026 10:00:00 +0000, the corpus' "now".
const NOW: i64 = 1_789_552_800;
/// The default limits of `SpamConfig`.
const JUNK: f32 = 5.0;
const GREYLIST: f32 = 2.0;

struct Sample {
    file: String,
    class: String,
    sender: String,
    auth: Authentication,
    raw: Vec<u8>,
}

fn auth(line: &str) -> Authentication {
    let value = |key: &str| {
        line.split_whitespace().find_map(|part| part.strip_prefix(&format!("{key}="))).unwrap_or("none").to_owned()
    };
    let (spf, dkim, dmarc) = (value("spf"), value("dkim"), value("dmarc"));
    Authentication {
        spf_failed: spf == "fail",
        dkim_failed: dkim == "fail",
        dmarc_failed: dmarc == "fail",
        dmarc_passed: dmarc == "pass",
        sender_verified: spf == "pass" || dkim == "pass",
    }
}

fn corpus() -> Vec<Sample> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut samples = Vec::new();
    for class in ["ham", "spam", "phishing"] {
        let mut files: Vec<_> =
            std::fs::read_dir(root.join(class)).unwrap().map(|entry| entry.unwrap().path()).collect();
        files.sort();
        for path in files.into_iter().filter(|path| path.extension().is_some_and(|ext| ext == "eml")) {
            let text = std::fs::read(&path).unwrap();
            let mut meta = BTreeMap::new();
            let mut rest = &text[..];
            // The corpus headers come first and are not part of the mail.
            while rest.starts_with(b"X-Corpus-") {
                let end = rest.windows(2).position(|pair| pair == b"\r\n").unwrap();
                let line = String::from_utf8_lossy(&rest[..end]).into_owned();
                let (name, value) = line.split_once(": ").unwrap();
                meta.insert(name.trim_start_matches("X-Corpus-").to_owned(), value.to_owned());
                rest = &rest[end + 2..];
            }
            assert_eq!(meta["Class"], class, "{}", path.display());
            samples.push(Sample {
                file: path.file_name().unwrap().to_string_lossy().into_owned(),
                class: class.to_owned(),
                sender: meta.get("Sender").cloned().unwrap_or_default(),
                auth: auth(&meta["Auth"]),
                raw: rest.to_vec(),
            });
        }
    }
    samples
}

#[derive(Default)]
struct Tally {
    total: usize,
    junk: usize,
    suspicious: usize,
}

#[test]
fn the_filter_keeps_wanted_mail_out_of_junk_and_catches_most_of_the_rest() {
    let samples = corpus();
    assert!(samples.len() >= 150, "the corpus has {} mails", samples.len());
    let started = std::time::Instant::now();

    let mut tallies: BTreeMap<(&str, bool), Tally> = BTreeMap::new();
    let mut ham_rules: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut misses = Vec::new();
    for sample in &samples {
        let score = score_offline(&sample.raw, NOW, sample.auth);
        // "warm": a sender the reader knows has delivered here before, which the reputation
        // rewards (KNOWN_GOOD_SENDER, -2.5); "cold" leaves that out.
        let known = matches!(sample.sender.as_str(), "known" | "contact");
        for warm in [false, true] {
            let points = score.points + if warm && known { -2.5 } else { 0.0 };
            let tally = tallies.entry((sample.class.as_str(), warm)).or_default();
            tally.total += 1;
            tally.junk += usize::from(points >= JUNK);
            tally.suspicious += usize::from(points >= GREYLIST);
            if !warm && sample.class == "ham" && points >= GREYLIST {
                misses.push(format!("ham {} {:.1} {}", sample.file, points, score.tests()));
            }
            if !warm && sample.class != "ham" && points < JUNK {
                misses.push(format!("{} {} {:.1} {}", sample.class, sample.file, points, score.tests()));
            }
        }
        if sample.class == "ham" {
            for hit in &score.hits {
                *ham_rules.entry(hit.rule).or_default() += 1;
            }
        }
    }

    println!("class     reputation  mails  junk  greylisted-or-worse");
    for ((class, warm), tally) in &tallies {
        println!(
            "{class:9} {:10}  {:5}  {:4}  {:4}",
            if *warm { "warm" } else { "cold" },
            tally.total,
            tally.junk,
            tally.suspicious
        );
    }
    println!("rules on wanted mail: {ham_rules:?}");
    for miss in &misses {
        println!("  {miss}");
    }

    let share = |class: &str, warm: bool, f: fn(&Tally) -> usize| {
        let tally = &tallies[&(class, warm)];
        f(tally) as f64 / tally.total as f64
    };
    // Wanted mail in Junk: what costs people mail. At most 1 %, without any reputation.
    let ham_junk = share("ham", false, |t| t.junk);
    assert!(ham_junk <= 0.01, "{:.1} % of wanted mail would go to Junk", ham_junk * 100.0);
    // Wanted mail delayed by greylisting, without reputation: a nuisance, kept small.
    let ham_grey = share("ham", false, |t| t.suspicious);
    assert!(ham_grey <= 0.10, "{:.1} % of wanted mail would be greylisted", ham_grey * 100.0);
    // Phishing is what the rules are for; spam relies on Bayes and blocklists more.
    let phishing = share("phishing", false, |t| t.junk);
    assert!(phishing >= 0.70, "only {:.1} % of phishing goes to Junk", phishing * 100.0);
    let phishing = share("phishing", false, |t| t.suspicious);
    assert!(phishing >= 0.85, "only {:.1} % of phishing is even greylisted", phishing * 100.0);
    let spam = share("spam", false, |t| t.suspicious);
    assert!(spam >= 0.60, "only {:.1} % of spam is even greylisted", spam * 100.0);
    assert!(started.elapsed().as_secs() < 20, "scoring the corpus took {:?}", started.elapsed());
}

/// The same on real mail, which stays on the machine it is on: `UWUMAIL_REAL_CORPUS` names a
/// directory with `meta.json` and the `.eml` files it lists (see the 0.22 brief), and
/// `truth-spam.json` next to it says which of them are spam (`{"spam": ["eml/m1.eml", …]}`).
/// Prints how much of the wanted mail the rules alone would greylist or put into Junk.
#[test]
#[ignore = "needs a local corpus of real mail"]
fn real_mail() {
    let Ok(root) = std::env::var("UWUMAIL_REAL_CORPUS") else { return };
    let root = Path::new(&root);
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("meta.json")).unwrap()).unwrap();
    let truth: serde_json::Value = std::fs::read(root.join("truth-spam.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({ "spam": [] }));
    let spam: Vec<&str> = truth["spam"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    let (mut ham, mut ham_grey, mut ham_junk, mut caught, mut rules) = (0, 0, 0, 0, BTreeMap::<&str, usize>::new());
    for mail in meta["mails"].as_array().unwrap() {
        let file = mail["file"].as_str().unwrap();
        let Ok(raw) = std::fs::read(root.join(file)) else { continue };
        // What this server found when the mail came in; without that (fetched or older mail), the
        // content alone: nothing failed, nothing passed DMARC.
        let found = mail["spam_log"]["auth"].as_str().map(str::to_lowercase);
        let result = |method: &str| {
            let found = found.as_deref()?;
            found
                .split(&format!("{method}="))
                .nth(1)
                .map(|rest| rest.split([' ', ';', '\r', '\n']).next().unwrap_or("").to_owned())
        };
        let (spf, dkim, dmarc) = (result("spf"), result("dkim"), result("dmarc"));
        let auth = if found.is_some() {
            Authentication {
                spf_failed: spf.as_deref() == Some("fail"),
                dkim_failed: dkim.as_deref() == Some("fail"),
                dmarc_failed: dmarc.as_deref() == Some("fail"),
                dmarc_passed: dmarc.as_deref() == Some("pass"),
                sender_verified: spf.as_deref() == Some("pass") || dkim.as_deref() == Some("pass"),
            }
        } else {
            Authentication { sender_verified: true, ..Authentication::default() }
        };
        let score = score_offline(&raw, mail["received_at"].as_i64().unwrap_or(NOW), auth);
        if spam.contains(&file) {
            caught += usize::from(score.points >= GREYLIST);
            println!("spam {file}: {:.1} {}", score.points, score.tests());
            continue;
        }
        ham += 1;
        ham_grey += usize::from(score.points >= GREYLIST);
        ham_junk += usize::from(score.points >= JUNK);
        if score.points >= GREYLIST {
            let details: Vec<String> =
                score.hits.iter().filter_map(|hit| Some(format!("{}={}", hit.rule, hit.detail.as_deref()?))).collect();
            println!("ham {file}: {:.1} {} {details:?}", score.points, score.tests());
        }
        for hit in &score.hits {
            *rules.entry(hit.rule).or_default() += 1;
        }
    }
    println!("wanted mail: {ham}, greylisted or worse: {ham_grey}, junk: {ham_junk}");
    println!("spam: {}, greylisted or worse: {caught}", spam.len());
    println!("rules on wanted mail: {rules:?}");
}
