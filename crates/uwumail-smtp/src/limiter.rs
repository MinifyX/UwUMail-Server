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
/// Wrong passwords for one login, from all networks together, before its tries are spaced out.
/// Many networks are cheap to come by; this is what keeps guessing at one account slow even then.
const ACCOUNT_FREE_FAILURES: u32 = 10;
/// After that, one try per login in this long. A delay and not a lockout: whoever knows a login
/// name must not be able to keep its owner out.
const ACCOUNT_SPACING: Duration = Duration::from_secs(30);
/// Logins a network's count keeps apart, so a success can take back its own mistakes. Failures
/// beyond them simply wait out the window.
const LOGINS_PER_NETWORK: usize = 16;
/// Entries kept before the expired ones are swept out.
const MAX_ENTRIES: usize = 100_000;

/// What a network has been up to in this window.
#[derive(Default, Clone)]
struct Failures {
    /// Logins that exist, with the wrong password.
    wrong: u32,
    /// Logins that do not exist here.
    unknown: u32,
    /// Of `wrong`, how many each login had.
    by_login: HashMap<String, u32>,
}

impl Failures {
    fn over_the_line(&self) -> bool {
        self.wrong >= MAX_FAILURES || self.unknown >= MAX_UNKNOWN
    }
}

/// Wrong passwords for one login, from wherever they came.
struct AccountFailures {
    count: u32,
    last: Instant,
}

/// Told when a network is turned away, so it can be kept away further out than this server.
pub type Reporter = Arc<dyn Fn(IpAddr, &str) + Send + Sync>;

