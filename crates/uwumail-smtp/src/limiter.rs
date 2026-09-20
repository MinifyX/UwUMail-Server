use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_FAILURES: u32 = 10;
/// Tries at logins that do not exist here before a network is turned away. Far fewer than
/// [`MAX_FAILURES`], because the two are not the same mistake: someone typing their own password
/// wrong gets ten tries, someone working through `info@`, `sales@` and `admin@` is not guessing a
/// password at all — they are reading the address book, and three names is plenty to see that.
///
/// Not one, on purpose. At one try, whether an address gets blocked would answer the question
/// "does this mailbox exist?" — and a stranger could ask it once per address from a fresh
/// connection. At three the answer costs three tries and the address itself, which is no way to
/// map anything, while a scanner still stops long before it would have.
const MAX_UNKNOWN: u32 = 3;

/// What a network has been up to in this window.
#[derive(Default, Clone, Copy)]
struct Failures {
    /// Logins that exist, with the wrong password.
    wrong: u32,
    /// Logins that do not exist here.
    unknown: u32,
}

impl Failures {
    fn over_the_line(&self) -> bool {
        self.wrong >= MAX_FAILURES || self.unknown >= MAX_UNKNOWN
    }
}

/// Told when a network is turned away, so it can be kept away further out than this server.
pub type Reporter = Arc<dyn Fn(IpAddr, &str) + Send + Sync>;

/// Blocks login attempts from networks with too many recent failures.
#[derive(Default)]
pub struct AuthLimiter {
    failures: Mutex<HashMap<IpAddr, (Failures, Instant)>>,
    /// Where a new block is reported, when someone listens. The UwUMail Gateway does: it keeps the
    /// address off the machine in front, so the next try does not reach the house at all.
    reporter: RwLock<Option<Reporter>>,
}

/// IPv6 users usually own a whole /64, so failures count per /64. An IPv4-mapped address is
/// canonicalised first, so `::ffff:a.b.c.d` keys as the IPv4 address, not the shared `::` (S-27).
fn key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        v4 @ IpAddr::V4(_) => v4,
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            IpAddr::V6(segments.into())
        }
    }
}

impl AuthLimiter {
    /// Hands every new block to `reporter` as well. Set while the server runs, when a gateway is
    /// paired; cleared with `None` when it is forgotten.
    pub fn report_to(&self, reporter: Option<Reporter>) {
        *self.reporter.write().expect("limiter poisoned") = reporter;
    }

    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let mut failures = self.failures.lock().expect("limiter poisoned");
        match failures.get(&key(ip)) {
            Some((_, since)) if since.elapsed() > WINDOW => {
                failures.remove(&key(ip));
                false
            }
            Some((counted, _)) => counted.over_the_line(),
            None => false,
        }
    }

    /// A login that exists, with the wrong password.
    pub fn record_failure(&self, ip: IpAddr) {
        self.record(ip, false);
    }

    /// A login that does not exist here. Counted apart, and much more strictly.
    pub fn record_unknown_login(&self, ip: IpAddr) {
        self.record(ip, true);
    }

    fn record(&self, ip: IpAddr, unknown: bool) {
        let crossed = {
            let mut failures = self.failures.lock().expect("limiter poisoned");
            if failures.len() > 100_000 {
                failures.retain(|_, (_, since)| since.elapsed() <= WINDOW);
            }
            let entry = failures.entry(key(ip)).or_insert((Failures::default(), Instant::now()));
            if entry.1.elapsed() > WINDOW {
                *entry = (Failures::default(), Instant::now());
            }
            let before = entry.0.over_the_line();
            if unknown {
                entry.0.unknown += 1;
            } else {
                entry.0.wrong += 1;
            }
            // Only the try that crosses the line is reported, not every one after it.
            !before && entry.0.over_the_line()
        };
        if crossed {
            let reporter = self.reporter.read().expect("limiter poisoned").clone();
            if let Some(reporter) = reporter {
                reporter(ip, if unknown { "tried logins that do not exist" } else { "wrong passwords" });
            }
        }
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

    #[test]
    fn ipv4_mapped_addresses_are_not_all_one_key() {
        // `::ffff:a.b.c.d` must key as the IPv4 address, not the shared `::`, so one IPv4 client
        // does not throttle all of them (security-audit-0.5.2 S-27).
        let limiter = AuthLimiter::default();
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        for _ in 0..MAX_UNKNOWN {
            limiter.record_unknown_login(mapped);
        }
        assert!(limiter.is_blocked(mapped), "the offending client is blocked");
        assert!(limiter.is_blocked("192.0.2.1".parse().unwrap()), "by its canonical address too");
        assert!(!limiter.is_blocked("::ffff:192.0.2.2".parse().unwrap()), "another IPv4 client is not");
    }

    #[test]
    fn guessing_at_names_is_stopped_sooner_than_guessing_at_passwords() {
        let limiter = AuthLimiter::default();
        let ip: IpAddr = "192.0.2.7".parse().unwrap();
        for _ in 0..MAX_UNKNOWN - 1 {
            limiter.record_unknown_login(ip);
            assert!(!limiter.is_blocked(ip));
        }
        limiter.record_unknown_login(ip);
        assert!(limiter.is_blocked(ip), "three names that do not exist is enough");

        // Someone typing their own password wrong that often is still let in.
        let typing: IpAddr = "192.0.2.8".parse().unwrap();
        for _ in 0..MAX_UNKNOWN {
            limiter.record_failure(typing);
        }
        assert!(!limiter.is_blocked(typing));
    }

    #[test]
    fn a_new_block_is_reported_once() {
        let reported = Arc::new(Mutex::new(Vec::new()));
        let limiter = AuthLimiter::default();
        let seen = reported.clone();
        limiter.report_to(Some(Arc::new(move |ip, why: &str| {
            seen.lock().unwrap().push((ip, why.to_owned()));
        })));

        let ip: IpAddr = "192.0.2.9".parse().unwrap();
        for _ in 0..MAX_UNKNOWN + 5 {
            limiter.record_unknown_login(ip);
        }
        let reported = reported.lock().unwrap();
        assert_eq!(reported.len(), 1, "the try that crosses the line, and no more");
        assert_eq!(reported[0].0, ip);
        assert_eq!(reported[0].1, "tried logins that do not exist");
    }
}
