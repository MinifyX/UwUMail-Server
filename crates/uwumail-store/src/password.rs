use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

use crate::{Result, StoreError};

/// Hash of a random password, so unknown logins cost as much time as known ones.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash(&hex::encode(crate::random_bytes::<16>())).expect("hashing a random password"))
}

pub fn hash(password: &str) -> Result<String> {
    if password.chars().count() < 8 {
        return Err(StoreError::Invalid("passwords need at least 8 characters".into()));
    }
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| StoreError::Internal(format!("could not hash password: {err}")))
}

/// Checks a password against a stored hash. `None` still spends the time of a real check.
pub fn verify(password: &str, stored: Option<&str>) -> bool {
    let (candidate, real) = match stored {
        Some(stored) => (stored, true),
        None => (dummy_hash(), false),
    };
    let Ok(parsed) = PasswordHash::new(candidate) else {
        return false;
    };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok() && real
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_verifies() {
        let stored = hash("correct horse battery").unwrap();
        assert!(verify("correct horse battery", Some(&stored)));
        assert!(!verify("wrong", Some(&stored)));
        assert!(!verify("correct horse battery", None));
        assert!(hash("short").is_err());
    }
}
