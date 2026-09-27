//! Finding someone's calendars and address books at another provider from their address alone
//! (RFC 6764): a server named by hand, the provider's known server, the domain's SRV and TXT
//! records, and its `/.well-known/` addresses, asked in this order. From the first that answers,
//! the principal leads to the home collection, and the home collection lists what is in it.

use serde::Serialize;
use url::Url;
use uwumail_smtp::dnscheck::DnsChecker;
use uwumail_store::DavKind;

use super::providers::provider_of;
use super::{Multistatus, Remote, RemoteError};
use crate::xml::{APPLE, CALDAV, CALSERVER, CARDDAV, DAV};

/// A calendar or address book found at the other provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCollection {
    #[serde(skip)]
    pub url: Url,
    pub kind: DavKind,
    pub name: String,
    pub description: String,
    pub color: Option<String>,
    /// For calendars: what they hold (`VEVENT`, `VTODO`); empty when the server does not say.
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovered {
    /// The provider's name when it is a known one.
    pub provider: Option<&'static str>,
    pub collections: Vec<RemoteCollection>,
}

const HOME_PROPS: &str =
    "<d:current-user-principal/><d:resourcetype/><c:calendar-home-set/><card:addressbook-home-set/>";
const COLLECTION_PROPS: &str = "<d:resourcetype/><d:displayname/><c:supported-calendar-component-set/>\
<c:calendar-description/><card:addressbook-description/><ical:calendar-color/>";

fn home_set(kind: DavKind) -> (&'static str, &'static str) {
    match kind {
        DavKind::Calendar => (CALDAV, "calendar-home-set"),
        DavKind::Addressbook => (CARDDAV, "addressbook-home-set"),
    }
}

/// Where to start looking for one kind of collection, best first.
async fn starts(dns: Option<&DnsChecker>, domain: &str, server: Option<&str>, kind: DavKind) -> Vec<String> {
    let mut starts = Vec::new();
    if let Some(server) = server.map(str::trim).filter(|server| !server.is_empty()) {
        let server = if server.contains("://") { server.to_owned() } else { format!("https://{server}") };
        starts.push(server.clone());
        if let Ok(url) = Url::parse(&server) {
            let well_known = if kind == DavKind::Calendar { "/.well-known/caldav" } else { "/.well-known/carddav" };
            if let Ok(joined) = url.join(well_known) {
                starts.push(joined.to_string());
            }
        }
        return starts;
    }
    if let Some(start) = provider_of(domain).and_then(|provider| provider.start(kind)) {
        starts.push(start.to_owned());
    }
    let service = if kind == DavKind::Calendar { "_caldavs._tcp" } else { "_carddavs._tcp" };
    if let Some(dns) = dns {
        let name = format!("{service}.{domain}");
        if let Some((host, port)) = dns.service_hosts(&name).await.into_iter().next() {
            let path = dns.service_path(&name).await.unwrap_or_else(|| {
                if kind == DavKind::Calendar { "/.well-known/caldav".into() } else { "/.well-known/carddav".into() }
            });
            let path = if path.starts_with('/') { path } else { format!("/{path}") };
            starts.push(if port == 443 {
                format!("https://{host}{path}")
            } else {
                format!("https://{host}:{port}{path}")
            });
        }
    }
    let well_known = if kind == DavKind::Calendar { "caldav" } else { "carddav" };
    starts.push(format!("https://{domain}/.well-known/{well_known}"));
    starts
}

/// The home collection a start leads to: straight from its answer, or through the principal.
async fn home(remote: &mut Remote<'_>, start: &str, kind: DavKind) -> Result<Option<Url>, RemoteError> {
    let (ns, name) = home_set(kind);
    let Some(found) = remote.propfind(start, 0, HOME_PROPS).await? else { return Ok(None) };
    let Some(entry) = found.entries.first() else { return Ok(None) };
    if let Some(home) = entry.href_in(ns, name, &found.url) {
        return Ok(Some(home));
    }
    let Some(principal) = entry.href_in(DAV, "current-user-principal", &found.url) else { return Ok(None) };
    let Some(found) = remote.propfind(principal.as_str(), 0, HOME_PROPS).await? else { return Ok(None) };
    Ok(found.entries.first().and_then(|entry| entry.href_in(ns, name, &found.url)))
}

