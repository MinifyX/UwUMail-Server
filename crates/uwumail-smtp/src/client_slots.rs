//! Connections counted per client address, so that one client cannot hold every connection slot
//! of a listener (security-audit-0.16.0 SMTP-5; the IMAP and ManageSieve side came later). IPv6
//! clients count per /64: one machine usually has a whole /64 to pick addresses from.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

/// The open connections of every client address. Cheap to clone; clones count together.
#[derive(Clone, Default)]
pub struct ClientSlots {
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

/// One connection of a client, counted until it is dropped.
pub struct ClientSlot {
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
    key: IpAddr,
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        let mut counts = self.counts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = counts.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.key);
            }
        }
    }
}

/// What a client is counted as: its IPv4 address, or its IPv6 /64.
pub fn client_key(peer: IpAddr) -> IpAddr {
    match peer.to_canonical() {
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            IpAddr::V6(segments.into())
        }
        v4 => v4,
    }
}

impl ClientSlots {
    pub fn new() -> ClientSlots {
        ClientSlots::default()
    }

    /// A place for one more connection from `peer`, or `None` when it has `max` open already.
    pub fn take(&self, peer: IpAddr, max: usize) -> Option<ClientSlot> {
        let key = client_key(peer);
        let mut counts = self.counts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = counts.entry(key).or_default();
        if *count >= max.max(1) {
            return None;
        }
        *count += 1;
        Some(ClientSlot { counts: self.counts.clone(), key })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_counted_per_address_and_ipv6_per_64() {
        let slots = ClientSlots::new();
        let first = slots.take("192.0.2.1".parse().unwrap(), 2).unwrap();
        let _second = slots.take("::ffff:192.0.2.1".parse().unwrap(), 2).unwrap();
        assert!(slots.take("192.0.2.1".parse().unwrap(), 2).is_none(), "the same address, mapped or not");
        assert!(slots.take("192.0.2.2".parse().unwrap(), 2).is_some(), "another address is not affected");
        drop(first);
        assert!(slots.take("192.0.2.1".parse().unwrap(), 2).is_some(), "a closed connection frees its place");

        let _a = slots.take("2001:db8:1:2::1".parse().unwrap(), 1).unwrap();
        assert!(slots.take("2001:db8:1:2:ffff::9".parse().unwrap(), 1).is_none(), "the same /64");
        assert!(slots.take("2001:db8:1:3::1".parse().unwrap(), 1).is_some(), "another /64");
    }
}
