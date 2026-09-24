//! Allowed and blocked senders: what a person, a domain or the whole server lists decides before the
//! score does.
//!
//! Every scope picks its most specific matching entry, a block winning a tie. A block by the server or
//! the recipient's domain refuses the message; otherwise a person's own entry goes first, then the
//! domain's and the server's allowances. Allowing a From address or domain needs SPF or DKIM to vouch
//! for it, since anyone can write any From; server addresses and confirmed host names cannot be faked.

use std::net::IpAddr;
use std::time::Duration;

use mail_parser::MessageParser;
use uwumail_store::{ListScope, SenderKind, SenderList, SenderListEntry, pattern_matches};

use crate::Context;
use crate::checks::normalized_address;
use crate::relay::IpNetwork;

/// A server may point back to many names; like iprev, only the first few are checked.
const MAX_HOST_NAMES: usize = 2;
const HOST_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// What we know about who sent a message.
#[derive(Debug, Clone, Default)]
pub struct Sender {
    /// The sending server, if it could be told.
    pub ip: Option<IpAddr>,
    /// The envelope sender, normalized; empty for bounces.
    pub envelope: Option<String>,
    /// The From address, normalized.
    pub from: Option<String>,
    /// SPF or DKIM vouch for the From domain.
    pub from_verified: bool,
}

impl Sender {
    pub fn new(ip: Option<IpAddr>, envelope: &str, verdict: Option<&crate::checks::Verdict>, raw: &[u8]) -> Sender {
        let from = match verdict {
            Some(verdict) => verdict.from_address.clone(),
            None => MessageParser::new()
                .parse_headers(raw)
                .and_then(|message| {
                    message.from().and_then(|from| from.first()).and_then(|addr| addr.address()).map(str::to_owned)
                })
                .and_then(|address| normalized_address(&address)),
        };
        Sender {
            ip: ip.map(|ip| ip.to_canonical()),
            envelope: normalized_address(envelope),
            from,
            from_verified: verdict.is_some_and(|verdict| verdict.from_verified),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Nothing listed; the score decides.
    None,
    /// Into the inbox whatever the score says.
    Allow(SenderListEntry),
    /// Into Junk: blocked by the person, or by an admin while others still take the message.
    Junk(SenderListEntry),
    /// Blocked by the server or the recipient's domain.
    Reject(SenderListEntry),
}

impl Decision {
    pub fn entry(&self) -> Option<&SenderListEntry> {
        match self {
            Decision::None => None,
            Decision::Allow(entry) | Decision::Junk(entry) | Decision::Reject(entry) => Some(entry),
        }
    }
}

/// The entries for one message and the host names its server confirmed.
#[derive(Debug, Default)]
pub struct Lists {
    entries: Vec<SenderListEntry>,
    hosts: Vec<String>,
}

/// Loads what can matter for these recipients: `(account, domain of the address it was sent to)`.
pub async fn load(ctx: &Context, sender: &Sender, recipients: &[(i64, String)]) -> Lists {
    let mut domains: Vec<String> = recipients.iter().map(|(_, domain)| domain.clone()).collect();
    domains.dedup();
    let accounts = recipients.iter().map(|(account, _)| *account).collect();
    let entries = match ctx.store.sender_lists_for(domains, accounts).await {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(%err, "reading the sender lists failed");
            return Lists::default();
        }
    };
    let hosts = match sender.ip {
        Some(ip) if entries.iter().any(|entry| entry.kind == SenderKind::Host) => confirmed_hosts(ctx, ip).await,
        _ => Vec::new(),
    };
    Lists { entries, hosts }
}

/// The names the address points back to that also point to the address.
async fn confirmed_hosts(ctx: &Context, ip: IpAddr) -> Vec<String> {
    let lookup = async {
        let Ok(names) = ctx.authenticator.ptr_lookup(ip, Some(&ctx.dns.ptr)).await else {
            return Vec::new();
        };
        let mut confirmed = Vec::new();
        for name in names.rrset.iter().take(MAX_HOST_NAMES) {
            let points_back = match ip {
                IpAddr::V4(v4) => ctx
                    .authenticator
                    .ipv4_lookup(name.as_ref(), Some(&ctx.dns.ipv4))
                    .await
                    .is_ok_and(|found| found.rrset.contains(&v4)),
                IpAddr::V6(v6) => ctx
                    .authenticator
                    .ipv6_lookup(name.as_ref(), Some(&ctx.dns.ipv6))
                    .await
                    .is_ok_and(|found| found.rrset.contains(&v6)),
            };
            if points_back {
                confirmed.push(name.trim_end_matches('.').to_ascii_lowercase());
            }
        }
        confirmed
    };
    tokio::time::timeout(HOST_LOOKUP_TIMEOUT, lookup).await.unwrap_or_default()
}

fn domain_of(address: &str) -> Option<&str> {
    address.rsplit_once('@').map(|(_, domain)| domain)
}

/// How narrowly an entry names a sender: within one scope the narrowest match decides, so a listed
/// address can be the exception to its listed domain. Patterns are the loosest, the longer the narrower.
fn specificity(entry: &SenderListEntry) -> u32 {
    let labels = |name: &str| name.split('.').count() as u32;
    match entry.kind {
        SenderKind::Address => 400,
        SenderKind::Host => entry.value.strip_prefix("*.").map_or(300, |parent| 200 + labels(parent)),
        SenderKind::Ip => match entry.value.split_once('/') {
            None => 300,
            Some((address, prefix)) => {
                let bits = prefix.parse::<u32>().unwrap_or(0);
                200 + bits * 32 / if address.contains(':') { 128 } else { 32 }
            }
        },
        SenderKind::Domain => 100 + labels(&entry.value),
        SenderKind::Pattern => 1 + entry.value.chars().filter(|c| *c != '*').count().min(98) as u32,
    }
}

impl Lists {
    fn matches(&self, entry: &SenderListEntry, sender: &Sender) -> bool {
        let block = entry.list == SenderList::Block;
        // Anyone can write any From, so allowing by it needs SPF or DKIM to vouch. Blocking may also go
        // by the envelope sender.
        let from = sender.from.as_deref().filter(|_| block || sender.from_verified);
        let envelope = sender.envelope.as_deref().filter(|_| block);
        let addresses = || from.into_iter().chain(envelope);
        match entry.kind {
            SenderKind::Ip => {
                sender.ip.is_some_and(|ip| entry.value.parse::<IpNetwork>().is_ok_and(|network| network.contains(ip)))
            }
            SenderKind::Host => self.hosts.iter().any(|host| match entry.value.strip_prefix("*.") {
                Some(parent) => host.ends_with(&format!(".{parent}")),
                None => *host == entry.value,
            }),
            SenderKind::Address => addresses().any(|address| address == entry.value),
            SenderKind::Domain => addresses()
                .filter_map(domain_of)
                .any(|domain| domain == entry.value || domain.ends_with(&format!(".{}", entry.value))),
            SenderKind::Pattern => addresses().any(|address| pattern_matches(&entry.value, address)),
        }
    }

