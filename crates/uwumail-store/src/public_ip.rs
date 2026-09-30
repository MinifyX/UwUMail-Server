//! Whether an address is on the open internet. One answer for everything that connects somewhere a
//! stranger named: fetched mailboxes and their outgoing servers, MX hosts, lists, MTA-STS policies,
//! pictures. A weaker copy of it once let addresses that carry a private IPv4 address inside an
//! IPv6 one through (EGRESS-3 of the 0.18.0 audit).

use std::net::IpAddr;

/// Whether `ip` is on the open internet: not this machine, not the local network, not reserved or
/// meant for documentation. A fetched mailbox, a list or another domain's mail server live
/// somewhere else, so any other address would only point a worker at this host or the LAN
/// (security-audit-0.5.2 S-10).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (b & 0xc0) == 64)
                || (a == 198 && (b & 0xfe) == 18)
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            let first = segments[0];
            let octets = v6.octets();
            let embedded = |at: usize| IpAddr::from([octets[at], octets[at + 1], octets[at + 2], octets[at + 3]]);
            // Addresses that carry an IPv4 address reach it where NAT64 or 6to4 routes them: they are
            // as public as that address (EGRESS-3 of the 0.18.0 audit).
            if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] || segments[..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback() {
                return !v6.is_unspecified() && is_public_ip(embedded(12));
            }
            if first == 0x2002 {
                return is_public_ip(embedded(2));
            }
            !(v6.is_unspecified()
                // Local NAT64 (RFC 8215) and Teredo, whose IPv4 address is hidden.
                || (first == 0x64 && segments[1] == 0xff9b && segments[2] == 1)
                || (first == 0x2001 && segments[1] == 0)
                || v6.is_loopback()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first == 0x2001 && segments[1] == 0x0db8))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addresses_on_the_internet_are_public() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.20",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "::",
            "::1",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            // IPv4 inside IPv6: NAT64, local NAT64, 6to4, IPv4-compatible, Teredo.
            "64:ff9b::a00:1",
            "64:ff9b::7f00:1",
            "64:ff9b:1::a00:1",
            "2002:a00:1::1",
            "2002:7f00:1::1",
            "::10.0.0.1",
            "::127.0.0.1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
        ] {
            assert!(!is_public_ip(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "9.9.9.9", "2a01:4f8::1", "64:ff9b::101:101", "2002:101:101::1", "::ffff:1.1.1.1"] {
            assert!(is_public_ip(public.parse().unwrap()), "{public}");
        }
    }
}
