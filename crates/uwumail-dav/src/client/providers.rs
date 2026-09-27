//! What well-known providers call their CalDAV and CardDAV servers. Discovery asks the domain
//! itself too (RFC 6764); these are for the providers whose domain does not say.

use uwumail_store::DavKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    /// Shown to the person, so they know whose server was asked.
    pub name: &'static str,
    pub domains: &'static [&'static str],
    pub caldav: Option<&'static str>,
    pub carddav: Option<&'static str>,
    /// A hint the portal shows before asking, as a code: which password the provider wants.
    pub hint: Option<&'static str>,
    /// Why this provider cannot be asked at all, as the error the portal explains.
    pub refusal: Option<&'static str>,
}

impl Provider {
    pub fn start(&self, kind: DavKind) -> Option<&'static str> {
        match kind {
            DavKind::Calendar => self.caldav,
            DavKind::Addressbook => self.carddav,
        }
    }
}

const PROVIDERS: &[Provider] = &[
    Provider {
        name: "iCloud",
        domains: &["icloud.com", "me.com", "mac.com"],
        caldav: Some("https://caldav.icloud.com/"),
        carddav: Some("https://contacts.icloud.com/"),
        hint: Some("appPassword"),
        refusal: None,
    },
    Provider {
        name: "WEB.DE",
        domains: &["web.de"],
        caldav: Some("https://caldav.web.de/"),
        carddav: Some("https://carddav.web.de/"),
        // An app password of WEB.DE works for one setup only: one for calendars, one for contacts.
        hint: Some("appPasswordEach"),
        refusal: None,
    },
    Provider {
        name: "GMX",
        domains: &["gmx.de", "gmx.net", "gmx.at", "gmx.ch", "gmx.eu"],
        caldav: Some("https://caldav.gmx.net/"),
        carddav: Some("https://carddav.gmx.net/"),
        hint: Some("appPasswordWith2fa"),
        refusal: None,
    },
    Provider {
        name: "GMX",
        domains: &["gmx.com", "gmx.us", "gmx.co.uk", "gmx.fr", "gmx.es"],
        caldav: Some("https://caldav.gmx.com/"),
        carddav: Some("https://carddav.gmx.com/"),
        hint: Some("appPasswordWith2fa"),
        refusal: None,
    },
    Provider {
        name: "Posteo",
        domains: &["posteo.de", "posteo.net", "posteo.org", "posteo.eu", "posteo.at", "posteo.ch", "posteo.jp"],
        caldav: Some("https://posteo.de:8443/"),
        carddav: Some("https://posteo.de:8843/"),
        hint: None,
        refusal: None,
    },
    Provider {
        name: "mailbox.org",
        domains: &["mailbox.org"],
        caldav: Some("https://dav.mailbox.org/"),
        carddav: Some("https://dav.mailbox.org/"),
        hint: Some("appPassword"),
        refusal: None,
    },
    Provider {
        name: "Fastmail",
        domains: &["fastmail.com", "fastmail.fm"],
        caldav: Some("https://caldav.fastmail.com/"),
        carddav: Some("https://carddav.fastmail.com/"),
        hint: Some("appPassword"),
        refusal: None,
    },
    // Google's CalDAV and CardDAV take OAuth only, which a server of one's own cannot offer
    // without registering with Google: the secret iCal address and the contacts export instead.
    Provider {
        name: "Google",
        domains: &["gmail.com", "googlemail.com"],
        caldav: None,
        carddav: None,
        hint: None,
        refusal: Some("googleUseIcs"),
    },
    Provider {
        name: "Outlook.com",
        domains: &["outlook.com", "outlook.de", "hotmail.com", "hotmail.de", "live.com", "live.de", "msn.com"],
        caldav: None,
        carddav: None,
        hint: None,
        refusal: Some("providerNoDav"),
    },
];

/// The provider an address belongs to, when it is one of the known ones.
pub fn provider_of(address: &str) -> Option<&'static Provider> {
    let domain = address.rsplit_once('@').map_or(address, |(_, domain)| domain).trim().to_ascii_lowercase();
    PROVIDERS.iter().find(|provider| provider.domains.contains(&domain.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_by_address() {
        assert_eq!(
            provider_of("mini@icloud.com").unwrap().start(DavKind::Addressbook),
            Some("https://contacts.icloud.com/")
        );
        assert_eq!(provider_of("mini@GMX.de").unwrap().start(DavKind::Calendar), Some("https://caldav.gmx.net/"));
        assert_eq!(provider_of("mini@gmail.com").unwrap().refusal, Some("googleUseIcs"));
        assert!(provider_of("mini@example.org").is_none());
    }
}
