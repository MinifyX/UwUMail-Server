//! Networks kept out after one login that cannot be anyone's here, and the people who are not.
//!
//! Every login here is an e-mail address. Whoever tries `admin`, `1026` or `info@` at a domain
//! that has no such person is not mistyping their own name, they are working through a list, and
//! one try is enough to see that ([`Store::unknown_login`]). Whoever tries `minifx@` where there
//! is a `minifyx@` most likely slipped on a key, and gets the usual few tries instead.
//!
//! The counting itself is the limiter's (limiter.rs). What is here is what it cannot know — which
//! addresses exist — and what has to outlive a restart.

use std::net::IpAddr;

use rusqlite::params;

use crate::address::{base_local_part, normalize_address};
use crate::limiter::{BLOCK_MEMORY, Block, KNOWN_FOR, Known, Network, SHARED_FOR, key, reason};
use crate::{Result, Store, StoreError, now};

const EXEMPT_SETTING: &str = "login_block_exempt";
/// How many networks the list of those never blocked may hold.
pub const BLOCK_EXEMPT_LIMIT: usize = 200;
/// Keys that may be off, between what was typed and an address that exists, for it to still count
/// as that address mistyped.
const SLIPS: usize = 2;
/// No address is longer than this (RFC 5321), so nothing longer is compared with any.
const ADDRESS_MAX_CHARS: usize = 320;

/// How many single keys — one too many, one missing, a wrong one, two swapped — lie between `a`
/// and `b`, as far as that is at most [`SLIPS`]; anything further apart is `None`.
fn slips(a: &[char], b: &[char]) -> Option<usize> {
    if a.len().abs_diff(b.len()) > SLIPS {
        return None;
    }
    let mut before: Vec<usize> = Vec::new();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut row = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let same = a[i - 1] == b[j - 1];
            row[j] = (previous[j] + 1).min(row[j - 1] + 1).min(previous[j - 1] + usize::from(!same));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                row[j] = row[j].min(before[j - 2] + 1);
            }
        }
        // Rows only grow from here on, so a row without anything in reach ends it.
        if row.iter().all(|distance| *distance > SLIPS) {
            return None;
        }
        before = std::mem::replace(&mut previous, row);
    }
    Some(previous[b.len()]).filter(|distance| *distance <= SLIPS)
}

/// Whether `tried`, an address nobody here has, looks like `known` typed by its owner: a few keys
/// off, the right name at the wrong one of our domains, or the name with a `+tag`.
fn resembles(tried: (&str, &str), known: &str) -> bool {
    let Some((known_local, known_domain)) = known.rsplit_once('@') else {
        return false;
    };
    if tried.0 == known_local || base_local_part(tried.0) == known_local {
        return true;
    }
    // The domain is given away with every mail sent from it. Only being close in the name says
    // anything, so a name that is far off is not saved by getting the domain right.
    let local: Vec<char> = tried.0.chars().collect();
    let Some(in_name) = slips(&local, &known_local.chars().collect::<Vec<_>>()) else {
        return false;
    };
    if tried.1 == known_domain {
        return true;
    }
    let domain: Vec<char> = tried.1.chars().collect();
    slips(&domain, &known_domain.chars().collect::<Vec<_>>()).is_some_and(|in_domain| in_name + in_domain <= SLIPS)
}

/// Why `login`, which is nobody's here, cannot have been meant for anyone among `known`: one of
/// [`reason`]. `None` when it may well have been.
fn cannot_exist(login: &str, known: &[String]) -> Option<&'static str> {
    let login = login.trim();
    if login.chars().count() > ADDRESS_MAX_CHARS {
        return Some(reason::NOT_AN_ADDRESS);
    }
    let Ok((local, domain)) = normalize_address(login) else {
        // Just the name, without the domain: what many mail apps try on their own while they are
        // being set up, and what people type who have one address and never think of the rest.
        let name = login.to_lowercase();
        let somebody = known.iter().any(|known| known.rsplit_once('@').is_some_and(|(local, _)| local == name));
        return (!somebody).then_some(reason::NOT_AN_ADDRESS);
    };
    let somebody = known.iter().any(|known| resembles((&local, &domain), known));
    (!somebody).then_some(reason::UNKNOWN_LOGIN)
}

fn block_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Block>> {
    let Ok(ip) = row.get::<_, String>(0)?.parse::<IpAddr>() else {
        return Ok(None);
    };
    Ok(Some(Block {
        ip,
        reason: row.get(1)?,
        login: row.get(2)?,
        protocol: row.get(3)?,
        since: row.get::<_, i64>(4)?.max(0) as u64,
        until: row.get::<_, i64>(5)?.max(0) as u64,
        times: row.get(6)?,
    }))
}

