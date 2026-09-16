//! The Bayes filter. It splits a message into tokens (words, word pairs and a few facts like the
//! sites it links to), and weighs each token by how often it turned up in spam and in wanted mail
//! that people marked, or that was clearly one or the other. Robinson's way of combining the tokens
//! with Fisher's method, as SpamBayes does it, copes well with little learned mail.
//!
//! Tokens are stored as keyed hashes, so the database holds no readable word from anyone's mail.
//! Our own headers (Received, Authentication-Results, X-Spam-*) are never tokens, or the filter
//! would learn its own verdicts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use mail_parser::{Message, MimeHeaders};
use tokio::sync::watch;
use uwumail_store::{BAYES_MIN_LEARNED as MIN_LEARNED, BayesJob, BayesTotals, Store};

use crate::{Context, Smtp};

/// Words shorter than this say little; longer ones are mostly encoded data or glued-together junk.
const MIN_WORD: usize = 3;
const MAX_WORD: usize = 24;
/// Enough tokens to know a message; later ones rarely change the verdict.
const MAX_TOKENS: usize = 2000;
const MAX_TEXT: usize = 200 * 1024;

const SECRET_KEY: &str = "bayes.secret";

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| (MIN_WORD..=MAX_WORD).contains(&word.chars().count()))
        .filter(|word| !word.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_lowercase)
}

fn push(tokens: &mut Vec<String>, seen: &mut HashSet<String>, token: String) {
    if tokens.len() < MAX_TOKENS && seen.insert(token.clone()) {
        tokens.push(token);
    }
}

/// The tokens of a message: facts about it first, then subject words, then the words and word pairs
/// of its text (HTML turned into text). `link_sites` are the sites its links lead to.
pub(crate) fn tokens(message: &Message<'_>, link_sites: &[String]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut seen = HashSet::new();

    if let Some(domain) = message
        .from()
        .and_then(|from| from.first())
        .and_then(|from| from.address.as_deref())
        .and_then(|address| address.rsplit_once('@'))
    {
        push(&mut tokens, &mut seen, format!("f:{}", domain.1.to_lowercase()));
    }
    for header in ["X-Mailer", "User-Agent"] {
        if let Some(mailer) = message.header_raw(header).and_then(|value| value.split_whitespace().next()) {
            push(&mut tokens, &mut seen, format!("m:{}", mailer.to_lowercase()));
        }
    }
    if let Some(content_type) = message.root_part().content_type() {
        let subtype = content_type.subtype().unwrap_or_default();
        push(&mut tokens, &mut seen, format!("ct:{}/{}", content_type.ctype(), subtype).to_lowercase());
    }
    for site in link_sites {
        push(&mut tokens, &mut seen, format!("u:{site}"));
    }
    for part in message.attachments() {
        if let Some(ending) = part.attachment_name().and_then(|name| name.rsplit_once('.')) {
            push(&mut tokens, &mut seen, format!("a:{}", ending.1.trim().to_lowercase()));
        }
    }
    for word in message.subject().map(words).into_iter().flatten() {
        push(&mut tokens, &mut seen, format!("s:{word}"));
    }

    let mut text = String::new();
    for index in 0..message.text_body.len() {
        if text.len() >= MAX_TEXT {
            break;
        }
        if let Some(body) = message.body_text(index) {
            text.push_str(&body);
            text.push('\n');
        }
    }
    let mut previous: Option<String> = None;
    for word in words(&text) {
        if let Some(previous) = previous.replace(word.clone()) {
            push(&mut tokens, &mut seen, format!("{previous} {word}"));
        }
        push(&mut tokens, &mut seen, word);
    }
    tokens
}

/// The key the tokens are hashed with, made once per server.
pub(crate) async fn key(store: &Store) -> Option<[u8; 32]> {
    if let Ok(Some(value)) = store.setting(SECRET_KEY).await
        && let Ok(bytes) = hex::decode(value)
        && let Ok(key) = <[u8; 32]>::try_from(bytes)
    {
        return Some(key);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).ok()?;
    store.set_setting(SECRET_KEY, &hex::encode(key)).await.ok()?;
    Some(key)
}

/// Tokens as the numbers they are stored under.
pub(crate) fn hashed(key: &[u8; 32], tokens: &[String]) -> Vec<i64> {
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key);
    tokens
        .iter()
        .map(|token| {
            let tag = aws_lc_rs::hmac::sign(&key, token.as_bytes());
            let mut first = [0u8; 8];
            first.copy_from_slice(&tag.as_ref()[..8]);
            i64::from_le_bytes(first)
        })
        .collect()
}

/// Whether a scope learned enough to be asked.
pub(crate) fn has_learned_enough(totals: BayesTotals) -> bool {
    totals.spam >= MIN_LEARNED && totals.ham >= MIN_LEARNED
}

