//! The checks of the setup assistant: how this server looks from the outside. Everything runs
//! from here, without an outside service, so some things (like a provider blocking port 25
//! inbound) cannot be seen and the portal says so.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde::Serialize;

use crate::client::Client;
use crate::dnscheck::{BLOCKLISTS, DnsChecker, Listing};
use crate::health::{ProbeReport, Route};
use crate::{Context, Smtp, now};

const SELF_CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressReport {
    pub ip: String,
    /// A private or local address: the internet cannot reach it like this.
    pub private: bool,
    /// Names in the PTR records.
    pub ptr: Vec<String>,
    /// One of the PTR names points back to this address.
    pub ptr_confirmed: bool,
    /// The PTR name is the server's own name.
    pub ptr_is_hostname: bool,
    /// Empty unless blocklists were asked for.
    pub listings: Vec<Listing>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundReport {
    pub ip: String,
    pub reachable: bool,
    /// The greeting names this server, so the call really arrived here.
    pub ours: bool,
    pub greeting: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCheck {
    pub checked_at: i64,
    pub hostname: String,
    /// Where the server name points: receiving and, without a relay, sending.
    pub addresses: Vec<AddressReport>,
    pub route: Route,
    /// The relay other servers see as the sender, if there is one.
    pub relay_host: Option<String>,
    pub relay_addresses: Vec<AddressReport>,
    pub outbound: ProbeReport,
    /// Port 25 of each public address, called from here.
    pub inbound: Vec<InboundReport>,
    /// Another mail server receives mail first (`smtp.trusted_relays`).
    pub upstream: bool,
    pub blocklists_checked: bool,
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

async fn address_report(dns: &DnsChecker, ip: IpAddr, hostname: &str, blocklists: bool) -> AddressReport {
    let ptr = dns.reverse_names(ip).await.unwrap_or_default();
    let mut ptr_confirmed = false;
    for name in &ptr {
        if dns.host_addresses(name).await.contains(&ip) {
            ptr_confirmed = true;
            break;
        }
    }
    let ptr_is_hostname = ptr.iter().any(|name| name.eq_ignore_ascii_case(hostname));
    let mut listings = Vec::new();
    if blocklists && !is_private(ip) {
        for list in BLOCKLISTS {
            if ip.is_ipv4() || list.ipv6 {
                listings.push(dns.blocklist_status(ip, list).await);
            }
        }
    }
    AddressReport { ip: ip.to_string(), private: is_private(ip), ptr, ptr_confirmed, ptr_is_hostname, listings }
}

async fn self_call(ctx: &Context, ip: IpAddr, port: u16, hostname: &str) -> InboundReport {
    let failed = |error: String| InboundReport {
        ip: ip.to_string(),
        reachable: false,
        ours: false,
        greeting: None,
        error: Some(error),
    };
    let mut client = match Client::connect(ctx, SocketAddr::new(ip, port), SELF_CALL_TIMEOUT, SELF_CALL_TIMEOUT).await {
        Ok(client) => client,
        Err(err) => return failed(err.to_string()),
    };
    match client.read_reply().await {
        Ok(reply) => {
            let greeting = reply.to_string();
            client.quit().await;
            InboundReport {
                ip: ip.to_string(),
                reachable: true,
                ours: greeting.to_ascii_lowercase().contains(&hostname.to_ascii_lowercase()),
                greeting: Some(greeting),
                error: None,
            }
        }
        Err(err) => failed(err.to_string()),
    }
}

impl Smtp {
    /// Looks at the server from the outside: addresses, reverse names, optionally blocklists,
    /// whether mail can leave, and whether port 25 answers on the public addresses.
    pub async fn check_server(&self, dns: &DnsChecker, blocklists: bool) -> ServerCheck {
        let ctx = &self.inner;
        let hostname = ctx.hostname.clone();
        let relay_host = self.relay_host();
        let upstream = self.behind_upstream_server();

        let addresses_future = async {
            let mut reports = Vec::new();
            for ip in dns.host_addresses(&hostname).await {
                reports.push(address_report(dns, ip, &hostname, blocklists && relay_host.is_none()).await);
            }
            reports
        };
        let relay_future = async {
            let mut reports = Vec::new();
            if let Some(relay) = &relay_host {
                for ip in dns.host_addresses(relay).await {
                    reports.push(address_report(dns, ip, relay, blocklists).await);
                }
            }
            reports
        };
        let (addresses, relay_addresses, outbound) =
            tokio::join!(addresses_future, relay_future, self.probe_delivery());

        let mut inbound = Vec::new();
        for report in addresses.iter().filter(|report| !report.private) {
            if let Ok(ip) = report.ip.parse() {
                inbound.push(self_call(ctx, ip, 25, &hostname).await);
            }
        }

        ServerCheck {
            checked_at: now(),
            hostname,
            addresses,
            route: if relay_host.is_some() { Route::Relay } else { Route::Direct },
            relay_host,
            relay_addresses,
            outbound,
            inbound,
            upstream,
            blocklists_checked: blocklists,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_recognised() {
        for ip in ["10.0.0.1", "192.168.1.20", "127.0.0.1", "100.64.0.1", "fd00::1", "fe80::1", "::1"] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["192.0.2.1", "2001:db8::1", "100.128.0.1"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }
}
