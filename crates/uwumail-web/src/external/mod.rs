//! Logging in to the portal through somebody else (docs/login-oidc-ldap.md): an OpenID Connect
//! provider such as Authentik, Keycloak, Authelia, Kanidm or Google, or an LDAP directory.
//!
//! The settings are part of the server's settings (`auth.oidc.*`, `auth.ldap.*`) and change while
//! the server runs. The directory also checks the passwords of its accounts for mail apps: the
//! store asks it through [`ExternalPasswords`].

pub mod ldap;
pub mod oidc;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use aws_lc_rs::digest;
use serde::{Deserialize, Serialize};
use uwumail_store::{ExternalFuture, ExternalPasswords};

/// A right directory password is believed this long without asking the directory again.
const LDAP_CACHE: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    pub oidc: OidcConfig,
    pub ldap: LdapConfig,
}

/// Logging in at an OpenID Connect provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OidcConfig {
    pub enabled: bool,
    /// The provider's issuer, e.g. `https://auth.example.com/application/o/uwumail/`.
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// What the button on the login page says after "Sign in with".
    pub button_label: String,
    /// Whether a login whose verified address has no account here gets one.
    pub auto_create: bool,
    /// The domains accounts may be created in. Empty means none.
    pub allowed_domains: Vec<String>,
    /// A claim, such as `groups`, whose value makes a new account an admin.
    pub admin_group_claim: String,
    pub admin_group_value: String,
}

/// Logging in with the password of an LDAP directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LdapConfig {
    pub enabled: bool,
    /// `ldaps://ldap.example.com` or `ldap://ldap.example.com` (then with STARTTLS).
    pub url: String,
    /// STARTTLS on an `ldap://` address. Only a directory on this machine may go without.
    pub starttls: bool,
    /// Allows `ldap://` without TLS when the directory is on this very machine (localhost).
    pub insecure_localhost: bool,
    /// The service account that searches for people, when there is no DN template.
    pub bind_dn: String,
    pub bind_password: String,
    /// A person's DN made from their address, e.g. `uid={user},ou=people,dc=example,dc=com`.
    /// With it, nobody searches: the person binds directly.
    pub user_dn_template: String,
    pub base_dn: String,
    /// How a person is found; `{email}` is the address typed, `{user}` the part before the @.
    pub user_filter: String,
    pub mail_attribute: String,
    pub name_attribute: String,
    /// Members of this group (by `memberOf`) become admins when their account is created.
    pub admin_group_dn: String,
    pub auto_create: bool,
    pub allowed_domains: Vec<String>,
}

impl Default for LdapConfig {
    fn default() -> Self {
        LdapConfig {
            enabled: false,
            url: String::new(),
            starttls: true,
            insecure_localhost: false,
            bind_dn: String::new(),
            bind_password: String::new(),
            user_dn_template: String::new(),
            base_dn: String::new(),
            user_filter: "(&(objectClass=person)(mail={email}))".into(),
            mail_attribute: "mail".into(),
            name_attribute: "cn".into(),
            admin_group_dn: String::new(),
            auto_create: false,
            allowed_domains: Vec::new(),
        }
    }
}

impl AuthConfig {
    /// Checks what types cannot say. Switched-off parts are not checked, so half-filled forms can
    /// be saved while they are off.
    pub fn validate(&self) -> Result<(), String> {
        if self.oidc.enabled {
            uwumail_smtp::fetch::check_url(&self.oidc.issuer, false)
                .map_err(|err| format!("`auth.oidc.issuer`: {err}"))?;
            if self.oidc.client_id.trim().is_empty() {
                return Err("`auth.oidc.client_id` is needed to log in with OpenID Connect".into());
            }
        }
        if self.ldap.enabled {
            ldap::check(&self.ldap)?;
        }
        Ok(())
    }
}

/// Whether an address may get an account by logging in elsewhere.
pub fn domain_allowed(allowed: &[String], address: &str) -> bool {
    let Some((_, domain)) = address.rsplit_once('@') else { return false };
    allowed.iter().any(|allowed| allowed.trim().trim_start_matches('@').eq_ignore_ascii_case(domain))
}

