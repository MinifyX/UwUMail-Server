//! How much an incoming message looks like spam, and what should happen with it.
//!
//! The score is a sum of small judgements: every rule adds or takes away points and says why, so
//! the portal can show what happened instead of only the result. Nothing here reads the message
//! text yet, that comes with the rule set; this is what can be told from the sending server, the
//! authentication results and how that sender behaved before.
//!
//! A rule never punishes a question that could not be asked. A blocklist that refuses to answer,
//! a resolver that is down or a missing DMARC record are worth nothing, in either direction.

mod attachments;
mod bayes;
mod content;
mod feeds;
mod html;
mod links;
mod lists;
mod words;

pub use bayes::run_learning;
pub use feeds::{FEEDS, Feed, feed, run_list_updates};
pub(crate) use feeds::{refresh_feed, refresh_word_source};

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use mail_auth::ResolverCache;
use serde::Serialize;
use uwumail_store::Greylist;

use crate::Context;
use crate::checks::Verdict;
use crate::config::SpamConfig;
use crate::dnscheck::{
    BLOCKLISTS, Blocklist, ListingStatus, blocklist_status_with, domain_list_answers_with, reverse_names_with,
};
use crate::reachability::generic_reverse_name;
use crate::servercheck::is_private;

/// How long a blocklist answer is reused, so a sender delivering a lot is not looked up again and
/// again. An unusable answer is retried sooner, because it is usually a resolver problem here.
const LISTING_TTL: Duration = Duration::from_secs(3600);
const UNKNOWN_TTL: Duration = Duration::from_secs(600);

pub(crate) type BlocklistCache = crate::dns::TtlCache<(IpAddr, &'static str), ListingStatus>;
pub(crate) type DomainCache = crate::dns::TtlCache<String, DomainListing>;

/// Spamhaus' list of domains seen in spam, phishing and malware, asked about link domains.
const DBL_ZONE: &str = "dbl.spamhaus.org";

/// What Spamhaus DBL says about a domain, from harmless to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DomainListing {
    Clean,
    /// The question was refused or got no answer, which says nothing.
    Unknown,
    /// A real domain that spammers abuse, e.g. a hacked site.
    Abused,
    Spam,
    /// Phishing, malware or a botnet's control server.
    Malicious,
}

/// What DBL's answers mean. 127.0.1.2 lists a spam domain, .4 to .6 phishing, malware and botnet
/// domains, .102 to .106 legitimate domains that are being abused. 127.0.1.255 and 127.255.255.x
/// mean the question was not allowed or not understood.
fn domain_listing(codes: &[Ipv4Addr]) -> DomainListing {
    codes
        .iter()
        .map(|code| match code.octets() {
            [127, 0, 1, 4..=6] => DomainListing::Malicious,
            [127, 0, 1, 2..=99] => DomainListing::Spam,
            [127, 0, 1, 102..=199] => DomainListing::Abused,
            _ => DomainListing::Unknown,
        })
        .max()
        .unwrap_or(DomainListing::Clean)
}

fn domain_rule(listing: DomainListing) -> Option<(&'static str, f32)> {
    match listing {
        DomainListing::Malicious => Some(("SPAMHAUS_DBL_MALICIOUS", 6.0)),
        DomainListing::Spam => Some(("SPAMHAUS_DBL", 4.0)),
        DomainListing::Abused => Some(("SPAMHAUS_DBL_ABUSED", 1.5)),
        DomainListing::Clean | DomainListing::Unknown => None,
    }
}

