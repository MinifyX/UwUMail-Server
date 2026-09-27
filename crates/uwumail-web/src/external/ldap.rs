//! Checking a password at an LDAP directory: find the person (or build their DN from a template),
//! then bind as them with the password they typed. Always over TLS (`ldaps://` or STARTTLS),
//! except to a directory on this very machine when an admin said so.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry, dn_escape, ldap_escape};
use url::{Host, Url};

use super::LdapConfig;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A person the directory knows, after their password was right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdapUser {
    pub dn: String,
    /// The addresses the directory has for them (the mail attribute may hold several), lowercase.
    pub emails: Vec<String>,
    pub name: String,
    /// In the admin group.
    pub admin: bool,
}

impl LdapUser {
    /// Whether this person may be `address` here: one of their addresses in the directory, or any
    /// when the directory keeps none (the filter or DN template alone found them).
    pub fn has_address(&self, address: &str) -> bool {
        self.emails.is_empty() || self.emails.iter().any(|email| email.eq_ignore_ascii_case(address.trim()))
    }
}

fn loopback(url: &Url) -> bool {
    match url.host() {
        // `ldap:` is no scheme the URL standard knows, so its host stays text, addresses too.
        Some(Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Checks the settings: a usable address, TLS, and a way to find people.
pub fn check(config: &LdapConfig) -> Result<(), String> {
    let url = Url::parse(config.url.trim()).map_err(|_| "`auth.ldap.url` is not an LDAP address".to_owned())?;
    match url.scheme() {
        "ldaps" => {}
        "ldap" if config.starttls => {}
        "ldap" if config.insecure_localhost && loopback(&url) => {}
        "ldap" => {
            return Err(
                "`auth.ldap.url` needs ldaps:// or STARTTLS; only a directory on this machine may go without".into()
            );
        }
        _ => return Err("`auth.ldap.url` has to start with ldaps:// or ldap://".into()),
    }
    if url.host().is_none() {
        return Err("`auth.ldap.url` has no host".into());
    }
    let template = config.user_dn_template.trim();
    if template.is_empty() {
        if config.base_dn.trim().is_empty() {
            return Err(
                "`auth.ldap.base_dn` is needed to search for people (or set `auth.ldap.user_dn_template`)".into()
            );
        }
        if !(config.user_filter.contains("{email}") || config.user_filter.contains("{user}")) {
            return Err("`auth.ldap.user_filter` has to contain {email} or {user}".into());
        }
    } else if !(template.contains("{email}") || template.contains("{user}")) {
        return Err("`auth.ldap.user_dn_template` has to contain {email} or {user}".into());
    }
    Ok(())
}

/// The web's certificate authorities and the system's, so a directory behind a company's own CA
/// works once its certificate is in the system's store. Read once.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            for cert in rustls_native_certs::load_native_certs().certs {
                let _ = roots.add(cert);
            }
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("the default TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

async fn connect(config: &LdapConfig) -> Result<ldap3::Ldap, String> {
    check(config)?;
    let url = Url::parse(config.url.trim()).map_err(|err| err.to_string())?;
    // `check` made sure plain LDAP without STARTTLS only goes to this machine.
    let settings = LdapConnSettings::new()
        .set_conn_timeout(TIMEOUT)
        .set_config(tls_config())
        .set_starttls(url.scheme() == "ldap" && config.starttls);
    let (connection, ldap) = LdapConnAsync::from_url_with_settings(settings, &url)
        .await
        .map_err(|err| format!("the directory could not be reached: {err}"))?;
    ldap3::drive!(connection);
    Ok(ldap)
}

/// The two ways to write a person into a filter or DN: the whole address and the part before @.
/// Placeholders are replaced in one pass, so a value that contains `{user}` is never read again.
fn fill(template: &str, login: &str, escape: fn(&str) -> String) -> String {
    let user = login.split('@').next().unwrap_or(login);
    let mut filled = String::with_capacity(template.len() + login.len() * 3);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        filled.push_str(&rest[..start]);
        let tail = &rest[start..];
        if let Some(after) = tail.strip_prefix("{email}") {
            filled.push_str(&escape(login));
            rest = after;
        } else if let Some(after) = tail.strip_prefix("{user}") {
            filled.push_str(&escape(user));
            rest = after;
        } else {
            filled.push('{');
            rest = &tail[1..];
        }
    }
    filled.push_str(rest);
    filled
}

fn filter_escape(value: &str) -> String {
    ldap_escape(value).into_owned()
}

fn dn_value_escape(value: &str) -> String {
    dn_escape(value).into_owned()
}

fn values(entry: &SearchEntry, attribute: &str) -> Vec<String> {
    let attribute = attribute.trim();
    entry
        .attrs
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(attribute))
        .flat_map(|(_, values)| values)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

fn user_from(config: &LdapConfig, entry: &SearchEntry) -> LdapUser {
    let group = config.admin_group_dn.trim();
    let admin = !group.is_empty()
        && entry
            .attrs
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("memberOf"))
            .flat_map(|(_, values)| values)
            .any(|value| value.trim().eq_ignore_ascii_case(group));
    LdapUser {
        dn: entry.dn.clone(),
        emails: values(entry, &config.mail_attribute).iter().map(|email| email.to_lowercase()).collect(),
        name: values(entry, &config.name_attribute).into_iter().next().unwrap_or_default(),
        admin,
    }
}

fn attributes(config: &LdapConfig) -> Vec<String> {
    vec![config.mail_attribute.trim().to_owned(), config.name_attribute.trim().to_owned(), "memberOf".to_owned()]
}

/// Checks `password` for the person who logs in as `login` (their address). `Ok(None)` for a wrong
/// password or a person the directory does not know.
pub async fn authenticate(config: &LdapConfig, login: &str, password: &str) -> Result<Option<LdapUser>, String> {
    // An empty password is an "unauthenticated bind" (RFC 4513 section 5.1.2), which many servers
    // answer with success. It never counts as a login.
    if password.is_empty() || login.trim().is_empty() {
        return Ok(None);
    }
    let login = login.trim().to_lowercase();
    let mut ldap = connect(config).await?;
    let result = async {
        let template = config.user_dn_template.trim();
        let mut searched: Option<SearchEntry> = None;
        let dn = if template.is_empty() {
            // Find the person with the service account (or anonymously without one).
            if !config.bind_dn.trim().is_empty() {
                ldap.with_timeout(TIMEOUT)
                    .simple_bind(config.bind_dn.trim(), &config.bind_password)
                    .await
                    .map_err(|err| format!("the directory could not be asked: {err}"))?
                    .success()
                    .map_err(|err| format!("the service account was refused: {err}"))?;
            }
            let filter = fill(&config.user_filter, &login, filter_escape);
            let (entries, _) = ldap
                .with_timeout(TIMEOUT)
                .search(config.base_dn.trim(), Scope::Subtree, &filter, attributes(config))
                .await
                .map_err(|err| format!("searching the directory failed: {err}"))?
                .success()
                .map_err(|err| format!("searching the directory failed: {err}"))?;
            // Nobody, or more than one person: no login either way.
            if entries.len() != 1 {
                return Ok(None);
            }
            let entry = SearchEntry::construct(entries.into_iter().next().expect("one entry"));
            searched = Some(entry.clone());
            entry.dn
        } else {
            fill(template, &login, dn_value_escape)
        };
        // An empty name with a password is an unauthenticated bind as well.
        if dn.trim().is_empty() {
            return Ok(None);
        }
        let bound = ldap
            .with_timeout(TIMEOUT)
            .simple_bind(&dn, password)
            .await
            .map_err(|err| format!("the directory could not be asked: {err}"))?;
        match bound.rc {
            0 => {}
            // invalidCredentials; also what a DN that does not exist answers.
            49 => return Ok(None),
            code => return Err(format!("the directory answered the login with code {code}")),
        }
        // What the directory says about the person, read as them.
        let found = ldap
            .with_timeout(TIMEOUT)
            .search(&dn, Scope::Base, "(objectClass=*)", attributes(config))
            .await
            .ok()
            .and_then(|result| result.success().ok())
            .and_then(|(entries, _)| entries.into_iter().next())
            .map(SearchEntry::construct)
            // The person may not read their own entry; the service account's search saw it.
            .or(searched);
        Ok(Some(match found {
            Some(entry) => LdapUser { dn: dn.clone(), ..user_from(config, &entry) },
            None => LdapUser { dn, emails: Vec::new(), name: String::new(), admin: false },
        }))
    }
    .await;
    let _ = ldap.unbind().await;
    result
}

/// Tries the settings without anyone's password: the connection, TLS, the service account and the
/// search base. Returns what worked, for the admin.
pub async fn test(config: &LdapConfig) -> Result<String, String> {
    let mut ldap = connect(config).await?;
    let result = async {
        if !config.bind_dn.trim().is_empty() {
            ldap.with_timeout(TIMEOUT)
                .simple_bind(config.bind_dn.trim(), &config.bind_password)
                .await
                .map_err(|err| format!("the directory could not be asked: {err}"))?
                .success()
                .map_err(|err| format!("the service account was refused: {err}"))?;
        }
        if config.user_dn_template.trim().is_empty() {
            ldap.with_timeout(TIMEOUT)
                .search(config.base_dn.trim(), Scope::Base, "(objectClass=*)", vec!["1.1"])
                .await
                .map_err(|err| format!("the search base could not be read: {err}"))?
                .success()
                .map_err(|err| format!("the search base could not be read: {err}"))?;
            Ok(format!("connected, and {} can be searched", config.base_dn.trim()))
        } else {
            Ok("connected".to_owned())
        }
    }
    .await;
    let _ = ldap.unbind().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_go_into_filters_escaped() {
        assert_eq!(fill("(mail={email})", "leni@example.org", filter_escape), "(mail=leni@example.org)");
        assert_eq!(fill("(uid={user})", "a*)(uid=*@example.org", filter_escape), "(uid=a\\2a\\29\\28uid=\\2a)");
        assert_eq!(
            fill("uid={user},ou=people", "x,cn=admin@example.org", dn_value_escape),
            "uid=x\\2ccn\\3dadmin,ou=people"
        );
    }

    #[test]
    fn tls_is_required_unless_on_this_machine() {
        let base = LdapConfig {
            enabled: true,
            url: "ldap://ldap.example.org".into(),
            base_dn: "dc=example,dc=org".into(),
            ..LdapConfig::default()
        };
        assert!(check(&base).is_ok(), "STARTTLS is on by default");
        let plain = LdapConfig { starttls: false, ..base.clone() };
        assert!(check(&plain).is_err());
        let local = LdapConfig { url: "ldap://127.0.0.1:3389".into(), insecure_localhost: true, ..plain.clone() };
        assert!(check(&local).is_ok());
        assert!(check(&LdapConfig { url: "ldap://[::1]:3389".into(), ..local.clone() }).is_ok());
        assert!(check(&LdapConfig { url: "ldap://localhost".into(), ..local.clone() }).is_ok());
        assert!(check(&LdapConfig { url: "ldap://127.0.0.1.example.org".into(), ..local.clone() }).is_err());
        let remote = LdapConfig { insecure_localhost: true, ..plain };
        assert!(check(&remote).is_err(), "only the machine itself");
        assert!(check(&LdapConfig { url: "ldaps://ldap.example.org".into(), ..base.clone() }).is_ok());
        assert!(check(&LdapConfig { url: "http://ldap.example.org".into(), ..base.clone() }).is_err());
        assert!(check(&LdapConfig { base_dn: String::new(), ..base.clone() }).is_err());
        let template =
            LdapConfig { base_dn: String::new(), user_dn_template: "uid={user},dc=example,dc=org".into(), ..base };
        assert!(check(&template).is_ok());
    }
}
