use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
///
/// A login that does not even resemble one here is the exception ([`AuthLimiter::block`]): it
/// keeps its network out at the first try. That does answer "is there anything like this
/// address?" once, and costs the asker the network for a day; the count of three stays for
/// everything that could be a real address typed wrong, which is where the address book is.
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
/// Networks kept out for long, or remembered for having been, at once.
const MAX_BLOCKS: usize = 20_000;
/// How long a network stays out after a login that cannot be anyone's here: not an address at all,
/// or an address that resembles none ([`AuthLimiter::block`]).
pub const BLOCK_FIRST: Duration = Duration::from_secs(24 * 3600);
/// And when it comes back with another one.
pub const BLOCK_AGAIN: Duration = Duration::from_secs(7 * 24 * 3600);
/// How long a block is remembered after it ended, so that coming back counts as coming back.
pub const BLOCK_MEMORY: Duration = Duration::from_secs(30 * 24 * 3600);
/// How long a network that somebody logged in from is trusted not to be a scanner. A household's
/// mail app with a mangled login name gets the usual few tries instead of a day outside.
pub const KNOWN_FOR: Duration = Duration::from_secs(30 * 24 * 3600);
/// A network already known is written down again this often, not on every login.
const KNOWN_REFRESH: u64 = 24 * 3600;

/// Why a network is kept out, as the portal and the database name it.
pub mod reason {
    /// The login was not an e-mail address, and every login here is one.
    pub const NOT_AN_ADDRESS: &str = "not_an_address";
    /// An address nobody here has, and that looks like nobody's.
    pub const UNKNOWN_LOGIN: &str = "unknown_login";
    /// Several logins that do not exist, each of which could have been a slip.
    pub const UNKNOWN_LOGINS: &str = "unknown_logins";
    pub const WRONG_PASSWORDS: &str = "wrong_passwords";
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// A network that is kept out, or was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The address that did it. The whole /64 of an IPv6 address is kept out with it.
    pub ip: IpAddr,
    /// One of [`reason`].
    pub reason: String,
    /// The login that was tried, when one try did it.
    pub login: String,
    /// "imap", "smtp", "web", ...
    pub protocol: String,
    /// Seconds since 1970.
    pub since: u64,
    pub until: u64,
    /// How often this network has been blocked, this time included.
    pub times: u32,
}

/// An address or a network in the list of those that are never blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    address: IpAddr,
    prefix: u8,
}

impl Network {
    /// How many leading bits of an address the network fixes.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    pub fn is_ipv4(&self) -> bool {
        self.address.is_ipv4()
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

impl std::str::FromStr for Network {
    type Err = ();

    /// `192.0.2.7`, `192.0.2.0/24`, `2001:db8::/32`.
    fn from_str(text: &str) -> Result<Self, ()> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let address = address.parse::<IpAddr>().map_err(|_| ())?.to_canonical();
        let longest = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            // No signs and no spaces, which `parse` alone would let through.
            Some(prefix) if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()) => {
                prefix.parse::<u8>().map_err(|_| ())?
            }
            Some(_) => return Err(()),
            None => longest,
        };
        if prefix > longest {
            return Err(());
        }
        Ok(Network { address, prefix })
    }
}

impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let longest = if self.address.is_ipv4() { 32 } else { 128 };
        if self.prefix == longest {
            write!(f, "{}", self.address)
        } else {
            write!(f, "{}/{}", self.address, self.prefix)
        }
    }
}

/// Whether strangers can come from this address at all. One that only exists in the house or on
/// the machine is a reverse proxy, a container network or the people living here: counting its
/// mistakes is fine, keeping it out for a day would keep out everyone behind it.
fn public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // 100.64.0.0/10, what carriers and overlay networks hand out.
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback() || v6.is_unspecified() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80)
        }
    }
}

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

/// What a failed try was.
enum Tried {
    /// A login that does not exist here.
    Unknown,
    /// A wrong secret, for this login if it named one.
    Wrong(Option<String>),
}

/// Wrong passwords for one login, from wherever they came.
struct AccountFailures {
    count: u32,
    last: Instant,
}

