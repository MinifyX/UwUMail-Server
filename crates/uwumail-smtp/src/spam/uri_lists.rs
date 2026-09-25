//! SURBL and URIBL: lists of domains that show up in the links of spam, phishing and malware mail.
//!
//! Both answer one question per registrable domain (`news.shop.example.org` is asked as
//! `example.org`) with an address in 127.0.0.0/24 whose last octet is a bit mask: each bit is one
//! of their sub-lists. The lowest bit is not a listing but a refusal: URIBL answers 127.0.0.1 and
//! SURBL sets it (up to 127.0.0.255) when the question came through a public resolver or from a
//! server that asks too much. A refusal is worth nothing and is logged once, so an admin finds out
//! why the lists never count.
//!
//! Both are free only for small servers and do not answer through public resolvers such as
//! Google's 8.8.8.8, so they stay off unless `spam.uri_blocklists` is switched on.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use mail_auth::ResolverCache;

use super::{LISTING_TTL, UNKNOWN_TTL, in_time};
use crate::Context;
use crate::dnscheck::domain_list_answers_with;

/// How many registrable domains are asked about per message, on each list.
const MAX_DOMAINS: usize = 8;

/// A link blocklist and what the bits of its answers mean.
pub(crate) struct UriList {
    pub name: &'static str,
    pub zone: &'static str,
    /// Each sub-list: its bit, the rule it fires and what that is worth.
    bits: &'static [(u8, &'static str, f32)],
    /// Whether a refusal was logged already.
    warned: AtomicBool,
}

/// SURBL's multi list: phishing, malware, abused (spam sites on otherwise real hosting) and cracked
/// sites (real sites taken over). Bits 2, 4 and 32 belong to lists SURBL retired.
/// URIBL's multi list: black is spam domains, red new domains seen in spam, grey domains that also
/// send bulk mail people signed up for, hence worth the least.
pub(crate) static URI_LISTS: [UriList; 2] = [
    UriList {
        name: "SURBL",
        zone: "multi.surbl.org",
        bits: &[(8, "SURBL_PH", 6.0), (16, "SURBL_MW", 6.0), (64, "SURBL_ABUSE", 4.0), (128, "SURBL_CR", 2.0)],
        warned: AtomicBool::new(false),
    },
    UriList {
        name: "URIBL",
        zone: "multi.uribl.com",
        bits: &[(2, "URIBL_BLACK", 4.0), (8, "URIBL_RED", 1.0), (4, "URIBL_GREY", 0.5)],
        warned: AtomicBool::new(false),
    },
];

pub(crate) type UriCache = crate::dns::TtlCache<(String, &'static str), UriAnswer>;

/// What a list said about one domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UriAnswer {
    /// Not listed.
    Clean,
    /// No answer in time, or not one that could be read.
    Unknown,
    /// The list would not answer this server; see the module comment.
    Refused,
    /// Listed; the bits of the answer.
    Listed(u8),
}

/// Reads a list's answers. Anything outside 127.0.0.0/24 is not an answer this kind of list gives,
/// and the lowest bit means the question was refused.
pub(crate) fn read_answer(codes: &[Ipv4Addr]) -> UriAnswer {
    let mut mask = 0u8;
    for code in codes {
        match code.octets() {
            [127, 0, 0, last] if last & 1 == 1 => return UriAnswer::Refused,
            [127, 0, 0, last] => mask |= last,
            _ => return UriAnswer::Unknown,
        }
    }
    if mask == 0 { UriAnswer::Clean } else { UriAnswer::Listed(mask) }
}

impl UriList {
    /// The rules an answer fires, worst first.
    pub(crate) fn rules(&self, answer: UriAnswer) -> Vec<(&'static str, f32)> {
        let UriAnswer::Listed(mask) = answer else { return Vec::new() };
        self.bits.iter().filter(|(bit, _, _)| mask & bit != 0).map(|&(_, rule, points)| (rule, points)).collect()
    }

    fn note_refusal(&self) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                list = self.name,
                zone = self.zone,
                "the link blocklist refuses to answer this server, so it never counts: it does not answer through \
                 public resolvers such as 8.8.8.8, and is free only for small servers"
            );
        }
    }
}

/// The registrable domains of link hosts, as these lists want them: `click.mailer.example.org`
/// becomes `example.org`. Names without a known public suffix are left out.
pub(crate) fn registrable_domains(hosts: &[String]) -> Vec<String> {
    let mut domains: Vec<String> = Vec::new();
    for host in hosts {
        if !psl::suffix(host.as_bytes()).is_some_and(|suffix| suffix.is_known()) {
            continue;
        }
        let Some(domain) = psl::domain_str(host) else { continue };
        if !domains.iter().any(|known| known == domain) {
            domains.push(domain.to_owned());
            if domains.len() == MAX_DOMAINS {
                break;
            }
        }
    }
    domains
}