impl Store {
    /// Puts back what the limiter knew before the server stopped: who is kept out, who logged in
    /// lately, and whom the admin excepted. From here on it is told when to write things down.
    pub(crate) async fn restore_blocks(&self) -> Result<()> {
        let (blocks, known, exempt) = self
            .read(|conn| {
                let now = now();
                let mut stmt = conn.prepare(
                    "SELECT ip, reason, login, protocol, blocked_at, until, times FROM ip_blocks WHERE until > ?1",
                )?;
                let blocks: Vec<Block> = stmt
                    .query_map([now - BLOCK_MEMORY.as_secs() as i64], block_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                let mut stmt = conn.prepare(
                    "SELECT network, last_seen, login, shared FROM known_networks
                     WHERE last_seen > ?1 OR (shared AND last_seen > ?2)",
                )?;
                let known: Vec<(IpAddr, Known)> = stmt
                    .query_map([now - KNOWN_FOR.as_secs() as i64, now - SHARED_FOR.as_secs() as i64], |row| {
                        let known = Known {
                            seen: row.get::<_, i64>(1)?.max(0) as u64,
                            login: row.get(2)?,
                            shared: row.get(3)?,
                        };
                        Ok((row.get::<_, String>(0)?, known))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .filter_map(|(network, known)| Some((network.parse().ok()?, known)))
                    .collect();
                let exempt = crate::db::get_setting(conn, EXEMPT_SETTING)?;
                Ok((blocks, known, exempt))
            })
            .await?;
        let exempt: Vec<String> = exempt.and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default();
        let exempt = exempt.iter().filter_map(|network| network.parse().ok()).collect();
        let limiter = self.auth_limiter();
        limiter.restore(blocks, known, exempt);

        // Weak, or the store would hold itself through its own limiter.
        let inner = std::sync::Arc::downgrade(&self.inner);
        limiter.remember_with(Box::new(move |network, seen, login, shared| {
            let login = login.to_owned();
            let (Some(inner), Ok(runtime)) = (inner.upgrade(), tokio::runtime::Handle::try_current()) else {
                return;
            };
            let store = Store { inner };
            runtime.spawn(async move {
                let written = store
                    .write(move |tx| {
                        tx.execute(
                            "INSERT INTO known_networks (network, last_seen, login, shared) VALUES (?1, ?2, ?3, ?4)
                             ON CONFLICT (network) DO UPDATE SET
                                 -- Two logins close together are written in whatever order they
                                 -- get here, so neither may take back what the other noted.
                                 login = CASE WHEN last_seen > ?5 AND login != '' THEN login ELSE excluded.login END,
                                 shared = CASE WHEN last_seen > ?5 THEN MAX(shared, excluded.shared) ELSE excluded.shared END,
                                 last_seen = MAX(last_seen, excluded.last_seen)",
                            params![network.to_string(), seen as i64, login, shared, seen as i64 - KNOWN_FOR.as_secs() as i64],
                        )?;
                        tx.execute(
                            "DELETE FROM known_networks
                             WHERE last_seen <= ?2 OR (NOT shared AND last_seen <= ?1)",
                            [seen as i64 - KNOWN_FOR.as_secs() as i64, seen as i64 - SHARED_FOR.as_secs() as i64],
                        )?;
                        Ok(())
                    })
                    .await;
                if let Err(err) = written {
                    tracing::warn!(%err, "could not note a network somebody logged in from");
                }
            });
        }));
        Ok(())
    }

    /// Somebody at `ip` tried `login`, and there is no such login here.
    ///
    /// When it cannot have been meant for anyone — it is not an address, or it resembles none of
    /// ours — the network is kept out at once: a day, a week when it comes back. Otherwise, and
    /// for networks that are spared that ([`crate::AuthLimiter::may_block_at_once`]), it is one
    /// more unknown login in the count.
    ///
    /// Says nothing back and never fails: the answer to the client is the same either way.
    pub async fn unknown_login(&self, ip: IpAddr, login: &str, protocol: &str) {
        let limiter = self.auth_limiter();
        let verdict = if limiter.may_block_at_once(ip) {
            let tried = login.to_owned();
            let asked = self
                .read(move |conn| {
                    // Where a directory makes accounts at the first login, a login nobody has
                    // here may still be somebody's.
                    let on =
                        |key: &str| Ok::<_, StoreError>(crate::db::get_setting(conn, key)?.as_deref() == Some("true"));
                    if on("auth.ldap.enabled")? && on("auth.ldap.auto_create")? {
                        return Ok(None);
                    }
                    let mut stmt = conn.prepare(
                        "SELECT login FROM accounts
                         UNION
                         SELECT a.local_part || '@' || d.name FROM addresses a JOIN domains d ON d.id = a.domain_id",
                    )?;
                    let known =
                        stmt.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(cannot_exist(&tried, &known))
                })
                .await;
            // Not knowing is no reason to keep anyone out for a day.
            asked.unwrap_or(None)
        } else {
            None
        };
        let Some(block) = verdict.and_then(|why| limiter.block(ip, why, login, protocol)) else {
            limiter.record_unknown_login(ip);
            return;
        };
        tracing::warn!(
            %ip,
            login = %block.login,
            protocol,
            reason = %block.reason,
            hours = (block.until - block.since) / 3600,
            times = block.times,
            "blocked an address: nobody here could have meant this login"
        );
        let written = self
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO ip_blocks (network, ip, reason, login, protocol, blocked_at, until, times)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (network) DO UPDATE SET ip = excluded.ip, reason = excluded.reason,
                         login = excluded.login, protocol = excluded.protocol, blocked_at = excluded.blocked_at,
                         until = excluded.until, times = excluded.times",
                    params![
                        key(block.ip).to_string(),
                        block.ip.to_string(),
                        block.reason,
                        block.login,
                        block.protocol,
                        block.since as i64,
                        block.until as i64,
                        block.times
                    ],
                )?;
                tx.execute("DELETE FROM ip_blocks WHERE until <= ?1", [now() - BLOCK_MEMORY.as_secs() as i64])?;
                Ok(())
            })
            .await;
        if let Err(err) = written {
            // It holds until the next restart all the same.
            tracing::warn!(%err, "could not write down a blocked address");
        }
    }