/// Told when a network is turned away and for how long, so it can be kept away further out than
/// this server; and with `None` when it is let back in before its time.
pub type Reporter = Arc<dyn Fn(IpAddr, Option<Duration>, &str) + Send + Sync>;

/// Told that somebody logged in from a network, so it is still known after a restart.
pub(crate) type Remember = Box<dyn Fn(IpAddr, u64) + Send + Sync>;

/// What the window's own, short blocks are worth further out. Longer than the window: whoever
/// crossed the line once is not owed a fresh count every quarter of an hour at the front door.
const WINDOW_BLOCK_OUTSIDE: Duration = Duration::from_secs(3600);

/// Password checks under way, per network and per login.
#[derive(Default)]
struct InFlight {
    networks: HashMap<IpAddr, u32>,
    logins: HashMap<String, u32>,
}

/// Blocks login attempts from networks with too many recent failures, and spaces out the tries at
/// a login that has had too many from anywhere.
///
/// A success only takes back the failures of the login that succeeded. Clearing its whole network
/// let anyone with an account of their own reset the count between guesses at somebody else's
/// (security-audit-0.8.0 W-1).
///
/// One limiter serves the whole server: the portal, IMAP, ManageSieve, SMTP, JMAP and DAV all count
/// into it, so guesses spread over several protocols are counted together. The store holds it
/// ([`crate::Store::auth_limiter`]).
#[derive(Default)]
pub struct AuthLimiter {
    failures: Mutex<HashMap<IpAddr, (Failures, Instant)>>,
    accounts: Mutex<HashMap<String, AccountFailures>>,
    /// Checks that have started and not ended yet. They count like failures until they end: the
    /// failures above are only known once a check is over, and a burst of tries that all start
    /// before the first one ends would otherwise all get past a count that still says nothing
    /// (security-audit-0.16.0 WEB-1).
    in_flight: Mutex<InFlight>,
    /// Where a new block is reported, when someone listens. The UwUMail Gateway does: it keeps the
    /// address off the machine in front, so the next try does not reach the house at all.
    reporter: RwLock<Option<Reporter>>,
    /// Networks kept out for a day or a week, and those that were not long ago. Unlike the counts
    /// above these are written to the database ([`crate::Store::unknown_login`]).
    blocks: Mutex<HashMap<IpAddr, Block>>,
    /// Networks somebody logged in from, and when last.
    known: Mutex<HashMap<IpAddr, u64>>,
    remember: std::sync::OnceLock<Remember>,
    /// Never blocked, whatever comes from them: the admin's list.
    exempt: RwLock<Vec<Network>>,
}