/// Asks both lists about the registrable domains of `hosts` at the same time, reusing recent
/// answers. Only listings come back, in a fixed order.
pub(crate) async fn listings(ctx: &Context, hosts: &[String]) -> Vec<(String, &'static UriList, UriAnswer)> {
    let domains = registrable_domains(hosts);
    let mut answers = Vec::new();
    let mut asking = tokio::task::JoinSet::new();
    for domain in &domains {
        for list in &URI_LISTS {
            match ctx.uri_cache.get(&(domain.clone(), list.zone)) {
                Some(answer) => answers.push((domain.clone(), list, answer)),
                None => {
                    let resolver = ctx.authenticator.resolver().clone();
                    let domain = domain.clone();
                    asking.spawn(async move {
                        let codes = in_time(domain_list_answers_with(&resolver, &domain, list.zone)).await.flatten();
                        let answer = codes.map_or(UriAnswer::Unknown, |codes| read_answer(&codes));
                        (domain, list, answer)
                    });
                }
            }
        }
    }
    while let Some(joined) = asking.join_next().await {
        let Ok((domain, list, answer)) = joined else { continue };
        let ttl = match answer {
            UriAnswer::Unknown | UriAnswer::Refused => UNKNOWN_TTL,
            UriAnswer::Clean | UriAnswer::Listed(_) => LISTING_TTL,
        };
        ctx.uri_cache.insert((domain.clone(), list.zone), answer, Instant::now() + ttl);
        answers.push((domain, list, answer));
    }
    for (_, list, answer) in &answers {
        if *answer == UriAnswer::Refused {
            list.note_refusal();
        }
    }
    answers.retain(|(_, _, answer)| matches!(answer, UriAnswer::Listed(_)));
    let order = |domain: &String, list: &UriList| {
        let at = domains.iter().position(|known| known == domain);
        (at, URI_LISTS.iter().position(|known| known.zone == list.zone))
    };
    answers.sort_by_key(|(domain, list, _)| order(domain, list));
    answers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(127, 0, 0, last)
    }

    fn fired(list: &UriList, codes: &[Ipv4Addr]) -> Vec<&'static str> {
        list.rules(read_answer(codes)).into_iter().map(|(rule, _)| rule).collect()
    }

    #[test]
    fn surbl_bits_become_sub_lists() {
        let surbl = &URI_LISTS[0];
        assert_eq!(fired(surbl, &[]), Vec::<&str>::new());
        assert_eq!(fired(surbl, &[code(8)]), ["SURBL_PH"]);
        assert_eq!(fired(surbl, &[code(16 + 64)]), ["SURBL_MW", "SURBL_ABUSE"]);
        assert_eq!(fired(surbl, &[code(128)]), ["SURBL_CR"]);
        // Retired lists' bits fire nothing.
        assert_eq!(fired(surbl, &[code(2 + 4 + 32)]), Vec::<&str>::new());
    }

    #[test]
    fn uribl_bits_become_sub_lists_and_grey_is_worth_little() {
        let uribl = &URI_LISTS[1];
        assert_eq!(fired(uribl, &[code(2)]), ["URIBL_BLACK"]);
        assert_eq!(fired(uribl, &[code(4 + 8)]), ["URIBL_RED", "URIBL_GREY"]);
        let grey = uribl.rules(UriAnswer::Listed(4));
        assert!(grey[0].1 < 1.0);
    }

    #[test]
    fn refusals_and_odd_answers_are_never_listings() {
        // URIBL's "query refused" and SURBL's error codes.
        assert_eq!(read_answer(&[code(1)]), UriAnswer::Refused);
        assert_eq!(read_answer(&[code(255)]), UriAnswer::Refused);
        assert_eq!(read_answer(&[code(8), code(1)]), UriAnswer::Refused);
        assert_eq!(read_answer(&[Ipv4Addr::new(127, 255, 255, 254)]), UriAnswer::Unknown);
        assert_eq!(read_answer(&[Ipv4Addr::new(192, 0, 2, 1)]), UriAnswer::Unknown);
        for list in &URI_LISTS {
            assert!(list.rules(UriAnswer::Refused).is_empty());
            assert!(list.rules(UriAnswer::Unknown).is_empty());
        }
    }

    #[test]
    fn links_are_asked_about_by_registrable_domain() {
        let hosts: Vec<String> =
            ["click.mailer.example.com", "www.example.com", "shop.example.co.uk", "intranet.test", "b.example.org"]
                .map(str::to_owned)
                .to_vec();
        assert_eq!(registrable_domains(&hosts), ["example.com", "example.co.uk", "example.org"]);
        let many: Vec<String> = (0..20).map(|n| format!("www.d{n}.example.com")).collect();
        assert_eq!(registrable_domains(&many).len(), 1);
        let many: Vec<String> = (0..20).map(|n| format!("www.example{n}.com")).collect();
        assert_eq!(registrable_domains(&many).len(), MAX_DOMAINS);
    }

    /// Both lists answer for their test entries; run with
    /// `cargo test -p uwumail-smtp uri_list_test_entries -- --ignored` on a resolver they answer.
    #[tokio::test]
    #[ignore = "needs the internet and a resolver SURBL and URIBL answer"]
    async fn uri_list_test_entries() {
        let resolver = mail_auth::MessageAuthenticator::new_system_conf().unwrap();
        let surbl = domain_list_answers_with(resolver.resolver(), "test.surbl.org", URI_LISTS[0].zone).await.unwrap();
        assert!(matches!(read_answer(&surbl), UriAnswer::Listed(_)), "{surbl:?}");
        let uribl = domain_list_answers_with(resolver.resolver(), "test.uribl.com", URI_LISTS[1].zone).await.unwrap();
        assert!(matches!(read_answer(&uribl), UriAnswer::Listed(_)), "{uribl:?}");
    }
}