/// How strongly an unseen or rarely seen token is pulled towards undecided (Robinson's s and x).
const PRIOR_STRENGTH: f64 = 0.45;
const PRIOR: f64 = 0.5;
/// Tokens this close to undecided say nothing and are left out.
const MIN_DEVIATION: f64 = 0.1;
/// The most telling tokens are enough; more only add noise.
const MAX_CLUES: usize = 150;

/// How much a token points to spam, from how often it was seen in each kind of learned mail.
fn spamminess(spam: i64, ham: i64, totals: BayesTotals) -> Option<f64> {
    let seen = spam + ham;
    let spam_share = spam as f64 / totals.spam.max(1) as f64;
    let ham_share = ham as f64 / totals.ham.max(1) as f64;
    if seen == 0 || spam_share + ham_share == 0.0 {
        return None;
    }
    let probability = spam_share / (spam_share + ham_share);
    Some((PRIOR_STRENGTH * PRIOR + seen as f64 * probability) / (PRIOR_STRENGTH + seen as f64))
}

/// The chance of a chi-square value at least this large, for an even number of degrees of freedom.
fn chi2q(value: f64, freedom: usize) -> f64 {
    let half = value / 2.0;
    let mut term = (-half).exp();
    let mut sum = term;
    for i in 1..freedom / 2 {
        term *= half / i as f64;
        sum += term;
    }
    sum.min(1.0)
}

/// The chance that a message is spam, from 0 to 1, given its tokens and what one scope learned.
/// `None` while the scope has learned too little to be asked.
pub(crate) fn spam_chance(tokens: &[i64], counts: &HashMap<i64, (i64, i64)>, totals: BayesTotals) -> Option<f64> {
    if !has_learned_enough(totals) {
        return None;
    }
    let mut clues: Vec<f64> = tokens
        .iter()
        .filter_map(|token| counts.get(token))
        .filter_map(|&(spam, ham)| spamminess(spam, ham, totals))
        .filter(|chance| (chance - 0.5).abs() >= MIN_DEVIATION)
        .collect();
    if clues.is_empty() {
        return Some(0.5);
    }
    clues.sort_by(|a, b| (b - 0.5).abs().total_cmp(&(a - 0.5).abs()));
    clues.truncate(MAX_CLUES);
    let (spam_logs, ham_logs): (f64, f64) =
        clues.iter().fold((0.0, 0.0), |(spam, ham), chance| (spam + (1.0 - chance).ln(), ham + chance.ln()));
    let freedom = 2 * clues.len();
    let spam = 1.0 - chi2q(-2.0 * spam_logs, freedom);
    let ham = 1.0 - chi2q(-2.0 * ham_logs, freedom);
    Some((spam - ham + 1.0) / 2.0)
}

/// Points for a chance: none between 20 % and 80 %, then up to +5 for certain spam and down to −3
/// for certainly wanted mail.
pub(crate) fn points(chance: f64) -> f32 {
    let points = if chance >= 0.8 {
        5.0 * (chance - 0.8) / 0.2
    } else if chance <= 0.2 {
        -3.0 * (0.2 - chance) / 0.2
    } else {
        0.0
    };
    ((points * 10.0).round() / 10.0) as f32
}

/// How much a person's own chance counts next to the server's, once they learned enough: 60 %,
/// growing to 90 % by 500 learned messages.
pub(crate) fn personal_weight(totals: BayesTotals) -> f64 {
    let learned = (totals.spam + totals.ham) as f64;
    0.6 + 0.3 * ((learned - 2.0 * MIN_LEARNED as f64) / 400.0).clamp(0.0, 1.0)
}

/// The chance for one person: the server's, blended with their own once they learned enough.
pub(crate) fn blended(server: Option<f64>, person: Option<(f64, BayesTotals)>) -> Option<f64> {
    match (server, person) {
        (server, None) => server,
        (None, Some((own, _))) => Some(own),
        (Some(server), Some((own, totals))) => {
            let weight = personal_weight(totals);
            Some(weight * own + (1.0 - weight) * server)
        }
    }
}

/// The server's key for hashing tokens, made on first use and kept for the life of the process.
pub(crate) async fn context_key(ctx: &Context) -> Option<[u8; 32]> {
    ctx.bayes_key.get_or_try_init(|| async { key(&ctx.store).await.ok_or(()) }).await.ok().copied()
}

