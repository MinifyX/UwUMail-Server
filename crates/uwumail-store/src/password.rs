use std::sync::{Arc, OnceLock};
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

use crate::{Result, StoreError};

/// How long a password check waits for its turn before the answer is "try again later".
const GATE_WAIT: Duration = Duration::from_secs(3);

/// Password hashes checked at once, for the whole server, whatever protocol asked.
///
/// Each check is slow and takes about 19 MiB on purpose, and it runs on the thread pool the
/// database uses as well. Without a limit, a burst of logins, most of them from people without an
/// account at all, put thousands of checks in flight: gigabytes of memory, and every database read
/// of every protocol queued behind them (security-audit-0.16.0 WEB-1). A few at a time, as many as
/// the machine has cores (two to eight), and whoever waits longer than [`GATE_WAIT`] hears "try
/// again later" instead of waiting in an endless line.
pub(crate) struct Gate {
    permits: Arc<tokio::sync::Semaphore>,
    wait: Duration,
}

impl Gate {
    pub(crate) fn new() -> Gate {
        let cores = std::thread::available_parallelism().map_or(2, |cores| cores.get());
        Gate::with(cores.clamp(2, 8), GATE_WAIT)
    }

    fn with(permits: usize, wait: Duration) -> Gate {
        Gate { permits: Arc::new(tokio::sync::Semaphore::new(permits)), wait }
    }

    /// Runs one password check (`check`) on the blocking pool once it has its turn, or answers
    /// [`StoreError::Busy`]. The turn is held until the check itself ends, even when whoever asked
    /// stopped waiting: a client that hangs up must not free its place while the hashing goes on.
    pub(crate) async fn run<T: Send + 'static>(&self, check: impl FnOnce() -> T + Send + 'static) -> Result<T> {
        let permit = tokio::time::timeout(self.wait, self.permits.clone().acquire_owned())
            .await
            .map_err(|_| StoreError::Busy)?
            .map_err(|_| StoreError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let result = check();
            drop(permit);
            result
        })
        .await
        .map_err(|err| StoreError::Internal(err.to_string()))
    }
}

impl crate::Store {
    /// [`verify`], through the server's [`Gate`].
    pub(crate) async fn verify_password(&self, password: &str, stored: Option<String>) -> Result<bool> {
        let password = password.to_owned();
        self.inner.hashing.run(move || verify(&password, stored.as_deref())).await
    }

    /// Runs `check`, which checks one or more password hashes, through the server's [`Gate`].
    pub(crate) async fn check_passwords<T: Send + 'static>(
        &self,
        check: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T> {
        self.inner.hashing.run(check).await
    }
}

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

    #[tokio::test]
    async fn the_gate_turns_away_what_it_cannot_take_in_time() {
        let gate = Gate::with(2, Duration::from_millis(20));
        let (started, release) = (Arc::new(std::sync::Barrier::new(3)), Arc::new(std::sync::Barrier::new(3)));
        let running: Vec<_> = (0..2)
            .map(|_| {
                let (started, release) = (started.clone(), release.clone());
                let permits = gate.permits.clone();
                let run = Gate { permits, wait: gate.wait };
                tokio::spawn(async move {
                    run.run(move || {
                        started.wait();
                        release.wait();
                    })
                    .await
                })
            })
            .collect();
        tokio::task::spawn_blocking({
            let started = started.clone();
            move || started.wait()
        })
        .await
        .unwrap();
        // Both places are taken by checks that are running: the third is turned away, not queued.
        assert!(matches!(gate.run(|| ()).await, Err(StoreError::Busy)));
        tokio::task::spawn_blocking(move || release.wait()).await.unwrap();
        for run in running {
            run.await.unwrap().unwrap();
        }
        assert!(gate.run(|| true).await.unwrap(), "places come back once the checks end");
    }

    #[tokio::test]
    async fn a_caller_that_gives_up_does_not_free_its_place_early() {
        let gate = Gate::with(1, Duration::from_millis(20));
        let (started, release) = (Arc::new(std::sync::Barrier::new(2)), Arc::new(std::sync::Barrier::new(2)));
        let check = {
            let (started, release) = (started.clone(), release.clone());
            let run = Gate { permits: gate.permits.clone(), wait: gate.wait };
            tokio::spawn(async move {
                run.run(move || {
                    started.wait();
                    release.wait();
                })
                .await
            })
        };
        let waited = started.clone();
        tokio::task::spawn_blocking(move || waited.wait()).await.unwrap();
        check.abort();
        let _ = check.await;
        assert!(matches!(gate.run(|| ()).await, Err(StoreError::Busy)), "the hashing still runs");
        tokio::task::spawn_blocking(move || release.wait()).await.unwrap();
        // The place comes back once the check itself ends.
        let permit = tokio::time::timeout(Duration::from_secs(10), gate.permits.clone().acquire_owned()).await;
        assert!(permit.is_ok());
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