/// Blocks login attempts from networks with too many recent failures, and spaces out the tries at
/// a login that has had too many from anywhere.
///
/// A success only takes back the failures of the login that succeeded. Clearing its whole network
/// let anyone with an account of their own reset the count between guesses at somebody else's
/// (security-audit-0.8.0 W-1).
#[derive(Default)]
pub struct AuthLimiter {
    failures: Mutex<HashMap<IpAddr, (Failures, Instant)>>,
    accounts: Mutex<HashMap<String, AccountFailures>>,
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

/// A login the way it is counted: `Mini@Example.org ` and `mini@example.org` are one.
fn login_key(login: &str) -> String {
    login.trim().to_lowercase()
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

    /// Whether a try at this login has to wait: it had [`ACCOUNT_FREE_FAILURES`] wrong passwords
    /// lately, and the last one is less than [`ACCOUNT_SPACING`] ago.
    pub fn account_throttled(&self, login: &str) -> bool {
        let mut accounts = self.accounts.lock().expect("limiter poisoned");
        let login = login_key(login);
        match accounts.get(&login) {
            Some(account) if account.last.elapsed() > WINDOW => {
                accounts.remove(&login);
                false
            }
            Some(account) => account.count >= ACCOUNT_FREE_FAILURES && account.last.elapsed() < ACCOUNT_SPACING,
            None => false,
        }
    }

    /// A login that exists, with the wrong password (or the wrong second factor).
    pub fn record_failure(&self, ip: IpAddr, login: &str) {
        let login = login_key(login);
        {
            let mut accounts = self.accounts.lock().expect("limiter poisoned");
            if accounts.len() > MAX_ENTRIES {
                accounts.retain(|_, account| account.last.elapsed() <= WINDOW);
            }
            let account = accounts.entry(login.clone()).or_insert(AccountFailures { count: 0, last: Instant::now() });
            if account.last.elapsed() > WINDOW {
                account.count = 0;
            }
            account.count += 1;
            account.last = Instant::now();
        }
        self.record(ip, Some(login));
    }

    /// A login that does not exist here. Counted apart, and much more strictly.
    pub fn record_unknown_login(&self, ip: IpAddr) {
        self.record(ip, None);
    }

    /// `login` is `None` for a login that does not exist.
    fn record(&self, ip: IpAddr, login: Option<String>) {
        let unknown = login.is_none();
        let crossed = {
            let mut failures = self.failures.lock().expect("limiter poisoned");
            if failures.len() > MAX_ENTRIES {
                failures.retain(|_, (_, since)| since.elapsed() <= WINDOW);
            }
            let entry = failures.entry(key(ip)).or_insert((Failures::default(), Instant::now()));
            if entry.1.elapsed() > WINDOW {
                *entry = (Failures::default(), Instant::now());
            }
            let before = entry.0.over_the_line();
            match login {
                None => entry.0.unknown += 1,
                Some(login) => {
                    entry.0.wrong += 1;
                    let known = entry.0.by_login.len();
                    if let Some(own) = entry.0.by_login.get_mut(&login) {
                        *own += 1;
                    } else if known < LOGINS_PER_NETWORK {
                        entry.0.by_login.insert(login, 1);
                    }
                }
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

    /// `login` got in from `ip`. Its own mistakes are forgiven, on that network and everywhere;
    /// what others tried from the same network stays counted until the window ends.
    pub fn record_success(&self, ip: IpAddr, login: &str) {
        let login = login_key(login);
        self.accounts.lock().expect("limiter poisoned").remove(&login);
        let mut failures = self.failures.lock().expect("limiter poisoned");
        if let Some((counted, _)) = failures.get_mut(&key(ip))
            && let Some(own) = counted.by_login.remove(&login)
        {
            counted.wrong = counted.wrong.saturating_sub(own);
        }
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
            limiter.record_failure(ip, "mini@example.org");
        }
        assert!(limiter.is_blocked(neighbour));
        assert!(!limiter.is_blocked("192.0.2.1".parse().unwrap()));
        limiter.record_success(ip, "Mini@Example.org");
        assert!(!limiter.is_blocked(ip), "someone's own mistakes are forgiven once they get in");
    }

    #[test]
    fn a_success_does_not_forgive_what_others_tried() {
        // security-audit-0.8.0 W-1: logging into one's own account now and then must not reset
        // the count of guesses at somebody else's.
        let limiter = AuthLimiter::default();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        for _ in 0..MAX_FAILURES - 1 {
            limiter.record_failure(ip, "admin@example.org");
        }
        limiter.record_success(ip, "mallory@example.org");
        limiter.record_success(ip, "nyu@example.org");
        assert!(!limiter.is_blocked(ip));
        limiter.record_failure(ip, "admin@example.org");
        assert!(limiter.is_blocked(ip), "the tenth guess still blocks");

        // A neighbour's typo and success, on the other hand, take back only the neighbour's own.
        let office: IpAddr = "198.51.100.1".parse().unwrap();
        for _ in 0..MAX_FAILURES - 1 {
            limiter.record_failure(office, "admin@example.org");
        }
        limiter.record_failure(office, "nyu@example.org");
        assert!(limiter.is_blocked(office));
        limiter.record_success(office, "nyu@example.org");
        assert!(!limiter.is_blocked(office));
        limiter.record_failure(office, "admin@example.org");
        assert!(limiter.is_blocked(office));
    }

    #[test]
    fn guesses_at_one_login_from_many_networks_are_spaced_out() {
        let limiter = AuthLimiter::default();
        for network in 0..ACCOUNT_FREE_FAILURES {
            assert!(!limiter.account_throttled("admin@example.org"));
            limiter.record_failure(format!("198.51.100.{network}").parse().unwrap(), "admin@example.org");
        }
        assert!(limiter.account_throttled("ADMIN@example.org"), "no network is blocked, but the login waits");
        assert!(!limiter.account_throttled("mini@example.org"), "other logins do not");
        limiter.record_success("192.0.2.1".parse().unwrap(), "mallory@example.org");
        assert!(limiter.account_throttled("admin@example.org"), "somebody else's success changes nothing");
        limiter.record_success("192.0.2.1".parse().unwrap(), "admin@example.org");
        assert!(!limiter.account_throttled("admin@example.org"), "its own success does");
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
            limiter.record_failure(typing, "mini@example.org");
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