fn collections(found: &Multistatus, kind: DavKind) -> Vec<RemoteCollection> {
    let mut list = Vec::new();
    let site = super::site_of(&found.url);
    for entry in &found.entries {
        // A collection on another site would take the login there when it is fetched.
        if super::site_of(&entry.href) != site {
            continue;
        }
        let is_kind = match kind {
            DavKind::Calendar => entry.has_type(CALDAV, "calendar"),
            DavKind::Addressbook => entry.has_type(CARDDAV, "addressbook"),
        };
        // Calendars one subscribed to at the provider are feeds of their own, not its to hand out.
        if !is_kind || entry.has_type(CALSERVER, "subscribed") {
            continue;
        }
        let components: Vec<String> = entry
            .prop(CALDAV, "supported-calendar-component-set")
            .map(|set| {
                set.children_named(CALDAV, "comp")
                    .filter_map(|comp| comp.attribute("name"))
                    .map(str::to_ascii_uppercase)
                    .collect()
            })
            .unwrap_or_default();
        if kind == DavKind::Calendar
            && !components.is_empty()
            && !components.iter().any(|c| c == "VEVENT" || c == "VTODO")
        {
            continue;
        }
        let fallback = entry
            .href
            .path_segments()
            .and_then(|mut segments| segments.rfind(|s| !s.is_empty()).map(str::to_owned))
            .unwrap_or_default();
        let description_prop = if kind == DavKind::Calendar {
            (CALDAV, "calendar-description")
        } else {
            (CARDDAV, "addressbook-description")
        };
        list.push(RemoteCollection {
            url: entry.href.clone(),
            kind,
            name: entry.text(DAV, "displayname").unwrap_or(fallback),
            description: entry.text(description_prop.0, description_prop.1).unwrap_or_default(),
            color: entry.text(APPLE, "calendar-color"),
            components,
        });
    }
    list
}

/// Everything of the given kinds the login reaches at the provider of `address`, or at `server`
/// when one is given. A refused password ends the search at once: asking elsewhere with it only
/// fills the provider's lockout counter.
pub async fn discover(
    remote: &mut Remote<'_>,
    dns: Option<&DnsChecker>,
    address: &str,
    server: Option<&str>,
    kinds: &[DavKind],
) -> Result<Discovered, RemoteError> {
    let domain = address.rsplit_once('@').map(|(_, domain)| domain.trim().to_ascii_lowercase()).unwrap_or_default();
    let provider = provider_of(&domain);
    if server.is_none_or(|server| server.trim().is_empty())
        && let Some(refusal) = provider.and_then(|provider| provider.refusal)
    {
        return Err(if refusal == "googleUseIcs" { RemoteError::GoogleUseIcs } else { RemoteError::NoDav });
    }
    if domain.is_empty() && server.is_none_or(|server| server.trim().is_empty()) {
        return Err(RemoteError::NotFound);
    }
    let mut discovered = Discovered { provider: provider.map(|provider| provider.name), collections: Vec::new() };
    let mut found_any = false;
    let mut last_error = None;
    for &kind in kinds {
        for start in starts(dns, &domain, server, kind).await {
            // Every start is a fresh site: the login may go to each of them, but not beyond.
            let mut fresh = remote.fork();
            match home(&mut fresh, &start, kind).await {
                Ok(Some(home)) => {
                    let listed = fresh.propfind(home.as_str(), 1, COLLECTION_PROPS).await?;
                    found_any = true;
                    if let Some(listed) = listed {
                        discovered.collections.extend(collections(&listed, kind));
                    }
                    break;
                }
                Ok(None) => {}
                Err(err @ (RemoteError::WrongPassword | RemoteError::RedirectedElsewhere)) => return Err(err),
                Err(err) => last_error = Some(err),
            }
        }
    }
    if !found_any {
        return Err(match last_error {
            Some(RemoteError::NotAllowed(reason)) => RemoteError::NotAllowed(reason),
            _ => RemoteError::NotFound,
        });
    }
    Ok(discovered)
}

