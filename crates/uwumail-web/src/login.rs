//! Short-lived login state kept in memory: logins waiting for their second factor, and sessions
//! whose password was confirmed a moment ago for a sensitive change.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A login waits this long for its second factor.
const PENDING_LIFETIME: Duration = Duration::from_secs(5 * 60);
const MAX_ATTEMPTS: u32 = 5;
/// Wrong second factors for one account, over all its pending logins, before every further try is
/// refused for [`SECOND_FACTOR_LOCK`]. A pending login only needs the right password, so without
/// this a new one gave five more tries each time (security-audit-0.8.0 W-1).
const MAX_ACCOUNT_ATTEMPTS: u32 = 10;
/// How long an account's second factor stays locked after the last wrong one that counted.
pub const SECOND_FACTOR_LOCK: Duration = Duration::from_secs(15 * 60);
/// After confirming the password, sensitive changes need no new confirmation for this long.
pub const CONFIRMATION_LIFETIME: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub struct PendingLogin {
    pub account_id: i64,
    created: Instant,
    attempts: u32,
    /// The WebAuthn challenge handed out for this login, if any.
    pub challenge: Option<Vec<u8>>,
    /// Where the first step happened: `password` here, or `oidc` at another provider.
    pub via: &'static str,
}

/// One try at a second factor, counted before it is checked: see [`LoginState::attempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct Attempt {
    /// Whether this try, if it is wrong, is the one that locks the account.
    locks: bool,
}

/// Why a second factor is not even looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The pending login is gone, ran out, or used up its tries.
    Expired,
    /// The account's second factor is locked for a while.
    Locked,
}

#[derive(Default)]
pub struct LoginState {
    pending: Mutex<HashMap<String, PendingLogin>>,
    confirmed: Mutex<HashMap<String, Instant>>,
    /// Challenges for adding a passkey, by session id.
    registrations: Mutex<HashMap<String, (Vec<u8>, Instant)>>,
    /// Wrong second factors per account, and when the last one was.
    second_factor_failures: Mutex<HashMap<i64, (u32, Instant)>>,
}

pub fn random_bytes() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system RNG failed");
    bytes
}