/// Settings in effect, caches and logins on their way, shared by the portal and the store.
pub struct ExternalLogin {
    config: RwLock<Arc<AuthConfig>>,
    secret: [u8; 32],
    ldap_cache: Mutex<HashMap<[u8; 32], (Instant, ldap::LdapUser)>>,
    oidc: oidc::OidcState,
}

impl Default for ExternalLogin {
    fn default() -> Self {
        ExternalLogin::new()
    }
}

impl ExternalLogin {
    pub fn new() -> ExternalLogin {
        ExternalLogin {
            config: RwLock::default(),
            secret: crate::login::random_bytes(),
            ldap_cache: Mutex::default(),
            oidc: oidc::OidcState::default(),
        }
    }

    /// Puts new settings into effect. Whatever was learned under the old ones is forgotten.
    pub fn configure(&self, config: AuthConfig) {
        let mut current = self.config.write().expect("auth config poisoned");
        if **current != config {
            *current = Arc::new(config);
            self.ldap_cache.lock().expect("ldap cache poisoned").clear();
            self.oidc.forget();
        }
    }

    pub fn config(&self) -> Arc<AuthConfig> {
        self.config.read().expect("auth config poisoned").clone()
    }

    fn cache_key(&self, login: &str, password: &str) -> [u8; 32] {
        let mut context = digest::Context::new(&digest::SHA256);
        context.update(&self.secret);
        context.update(login.to_lowercase().as_bytes());
        context.update(&[0]);
        context.update(password.as_bytes());
        context.finish().as_ref().try_into().expect("32 bytes")
    }

    /// Checks a directory password, from the short cache when it was right a moment ago.
    pub async fn ldap_login(&self, login: &str, password: &str) -> Result<Option<ldap::LdapUser>, String> {
        let config = self.config();
        if !config.ldap.enabled || password.is_empty() {
            return Ok(None);
        }
        let key = self.cache_key(login, password);
        {
            let mut cache = self.ldap_cache.lock().expect("ldap cache poisoned");
            cache.retain(|_, (at, _)| at.elapsed() < LDAP_CACHE);
            if let Some((_, user)) = cache.get(&key) {
                return Ok(Some(user.clone()));
            }
        }
        let user = ldap::authenticate(&config.ldap, login, password).await?;
        if let Some(user) = &user {
            let mut cache = self.ldap_cache.lock().expect("ldap cache poisoned");
            if cache.len() < 10_000 {
                cache.insert(key, (Instant::now(), user.clone()));
            }
        }
        Ok(user)
    }

    pub(crate) fn oidc(&self) -> &oidc::OidcState {
        &self.oidc
    }
}

impl ExternalPasswords for ExternalLogin {
    /// The password of an account that checks it at the directory. The person the directory finds
    /// has to have the account's address, when the directory keeps addresses at all: a filter or DN
    /// template that only looks at the part before the @ must not open `leni@` of another domain.
    fn check<'a>(&'a self, login: &'a str, password: &'a str) -> ExternalFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let user = self.ldap_login(login, password).await?;
            Ok(user.is_some_and(|user| user.has_address(login)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_for_new_accounts() {
        let allowed = vec!["example.org".to_owned(), "@Example.net".to_owned()];
        assert!(domain_allowed(&allowed, "leni@example.org"));
        assert!(domain_allowed(&allowed, "leni@example.net"));
        assert!(!domain_allowed(&allowed, "leni@sub.example.org"));
        assert!(!domain_allowed(&[], "leni@example.org"));
    }

    #[test]
    fn settings_are_checked_only_when_switched_on() {
        let mut config = AuthConfig::default();
        config.oidc.issuer = "http://idp.example.net".into();
        assert!(config.validate().is_ok());
        config.oidc.enabled = true;
        assert!(config.validate().is_err(), "an issuer needs https");
        config.oidc.issuer = "https://idp.example.net".into();
        assert!(config.validate().is_err(), "and a client id");
        config.oidc.client_id = "uwumail".into();
        assert!(config.validate().is_ok());
    }
}
