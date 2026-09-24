//! Trusted relays: mail servers in front of us (an existing mail server, later the
//! UwUMail Gateway) that receive mail from the internet and forward it here.
//! For their mail, sender checks must use the address the relay saw, not the relay's.

use std::net::IpAddr;
use std::str::FromStr;

use crate::headers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNetwork {
    address: IpAddr,
    prefix: u8,
}

impl FromStr for IpNetwork {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (value, None),
        };
        let address: IpAddr = address.parse().map_err(|_| format!("'{value}' is not an IP address or network"))?;
        let address = address.to_canonical();
        let max = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(prefix) => prefix.parse::<u8>().ok().filter(|p| *p <= max),
            None => Some(max),
        }
        .ok_or_else(|| format!("'{value}' has an invalid prefix length"))?;
        Ok(IpNetwork { address, prefix })
    }
}

impl IpNetwork {
    /// Parses addresses and CIDR networks, e.g. `["192.168.1.51", "10.0.0.0/8"]`.
    pub fn parse_list(values: &[String]) -> Result<Vec<IpNetwork>, String> {
        values.iter().map(|value| value.parse()).collect()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.address, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(self.prefix)).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(self.prefix)).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

pub fn parse_networks(values: &[String]) -> Result<Vec<IpNetwork>, String> {
    IpNetwork::parse_list(values)
}

/// Walks the Received headers from the newest down and returns the first hop that did not
/// come from a trusted relay: the client's IP address and the name it greeted with.
pub fn original_client(raw: &[u8], trusted: &[IpNetwork]) -> Option<(IpAddr, String)> {
    let (fields, _) = headers::split(raw);
    for field in fields.iter().filter(|h| h.name.eq_ignore_ascii_case("Received")) {
        let value = field.value();
        let Some((helo, ip)) = parse_received_from(&value) else {
            // A hop we cannot read: stop rather than trust something further down.
            return None;
        };
        if !trusted.iter().any(|network| network.contains(ip)) {
            return Some((ip, helo));
        }
        // A trusted hop: keep walking down to the server that talked to it.
    }
    None
}

/// Reads `from <helo> (<rdns> [<ip>])` out of a Received header value.
///
/// The client address is taken from the parenthesised comment the relay writes, never from the
/// HELO. The HELO comes first and may itself be an address literal `[a.b.c.d]`; trusting its
/// bracket, as taking the first `[...]` did, let a sender behind a trusted relay choose the address
/// every SPF/DMARC/list/reputation check then ran against (security-audit-0.5.2 S-5). A HELO has no
/// space or `(`, so the first `(` always begins the relay's own comment.
fn parse_received_from(value: &str) -> Option<(String, IpAddr)> {
    let lower = value.to_ascii_lowercase();
    let from_start = lower.find("from ")? + 5;
    let from_end = lower[from_start..].find(" by ").map_or(value.len(), |i| from_start + i);
    let from = &value[from_start..from_end];
    let helo = from.split_whitespace().next()?.trim_matches(['[', ']']).to_owned();
    let comment = &from[from.find('(')?..];
    let open = comment.find('[')? + 1;
    let close = comment[open..].find(']')? + open;
    let literal = comment[open..close].trim();
    let literal = literal.strip_prefix("IPv6:").or_else(|| literal.strip_prefix("ipv6:")).unwrap_or(literal);
    let ip = literal.parse::<IpAddr>().ok()?.to_canonical();
    Some((helo, ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_match() {
        let lan: IpNetwork = "192.168.1.0/24".parse().unwrap();
        assert!(lan.contains("192.168.1.16".parse().unwrap()));
        assert!(lan.contains("::ffff:192.168.1.16".parse().unwrap()));
        assert!(!lan.contains("192.168.2.1".parse().unwrap()));
        let host: IpNetwork = "2001:db8::1".parse().unwrap();
        assert!(host.contains("2001:db8::1".parse().unwrap()));
        assert!(!host.contains("2001:db8::2".parse().unwrap()));
        let everything: IpNetwork = "0.0.0.0/0".parse().unwrap();
        assert!(everything.contains("203.0.113.9".parse().unwrap()));
        assert!("300.1.1.1".parse::<IpNetwork>().is_err());
        assert!("10.0.0.0/33".parse::<IpNetwork>().is_err());
    }

    #[test]
    fn finds_the_first_untrusted_hop() {
        let raw = b"Received: from mx.relay.local (mx.relay.local [192.168.1.20])\r\n\tby mail.example.net (Postfix) with ESMTP id AB;\r\n\tMon, 14 Sep 2026 10:00:01 +0200\r\n\
Received: from mail.example.com (mail.example.com [203.0.113.41])\r\n\tby mx.relay.local (Postfix) with ESMTPS id CD\r\n\tfor <nyu@uwu.example>; Mon, 14 Sep 2026 10:00:00 +0200\r\n\
Subject: hi\r\n\r\nbody\r\n";
        let trusted = parse_networks(&["192.168.1.0/24".into()]).unwrap();
        assert_eq!(original_client(raw, &trusted), Some(("203.0.113.41".parse().unwrap(), "mail.example.com".into())));
        assert_eq!(original_client(raw, &[]), Some(("192.168.1.20".parse().unwrap(), "mx.relay.local".into())));
    }

    #[test]
    fn reads_ipv6_literals_and_stops_at_unreadable_hops() {
        assert_eq!(
            parse_received_from("from mx.example ([IPv6:2001:db8::5]) by us"),
            Some(("mx.example".into(), "2001:db8::5".parse().unwrap()))
        );
        let raw = b"Received: by localhost with LMTP\r\nSubject: x\r\n\r\n";
        assert_eq!(original_client(raw, &[]), None);
    }

    #[test]
    fn a_helo_address_literal_does_not_become_the_client() {
        // A sender behind a trusted relay greets with an address literal of their choosing. The
        // client address is the one the relay wrote in the comment, not the sender's first bracket.
        let value = "from [203.0.113.7] (unknown [198.51.100.9]) by relay.local (Postfix) with ESMTP id 1";
        assert_eq!(parse_received_from(value), Some(("203.0.113.7".into(), "198.51.100.9".parse().unwrap())));
        // A hop with only a HELO literal and no relay comment cannot be read: it is not trusted.
        assert_eq!(parse_received_from("from [203.0.113.7] by relay.local"), None);
    }
}
