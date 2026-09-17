use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

use crate::{Result, StoreError};

/// How dovecot and mailcow mark a bcrypt hash.
const BLF_CRYPT_PREFIX: &str = "{BLF-CRYPT}";

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

/// A bcrypt hash taken over from another server, without dovecot's scheme prefix.
fn bcrypt_hash(stored: &str) -> Option<&str> {
    let hash = stored.strip_prefix(BLF_CRYPT_PREFIX).unwrap_or(stored);
    ["$2a$", "$2b$", "$2x$", "$2y$"].iter().any(|version| hash.starts_with(version)).then_some(hash)
}

/// Whether a stored hash came from another server and should be replaced by our own once the
/// password is known.
pub fn is_imported(stored: &str) -> bool {
    bcrypt_hash(stored).is_some()
}

/// Checks a hash taken over from another server and returns it the way it is stored: bcrypt
/// (`{BLF-CRYPT}$2y$…` or `$2b$…`) or our own Argon2.
pub fn import_hash(stored: &str) -> Result<String> {
    let stored = stored.trim();
    if let Some(hash) = bcrypt_hash(stored) {
        // A well-formed bcrypt hash is 60 characters; checking a dummy password tells whether it parses.
        if hash.len() == 60 && bcrypt::verify("", hash).is_ok() {
            return Ok(format!("{BLF_CRYPT_PREFIX}{hash}"));
        }
    } else if PasswordHash::new(stored).is_ok_and(|parsed| parsed.algorithm.as_str().starts_with("argon2")) {
        return Ok(stored.to_owned());
    }
    Err(StoreError::Invalid("only bcrypt (BLF-CRYPT) and Argon2 password hashes can be taken over".into()))
}

/// Checks a password against a stored hash. `None` still spends the time of a real check.
pub fn verify(password: &str, stored: Option<&str>) -> bool {
    if let Some(hash) = stored.and_then(bcrypt_hash) {
        return bcrypt::verify(password, hash).unwrap_or(false);
    }
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

    /// bcrypt of "katzenpfote-123" with cost 4, as mailcow would store it.
    fn mailcow_hash() -> String {
        format!(
            "{BLF_CRYPT_PREFIX}{}",
            bcrypt::hash_with_result("katzenpfote-123", 4).unwrap().format_for_version(bcrypt::Version::TwoY)
        )
    }

    #[test]
    fn hashes_and_verifies() {
        let stored = hash("correct horse battery").unwrap();
        assert!(verify("correct horse battery", Some(&stored)));
        assert!(!verify("wrong", Some(&stored)));
        assert!(!verify("correct horse battery", None));
        assert!(hash("short").is_err());
        assert!(!is_imported(&stored));
        assert_eq!(import_hash(&stored).unwrap(), stored);
    }

    #[test]
    fn hashes_from_mailcow_are_taken_over() {
        let stored = mailcow_hash();
        assert!(stored.starts_with("{BLF-CRYPT}$2y$04$"), "{stored}");
        assert!(verify("katzenpfote-123", Some(&stored)));
        assert!(!verify("katzenpfote-124", Some(&stored)));
        assert!(is_imported(&stored));
        assert_eq!(import_hash(&stored).unwrap(), stored);
        let bare = stored.strip_prefix(BLF_CRYPT_PREFIX).unwrap();
        assert_eq!(import_hash(bare).unwrap(), stored, "the prefix is added");
        assert!(import_hash("{SHA512-CRYPT}$6$abc$def").is_err());
        assert!(import_hash("{BLF-CRYPT}$2y$10$broken").is_err());
    }
}