    /// Lets `ip` back in, here and at the gateway, and forgets it was ever kept out. Says whether
    /// it was.
    pub async fn unblock(&self, ip: IpAddr) -> Result<bool> {
        let lifted = self.auth_limiter().unblock(ip);
        let network = key(ip).to_string();
        let deleted =
            self.write(move |tx| Ok(tx.execute("DELETE FROM ip_blocks WHERE network = ?1", [network])?)).await?;
        Ok(lifted || deleted > 0)
    }

    /// Replaces the list of addresses and networks that are never blocked, and lets back in those
    /// of them that are kept out. Returns the list as it is kept.
    pub async fn set_block_exempt(&self, networks: &[String]) -> Result<Vec<String>> {
        if networks.len() > BLOCK_EXEMPT_LIMIT {
            return Err(StoreError::Invalid(format!("at most {BLOCK_EXEMPT_LIMIT} addresses can be excepted")));
        }
        let mut parsed: Vec<Network> = Vec::with_capacity(networks.len());
        for text in networks {
            let network: Network = text
                .parse()
                .map_err(|()| StoreError::Invalid(format!("'{}' is not an address or a network", text.trim())))?;
            // Half the internet is not an office: a list like that would switch the limits off.
            let least = if network.is_ipv4() { 8 } else { 16 };
            if network.prefix() < least {
                return Err(StoreError::Invalid(format!("'{network}' is too large a network (at most /{least})")));
            }
            if !parsed.contains(&network) {
                parsed.push(network);
            }
        }
        let kept: Vec<String> = parsed.iter().map(Network::to_string).collect();
        let json = serde_json::to_string(&kept).map_err(|err| StoreError::Internal(err.to_string()))?;
        self.set_setting(EXEMPT_SETTING, &json).await?;
        for ip in self.auth_limiter().set_exempt(parsed) {
            let network = key(ip).to_string();
            self.write(move |tx| Ok(tx.execute("DELETE FROM ip_blocks WHERE network = ?1", [network])?)).await?;
        }
        Ok(kept)
    }

