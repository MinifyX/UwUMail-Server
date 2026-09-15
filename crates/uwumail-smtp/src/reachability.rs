//! Where this server stands on the internet, for the setup assistant: the address it sends from,
//! whether mail servers see that address as a home connection, who runs the network, and whether
//! port 25 works. From that it recommends sending directly or through a UwUMail Gateway.
//!
//! Everything comes from DNS and one connection to a big mail provider: Google's name servers tell
//! the public address, Spamhaus ZEN whether it is a home connection (PBL) or listed for spam,
//! Team Cymru which network it belongs to.

use std::net::{IpAddr, Ipv4Addr};

use serde::Serialize;

use crate::dnscheck::DnsChecker;
use crate::health::{ProbeReport, ProbeStage};
use crate::servercheck::InboundReport;
use crate::{Smtp, now};

/// A network operator that blocks or restricts outgoing port 25, and where it says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    /// Stable key the portal explains, e.g. `hetzner`.
    pub key: &'static str,
    pub advice: Advice,
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Advice {
    /// Opening port 25 is hard to get; better pick another provider.
    Avoid,
    /// Port 25 is closed until the provider's support opens it.
    AskSupport,
}

/// Only what the providers write themselves.
const PROVIDERS: &[(u32, Provider)] = &[
    (24940, Provider { key: "hetzner", advice: Advice::Avoid, source: "https://docs.hetzner.com/cloud/servers/faq/" }),
    (
        6724,
        Provider {
            key: "strato",
            advice: Advice::AskSupport,
            source: "https://www.strato.de/faq/server/wie-stelle-ich-die-netzwerk-firewall-bei-meinem-windows-root-server-ein/",
        },
    ),
    (
        8560,
        Provider {
            key: "ionos",
            advice: Advice::AskSupport,
            source: "https://www.ionos.com/help/server-cloud-infrastructure/firewall-policies/unblocking-port-25-for-sending-emails/",
        },
    ),
];

pub fn provider_for(asn: u32) -> Option<Provider> {
    PROVIDERS.iter().find(|(number, _)| *number == asn).map(|(_, provider)| *provider)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Recommendation {
    Direct,
    Gateway,
    /// Not enough answers to say.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAddress {
    pub ip: String,
    pub ptr: Vec<String>,
    /// No reverse name, or one the network made up from the address, like `p5b0c1d2e.dip0.example.net`.
    pub generic_ptr: bool,
    /// Spamhaus PBL: an address of a home or dynamic connection that should not send mail itself.
    pub home_connection: bool,
    /// Spamhaus lists it for spam or infected machines (SBL, CSS, XBL).
    pub listed: bool,
    /// Spamhaus did not answer usefully, so neither of the two above is known.
    pub spamhaus_unknown: bool,
    pub asn: Option<u32>,
    pub network: Option<String>,
    pub provider: Option<Provider>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reachability {
    pub checked_at: i64,
    /// The public addresses this machine sends from, IPv4 first.
    pub addresses: Vec<PublicAddress>,
    /// Port 25 of a big mail provider, called from here (through the gateway, if one is paired).
    pub outbound: ProbeReport,
    /// Port 25 of the public IPv4 address, called from here. Many home routers cannot call their
    /// own address, so only a success says something.
    pub inbound: Option<InboundReport>,
    /// Connections to other servers already go through a UwUMail Gateway.
    pub through_gateway: bool,
    pub recommendation: Recommendation,
    /// Why, as stable keys for the portal, most important first.
    pub reasons: Vec<&'static str>,
}

/// What Spamhaus ZEN's answers mean: (home connection, listed for spam).
pub fn spamhaus_meaning(codes: &[Ipv4Addr]) -> (bool, bool) {
    let home = codes.iter().any(|code| matches!(code.octets(), [127, 0, 0, 10 | 11]));
    let listed = codes.iter().any(|code| matches!(code.octets(), [127, 0, 0, 2..=9]));
    (home, listed)
}

/// Whether a reverse name was made up by the network rather than set for a mail server: it holds
/// the address itself (digits or hex) or a word networks use for dynamic and customer ranges.
pub fn generic_reverse_name(name: &str, ip: IpAddr) -> bool {
    let name = name.to_ascii_lowercase();
    if let IpAddr::V4(v4) = ip {
        let [a, b, c, d] = v4.octets();
        if name.contains(&format!("{a:02x}{b:02x}{c:02x}{d:02x}")) {
            return true;
        }
        for separator in ['.', '-'] {
            let forward = format!("{a}{separator}{b}{separator}{c}{separator}{d}");
            let backward = format!("{d}{separator}{c}{separator}{b}{separator}{a}");
            if name.contains(&forward) || name.contains(&backward) {
                return true;
            }
        }
    }
    const WORDS: [&str; 14] = [
        "dyn",
        "dynamic",
        "dip",
        "dhcp",
        "dsl",
        "cable",
        "pool",
        "ppp",
        "pppoe",
        "broadband",
        "customer",
        "client",
        "clients",
        "static",
    ];
    name.split(|c: char| !c.is_ascii_alphanumeric()).any(|label| {
        WORDS.iter().any(|word| label.strip_prefix(word).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())))
    })
}

