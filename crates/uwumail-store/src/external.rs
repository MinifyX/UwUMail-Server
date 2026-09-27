//! Passwords kept elsewhere: accounts whose `auth_source` is `ldap` have no password hash here,
//! and their password is checked by binding to the directory as them. The server plugs the check
//! in at start (docs/login-oidc-ldap.md); without it such accounts cannot log in with a password.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::{Store, StoreError};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Checks the password of an account whose password lives in a directory.
pub trait ExternalPasswords: Send + Sync {
    /// `Ok(true)` when the password is right, `Ok(false)` when it is wrong or the directory does not
    /// know the login, `Err` when the directory could not be asked at all.
    fn check<'a>(&'a self, login: &'a str, password: &'a str) -> BoxFuture<'a, Result<bool, String>>;
}

impl Store {
    /// Hands the store the check for directory passwords. The last call counts.
    pub fn set_external_passwords(&self, external: Arc<dyn ExternalPasswords>) {
        *self.inner.external.write().expect("external passwords poisoned") = Some(external);
    }

    /// Checks a directory password. Without a directory plugged in, no password is right.
    pub(crate) async fn check_external_password(&self, login: &str, password: &str) -> crate::Result<bool> {
        let external = self.inner.external.read().expect("external passwords poisoned").clone();
        let Some(external) = external else {
            return Ok(false);
        };
        if password.is_empty() {
            return Ok(false);
        }
        external.check(login, password).await.map_err(|err| {
            tracing::warn!(%err, "the directory could not check a password");
            StoreError::Internal(format!("the directory could not check the password: {err}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppScope, MailAuth, MailAuthDenied, NewAccount, Role};

    /// A directory that knows one password, or cannot be reached at all.
    struct Directory {
        reachable: bool,
    }

    impl ExternalPasswords for Directory {
        fn check<'a>(&'a self, login: &'a str, password: &'a str) -> BoxFuture<'a, Result<bool, String>> {
            Box::pin(async move {
                if !self.reachable {
                    return Err("connection refused".into());
                }
                Ok(login == "leni@example.org" && password == "aus-dem-verzeichnis")
            })
        }
    }

    #[tokio::test]
    async fn directory_accounts_check_their_password_there() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        store
            .create_account(NewAccount {
                address: "leni@example.org".into(),
                display_name: "Leni".into(),
                password: Some("Seifenblase-Wanderweg-17".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        store.set_auth_source("leni@example.org", "ldap").await.unwrap();
        // Without a directory plugged in nothing opens the account, not even the old password.
        assert!(store.authenticate("leni@example.org", "Seifenblase-Wanderweg-17").await.unwrap().is_none());

        store.set_external_passwords(Arc::new(Directory { reachable: true }));
        assert!(store.authenticate("leni@example.org", "aus-dem-verzeichnis").await.unwrap().is_some());
        assert!(store.authenticate("leni@example.org", "Seifenblase-Wanderweg-17").await.unwrap().is_none());
        let mail = store.authenticate_mail("leni@example.org", "aus-dem-verzeichnis", AppScope::Mail, "imap", "").await;
        assert!(matches!(mail.unwrap(), MailAuth::Ok { app_password: None, .. }));
        let wrong = store.authenticate_mail("leni@example.org", "falsch-falsch", AppScope::Mail, "imap", "").await;
        assert!(matches!(wrong.unwrap(), MailAuth::Denied(MailAuthDenied::Invalid)));

        // Unreachable is a temporary failure for mail apps and a plain "no" for the portal.
        store.set_external_passwords(Arc::new(Directory { reachable: false }));
        assert!(store.authenticate("leni@example.org", "aus-dem-verzeichnis").await.unwrap().is_none());
        let mail = store.authenticate_mail("leni@example.org", "aus-dem-verzeichnis", AppScope::Mail, "imap", "").await;
        assert!(mail.is_err());
    }
}
