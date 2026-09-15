//! Short-lived login state kept in memory: logins waiting for their second factor, and sessions
//! whose password was confirmed a moment ago for a sensitive change.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A login waits this long for its second factor.
const PENDING_LIFETIME: Duration = Duration::from_secs(5 * 60);
const MAX_ATTEMPTS: u32 = 5;
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

    /// Counts a wrong second factor. After too many, the login has to start over.
    pub fn failed(&self, token: &str) {
        let mut pending = self.pending.lock().expect("pending logins poisoned");
        if let Some(login) = pending.get_mut(token) {
            login.attempts += 1;
            if login.attempts >= MAX_ATTEMPTS {
                pending.remove(token);
            }
        }
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
}