pub fn random_token() -> String {
    random_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

impl LoginState {
    /// Remembers a login whose password was right. The token goes to the browser.
    pub fn start(&self, account_id: i64) -> String {
        self.start_via(account_id, "password")
    }

    /// Remembers a login whose first step happened `via` somewhere else than the password here.
    pub fn start_via(&self, account_id: i64, via: &'static str) -> String {
        let token = random_token();
        let mut pending = self.pending.lock().expect("pending logins poisoned");
        pending.retain(|_, login| login.created.elapsed() < PENDING_LIFETIME);
        let login = PendingLogin { account_id, created: Instant::now(), attempts: 0, challenge: None, via };
        pending.insert(token.clone(), login);
        token
    }

    pub fn get(&self, token: &str) -> Option<PendingLogin> {
        let pending = self.pending.lock().expect("pending logins poisoned");
        pending.get(token).filter(|login| login.created.elapsed() < PENDING_LIFETIME).cloned()
    }

    pub fn set_challenge(&self, token: &str, challenge: Vec<u8>) -> bool {
        let mut pending = self.pending.lock().expect("pending logins poisoned");
        match pending.get_mut(token).filter(|login| login.created.elapsed() < PENDING_LIFETIME) {
            Some(login) => {
                login.challenge = Some(challenge);
                true
            }
            None => false,
        }
    }

    /// Takes one try at the second factor for the pending login `token`, before the code or passkey
    /// is checked. The try counts for the login and for the account right away, both under one lock,
    /// so tries sent side by side cannot all slip past the limits while the first are still being
    /// checked (security audit 2026-11 M-03). A right code then forgives the account its count
    /// ([`LoginState::second_factor_passed`]); a wrong one is reported with [`LoginState::wrong`].
    pub fn attempt(&self, token: &str) -> Result<Attempt, Refused> {
        let mut pending = self.pending.lock().expect("pending logins poisoned");
        let Some(login) = pending.get_mut(token).filter(|login| login.created.elapsed() < PENDING_LIFETIME) else {
            return Err(Refused::Expired);
        };
        if login.attempts >= MAX_ATTEMPTS {
            return Err(Refused::Expired);
        }
        let mut failures = self.second_factor_failures.lock().expect("second factor failures poisoned");
        failures.retain(|_, (_, last)| last.elapsed() < SECOND_FACTOR_LOCK);
        let (count, last) = failures.entry(login.account_id).or_insert((0, Instant::now()));
        if *count >= MAX_ACCOUNT_ATTEMPTS {
            return Err(Refused::Locked);
        }
        login.attempts += 1;
        *count += 1;
        *last = Instant::now();
        Ok(Attempt { locks: *count == MAX_ACCOUNT_ATTEMPTS })
    }

    /// The try was wrong. After the login's last try it has to start over; after too many for the
    /// account, whichever login they came from, its second factor stays locked for a while.
    /// Returns true for the try that locked it, so its owner can be told.
    pub fn wrong(&self, token: &str, attempt: Attempt) -> bool {
        let mut pending = self.pending.lock().expect("pending logins poisoned");
        if pending.get(token).is_some_and(|login| login.attempts >= MAX_ATTEMPTS) {
            pending.remove(token);
        }
        attempt.locks
    }

    /// Whether the account's second factor is locked: no code, passkey or recovery code is even
    /// looked at until the lock runs out. Another account's success does not lift it. The routes
    /// learn it from [`LoginState::attempt`].
    #[cfg(test)]
    pub fn second_factor_locked(&self, account_id: i64) -> bool {
        let failures = self.second_factor_failures.lock().expect("second factor failures poisoned");
        failures
            .get(&account_id)
            .is_some_and(|(count, last)| *count >= MAX_ACCOUNT_ATTEMPTS && last.elapsed() < SECOND_FACTOR_LOCK)
    }

    /// The account's second factor was right: its own count starts over.
    pub fn second_factor_passed(&self, account_id: i64) {
        self.second_factor_failures.lock().expect("second factor failures poisoned").remove(&account_id);
    }

    pub fn finish(&self, token: &str) -> Option<PendingLogin> {
        let mut pending = self.pending.lock().expect("pending logins poisoned");
        pending.remove(token).filter(|login| login.created.elapsed() < PENDING_LIFETIME)
    }

    pub fn start_registration(&self, session_id: &str) -> Vec<u8> {
        let challenge = random_bytes().to_vec();
        let mut registrations = self.registrations.lock().expect("registrations poisoned");
        registrations.retain(|_, (_, at)| at.elapsed() < PENDING_LIFETIME);
        registrations.insert(session_id.to_owned(), (challenge.clone(), Instant::now()));
        challenge
    }

    pub fn finish_registration(&self, session_id: &str) -> Option<Vec<u8>> {
        let mut registrations = self.registrations.lock().expect("registrations poisoned");
        registrations
            .remove(session_id)
            .filter(|(_, at)| at.elapsed() < PENDING_LIFETIME)
            .map(|(challenge, _)| challenge)
    }

    pub fn confirm(&self, session_id: &str) {
        let mut confirmed = self.confirmed.lock().expect("confirmations poisoned");
        confirmed.retain(|_, at| at.elapsed() < CONFIRMATION_LIFETIME);
        confirmed.insert(session_id.to_owned(), Instant::now());
    }

    pub fn recently_confirmed(&self, session_id: &str) -> bool {
        let confirmed = self.confirmed.lock().expect("confirmations poisoned");
        confirmed.get(session_id).is_some_and(|at| at.elapsed() < CONFIRMATION_LIFETIME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_login_survives_four_wrong_codes() {
        let state = LoginState::default();
        let token = state.start(7);
        for _ in 0..4 {
            let attempt = state.attempt(&token).unwrap();
            state.wrong(&token, attempt);
        }
        assert_eq!(state.get(&token).map(|login| login.account_id), Some(7));
        let attempt = state.attempt(&token).unwrap();
        state.wrong(&token, attempt);
        assert!(state.get(&token).is_none());
        assert_eq!(state.attempt(&token), Err(Refused::Expired));
        let token = state.start(7);
        assert!(state.finish(&token).is_some());
        assert!(state.finish(&token).is_none(), "a login finishes once");
    }

    #[test]
    fn a_new_pending_login_does_not_bring_new_tries() {
        // security-audit-0.8.0 W-1: the password alone starts a pending login, so the tries for
        // the second factor are counted per account as well.
        let state = LoginState::default();
        let mut locked = Vec::new();
        for _ in 0..2 {
            let token = state.start(7);
            for _ in 0..MAX_ATTEMPTS {
                assert!(!state.second_factor_locked(7));
                let attempt = state.attempt(&token).unwrap();
                locked.push(state.wrong(&token, attempt));
            }
        }
        assert!(state.second_factor_locked(7), "ten wrong codes over two logins lock the account");
        assert_eq!(locked.iter().filter(|locked| **locked).count(), 1, "its owner is told once");
        assert!(!state.second_factor_locked(8), "another account is not affected");
        state.second_factor_passed(8);
        assert!(state.second_factor_locked(7), "nor does another account's success lift it");
        state.second_factor_passed(7);
        assert!(!state.second_factor_locked(7));
    }

    #[test]
    fn tries_side_by_side_are_counted_before_they_are_checked() {
        // Security audit 2026-11 M-03: tries that are still being checked count already, so a
        // burst of them gets no further than the same tries one after another.
        let state = LoginState::default();
        let token = state.start(7);
        let taken: Vec<_> = (0..MAX_ATTEMPTS + 3).map(|_| state.attempt(&token)).collect();
        assert_eq!(taken.iter().filter(|taken| taken.is_ok()).count(), MAX_ATTEMPTS as usize);
        assert!(taken[MAX_ATTEMPTS as usize..].iter().all(|taken| *taken == Err(Refused::Expired)));
        // Over several pending logins, the account's limit holds the same way.
        let mut taken = 0;
        let mut locking = 0;
        for _ in 0..4 {
            let token = state.start(7);
            for _ in 0..MAX_ATTEMPTS {
                match state.attempt(&token) {
                    Ok(attempt) => {
                        taken += 1;
                        locking += usize::from(attempt.locks);
                    }
                    Err(refused) => assert_eq!(refused, Refused::Locked),
                }
            }
        }
        assert_eq!(taken + MAX_ATTEMPTS, MAX_ACCOUNT_ATTEMPTS);
        assert_eq!(locking, 1);
        assert!(state.second_factor_locked(7));
        // A right code among them forgives the account, as one alone would.
        state.second_factor_passed(7);
        assert!(state.attempt(&state.start(7)).is_ok());
    }
}
