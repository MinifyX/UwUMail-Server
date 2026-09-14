use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_FAILURES: u32 = 10;

/// Blocks login attempts from networks with too many recent failures.
#[derive(Default)]
pub struct AuthLimiter {
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

/// IPv6 users usually own a whole /64, so failures count per /64.
fn key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            IpAddr::V6(segments.into())
        }
    }
}

impl AuthLimiter {
    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let mut failures = self.failures.lock().expect("limiter poisoned");
        match failures.get(&key(ip)) {
            Some((_, since)) if since.elapsed() > WINDOW => {
                failures.remove(&key(ip));
                false
            }
            Some((count, _)) => *count >= MAX_FAILURES,
            None => false,
        }
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut failures = self.failures.lock().expect("limiter poisoned");
        if failures.len() > 100_000 {
            failures.retain(|_, (_, since)| since.elapsed() <= WINDOW);
        }
        let entry = failures.entry(key(ip)).or_insert((0, Instant::now()));
        if entry.1.elapsed() > WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    pub fn record_success(&self, ip: IpAddr) {
        self.failures.lock().expect("limiter poisoned").remove(&key(ip));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_repeated_failures() {
        let limiter = AuthLimiter::default();
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        let neighbour: IpAddr = "2001:db8::2".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(!limiter.is_blocked(ip));
            limiter.record_failure(ip);
        }
        assert!(limiter.is_blocked(neighbour));
        assert!(!limiter.is_blocked("192.0.2.1".parse().unwrap()));
        limiter.record_success(ip);
        assert!(!limiter.is_blocked(ip));
    }
}
