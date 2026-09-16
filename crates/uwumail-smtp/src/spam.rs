//! How much an incoming message looks like spam, and what should happen with it.
//!
//! The score is a sum of small judgements: every rule adds or takes away points and says why, so
//! the portal can show what happened instead of only the result. Nothing here reads the message
//! text yet, that comes with the rule set; this is what can be told from the sending server, the
//! authentication results and how that sender behaved before.
//!
//! A rule never punishes a question that could not be asked. A blocklist that refuses to answer,
//! a resolver that is down or a missing DMARC record are worth nothing, in either direction.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use mail_auth::ResolverCache;
use serde::Serialize;
use uwumail_store::Greylist;

use crate::Context;
use crate::checks::Verdict;
use crate::config::SpamConfig;
use crate::dnscheck::{BLOCKLISTS, Blocklist, ListingStatus, blocklist_status_with, reverse_names_with};
use crate::reachability::generic_reverse_name;
use crate::servercheck::is_private;

/// How long a blocklist answer is reused, so a sender delivering a lot is not looked up again and
/// again. An unusable answer is retried sooner, because it is usually a resolver problem here.
const LISTING_TTL: Duration = Duration::from_secs(3600);
const UNKNOWN_TTL: Duration = Duration::from_secs(600);

pub(crate) type BlocklistCache = crate::dns::TtlCache<(IpAddr, &'static str), ListingStatus>;

/// A question the filter asks never holds up the SMTP dialogue for long. A resolver that does not
/// answer in time means the question was not asked, which is worth no points in either direction.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

async fn in_time<T>(lookup: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await.ok()
}

/// The rules that come from the reputation itself.
const REPUTATION_RULES: [&str; 2] = ["KNOWN_GOOD_SENDER", "KNOWN_JUNK_SENDER"];

/// One reason the score is what it is.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub rule: &'static str,
    pub points: f32,
    /// What exactly was seen, e.g. the reverse name or which blocklist answered.
    pub detail: Option<String>,
}

/// What the filter thinks of a message.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Score {
    pub points: f32,
    pub hits: Vec<Hit>,
}

impl Score {
    fn add(&mut self, rule: &'static str, points: f32, detail: Option<String>) {
        self.points += points;
        self.hits.push(Hit { rule, points, detail });
    }

    /// The score without what the sender's reputation added or took away. The reputation is
    /// counted from this, so a sender's past cannot keep itself going: one that was filed as junk
    /// and has since fixed its setup recovers instead of staying junk for good.
    pub fn points_without_reputation(&self) -> f32 {
        self.hits.iter().filter(|hit| !REPUTATION_RULES.contains(&hit.rule)).map(|hit| hit.points).sum()
    }

    /// The rules that fired, for the `X-Spam-Status` header.
    pub fn tests(&self) -> String {
        self.hits.iter().map(|hit| hit.rule).collect::<Vec<_>>().join(",")
    }
}

/// What the score means on its own, before greylisting has a say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Deliver,
    /// Not bad enough for Junk, but not trusted either: worth greylisting an unknown sender.
    Suspicious,
    Junk,
    Reject,
}

pub fn outcome(config: &SpamConfig, points: f32) -> Outcome {
    if let Some(reject) = config.reject_score
        && points >= reject
    {
        return Outcome::Reject;
    }
    if points >= config.junk_score {
        return Outcome::Junk;
    }
    if points >= config.greylist_score {
        return Outcome::Suspicious;
    }
    Outcome::Deliver
}

/// The network a sending server belongs to: a /24 or /64. Senders retry from a neighbouring
/// address often enough that a single address is too narrow to recognise them by.
pub fn network_of(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            format!("{a}.{b}.{c}.0/24")
        }
        IpAddr::V6(v6) => {
            let parts = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", parts[0], parts[1], parts[2], parts[3])
        }
    }
}

/// Who a message counts for in the reputation: the From domain when DMARC vouches for it, the
/// sending network otherwise, because an unauthenticated domain name can be anyone's.
pub fn reputation_subject(ip: IpAddr, verdict: Option<&Verdict>) -> String {
    match verdict {
        Some(Verdict { dmarc_passed: true, from_domain: Some(domain), .. }) if !domain.is_empty() => {
            format!("domain:{domain}")
        }
        _ => format!("network:{}", network_of(ip)),
    }
}

/// A server announces itself with a name it owns. An address literal, something without a dot or
/// our own name are all things a normal mail server does not say.
fn helo_looks_wrong(helo: &str, hostname: &str) -> bool {
    let helo = helo.trim();
    helo.is_empty()
        || helo.starts_with('[')
        || !helo.contains('.')
        || helo.parse::<IpAddr>().is_ok()
        || helo.eq_ignore_ascii_case(hostname)
}