/// The recommendation and its reasons.
pub fn recommend(
    addresses: &[PublicAddress],
    outbound: &ProbeReport,
    through_gateway: bool,
) -> (Recommendation, Vec<&'static str>) {
    let mut reasons = Vec::new();
    if addresses.is_empty() {
        reasons.push("noPublicAddress");
    }
    let home = addresses.iter().any(|address| address.home_connection);
    let listed = addresses.iter().any(|address| address.listed);
    let port_blocked = !outbound.ok && matches!(outbound.stage, Some(ProbeStage::Connect));
    let provider = addresses.iter().find_map(|address| address.provider);
    let generic_ptr =
        addresses.first().is_some_and(|address| address.ip.parse::<Ipv4Addr>().is_ok() && address.generic_ptr);

    if home {
        reasons.push("homeConnection");
    }
    if listed {
        reasons.push("listed");
    }
    if port_blocked {
        reasons.push(if through_gateway { "port25BlockedAtGateway" } else { "port25Blocked" });
    }
    match provider.map(|provider| provider.advice) {
        Some(Advice::Avoid) => reasons.push("providerAvoid"),
        Some(Advice::AskSupport) if port_blocked => reasons.push("providerAskSupport"),
        _ => {}
    }
    if generic_ptr && !home {
        reasons.push("genericPtr");
    }

    let recommendation = if through_gateway {
        Recommendation::Gateway
    } else if addresses.is_empty() {
        Recommendation::Unknown
    } else if home || listed || (port_blocked && provider.is_none()) {
        Recommendation::Gateway
    } else {
        Recommendation::Direct
    };
    (recommendation, reasons)
}

async fn public_address(dns: &DnsChecker, ip: IpAddr) -> PublicAddress {
    let (ptr, codes, network) = tokio::join!(dns.reverse_names(ip), dns.spamhaus_codes(ip), dns.network(ip));
    let ptr = ptr.unwrap_or_default();
    let (home_connection, listed) = codes.as_deref().map(spamhaus_meaning).unwrap_or((false, false));
    let (asn, network) = network.map(|(asn, name)| (Some(asn), Some(name))).unwrap_or((None, None));
    PublicAddress {
        ip: ip.to_string(),
        generic_ptr: ptr.is_empty() || ptr.iter().any(|name| generic_reverse_name(name, ip)),
        ptr,
        home_connection,
        listed,
        spamhaus_unknown: codes.is_none(),
        provider: asn.and_then(provider_for),
        asn,
        network,
    }
}