/// IPv6 users usually own a whole /64, so failures count per /64. An IPv4-mapped address is
/// canonicalised first, so `::ffff:a.b.c.d` keys as the IPv4 address, not the shared `::` (S-27).
pub(crate) fn key(ip: IpAddr) -> IpAddr {
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
        if self.is_exempt(ip) {
            return false;
        }
        if self.kept_out(ip) {
            return true;
        }
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

    /// Lets one password check for `login` from `ip` begin, or says no (`None`) when the network is
    /// blocked or the login has to wait. Unlike [`Self::is_blocked`] and
    /// [`Self::account_throttled`], checks that are still running count as failures here, until
    /// the returned [`Attempt`] is dropped: at most [`MAX_FAILURES`] checks from one network, and
    /// [`ACCOUNT_FREE_FAILURES`] for one login, can be under way at once, fewer the more have
    /// failed lately. Once a login is spaced out, only one try at it runs at a time.
    ///
    /// Record the outcome with `record_*` as before; the attempt only ends the reservation.
    pub fn begin(self: &Arc<Self>, ip: IpAddr, login: &str) -> Option<Attempt> {
        let network = key(ip);
        let login = login_key(login);
        let exempt = self.is_exempt(ip);
        if !exempt && self.kept_out(ip) {
            return None;
        }
        let wrong = {
            let mut failures = self.failures.lock().expect("limiter poisoned");
            match failures.get(&network) {
                Some((_, since)) if since.elapsed() > WINDOW => {
                    failures.remove(&network);
                    0
                }
                Some((counted, _)) if counted.over_the_line() && !exempt => return None,
                Some((counted, _)) => counted.wrong,
                None => 0,
            }
        };
        let (count, last) = {
            let mut accounts = self.accounts.lock().expect("limiter poisoned");
            match accounts.get(&login) {
                Some(account) if account.last.elapsed() > WINDOW => {
                    accounts.remove(&login);
                    (0, None)
                }
                Some(account) => (account.count, Some(account.last)),
                None => (0, None),
            }
        };
        let mut in_flight = self.in_flight.lock().expect("limiter poisoned");
        let from_network = in_flight.networks.get(&network).copied().unwrap_or(0);
        let for_login = in_flight.logins.get(&login).copied().unwrap_or(0);
        if !exempt && wrong + from_network >= MAX_FAILURES {
            return None;
        }
        let spaced_out = count + for_login >= ACCOUNT_FREE_FAILURES;
        if spaced_out && (for_login > 0 || last.is_some_and(|last| last.elapsed() < ACCOUNT_SPACING)) {
            return None;
        }
        *in_flight.networks.entry(network).or_default() += 1;
        *in_flight.logins.entry(login.clone()).or_default() += 1;
        Some(Attempt { limiter: self.clone(), network, login })
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
        self.record(ip, Tried::Wrong(Some(login)));
    }

    /// A wrong secret that names no login by itself, like a bearer token (an app password or an
    /// OAuth token alone). It counts for the network like a wrong password.
    pub fn record_wrong_token(&self, ip: IpAddr) {
        self.record(ip, Tried::Wrong(None));
    }

    /// A login that does not exist here. Counted apart, and much more strictly.
    pub fn record_unknown_login(&self, ip: IpAddr) {
        self.record(ip, Tried::Unknown);
    }

    fn record(&self, ip: IpAddr, tried: Tried) {
        // Nothing to count for a network that is never turned away. What is tried at one login
        // from everywhere is still counted, and still spaced out.
        if self.is_exempt(ip) {
            return;
        }
        let unknown = matches!(tried, Tried::Unknown);
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
            match tried {
                Tried::Unknown => entry.0.unknown += 1,
                Tried::Wrong(None) => entry.0.wrong += 1,
                Tried::Wrong(Some(login)) => {
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
                let why = if unknown { "tried logins that do not exist" } else { "wrong passwords" };
                reporter(ip, Some(WINDOW_BLOCK_OUTSIDE), why);
            }
        }
    }

    /// `login` got in from `ip`. Its own mistakes are forgiven, on that network and everywhere;
    /// what others tried from the same network stays counted until the window ends.
    pub fn record_success(&self, ip: IpAddr, login: &str) {
        let login = login_key(login);
        self.accounts.lock().expect("limiter poisoned").remove(&login);
        {
            let mut failures = self.failures.lock().expect("limiter poisoned");
            if let Some((counted, _)) = failures.get_mut(&key(ip))
                && let Some(own) = counted.by_login.remove(&login)
            {
                counted.wrong = counted.wrong.saturating_sub(own);
            }
        }
        let now = unix_now();
        let before = {
            let mut known = self.known.lock().expect("limiter poisoned");
            if known.len() > MAX_ENTRIES {
                known.retain(|_, seen| now.saturating_sub(*seen) <= KNOWN_FOR.as_secs());
            }
            known.insert(key(ip), now)
        };
        if before.is_none_or(|before| now.saturating_sub(before) >= KNOWN_REFRESH)
            && let Some(remember) = self.remember.get()
        {
            remember(key(ip), now);
        }
    }

    /// Whether `ip` is in the list of networks that are never blocked.
    pub fn is_exempt(&self, ip: IpAddr) -> bool {
        self.exempt.read().expect("limiter poisoned").iter().any(|network| network.contains(ip))
    }

    /// The networks that are never blocked.
    pub fn exempt(&self) -> Vec<Network> {
        self.exempt.read().expect("limiter poisoned").clone()
    }

    /// Replaces the list of networks that are never blocked. Those of them that are kept out right
    /// now are let back in, here and wherever blocks are reported; they are returned.
    pub fn set_exempt(&self, networks: Vec<Network>) -> Vec<IpAddr> {
        // Asked before the list changes: afterwards none of them counts as kept out any more.
        let blocked = self.blocked();
        *self.exempt.write().expect("limiter poisoned") = networks;
        let inside: Vec<IpAddr> = blocked.into_iter().map(|block| block.ip).filter(|ip| self.is_exempt(*ip)).collect();
        for ip in &inside {
            self.unblock(*ip);
        }
        inside
    }

    /// Whether somebody logged in from this network within [`KNOWN_FOR`].
    pub fn is_known(&self, ip: IpAddr) -> bool {
        let known = self.known.lock().expect("limiter poisoned");
        known.get(&key(ip)).is_some_and(|seen| unix_now().saturating_sub(*seen) <= KNOWN_FOR.as_secs())
    }

    /// Whether one bad login may keep `ip` out for a day: only an address strangers come from,
    /// that nobody logged in from lately and that the admin did not except.
    pub fn may_block_at_once(&self, ip: IpAddr) -> bool {
        public(ip) && !self.is_exempt(ip) && !self.is_known(ip)
    }

    fn kept_out(&self, ip: IpAddr) -> bool {
        let blocks = self.blocks.lock().expect("limiter poisoned");
        blocks.get(&key(ip)).is_some_and(|block| block.until > unix_now())
    }

    /// Keeps `ip` out from now on: [`BLOCK_FIRST`], or [`BLOCK_AGAIN`] when it was blocked before
    /// and that is not forgotten yet. Returns the block for writing down, or `None` when the
    /// network is kept out already, or may not be ([`Self::may_block_at_once`]).
    pub fn block(&self, ip: IpAddr, reason: &str, login: &str, protocol: &str) -> Option<Block> {
        if !self.may_block_at_once(ip) {
            return None;
        }
        let now = unix_now();
        let block = {
            let mut blocks = self.blocks.lock().expect("limiter poisoned");
            if blocks.len() >= MAX_BLOCKS {
                blocks.retain(|_, block| block.until + BLOCK_MEMORY.as_secs() > now);
                // Full of networks that are still remembered: the next one is counted like any
                // other failed login instead, so the list cannot be grown without end.
                if blocks.len() >= MAX_BLOCKS && !blocks.contains_key(&key(ip)) {
                    return None;
                }
            }
            let before = match blocks.get(&key(ip)) {
                Some(block) if block.until > now => return None,
                Some(block) if block.until + BLOCK_MEMORY.as_secs() > now => block.times,
                _ => 0,
            };
            let how_long = if before == 0 { BLOCK_FIRST } else { BLOCK_AGAIN };
            let block = Block {
                ip,
                reason: reason.to_owned(),
                // Whatever was sent as a login, cut to what a login can be.
                login: login.trim().chars().filter(|c| !c.is_control()).take(320).collect(),
                protocol: protocol.to_owned(),
                since: now,
                until: now + how_long.as_secs(),
                times: before + 1,
            };
            blocks.insert(key(ip), block.clone());
            block
        };
        let reporter = self.reporter.read().expect("limiter poisoned").clone();
        if let Some(reporter) = reporter {
            let why = if reason == reason::NOT_AN_ADDRESS {
                "tried a login that is not an address"
            } else {
                "tried a login that nobody here has"
            };
            reporter(ip, Some(Duration::from_secs(block.until - block.since)), why);
        }
        Some(block)
    }

    /// Lets `ip` back in and forgets what it did, here and wherever blocks are reported. Says
    /// whether there was anything to forget here; further out it is let back in either way, a
    /// block there outlives the count it came from.
    pub fn unblock(&self, ip: IpAddr) -> bool {
        let lasting = self.blocks.lock().expect("limiter poisoned").remove(&key(ip)).is_some();
        let counted = self.failures.lock().expect("limiter poisoned").remove(&key(ip)).is_some();
        let reporter = self.reporter.read().expect("limiter poisoned").clone();
        if let Some(reporter) = reporter {
            reporter(ip, None, "");
        }
        lasting || counted
    }

    /// Every network that is kept out right now, the newest block first: the lasting ones, and
    /// those that are over the line for what is left of their window.
    pub fn blocked(&self) -> Vec<Block> {
        let now = unix_now();
        let mut blocked: Vec<Block> = {
            let blocks = self.blocks.lock().expect("limiter poisoned");
            blocks.values().filter(|block| block.until > now).cloned().collect()
        };
        {
            let failures = self.failures.lock().expect("limiter poisoned");
            for (network, (counted, since)) in failures.iter() {
                let age = since.elapsed();
                if age > WINDOW || !counted.over_the_line() || blocked.iter().any(|block| key(block.ip) == *network) {
                    continue;
                }
                let unknown = counted.unknown >= MAX_UNKNOWN;
                blocked.push(Block {
                    ip: *network,
                    reason: if unknown { reason::UNKNOWN_LOGINS } else { reason::WRONG_PASSWORDS }.to_owned(),
                    login: String::new(),
                    protocol: String::new(),
                    since: now.saturating_sub(age.as_secs()),
                    until: now + (WINDOW - age).as_secs(),
                    times: 1,
                });
            }
        }
        blocked.retain(|block| !self.is_exempt(block.ip));
        blocked.sort_by(|a, b| b.since.cmp(&a.since).then_with(|| a.ip.cmp(&b.ip)));
        blocked
    }

    /// What the database remembers, put back after a start. Nothing is reported: whoever was told
    /// about these blocks was told when they were made.
    pub(crate) fn restore(&self, blocks: Vec<Block>, known: Vec<(IpAddr, u64)>, exempt: Vec<Network>) {
        self.blocks.lock().expect("limiter poisoned").extend(blocks.into_iter().map(|block| (key(block.ip), block)));
        self.known.lock().expect("limiter poisoned").extend(known.into_iter().map(|(ip, seen)| (key(ip), seen)));
        *self.exempt.write().expect("limiter poisoned") = exempt;
    }

    /// Where a network somebody logged in from is written down. Set once, by the store.
    pub(crate) fn remember_with(&self, remember: Remember) {
        let _ = self.remember.set(remember);
    }
}