/// Asks Spamhaus DBL about link domains at the same time, reusing recent answers.
async fn domain_listings(ctx: &Context, domains: &[String]) -> Vec<(String, DomainListing)> {
    let mut answers = Vec::with_capacity(domains.len());
    let mut asking = tokio::task::JoinSet::new();
    for domain in domains {
        match ctx.domain_cache.get(domain) {
            Some(listing) => answers.push((domain.clone(), listing)),
            None => {
                let resolver = ctx.authenticator.resolver().clone();
                let domain = domain.clone();
                asking.spawn(async move {
                    let codes = in_time(domain_list_answers_with(&resolver, &domain, DBL_ZONE)).await.flatten();
                    let listing = codes.map_or(DomainListing::Unknown, |codes| domain_listing(&codes));
                    (domain, listing)
                });
            }
        }
    }
    while let Some(joined) = asking.join_next().await {
        let Ok((domain, listing)) = joined else { continue };
        let ttl = if listing == DomainListing::Unknown { UNKNOWN_TTL } else { LISTING_TTL };
        ctx.domain_cache.insert(domain.clone(), listing, Instant::now() + ttl);
        answers.push((domain, listing));
    }
    answers.sort_by_key(|(domain, _)| domains.iter().position(|known| known == domain));
    answers
}

/// A question the filter asks never holds up the SMTP dialogue for long. A resolver that does not
/// answer in time means the question was not asked, which is worth no points in either direction.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

async fn in_time<T>(lookup: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await.ok()
}

/// The rules that come from what the filter learned: the sender's reputation and the Bayes filter.
const LEARNED_RULES: [&str; 4] = ["KNOWN_GOOD_SENDER", "KNOWN_JUNK_SENDER", "BAYES_SPAM", "BAYES_HAM"];

/// From this score on the message's own merits, a message teaches the Bayes filter what spam is
/// without anyone marking it.
pub(crate) const AUTOLEARN_SPAM: f32 = 12.0;

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
    /// The message's Bayes tokens, to weigh them against a person's own knowledge too.
    #[serde(skip)]
    pub(crate) tokens: Vec<i64>,
    /// The chance of spam by what the whole server learned, when it learned enough.
    #[serde(skip)]
    pub(crate) server_chance: Option<f64>,
    /// The subject and visible text, to look for a domain's and a person's own word lists too.
    #[serde(skip)]
    pub(crate) subject: String,
    #[serde(skip)]
    pub(crate) text: String,
}

/// The compiled word lists and built-in lists, kept between messages.
pub(crate) type CompiledLists = tokio::sync::Mutex<Option<std::sync::Arc<lists::Lists>>>;

impl Score {
    fn add(&mut self, rule: &'static str, points: f32, detail: Option<String>) {
        self.points += points;
        self.hits.push(Hit { rule, points, detail });
    }

    /// The score on the message's own merits, without what the filter learned: the sender's
    /// reputation and the Bayes filter. Both are fed from this, so what was learned cannot keep
    /// itself going: a sender that was filed as junk and has since fixed its setup recovers instead of
    /// staying junk for good.
    pub fn points_on_its_own(&self) -> f32 {
        self.hits.iter().filter(|hit| !LEARNED_RULES.contains(&hit.rule)).map(|hit| hit.points).sum()
    }