    /// The addresses and networks that are never blocked.
    pub fn block_exempt(&self) -> Vec<String> {
        self.auth_limiter().exempt().iter().map(Network::to_string).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        ["minifyx@example.org", "nyu@example.org", "leni.k@example.net", "office@example.net"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn a_slip_of_the_keys_is_not_a_scanner() {
        for typo in [
            "minifx@example.org",   // one missing
            "minifyxx@example.org", // one too many
            "minfiyx@example.org",  // two swapped
            "minifyx@example.og",   // in the domain
            "mjnifyx@exampel.org",  // one in each
            "minifyx@example.net",  // the right name at the wrong domain
            "minifyx@example.com",  // or at one that is not ours at all
            "nyu+shop@example.org", // a tag
            "MiniFyx@Example.Org ",
            "nyi@example.org",
            "leni@example.net", // two missing
        ] {
            assert_eq!(cannot_exist(typo, &known()), None, "{typo}");
        }
    }

    #[test]
    fn a_login_nobody_could_have_meant_is_told_apart() {
        for stranger in
            ["admin@example.org", "info@example.org", "sales@example.net", "test@example.com", "mini@example.org"]
        {
            assert_eq!(cannot_exist(stranger, &known()), Some(reason::UNKNOWN_LOGIN), "{stranger}");
        }
        // Three off in the name is not saved by the right domain, nor two in the name by a wrong one.
        assert_eq!(cannot_exist("nya@example.org", &known()), None);
        assert_eq!(cannot_exist("abc@example.org", &known()), Some(reason::UNKNOWN_LOGIN));
        assert_eq!(cannot_exist("minify@exmple.org", &known()), None);
        assert_eq!(cannot_exist("minif@exmple.org", &known()), Some(reason::UNKNOWN_LOGIN));

        for not_one in ["admin", "1026", "gabriela", "", "  ", "@example.org", "a b@example.org", "root@", "nyu@@"] {
            assert_eq!(cannot_exist(not_one, &known()), Some(reason::NOT_AN_ADDRESS), "{not_one:?}");
        }
        // The bare name of somebody who is here is what mail apps try by themselves.
        assert_eq!(cannot_exist("Nyu", &known()), None);
        assert_eq!(cannot_exist("leni.k", &known()), None);
        assert_eq!(cannot_exist("ny", &known()), Some(reason::NOT_AN_ADDRESS));
        // Too long to be anything, and not worth comparing.
        assert_eq!(cannot_exist(&format!("{}@example.org", "ä".repeat(400)), &known()), Some(reason::NOT_AN_ADDRESS));
        assert_eq!(cannot_exist("nyu@example.org", &[]), Some(reason::UNKNOWN_LOGIN), "nobody here at all");
    }

    #[tokio::test]
    async fn a_shared_network_is_still_one_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let office: IpAddr = "198.51.100.20".parse().unwrap();
        let home: IpAddr = "2001:db8:5::1".parse().unwrap();
        store.auth_limiter().record_success(office, "minifyx@example.org");
        store.auth_limiter().record_success(office, "nyu@example.org");
        store.auth_limiter().record_success(home, "nyu@example.org");
        // Written down behind the login's back: wait for it instead of guessing how long it takes.
        let patience = std::time::Instant::now();
        loop {
            assert!(patience.elapsed() < std::time::Duration::from_secs(30), "never written down");
            let written: Vec<(String, String, bool)> = store
                .read(|conn| {
                    let mut stmt =
                        conn.prepare("SELECT network, login, shared FROM known_networks ORDER BY network")?;
                    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
                })
                .await
                .unwrap();
            if written.len() == 2 && written[0].2 {
                assert_eq!(written[0], ("198.51.100.20".to_owned(), "minifyx@example.org".to_owned(), true));
                assert_eq!(written[1], ("2001:db8:5::".to_owned(), "nyu@example.org".to_owned(), false));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        drop(store);

        let store = Store::open(dir.path()).await.unwrap();
        let limiter = store.auth_limiter();
        assert!(limiter.is_shared(office));
        assert!(limiter.is_known(home) && !limiter.is_shared(home));
        store.unknown_login(office, "root", "imap").await;
        assert!(!limiter.is_blocked(office), "counted, not kept out");
        limiter.record_success(home, "minifyx@example.org");
        assert!(limiter.is_shared(home), "the first login was remembered too");
    }

    #[test]
    fn slips_are_counted_up_to_two() {
        let count = |a: &str, b: &str| slips(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>());
        assert_eq!(count("nyu", "nyu"), Some(0));
        assert_eq!(count("nyu", "nuy"), Some(1));
        assert_eq!(count("nyu", "ny"), Some(1));
        assert_eq!(count("", "ab"), Some(2));
        assert_eq!(count("", ""), Some(0));
        assert_eq!(count("nyu", "abc"), None);
        assert_eq!(count("käse", "kaese"), Some(2));
        assert_eq!(count("admin", "marvin"), None);
    }
}