#[cfg(test)]
mod tests {
    use uwumail_jmap::ClientInfo;
    use uwumail_store::{DavPrecondition, DavWrite, NewAccount, Role, Store, split_ics, split_vcf};

    use super::super::fetch_collection;
    use super::super::testing::RouterTransport;
    use super::*;
    use crate::{Dav, DavSettings};

    /// Another provider, played by this server's own CalDAV and CardDAV.
    async fn provider() -> (RouterTransport, Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let leni = store
            .create_account(NewAccount {
                address: "leni@example.org".into(),
                display_name: "Leni".into(),
                password: Some("katzenpfote-123".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let dav = Dav::new(
            store.clone(),
            DavSettings { calendar_name: "Kalender".into(), addressbook_name: "Kontakte".into() },
        );
        let calendar =
            store.dav_collections(leni, DavKind::Calendar, dav.default_collection(DavKind::Calendar)).await.unwrap();
        for (uid, summary) in [("a", "Tierarzt"), ("b", "Friseur")] {
            let content = format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART:20261001T100000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            let write = DavWrite {
                name: format!("{uid}.ics"),
                content,
                uid: uid.into(),
                component: "VEVENT".into(),
                starts_at: None,
                ends_at: None,
            };
            store.dav_put(leni, calendar[0].id, write, DavPrecondition::default()).await.unwrap();
        }
        let books = store
            .dav_collections(leni, DavKind::Addressbook, dav.default_collection(DavKind::Addressbook))
            .await
            .unwrap();
        let card = DavWrite {
            name: "nyu.vcf".into(),
            content: "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:nyu\r\nFN:Nyu\r\nEND:VCARD\r\n".into(),
            uid: "nyu".into(),
            component: "VCARD".into(),
            starts_at: None,
            ends_at: None,
        };
        store.dav_put(leni, books[0].id, card, DavPrecondition::default()).await.unwrap();
        let router = dav.router().layer(axum::Extension(ClientInfo { https: true, ..ClientInfo::default() }));
        (RouterTransport::new(router), store, dir)
    }

    #[tokio::test]
    async fn calendars_and_contacts_move_over() {
        let (transport, _store, _dir) = provider().await;
        let mut remote = Remote::with_login(&transport, "leni@example.org", "katzenpfote-123");
        let found = discover(
            &mut remote,
            None,
            "leni@example.org",
            Some("dav.example.org"),
            &[DavKind::Calendar, DavKind::Addressbook],
        )
        .await
        .unwrap();
        assert_eq!(found.collections.len(), 2, "{found:?}");
        let calendar = found.collections.iter().find(|c| c.kind == DavKind::Calendar).unwrap();
        assert_eq!(calendar.name, "Kalender");
        assert!(calendar.components.contains(&"VEVENT".to_owned()));
        let mut fetching = remote.fork();
        let texts = fetch_collection(&mut fetching, calendar).await.unwrap();
        let split = split_ics(&texts.concat(), false);
        let mut uids: Vec<&str> = split.objects.iter().map(|o| o.uid.as_str()).collect();
        uids.sort_unstable();
        assert_eq!(uids, vec!["a", "b"]);
        let book = found.collections.iter().find(|c| c.kind == DavKind::Addressbook).unwrap();
        let texts = fetch_collection(&mut remote.fork(), book).await.unwrap();
        assert_eq!(split_vcf(&texts.concat()).objects[0].label, "Nyu");

        let mut wrong = Remote::with_login(&transport, "leni@example.org", "falsch");
        let refused =
            discover(&mut wrong, None, "leni@example.org", Some("https://dav.example.org/"), &[DavKind::Calendar])
                .await;
        assert_eq!(refused.unwrap_err(), RemoteError::WrongPassword);
        let google = discover(&mut wrong, None, "leni@gmail.com", None, &[DavKind::Calendar]).await;
        assert_eq!(google.unwrap_err(), RemoteError::GoogleUseIcs);
    }
}