/// Learns queued messages in the background: reads each message once, splits it into tokens and counts
/// them for every scope that asked. A message that is gone by then is simply dropped.
pub async fn run_learning(smtp: Smtp, mut shutdown: watch::Receiver<bool>) {
    let ctx = smtp.inner.clone();
    loop {
        let jobs = match ctx.store.bayes_jobs(100).await {
            Ok(jobs) => jobs,
            Err(err) => {
                tracing::warn!(%err, "reading the Bayes learning queue failed");
                Vec::new()
            }
        };
        let key = if jobs.is_empty() { None } else { context_key(&ctx).await };
        let Some(key) = key else {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                _ = shutdown.changed() => return,
            }
        };
        let mut by_message: BTreeMap<String, Vec<BayesJob>> = BTreeMap::new();
        for job in jobs {
            by_message.entry(job.blob.as_str().to_owned()).or_default().push(job);
        }
        for jobs in by_message.into_values() {
            let raw = match ctx.store.blob(&jobs[0].blob).await {
                Ok(raw) => raw,
                Err(_) => {
                    for job in jobs {
                        let _ = ctx.store.drop_bayes_job(job.id).await;
                    }
                    continue;
                }
            };
            let tokens = tokio::task::spawn_blocking(move || super::content::learning_tokens(&raw, &key))
                .await
                .unwrap_or_default();
            for job in jobs {
                if let Err(err) = ctx.store.learn_bayes(job, tokens.clone()).await {
                    tracing::warn!(%err, "learning a message for the Bayes filter failed");
                }
            }
        }
        if *shutdown.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use mail_parser::MessageParser;

    use super::*;

    fn totals(spam: i64, ham: i64) -> BayesTotals {
        BayesTotals { spam, ham }
    }

    #[test]
    fn a_message_becomes_words_word_pairs_and_a_few_facts() {
        let raw = b"From: Shop <news@Shop.Example>\r\nX-Mailer: MassMailer 5.1\r\nSubject: Gratis Gewinnspiel 2026\r\n\r\nJetzt teilnehmen und 1000 Euro gewinnen! Ja ja\r\n";
        let message = MessageParser::default().parse(raw).unwrap();
        let tokens = tokens(&message, &["shop.example".to_owned()]);
        for expected in [
            "f:shop.example",
            "m:massmailer",
            "u:shop.example",
            "s:gratis",
            "s:gewinnspiel",
            "jetzt",
            "jetzt teilnehmen",
            "und euro",
            "euro gewinnen",
        ] {
            assert!(tokens.iter().any(|token| token == expected), "{expected} in {tokens:?}");
        }
        // Numbers and short words are no tokens, and every token is there once.
        assert!(!tokens.iter().any(|token| token == "s:2026" || token == "1000" || token == "ja"), "{tokens:?}");
        assert_eq!(tokens.len(), tokens.iter().collect::<HashSet<_>>().len());
    }

    #[test]
    fn hashing_is_keyed_and_stable() {
        let words = vec!["gratis".to_owned(), "elternabend".to_owned()];
        let first = hashed(&[1; 32], &words);
        assert_eq!(first, hashed(&[1; 32], &words));
        assert_ne!(first, hashed(&[2; 32], &words), "another server's key gives other numbers");
        assert_ne!(first[0], first[1]);
    }

    #[test]
    fn chi_square_tails_look_right() {
        assert!((chi2q(0.0, 2) - 1.0).abs() < 1e-12);
        assert!((chi2q(2.0, 2) - (-1.0f64).exp()).abs() < 1e-12);
        assert!(chi2q(200.0, 4) < 1e-20);
    }

    #[test]
    fn learned_tokens_decide_and_too_little_learning_says_nothing() {
        let counts: HashMap<i64, (i64, i64)> =
            [(1, (45, 1)), (2, (40, 2)), (3, (1, 48)), (4, (2, 45)), (5, (20, 20))].into();
        let learned = totals(50, 50);
        let spam = spam_chance(&[1, 2, 5, 99], &counts, learned).unwrap();
        let wanted = spam_chance(&[3, 4, 5], &counts, learned).unwrap();
        assert!(spam > 0.95, "{spam}");
        assert!(wanted < 0.05, "{wanted}");
        assert_eq!(spam_chance(&[5, 99], &counts, learned), Some(0.5), "nothing telling is undecided");
        assert_eq!(spam_chance(&[1, 2], &counts, totals(49, 500)), None, "not enough spam learned yet");
    }

    #[test]
    fn chances_become_points_and_a_persons_own_view_counts_more_as_they_learn() {
        assert_eq!((points(0.5), points(0.8), points(0.9), points(1.0)), (0.0, 0.0, 2.5, 5.0));
        assert_eq!((points(0.2), points(0.0)), (0.0, -3.0));
        assert!((personal_weight(totals(50, 50)) - 0.6).abs() < 1e-9);
        assert!((personal_weight(totals(300, 300)) - 0.9).abs() < 1e-9);
        assert_eq!(blended(Some(0.2), None), Some(0.2));
        assert_eq!(blended(None, Some((0.9, totals(50, 50)))), Some(0.9));
        let mixed = blended(Some(0.1), Some((0.9, totals(50, 50)))).unwrap();
        assert!((mixed - (0.6 * 0.9 + 0.4 * 0.1)).abs() < 1e-9);
    }
}
