//! How many connections the gateway carries at once, in total and per client network.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct Limits {
    total: Arc<Semaphore>,
    per_network: Mutex<HashMap<IpAddr, usize>>,
    max_per_network: usize,
}

/// A place for one connection. Gives it back when dropped.
pub(crate) struct Admission {
    limits: Arc<Limits>,
    network: IpAddr,
    _permit: OwnedSemaphorePermit,
}

impl Limits {
    pub fn new(max_total: usize, max_per_network: usize) -> Arc<Limits> {
        Arc::new(Limits {
            total: Arc::new(Semaphore::new(max_total.max(1))),
            per_network: Mutex::new(HashMap::new()),
            max_per_network: max_per_network.max(1),
        })
    }

    pub fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Admission> {
        let permit = self.total.clone().try_acquire_owned().ok()?;
        let network = network_of(ip);
        let mut counts = self.per_network.lock().expect("connection counts poisoned");
        let count = counts.entry(network).or_default();
        if *count >= self.max_per_network {
            return None;
        }
        *count += 1;
        Some(Admission { limits: self.clone(), network, _permit: permit })
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut counts = self.limits.per_network.lock().expect("connection counts poisoned");
        if let Some(count) = counts.get_mut(&self.network) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.network);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_network_and_in_total() {
        let limits = Limits::new(3, 2);
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
}
