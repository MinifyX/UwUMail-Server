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
    /// The scopes asked for at login. Empty means `openid email profile`.
    pub scopes: Vec<String>,
    /// The provider lives in the local network (a UwUAuth at home): its host, and only its host, may
    /// have a private address. https stays required.
    pub private_network: bool,
    /// The admin group claim decides at every login whether an account that logs in this way is an
    /// admin, not only when it is made.
    pub sync_admin: bool,
}

/// What is asked for when the settings name no scopes.
pub const DEFAULT_SCOPES: &str = "openid email profile";

impl OidcConfig {
    /// The provider's host, when it may be in the local network.
    pub fn lan_host(&self) -> Option<String> {
        if !self.private_network {
            return None;
        }
        url::Url::parse(self.issuer.trim()).ok()?.host_str().map(|host| host.to_ascii_lowercase())
    }

    /// The scopes asked for at login, `openid` always among them.
    pub fn scope(&self) -> String {
        let mut scopes: Vec<&str> = self.scopes.iter().map(|scope| scope.trim()).filter(|s| !s.is_empty()).collect();
        if scopes.is_empty() {
            return DEFAULT_SCOPES.to_owned();
        }
        if !scopes.contains(&"openid") {
            scopes.insert(0, "openid");
        }
        scopes.dedup();
        scopes.join(" ")
    }
}

/// Checks an address at the provider: like every request of the server (https, no login in it, a
/// public host), except that `lan`, the provider's own host when the admin said it is in the local
/// network, may be a private address. The machine itself stays out of reach either way.
pub fn checked_issuer_url(url: &str, lan: Option<&str>) -> Result<url::Url, String> {
    use url::Host;
    use uwumail_smtp::egress::Reach;
    let parsed = url::Url::parse(url.trim()).map_err(|_| "that is not a web address".to_owned())?;
    let host = parsed.host_str().map(str::to_ascii_lowercase);
    let Some(lan) = lan.filter(|lan| host.as_deref() == Some(*lan)) else {
        return uwumail_smtp::fetch::check_url(url, false);
    };
    if parsed.scheme() != "https" {
        return Err(format!("{lan} needs https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the address may not contain a login".into());
    }
    let allowed = match parsed.host() {
        Some(Host::Ipv4(ip)) => Reach::Lan.allows(ip.into()),
        Some(Host::Ipv6(ip)) => Reach::Lan.allows(ip.into()),
        Some(Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain != "localhost" && !domain.ends_with(".localhost")
        }
        None => false,
    };
    if !allowed {
        return Err("the address points at this machine".into());
    }
    Ok(parsed)
}

/// Requests to the provider's host in the local network leave through one transport (allowed to
/// reach it), everything else through the usual one.
pub struct LanRoute {
    pub hosts: Vec<String>,
    pub lan: Box<dyn uwumail_dav::client::Transport>,
    pub public: Box<dyn uwumail_dav::client::Transport>,
}

impl uwumail_dav::client::Transport for LanRoute {
    fn send(
        &self,
        request: axum::http::Request<bytes::Bytes>,
        max_bytes: usize,
    ) -> uwumail_dav::client::BoxFuture<'_, Result<uwumail_dav::client::Answer, uwumail_dav::client::RemoteError>> {
        let host =
            request.uri().host().map(|host| host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase());
        let local = request.uri().scheme_str() == Some("https")
            && host.is_some_and(|host| {
                self.hosts.iter().any(|lan| lan.trim_start_matches('[').trim_end_matches(']') == host)
            });
        if local { self.lan.send(request, max_bytes) } else { self.public.send(request, max_bytes) }
    }
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
            checked_issuer_url(&self.oidc.issuer, self.oidc.lan_host().as_deref())
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
        config.oidc.issuer = "https://192.168.1.20".into();
        assert!(config.validate().is_err(), "a private address only when the admin says so");
        config.oidc.private_network = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn only_the_providers_own_host_may_be_in_the_local_network() {
        let lan = Some("auth.home.example");
        assert!(checked_issuer_url("https://auth.home.example/x", lan).is_ok());
        assert!(checked_issuer_url("https://AUTH.home.example/x", lan).is_ok());
        assert!(checked_issuer_url("http://auth.home.example/x", lan).is_err(), "https stays required");
        assert!(checked_issuer_url("https://user:pw@auth.home.example/", lan).is_err());
        // Other hosts are checked as always.
        assert!(checked_issuer_url("https://10.0.0.1/", lan).is_err());
        assert!(checked_issuer_url("https://intranet/", lan).is_err());
        assert!(checked_issuer_url("https://idp.example.net/", lan).is_ok());

        assert!(checked_issuer_url("https://192.168.1.20/", Some("192.168.1.20")).is_ok());
        assert!(checked_issuer_url("https://[fd00::1]/", Some("[fd00::1]")).is_ok());
        assert!(checked_issuer_url("https://127.0.0.1/", Some("127.0.0.1")).is_err(), "never the machine itself");
        assert!(checked_issuer_url("https://169.254.169.254/", Some("169.254.169.254")).is_err());
        assert!(checked_issuer_url("https://localhost/", Some("localhost")).is_err());
        assert!(checked_issuer_url("https://192.168.1.20/", None).is_err());
    }

    #[test]
    fn scopes_always_ask_for_openid() {
        let mut config = OidcConfig::default();
        assert_eq!(config.scope(), "openid email profile");
        config.scopes = vec!["email".into(), "roles".into()];
        assert_eq!(config.scope(), "openid email roles");
        config.scopes = vec!["openid".into(), "email".into(), "profile".into(), "groups".into(), "roles".into()];
        assert_eq!(config.scope(), "openid email profile groups roles");
        config.issuer = "https://Auth.Home.example/".into();
        assert_eq!(config.lan_host(), None);
        config.private_network = true;
        assert_eq!(config.lan_host().as_deref(), Some("auth.home.example"));
    }

    struct Named(&'static str);

    impl uwumail_dav::client::Transport for Named {
        fn send(
            &self,
            _: axum::http::Request<bytes::Bytes>,
            _: usize,
        ) -> uwumail_dav::client::BoxFuture<'_, Result<uwumail_dav::client::Answer, uwumail_dav::client::RemoteError>>
        {
            let body = bytes::Bytes::from_static(self.0.as_bytes());
            Box::pin(async move { Ok(uwumail_dav::client::Answer { status: 200, headers: Default::default(), body }) })
        }
    }

    #[tokio::test]
    async fn the_lan_host_goes_its_own_way() {
        let route = LanRoute {
            hosts: vec!["auth.home.example".into(), "[fd00::1]".into()],
            lan: Box::new(Named("lan")),
            public: Box::new(Named("public")),
        };
        let to = |uri: &str| axum::http::Request::get(uri).body(bytes::Bytes::new()).unwrap();
        use uwumail_dav::client::Transport;
        assert_eq!(route.send(to("https://auth.home.example/x"), 10).await.unwrap().body, "lan");
        assert_eq!(route.send(to("https://[fd00::1]/x"), 10).await.unwrap().body, "lan");
        assert_eq!(route.send(to("https://idp.example.net/x"), 10).await.unwrap().body, "public");
        assert_eq!(route.send(to("http://auth.home.example/x"), 10).await.unwrap().body, "public");
    }
}