    fn best(&self, sender: &Sender, belongs: impl Fn(&SenderListEntry) -> bool) -> Option<&SenderListEntry> {
        self.entries
            .iter()
            .filter(|entry| belongs(entry) && self.matches(entry, sender))
            .max_by_key(|entry| (specificity(entry), entry.list == SenderList::Block))
    }

    /// What the lists say for one recipient: a person and the domain of the address it was sent to.
    pub fn decide(&self, sender: &Sender, account: i64, domain: &str) -> Decision {
        if self.entries.is_empty() {
            return Decision::None;
        }
        let server = self.best(sender, |entry| entry.scope == ListScope::Server);
        let domain = self.best(sender, |entry| {
            matches!(entry.scope, ListScope::Domain(_)) && entry.domain.as_deref() == Some(domain)
        });
        let own = self.best(sender, |entry| entry.scope == ListScope::Account(account));
        if let Some(entry) = [domain, server].into_iter().flatten().find(|entry| entry.list == SenderList::Block) {
            return Decision::Reject(entry.clone());
        }
        match own.or(domain).or(server) {
            Some(entry) if entry.list == SenderList::Block => Decision::Junk(entry.clone()),
            Some(entry) => Decision::Allow(entry.clone()),
            None => Decision::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LENI: i64 = 7;

    fn entry(scope: ListScope, list: SenderList, kind: SenderKind, value: &str) -> SenderListEntry {
        SenderListEntry {
            id: 0,
            list,
            kind,
            value: value.into(),
            note: String::new(),
            domain: matches!(scope, ListScope::Domain(_)).then(|| "example.org".into()),
            scope,
            created_at: 0,
            created_by: String::new(),
            expires_at: None,
            hits: 0,
            last_hit_at: None,
        }
    }

    fn sender(from: &str, verified: bool) -> Sender {
        Sender {
            ip: Some("192.0.2.25".parse().unwrap()),
            envelope: Some("bounces@lists.example.com".into()),
            from: Some(from.into()),
            from_verified: verified,
        }
    }

    /// The decision in words, e.g. "junk example.com".
    fn decided(entries: Vec<SenderListEntry>, sender: &Sender) -> Option<String> {
        let lists = Lists { entries, hosts: vec!["mx2.mail.example.net".into()] };
        let decision = lists.decide(sender, LENI, "example.org");
        let word = match &decision {
            Decision::None => return None,
            Decision::Allow(_) => "allow",
            Decision::Junk(_) => "junk",
            Decision::Reject(_) => "reject",
        };
        decision.entry().map(|entry| format!("{word} {}", entry.value))
    }

    #[test]
    fn allowing_by_from_needs_someone_to_vouch_for_it() {
        let own = vec![entry(ListScope::Account(LENI), SenderList::Allow, SenderKind::Address, "oma@example.com")];
        assert_eq!(decided(own.clone(), &sender("oma@example.com", true)).as_deref(), Some("allow oma@example.com"));
        assert_eq!(decided(own, &sender("oma@example.com", false)), None, "a forged From is not allowed");

        let server = vec![entry(ListScope::Server, SenderList::Allow, SenderKind::Ip, "192.0.2.0/24")];
        assert_eq!(
            decided(server, &sender("anyone@example.test", false)).as_deref(),
            Some("allow 192.0.2.0/24"),
            "the server's address needs no From"
        );
    }

    #[test]
    fn blocks_match_the_envelope_and_subdomains_and_confirmed_hosts() {
        let own = vec![entry(ListScope::Account(LENI), SenderList::Block, SenderKind::Domain, "example.com")];
        assert_eq!(decided(own, &sender("x@example.test", false)).as_deref(), Some("junk example.com"));

        let wildcard = vec![entry(ListScope::Server, SenderList::Block, SenderKind::Host, "*.example.net")];
        assert_eq!(decided(wildcard, &sender("x@example.test", false)).as_deref(), Some("reject *.example.net"));
        let exact = vec![entry(ListScope::Server, SenderList::Block, SenderKind::Host, "mail.example.net")];
        assert_eq!(decided(exact, &sender("x@example.test", false)), None, "only the confirmed name itself");
    }

    #[test]
    fn admin_blocks_win_and_otherwise_the_nearest_scope_decides() {
        let boss = sender("boss@example.test", true);
        let other = sender("sales@example.test", true);

        let own = vec![
            entry(ListScope::Account(LENI), SenderList::Block, SenderKind::Domain, "example.test"),
            entry(ListScope::Account(LENI), SenderList::Allow, SenderKind::Address, "boss@example.test"),
        ];
        assert_eq!(decided(own.clone(), &boss).as_deref(), Some("allow boss@example.test"), "the exception");
        assert_eq!(decided(own, &other).as_deref(), Some("junk example.test"));

        let domain_block = vec![
            entry(ListScope::Domain(1), SenderList::Block, SenderKind::Domain, "example.test"),
            entry(ListScope::Account(LENI), SenderList::Allow, SenderKind::Address, "boss@example.test"),
        ];
        assert_eq!(decided(domain_block, &boss).as_deref(), Some("reject example.test"));

        let person_over_server = vec![
            entry(ListScope::Server, SenderList::Allow, SenderKind::Domain, "example.test"),
            entry(ListScope::Account(LENI), SenderList::Block, SenderKind::Domain, "example.test"),
        ];
        assert_eq!(decided(person_over_server, &other).as_deref(), Some("junk example.test"));

        let tie = vec![
            entry(ListScope::Server, SenderList::Allow, SenderKind::Domain, "example.test"),
            entry(ListScope::Server, SenderList::Block, SenderKind::Domain, "example.test"),
        ];
        assert_eq!(decided(tie, &other).as_deref(), Some("reject example.test"), "a tie goes to the block");
    }

    #[test]
    fn patterns_are_the_loosest_entries() {
        let own = vec![
            entry(ListScope::Account(LENI), SenderList::Block, SenderKind::Pattern, "*.invalid"),
            entry(ListScope::Account(LENI), SenderList::Allow, SenderKind::Domain, "example.test"),
        ];
        assert_eq!(decided(own.clone(), &sender("boss@example.test", true)).as_deref(), Some("allow example.test"));
        assert_eq!(decided(own, &sender("x@shop.invalid", true)).as_deref(), Some("junk *.invalid"));

        let longer = vec![
            entry(ListScope::Account(LENI), SenderList::Block, SenderKind::Pattern, "*news*"),
            entry(ListScope::Account(LENI), SenderList::Allow, SenderKind::Pattern, "*news@example.test"),
        ];
        assert_eq!(
            decided(longer, &sender("news@example.test", true)).as_deref(),
            Some("allow *news@example.test"),
            "the longer pattern is the exception"
        );

        let envelope = vec![entry(ListScope::Server, SenderList::Block, SenderKind::Pattern, "bounces@*")];
        assert_eq!(decided(envelope.clone(), &sender("x@example.test", false)).as_deref(), Some("reject bounces@*"));
        let allowed = vec![entry(ListScope::Server, SenderList::Allow, SenderKind::Pattern, "*@example.test")];
        assert_eq!(decided(allowed, &sender("x@example.test", false)), None, "allowing still needs a vouched From");
    }
}
