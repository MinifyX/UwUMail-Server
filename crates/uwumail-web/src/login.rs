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
        let token = random_token();
        let mut pending = self.pending.lock().expect("pending logins poisoned");
        pending.retain(|_, login| login.created.elapsed() < PENDING_LIFETIME);
        pending
            .insert(token.clone(), PendingLogin { account_id, created: Instant::now(), attempts: 0, challenge: None });
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

    /// Counts a wrong second factor. After too many, the login has to start over; after too many
    /// for the account, whichever login they came from, its second factor is locked for a while.
    /// Returns true for the one that locks it, so its owner can be told.
    pub fn failed(&self, token: &str) -> bool {
        let account_id = {
            let mut pending = self.pending.lock().expect("pending logins poisoned");
            let Some(login) = pending.get_mut(token) else { return false };
            login.attempts += 1;
            let account_id = login.account_id;
            if login.attempts >= MAX_ATTEMPTS {
                pending.remove(token);
            }
            account_id
        };
        let mut failures = self.second_factor_failures.lock().expect("second factor failures poisoned");
        failures.retain(|_, (_, last)| last.elapsed() < SECOND_FACTOR_LOCK);
        let (count, last) = failures.entry(account_id).or_insert((0, Instant::now()));
        *count += 1;
        *last = Instant::now();
        *count == MAX_ACCOUNT_ATTEMPTS
    }

    /// Whether the account's second factor is locked: no code, passkey or recovery code is even
    /// looked at until the lock runs out. Another account's success does not lift it.
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
            state.failed(&token);
        }
        assert_eq!(state.get(&token).map(|login| login.account_id), Some(7));
        state.failed(&token);
        assert!(state.get(&token).is_none());
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
                locked.push(state.failed(&token));
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
}