    /// The rules that fired, for the `X-Spam-Status` header: `none` when nothing did, the way
    /// SpamAssassin writes it, so a filter looking for a word after `tests=` always finds one.
    pub fn tests(&self) -> String {
        if self.hits.is_empty() {
            return "none".into();
        }
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
///
/// The network is only as right as the address the server sees: the connection's own, the server a
/// trusted relay talked to (from its Received header), or the sender the UwUMail Gateway reports
/// through the tunnel. Were that address ever wrong, e.g. the gateway's own, every sender without
/// DMARC would share one reputation, and one wave of spam would spoil it for all of them. Changes
/// to relays or the tunnel therefore touch this too.
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
    raw: &[u8],
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
    // What the message itself shows, read on a blocking thread because big messages take a moment,
    // and then what the domain blocklist says about its links.
    let message = async {
        let examination = if raw.len() <= content::MAX_MESSAGE {
            let (raw, now) = (raw.to_vec(), crate::now());
            let dmarc_passed = verdict.is_some_and(|verdict| verdict.dmarc_passed);
            let key = if config.bayes { bayes::context_key(ctx).await } else { None };
            tokio::task::spawn_blocking(move || content::examine(&raw, now, dmarc_passed, key.as_ref()))
                .await
                .unwrap_or_default()
        } else {
            content::Examination::default()
        };
        let domains =
            if config.blocklists { domain_listings(ctx, &examination.link_domains).await } else { Vec::new() };
        let chance = if examination.tokens.is_empty() { None } else { server_chance(ctx, &examination.tokens).await };
        (examination, domains, chance)
    };
    let (names, listed, (examination, domains, chance)) = tokio::join!(reverse, blocklists, message);
    let content::Examination {
        hits: content_hits,
        tokens,
        subject,
        text,
        urls,
        link_hosts,
        files,
        from_domain,
        reply_to_domain,
        ..
    } = examination;

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

    for hit in content_hits {
        score.add(hit.rule, hit.points, hit.detail);
    }
    for (domain, listing) in domains {
        if let Some((rule, points)) = domain_rule(listing)
            && !score.hits.iter().any(|hit| hit.rule == rule)
        {
            score.add(rule, points, Some(domain));
        }
    }

    // What the whole server's Bayes filter learned; a person's own knowledge is weighed per recipient.
    if let Some(chance) = chance {
        let points = bayes::points(chance);
        let detail = Some(format!("{:.0} %", chance * 100.0));
        if points > 0.0 {
            score.add("BAYES_SPAM", points, detail);
        } else if points < 0.0 {
            score.add("BAYES_HAM", points, detail);
        }
    }
    score.tokens = tokens;
    score.server_chance = chance;

    // The whole server's word lists, where a domain's and a person's own count per recipient, and what
    // the built-in lists know.
    let lists = lists::current(ctx).await;
    if !subject.is_empty() || !text.is_empty() {
        let found = lists.words.server.find(&subject, &text);
        if found.points > 0.0 {
            score.add("BAD_WORDS", tenths(found.points), Some(found.detail()));
        }
    }
    let known = &lists.feeds;
    if let Some(url) = urls.iter().find(|url| known.malware_links.contains(*url)) {
        let host = url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_owned));
        score.add("MALWARE_LINK", 10.0, host);
    }
    if let Some((name, _)) =
        files.iter().find(|(_, hashes)| hashes.iter().any(|hash| known.malware_files.contains(hash)))
    {
        score.add("MALWARE_ATTACHMENT", 10.0, Some(name.clone()));
    }
    if let Some(domain) = from_domain.as_deref().and_then(|domain| feeds::listed(&known.disposable, domain)) {
        score.add("DISPOSABLE_FROM", 1.5, Some(domain.to_owned()));
    }
    // A sender that is no freemail address, whose replies go to one: the classic of scams.
    if let Some(reply_to) = reply_to_domain.as_deref()
        && from_domain.as_deref().is_some_and(|from| feeds::listed(&known.freemail, from).is_none())
        && let Some(provider) = feeds::listed(&known.freemail, reply_to)
    {
        score.add("FREEMAIL_REPLYTO", 2.0, Some(provider.to_owned()));
    }
    if let Some(host) = link_hosts.iter().find_map(|host| feeds::listed(&known.redirectors, host)) {
        score.add("LINK_SHORTENER", 0.5, Some(host.to_owned()));
    }
    score.subject = subject;
    score.text = text;

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

/// The chance of spam by what the whole server's Bayes filter learned, when it learned enough.
async fn server_chance(ctx: &Context, tokens: &[i64]) -> Option<f64> {
    let totals = ctx.store.bayes_totals(None).await.ok()?;
    if !bayes::has_learned_enough(totals) {
        return None;
    }
    let counts = ctx.store.bayes_counts(None, tokens.to_vec()).await.ok()?;
    bayes::spam_chance(tokens, &counts, totals)
}

