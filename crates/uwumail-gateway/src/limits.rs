//! How many connections the gateway carries at once, in total, per client network, and per IPv6
//! site.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct Limits {
    total: Arc<Semaphore>,
    /// Connections per IPv4 address or IPv6 /64, and per IPv6 /48.
    counts: Mutex<HashMap<IpAddr, usize>>,
    max_per_network: usize,
    max_per_site: usize,
}

/// A place for one connection. Gives it back when dropped.
pub(crate) struct Admission {
    limits: Arc<Limits>,
    network: IpAddr,
    site: Option<IpAddr>,
    _permit: OwnedSemaphorePermit,
}

impl Limits {
    pub fn new(max_total: usize, max_per_network: usize, max_per_site: usize) -> Arc<Limits> {
        Arc::new(Limits {
            total: Arc::new(Semaphore::new(max_total.max(1))),
            counts: Mutex::new(HashMap::new()),
            max_per_network: max_per_network.max(1),
            max_per_site: max_per_site.max(1),
        })
    }

    pub fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Admission> {
        let permit = self.total.clone().try_acquire_owned().ok()?;
        let (network, site) = (network_of(ip), site_of(ip));
        let mut counts = self.counts.lock().expect("connection counts poisoned");
        if counts.get(&network).copied().unwrap_or(0) >= self.max_per_network {
            return None;
        }
        // Anyone with an IPv6 /48 has 65,536 /64s: the site as a whole has a limit too, or twenty of
        // them would take every place there is (security-audit-0.16.0 GW-6).
        if let Some(site) = site
            && counts.get(&site).copied().unwrap_or(0) >= self.max_per_site
        {
            return None;
        }
        *counts.entry(network).or_default() += 1;
        if let Some(site) = site {
            *counts.entry(site).or_default() += 1;
        }
        Some(Admission { limits: self.clone(), network, site, _permit: permit })
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut counts = self.limits.counts.lock().expect("connection counts poisoned");
        for key in std::iter::once(self.network).chain(self.site) {
            if let Some(count) = counts.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    counts.remove(&key);
                }
            }
        }
    }
}

/// One IPv4 address, or the /64 an IPv6 address belongs to (what one household or server gets).
fn network_of(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
        v4 => v4,
    }
}

/// The /48 an IPv6 address belongs to, what one site or customer is usually given. Kept apart from
/// the /64s by its last bit, which no /48 has set. IPv4 has none.
fn site_of(ip: IpAddr) -> Option<IpAddr> {
    match ip.to_canonical() {
        IpAddr::V6(v6) => Some(IpAddr::V6(Ipv6Addr::from((u128::from(v6) & (u128::MAX << 80)) | 1))),
        IpAddr::V4(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_network_and_in_total() {
        let limits = Limits::new(3, 2, 10);
        let a = "192.0.2.1".parse().unwrap();
        let first = limits.admit(a).unwrap();
        let _second = limits.admit(a).unwrap();
        assert!(limits.admit(a).is_none(), "two per address");

        // Same /64, other address.
        let _third = limits.admit("2001:db8::1".parse().unwrap()).unwrap();
        assert!(limits.admit("2001:db8::2".parse().unwrap()).is_none(), "three in total");

        drop(first);
        assert!(limits.admit(a).is_some(), "a place is free again");
        assert_eq!(network_of("2001:db8::1:2".parse().unwrap()), "2001:db8::".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn one_ipv6_site_cannot_take_every_place() {
        let limits = Limits::new(1000, 50, 100);
        // Twenty /64s of one /48 once took all thousand places.
        let mut held = Vec::new();
        for net in 0..20u16 {
            for host in 1..=50u16 {
                if let Some(admission) = limits.admit(format!("2001:db8:0:{net:x}::{host:x}").parse().unwrap()) {
                    held.push(admission);
                }
            }
        }
        assert_eq!(held.len(), 100, "one /48 gets a hundred");
        let others = limits.admit("2001:db8:1::1".parse().unwrap());
        assert!(others.is_some(), "the next /48 still gets in");
        assert!(limits.admit("192.0.2.1".parse().unwrap()).is_some(), "and IPv4 has no site");
        held.clear();
        drop(others);
        assert!(limits.counts.lock().unwrap().is_empty(), "every place is given back");
    }
}