/// What a listing on each blocklist is worth. Spamhaus ZEN is the most careful of the three, and
/// the only one that answers for IPv6 at all.
fn blocklist_rule(list: &Blocklist) -> (&'static str, f32) {
    match list.zone {
        "zen.spamhaus.org" => ("SPAMHAUS_ZEN", 4.0),
        "bl.spamcop.net" => ("SPAMCOP", 2.5),
        _ => ("BARRACUDA", 2.0),
    }
}

/// Asks every blocklist about `ip` at the same time, reusing recent answers. A list that does not
/// answer in time counts as unknown for a while, so a broken resolver slows down one message and
/// not every one after it.
async fn listings(ctx: &Context, ip: IpAddr) -> Vec<(&'static Blocklist, ListingStatus)> {
    let mut answers = Vec::with_capacity(BLOCKLISTS.len());
    let mut asking = tokio::task::JoinSet::new();
    for list in BLOCKLISTS {
        match ctx.blocklist_cache.get(&(ip, list.zone)) {
            Some(status) => answers.push((list, status)),
            None => {
                let resolver = ctx.authenticator.resolver().clone();
                asking.spawn(async move {
                    let listing = in_time(blocklist_status_with(&resolver, ip, list)).await;
                    (list, listing.map_or(ListingStatus::Unknown, |listing| listing.status))
                });
            }
        }
    }
    while let Some(joined) = asking.join_next().await {
        let Ok((list, status)) = joined else { continue };
        let ttl = if status == ListingStatus::Unknown { UNKNOWN_TTL } else { LISTING_TTL };
        ctx.blocklist_cache.insert((ip, list.zone), status, Instant::now() + ttl);
        answers.push((list, status));
    }
    // Answers arrive in any order; the score should read the same every time.
    answers.sort_by_key(|(list, _)| BLOCKLISTS.iter().position(|known| known.zone == list.zone));
    answers
}