/// How many points a person's own Bayes knowledge adds to or takes from the server's verdict for them,
/// once they marked enough mail themselves.
pub(crate) async fn personal_bayes_points(ctx: &Context, config: &SpamConfig, score: &Score, account_id: i64) -> f32 {
    if !config.bayes || score.tokens.is_empty() {
        return 0.0;
    }
    let Ok(totals) = ctx.store.bayes_totals(Some(account_id)).await else { return 0.0 };
    if !bayes::has_learned_enough(totals) {
        return 0.0;
    }
    let Ok(counts) = ctx.store.bayes_counts(Some(account_id), score.tokens.clone()).await else { return 0.0 };
    let Some(own) = bayes::spam_chance(&score.tokens, &counts, totals) else { return 0.0 };
    let blended = bayes::blended(score.server_chance, Some((own, totals))).unwrap_or(own);
    bayes::points(blended) - score.server_chance.map_or(0.0, bayes::points)
}

fn tenths(points: f32) -> f32 {
    (points * 10.0).round() / 10.0
}

/// How many points a domain's and a person's own word lists add for one recipient, within what word lists
/// may add together.
pub(crate) async fn personal_word_points(ctx: &Context, score: &Score, account_id: i64, domain: &str) -> f32 {
    if score.subject.is_empty() && score.text.is_empty() {
        return 0.0;
    }
    let lists = lists::current(ctx).await;
    let own = [lists.words.domains.get(domain), lists.words.accounts.get(&account_id)]
        .into_iter()
        .flatten()
        .map(|scope| scope.find(&score.subject, &score.text).points)
        .sum::<f32>();
    let server: f32 = score.hits.iter().filter(|hit| hit.rule == "BAD_WORDS").map(|hit| hit.points).sum();
    tenths(own.min((uwumail_store::WORD_POINTS_MAX - server).max(0.0)))
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
            from_address: None,
            from_verified: dmarc_passed,
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
    fn a_clean_message_says_that_no_rule_fired() {
        let headers = headers(&Score::default(), false, 5.0);
        assert!(headers.contains("X-Spam-Status: No, score=0.0 required=5.0 tests=none\r\n"), "{headers}");
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
        assert!((score.points_on_its_own() - 1.5).abs() < f32::EPSILON);
        assert_eq!(outcome(&SpamConfig::default(), score.points_on_its_own()), Outcome::Deliver);
    }

    #[test]
    fn domain_blocklist_answers_are_read_by_how_bad_they_are() {
        let code = |last: u8| Ipv4Addr::new(127, 0, 1, last);
        assert_eq!(domain_listing(&[]), DomainListing::Clean);
        assert_eq!(domain_listing(&[code(2)]), DomainListing::Spam);
        assert_eq!(domain_listing(&[code(4)]), DomainListing::Malicious);
        assert_eq!(domain_listing(&[code(102), code(5)]), DomainListing::Malicious);
        assert_eq!(domain_listing(&[code(103)]), DomainListing::Abused);
        // Refused or not understood: never a listing.
        assert_eq!(domain_listing(&[code(255)]), DomainListing::Unknown);
        assert_eq!(domain_listing(&[Ipv4Addr::new(127, 255, 255, 254)]), DomainListing::Unknown);
        assert_eq!(domain_rule(DomainListing::Unknown), None);
    }

    /// Spamhaus lists dbltest.com as a spam domain for testing; run with
    /// `cargo test -p uwumail-smtp domain_blocklist_test_entry -- --ignored`.
    #[tokio::test]
    #[ignore = "needs the internet and a resolver Spamhaus answers"]
    async fn domain_blocklist_test_entry() {
        let resolver = mail_auth::MessageAuthenticator::new_system_conf().unwrap();
        let codes = domain_list_answers_with(resolver.resolver(), "dbltest.com", DBL_ZONE).await.unwrap();
        assert_eq!(domain_listing(&codes), DomainListing::Spam);
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