impl Smtp {
    /// Looks at where this server stands on the internet and whether it should send through a
    /// UwUMail Gateway.
    pub async fn check_reachability(&self, dns: &DnsChecker) -> Reachability {
        let ctx = &self.inner;
        let through_gateway = self.has_connector();
        let (v4, v6) = tokio::join!(dns.public_address(false), dns.public_address(true));
        let mut addresses = Vec::new();
        for ip in [v4, v6].into_iter().flatten() {
            addresses.push(public_address(dns, ip).await);
        }
        let port = ctx.live().delivery.mx_port;
        let outbound = crate::health::probe_direct(ctx, port).await;
        let inbound = match v4 {
            Some(ip) if !through_gateway => Some(crate::servercheck::self_call(ctx, ip, 25, &ctx.hostname).await),
            _ => None,
        };
        let (recommendation, reasons) = recommend(&addresses, &outbound, through_gateway);
        Reachability { checked_at: now(), addresses, outbound, inbound, through_gateway, recommendation, reasons }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Route;

    #[test]
    fn made_up_reverse_names_are_recognised() {
        let home: IpAddr = "192.0.2.247".parse().unwrap();
        assert!(generic_reverse_name("pc000024f7.dip0.isp.example", home), "hex of the address");
        assert!(generic_reverse_name("static.247.2.0.192.clients.hoster.example", home), "reversed digits");
        assert!(generic_reverse_name("192-0-2-247.pool.isp.example", home));
        assert!(generic_reverse_name("host.dyn42.isp.example", "198.51.100.1".parse().unwrap()));
        assert!(!generic_reverse_name("mail.example.com", home));
        assert!(!generic_reverse_name("dynamo.example.com", home), "whole words only");
    }

    #[test]
    fn spamhaus_codes_mean_home_or_listed() {
        let code = |last: u8| Ipv4Addr::new(127, 0, 0, last);
        assert_eq!(spamhaus_meaning(&[code(10)]), (true, false));
        assert_eq!(spamhaus_meaning(&[code(11), code(4)]), (true, true));
        assert_eq!(spamhaus_meaning(&[code(2)]), (false, true));
        assert_eq!(spamhaus_meaning(&[]), (false, false));
    }

    fn address(home: bool, provider: Option<u32>, generic_ptr: bool) -> PublicAddress {
        PublicAddress {
            ip: "192.0.2.10".into(),
            ptr: vec![],
            generic_ptr,
            home_connection: home,
            listed: false,
            spamhaus_unknown: false,
            asn: provider,
            network: None,
            provider: provider.and_then(provider_for),
        }
    }

    fn probe(ok: bool) -> ProbeReport {
        ProbeReport {
            at: 0,
            route: Route::Direct,
            target: "mx.example.net:25".into(),
            ok,
            stage: (!ok).then_some(ProbeStage::Connect),
            error: None,
        }
    }

    #[test]
    fn home_connections_get_the_gateway() {
        let (recommendation, reasons) = recommend(&[address(true, None, true)], &probe(false), false);
        assert_eq!(recommendation, Recommendation::Gateway);
        assert_eq!(reasons, ["homeConnection", "port25Blocked"]);

        let (recommendation, reasons) = recommend(&[address(false, None, false)], &probe(true), false);
        assert_eq!((recommendation, reasons.len()), (Recommendation::Direct, 0));

        assert_eq!(recommend(&[], &probe(false), false).0, Recommendation::Unknown);
    }

    /// Asks real DNS about this machine; run with
    /// `cargo test -p uwumail-smtp live_reachability -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs the internet"]
    async fn live_reachability() {
        let dns = DnsChecker::new().unwrap();
        let ip = dns.public_address(false).await.expect("an IPv4 address");
        let report = public_address(&dns, ip).await;
        println!(
            "home connection: {}, listed: {}, spamhaus unknown: {}, generic PTR: {}, AS{:?} {:?}, provider: {:?}",
            report.home_connection,
            report.listed,
            report.spamhaus_unknown,
            report.generic_ptr,
            report.asn,
            report.network,
            report.provider.map(|provider| provider.key)
        );
        assert!(report.asn.is_some());
    }

    #[test]
    fn providers_that_block_port_25() {
        // A rented server whose port 25 opens on request: ask, no gateway needed.
        let (recommendation, reasons) = recommend(&[address(false, Some(6724), false)], &probe(false), false);
        assert_eq!((recommendation, reasons), (Recommendation::Direct, vec!["port25Blocked", "providerAskSupport"]));

        // Hetzner: advised against, even while port 25 happens to work.
        let (_, reasons) = recommend(&[address(false, Some(24940), false)], &probe(true), false);
        assert_eq!(reasons, ["providerAvoid"]);

        // Unknown network with a closed port 25: the gateway (or a relay) gets mail out.
        assert_eq!(recommend(&[address(false, None, false)], &probe(false), false).0, Recommendation::Gateway);
    }
}