/// Scores a message from another server. `verdict` is what SPF, DKIM and DMARC said, when they
/// were asked at all.
///
/// Mail from our own network is not judged at all: a relay in front is looked through already
/// (the address here is the server it talked to), so what is left is a scanner, a NAS or another
/// machine in the house, which has no reverse name, no blocklist entry and usually no DKIM.
pub async fn score(
    ctx: &Context,
    config: &SpamConfig,
    ip: IpAddr,
    helo: &str,
    verdict: Option<&Verdict>,
) -> Option<Score> {
    if is_private(ip) {
        return None;
    }
    let mut score = Score::default();

    if let Some(verdict) = verdict {
        if verdict.dmarc_failed {
            score.add("DMARC_FAIL", 2.5, None);
        }
        if verdict.spf_failed {
            score.add("SPF_FAIL", 2.0, None);
        }
        if verdict.dkim_failed {
            score.add("DKIM_FAIL", 1.0, None);
        }
        if !verdict.sender_verified {
            score.add("NO_AUTH", 1.0, None);
        }
    }

    if helo_looks_wrong(helo, &ctx.hostname) {
        score.add("HELO_NOT_A_NAME", 1.0, Some(helo.trim().to_owned()));
    }

    let blocklists = async { if config.blocklists { listings(ctx, ip).await } else { Vec::new() } };
    let reverse = in_time(reverse_names_with(ctx.authenticator.resolver(), ip));
    let (names, listed) = tokio::join!(reverse, blocklists);

    // No answer at all is not the same as no reverse name, so a timeout costs nothing.
    if let Some(names) = names {
        match names.first() {
            None => score.add("NO_REVERSE_DNS", 1.5, None),
            Some(name) if generic_reverse_name(name, ip) => {
                score.add("GENERIC_REVERSE_DNS", 1.0, Some(name.clone()));
            }
            Some(_) => {}
        }
    }

    for (list, status) in listed {
        if status == ListingStatus::Listed {
            let (rule, points) = blocklist_rule(list);
            score.add(rule, points, Some(list.name.to_owned()));
        }
    }

    match ctx.store.reputation(reputation_subject(ip, verdict)).await {
        Ok(reputation) if reputation.is_known() => {
            let share = reputation.junk_share();
            if share <= 0.1 {
                score.add("KNOWN_GOOD_SENDER", -2.5, Some(format!("{} delivered before", reputation.good)));
            } else if share >= 0.5 {
                score.add("KNOWN_JUNK_SENDER", 3.0, Some(format!("{} of them junk", reputation.junk)));
            }
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(%err, "reading the reputation of a sender failed"),
    }

    Some(score)
}

/// Asks a suspicious sender to come back later, unless it already did. `Some(seconds)` means the
/// message should be refused for now.
pub async fn greylist_wait(
    ctx: &Context,
    config: &SpamConfig,
    ip: IpAddr,
    sender: &str,
    recipient: &str,
) -> Option<i64> {
    let network = network_of(ip);
    let sender = sender.to_ascii_lowercase();
    let recipient = recipient.to_ascii_lowercase();
    let delay = config.greylist_delay_secs as i64;
    match ctx.store.greylist(network, sender, recipient, delay).await {
        Ok(Greylist::Wait { seconds }) => Some(seconds.max(1)),
        Ok(Greylist::Pass) => None,
        // A storage problem must not swallow mail: let it through and say so.
        Err(err) => {
            tracing::warn!(%err, "greylisting is not working, letting the message through");
            None
        }
    }
}

/// The headers that tell a mail app, and a curious person, what the filter thought.
pub fn headers(score: &Score, junk: bool, threshold: f32) -> String {
    let status = if junk { "Yes" } else { "No" };
    format!(
        "X-Spam-Score: {:.1}\r\nX-Spam-Status: {status}, score={:.1} required={:.1} tests={}\r\n",
        score.points,
        score.points,
        threshold,
        score.tests()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(dmarc_passed: bool, domain: Option<&str>) -> Verdict {
        Verdict {
            header: String::new(),
            action: crate::checks::Action::Accept,
            sender_verified: dmarc_passed,
            dmarc_passed,
            spf_failed: false,
            dkim_failed: false,
            dmarc_failed: false,
            from_domain: domain.map(str::to_owned),
        }
    }

    #[test]
    fn networks_group_neighbouring_addresses() {
        assert_eq!(network_of("192.0.2.77".parse().unwrap()), "192.0.2.0/24");
        assert_eq!(network_of("2001:db8:1:2:3:4:5:6".parse().unwrap()), "2001:db8:1:2::/64");
    }

    #[test]
    fn thresholds_decide_in_the_right_order() {
        let mut config = SpamConfig::default();
        assert_eq!(outcome(&config, 0.0), Outcome::Deliver);
        assert_eq!(outcome(&config, 1.9), Outcome::Deliver);
        assert_eq!(outcome(&config, 2.0), Outcome::Suspicious);
        assert_eq!(outcome(&config, 4.9), Outcome::Suspicious);
        assert_eq!(outcome(&config, 5.0), Outcome::Junk);
        // Refusing stays off until it is asked for, however bad the score is.
        assert_eq!(outcome(&config, 99.0), Outcome::Junk);
        config.reject_score = Some(12.0);
        assert_eq!(outcome(&config, 11.9), Outcome::Junk);
        assert_eq!(outcome(&config, 12.0), Outcome::Reject);
    }

    #[test]
    fn a_greeting_has_to_be_a_name_that_is_not_ours() {
        let ours = "mail.uwu.test";
        assert!(helo_looks_wrong("", ours));
        assert!(helo_looks_wrong("localhost", ours));
        assert!(helo_looks_wrong("[192.0.2.1]", ours));
        assert!(helo_looks_wrong("192.0.2.1", ours));
        assert!(helo_looks_wrong("MAIL.UWU.TEST", ours));
        assert!(!helo_looks_wrong("mail.example.com", ours));
    }

    #[test]
    fn reputation_follows_the_domain_only_when_dmarc_vouches_for_it() {
        let ip: IpAddr = "192.0.2.77".parse().unwrap();
        assert_eq!(reputation_subject(ip, None), "network:192.0.2.0/24");
        assert_eq!(reputation_subject(ip, Some(&verdict(true, Some("example.com")))), "domain:example.com");
        // Without DMARC the From domain could be anyone's, so the network counts instead.
        assert_eq!(reputation_subject(ip, Some(&verdict(false, Some("example.com")))), "network:192.0.2.0/24");
        assert_eq!(reputation_subject(ip, Some(&verdict(true, None))), "network:192.0.2.0/24");
    }

    #[test]
    fn the_header_says_what_fired() {
        let mut score = Score::default();
        score.add("SPF_FAIL", 2.0, None);
        score.add("SPAMHAUS_ZEN", 4.0, Some("Spamhaus ZEN".to_owned()));
        let headers = headers(&score, true, 5.0);
        assert!(headers.contains("X-Spam-Score: 6.0\r\n"));
        assert!(headers.contains("X-Spam-Status: Yes, score=6.0 required=5.0 tests=SPF_FAIL,SPAMHAUS_ZEN\r\n"));
    }

    #[test]
    fn the_reputation_is_counted_without_its_own_points() {
        let mut score = Score::default();
        score.add("NO_REVERSE_DNS", 1.5, None);
        score.add("KNOWN_JUNK_SENDER", 3.0, None);
        // Junk because of its past, but on its own merits this message is fine.
        assert_eq!(outcome(&SpamConfig::default(), score.points), Outcome::Suspicious);
        assert!((score.points_without_reputation() - 1.5).abs() < f32::EPSILON);
        assert_eq!(outcome(&SpamConfig::default(), score.points_without_reputation()), Outcome::Deliver);
    }

    #[test]
    fn a_known_good_sender_outweighs_a_small_complaint() {
        let mut score = Score::default();
        score.add("NO_AUTH", 1.0, None);
        score.add("KNOWN_GOOD_SENDER", -2.5, None);
        assert!(score.points < 0.0);
        assert_eq!(outcome(&SpamConfig::default(), score.points), Outcome::Deliver);
    }
}