/// A password check under way, from [`AuthLimiter::begin`]. It counts until it is dropped.
pub struct Attempt {
    limiter: Arc<AuthLimiter>,
    network: IpAddr,
    login: String,
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let mut in_flight = self.limiter.in_flight.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = in_flight.networks.get_mut(&self.network) {
            *count -= 1;
            if *count == 0 {
                in_flight.networks.remove(&self.network);
            }
        }
        if let Some(count) = in_flight.logins.get_mut(&self.login) {
            *count -= 1;
            if *count == 0 {
                in_flight.logins.remove(&self.login);
            }
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
    fn checks_under_way_count_before_they_end() {
        // security-audit-0.16.0 WEB-1: a burst of tries that all start before the first one ends
        // must not all get through a count that only sees finished failures.
        let limiter = Arc::new(AuthLimiter::default());
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        let running: Vec<Attempt> = (0..MAX_FAILURES)
            .map(|n| limiter.begin(ip, &format!("user{n}@example.org")).expect("room for this one"))
            .collect();
        assert!(limiter.begin("2001:db8::2".parse().unwrap(), "other@example.org").is_none(), "the /64 is full");
        assert!(limiter.begin("192.0.2.1".parse().unwrap(), "other@example.org").is_some(), "other networks are not");
        drop(running);
        assert!(limiter.begin(ip, "user0@example.org").is_some(), "ended checks give their place back");

        // Failures that did finish leave fewer places.
        for _ in 0..MAX_FAILURES - 2 {
            limiter.record_failure(ip, "mini@example.org");
        }
        let first = limiter.begin(ip, "mini@example.org").expect("two places left");
        let second = limiter.begin(ip, "nyu@example.org").expect("one place left");
        assert!(limiter.begin(ip, "admin@example.org").is_none());
        drop((first, second));
        assert!(limiter.begin(ip, "admin@example.org").is_some());
    }

    #[test]
    fn a_spaced_out_login_gets_one_try_at_a_time() {
        let limiter = Arc::new(AuthLimiter::default());
        for network in 0..ACCOUNT_FREE_FAILURES {
            limiter.record_failure(format!("198.51.100.{network}").parse().unwrap(), "admin@example.org");
        }
        assert!(limiter.begin("203.0.113.1".parse().unwrap(), "admin@example.org").is_none(), "too soon");
        assert!(limiter.begin("203.0.113.1".parse().unwrap(), "mini@example.org").is_some());

        // Under the line, concurrent tries at one login are bounded by what is left of it.
        let limiter = Arc::new(AuthLimiter::default());
        let running: Vec<Attempt> = (0..ACCOUNT_FREE_FAILURES)
            .map(|n| limiter.begin(format!("198.51.100.{n}").parse().unwrap(), "admin@example.org").unwrap())
            .collect();
        assert!(limiter.begin("203.0.113.1".parse().unwrap(), "admin@example.org").is_none());
        drop(running);
        assert!(limiter.begin("203.0.113.1".parse().unwrap(), "admin@example.org").is_some());
    }

    #[test]
    fn a_new_block_is_reported_once() {
        let reported = Arc::new(Mutex::new(Vec::new()));
        let limiter = AuthLimiter::default();
        let seen = reported.clone();
        limiter.report_to(Some(Arc::new(move |ip, _how_long, why: &str| {
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

    #[test]
    fn one_login_that_cannot_exist_keeps_a_network_out_and_longer_the_second_time() {
        let reported = Arc::new(Mutex::new(Vec::new()));
        let limiter = Arc::new(AuthLimiter::default());
        let seen = reported.clone();
        limiter.report_to(Some(Arc::new(move |ip, how_long, why: &str| {
            seen.lock().unwrap().push((ip, how_long, why.to_owned()));
        })));
        let ip: IpAddr = "2001:db8:7::1".parse().unwrap();
        let neighbour: IpAddr = "2001:db8:7::2".parse().unwrap();

        let block = limiter.block(ip, reason::NOT_AN_ADDRESS, " admin\r\n", "smtp").expect("blocked");
        assert_eq!((block.times, block.until - block.since, block.login.as_str()), (1, BLOCK_FIRST.as_secs(), "admin"));
        assert!(limiter.is_blocked(neighbour), "the whole /64");
        assert!(limiter.begin(neighbour, "mini@example.org").is_none());
        assert!(limiter.block(neighbour, reason::UNKNOWN_LOGIN, "x@example.org", "imap").is_none(), "already out");
        assert_eq!(limiter.blocked(), vec![block.clone()]);

        // It ended a while ago, and is not forgotten yet.
        let ended = Block { since: 1, until: unix_now() - 60, ..block };
        limiter.blocks.lock().unwrap().insert(key(ip), ended);
        assert!(!limiter.is_blocked(ip));
        assert!(limiter.blocked().is_empty());
        let again = limiter.block(ip, reason::UNKNOWN_LOGIN, "sales@example.org", "imap").expect("blocked again");
        assert_eq!((again.times, again.until - again.since), (2, BLOCK_AGAIN.as_secs()));

        assert!(limiter.unblock(ip));
        assert!(!limiter.is_blocked(ip));
        assert!(limiter.block(ip, reason::UNKNOWN_LOGIN, "sales@example.org", "imap").is_some_and(|b| b.times == 1));
        let reported = reported.lock().unwrap();
        assert_eq!(
            reported.iter().map(|(_, how_long, _)| *how_long).collect::<Vec<_>>(),
            [Some(BLOCK_FIRST), Some(BLOCK_AGAIN), None, Some(BLOCK_FIRST)]
        );
        assert_eq!(reported[0].2, "tried a login that is not an address");
    }

    #[test]
    fn known_local_and_excepted_networks_are_not_blocked_at_once() {
        let limiter = Arc::new(AuthLimiter::default());
        for local in
            ["127.0.0.1", "10.1.2.3", "192.168.178.20", "172.17.0.1", "100.64.0.9", "::1", "fd00::1", "fe80::1"]
        {
            assert!(limiter.block(local.parse().unwrap(), reason::NOT_AN_ADDRESS, "x", "imap").is_none(), "{local}");
        }

        let home: IpAddr = "198.51.100.7".parse().unwrap();
        limiter.record_success(home, "mini@example.org");
        assert!(limiter.is_known(home));
        assert!(limiter.block(home, reason::NOT_AN_ADDRESS, "mini", "imap").is_none());
        limiter.known.lock().unwrap().insert(key(home), unix_now() - KNOWN_FOR.as_secs() - 60);
        assert!(limiter.block(home, reason::NOT_AN_ADDRESS, "mini", "imap").is_some(), "known no longer");

        // Putting a network on the list lets it back in, and nothing it does counts after that.
        let office: IpAddr = "203.0.113.40".parse().unwrap();
        assert!(limiter.block(office, reason::NOT_AN_ADDRESS, "x", "imap").is_some());
        assert_eq!(limiter.set_exempt(vec!["203.0.113.0/24".parse().unwrap()]), vec![office]);
        assert!(!limiter.is_blocked(office));
        assert!(limiter.block(office, reason::NOT_AN_ADDRESS, "x", "imap").is_none());
        for _ in 0..MAX_FAILURES + MAX_UNKNOWN {
            limiter.record_unknown_login(office);
            limiter.record_failure(office, "mini@example.org");
        }
        assert!(!limiter.is_blocked(office));
        assert!(limiter.begin(office, "nyu@example.org").is_some());
        assert!(limiter.blocked().iter().all(|block| block.ip != office));
    }

    #[test]
    fn the_windows_own_blocks_are_listed_and_can_be_lifted() {
        let limiter = AuthLimiter::default();
        let ip: IpAddr = "192.0.2.50".parse().unwrap();
        for _ in 0..MAX_UNKNOWN {
            limiter.record_unknown_login(ip);
        }
        let blocked = limiter.blocked();
        assert_eq!((blocked.len(), blocked[0].ip, blocked[0].reason.as_str()), (1, ip, reason::UNKNOWN_LOGINS));
        assert!(blocked[0].until > blocked[0].since);
        assert!(limiter.unblock(ip));
        assert!(!limiter.is_blocked(ip));
        assert!(!limiter.unblock(ip), "nothing left to forget");
    }

    #[test]
    fn networks_are_read_and_written_the_same_way() {
        for (text, shown) in [
            ("192.0.2.7", "192.0.2.7"),
            (" 192.0.2.0/24 ", "192.0.2.0/24"),
            ("2001:db8::/32", "2001:db8::/32"),
            ("2001:db8::1/128", "2001:db8::1"),
            ("::ffff:192.0.2.7", "192.0.2.7"),
            ("0.0.0.0/0", "0.0.0.0/0"),
        ] {
            assert_eq!(text.parse::<Network>().unwrap().to_string(), shown);
        }
        for wrong in
            ["", "example.org", "192.0.2.0/33", "192.0.2.0/", "192.0.2.0/+8", "2001:db8::/129", "192.0.2.0/24/1"]
        {
            assert!(wrong.parse::<Network>().is_err(), "{wrong}");
        }
        let net: Network = "2001:db8:1::/48".parse().unwrap();
        assert!(net.contains("2001:db8:1:ffff::9".parse().unwrap()));
        assert!(!net.contains("2001:db8:2::9".parse().unwrap()));
        assert!(!net.contains("192.0.2.1".parse().unwrap()));
        assert!("192.0.2.0/24".parse::<Network>().unwrap().contains("::ffff:192.0.2.9".parse().unwrap()));
        assert!("0.0.0.0/0".parse::<Network>().unwrap().contains("203.0.113.1".parse().unwrap()));
    }
}
