//! Asking other servers for calendars and address books (docs/calendar-import.md): the CalDAV and
//! CardDAV servers of other providers, for moving to this server, and calendar feeds, for
//! subscriptions.
//!
//! Every request leaves the way fetched mailboxes leave (the egress's `fetch` route), only over
//! HTTPS, and only to public addresses; each redirect is checked again. A login is sent only
//! within the site it was given for, so a server cannot redirect it somewhere else.

mod discover;
mod fetch;
mod providers;
mod subscription;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, LOCATION, USER_AGENT};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use url::{Host, Url};
use uwumail_smtp::egress::{Dialer, DialerConnector};

pub use discover::{Discovered, RemoteCollection, discover};
pub use fetch::fetch_collection;
pub use providers::{Provider, provider_of};
pub use subscription::{FeedFetch, feed_url, fetch_feed, refresh_subscription, run_subscriptions};

use crate::xml::{self, DAV, Element};

/// One request, whoever answers it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;
/// Elements in one answer: a multistatus of a few hundred entries has tens of thousands.
const MAX_XML_ELEMENTS: usize = 500_000;
/// The agent string says what asks, and nothing about who.
const AGENT: &str = "UwUMail";

/// Why asking another server did not work, as a stable code the portal translates.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    #[error("only https addresses on the internet can be used: {0}")]
    NotAllowed(String),
    #[error("the server could not be reached")]
    Unreachable,
    #[error("the server did not answer in time")]
    Timeout,
    #[error("the answer is bigger than allowed")]
    TooLarge,
    #[error("the server answered {0}")]
    Status(u16),
    #[error("the server refused the login")]
    WrongPassword,
    #[error("the server sent the login elsewhere")]
    RedirectedElsewhere,
    #[error("too many redirects")]
    Redirects,
    #[error("no calendars or address books were found there")]
    NotFound,
    #[error("the answer is not a calendar")]
    NotICalendar,
    #[error("Google hands out calendars as a secret iCal address and contacts as a file")]
    GoogleUseIcs,
    #[error("this provider has no CalDAV or CardDAV; export a file there instead")]
    NoDav,
}

impl RemoteError {
    pub fn code(&self) -> &'static str {
        match self {
            RemoteError::NotAllowed(_) => "urlNotAllowed",
            RemoteError::Unreachable | RemoteError::Timeout => "providerUnreachable",
            RemoteError::TooLarge => "feedTooLarge",
            RemoteError::Status(404 | 410) => "feedNotFound",
            RemoteError::Status(_) | RemoteError::Redirects => "providerError",
            RemoteError::WrongPassword => "wrongPassword",
            RemoteError::RedirectedElsewhere => "redirectedElsewhere",
            RemoteError::NotFound => "providerNotFound",
            RemoteError::NotICalendar => "notICalendar",
            RemoteError::GoogleUseIcs => "googleUseIcs",
            RemoteError::NoDav => "providerNoDav",
        }
    }
}

/// What a server answered.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Sends one request and reads at most `max_bytes` of the answer. The real one goes out over
/// HTTPS; tests hand the requests to a router in the same process.
pub trait Transport: Send + Sync {
    fn send(&self, request: Request<Bytes>, max_bytes: usize) -> BoxFuture<'_, Result<Answer, RemoteError>>;
}

/// HTTPS through the egress, with the web's usual certificate authorities.
#[derive(Clone)]
pub struct HttpsTransport {
    client: Client<HttpsConnector<DialerConnector>, Full<Bytes>>,
}

impl HttpsTransport {
    pub fn new(dialer: &Dialer) -> HttpsTransport {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client = Client::builder(TokioExecutor::new()).build(dialer.https_connector(tls));
        HttpsTransport { client }
    }
}

impl Transport for HttpsTransport {
    fn send(&self, request: Request<Bytes>, max_bytes: usize) -> BoxFuture<'_, Result<Answer, RemoteError>> {
        Box::pin(async move {
            let exchange = async {
                let response = self.client.request(request.map(Full::new)).await.map_err(|err| reason(&err))?;
                let status = response.status().as_u16();
                let headers = response.headers().clone();
                let body = Limited::new(response.into_body(), max_bytes)
                    .collect()
                    .await
                    .map_err(|err| {
                        if err.is::<http_body_util::LengthLimitError>() {
                            RemoteError::TooLarge
                        } else {
                            RemoteError::Unreachable
                        }
                    })?
                    .to_bytes();
                Ok(Answer { status, headers, body })
            };
            tokio::time::timeout(REQUEST_TIMEOUT, exchange).await.map_err(|_| RemoteError::Timeout)?
        })
    }
}

fn reason(err: &(dyn std::error::Error + 'static)) -> RemoteError {
    let mut source = Some(err);
    while let Some(inner) = source {
        if let Some(io) = inner.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::PermissionDenied
        {
            return RemoteError::NotAllowed("the address does not lead to a public server".into());
        }
        source = inner.source();
    }
    RemoteError::Unreachable
}

/// The site a host belongs to, for keeping a login within it: its registrable domain, so
/// `p42-caldav.icloud.com` belongs with `caldav.icloud.com`, but `example.co.uk` not with
/// `other.co.uk`.
fn site_of(url: &Url) -> Option<String> {
    match url.host()? {
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            Some(psl::domain_str(&domain).unwrap_or(&domain).to_owned())
        }
        Host::Ipv4(ip) => Some(ip.to_string()),
        Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

fn within(url: &Url, site: &str) -> bool {
    let Some(host) = url.host_str() else { return false };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == site || host.ends_with(&format!(".{site}"))
}

/// Checks an address the way every request of this module is checked: https, no login in it, and
/// not a name or address inside the network.
pub fn checked_url(url: &str) -> Result<Url, RemoteError> {
    uwumail_smtp::fetch::check_url(url, false).map_err(RemoteError::NotAllowed)
}

/// Requests to one server, with or without a login.
pub struct Remote<'a> {
    transport: &'a dyn Transport,
    login: Option<String>,
    /// Where the login may go: the site of the first request.
    site: Option<String>,
}

/// What came back, and from where after redirects.
#[derive(Debug, Clone)]
pub struct Reply {
    pub url: Url,
    pub answer: Answer,
}

impl<'a> Remote<'a> {
    pub fn anonymous(transport: &'a dyn Transport) -> Remote<'a> {
        Remote { transport, login: None, site: None }
    }

    pub fn with_login(transport: &'a dyn Transport, username: &str, password: &str) -> Remote<'a> {
        let login = format!("Basic {}", BASE64.encode(format!("{username}:{password}")));
        Remote { transport, login: Some(login), site: None }
    }

    /// The same login for another server, to be given its own site by its first request.
    pub fn fork(&self) -> Remote<'a> {
        Remote { transport: self.transport, login: self.login.clone(), site: None }
    }

    /// Sends a request, following redirects. The first request decides the site the login stays
    /// in. A 401 with the login sent means the password is wrong.
    pub async fn request(
        &mut self,
        method: Method,
        url: &str,
        headers: &[(&'static str, String)],
        body: Option<String>,
        max_bytes: usize,
    ) -> Result<Reply, RemoteError> {
        let mut current = checked_url(url)?;
        if self.site.is_none() {
            self.site = site_of(&current);
        }
        let (mut method, mut body) = (method, body);
        for _ in 0..=MAX_REDIRECTS {
            let mut request = Request::builder().method(method.clone()).uri(current.as_str()).header(USER_AGENT, AGENT);
            for (name, value) in headers {
                request = request.header(*name, value.as_str());
            }
            if body.is_some() {
                request = request.header(CONTENT_TYPE, "application/xml; charset=utf-8");
            }
            let with_login = match (&self.login, &self.site) {
                (Some(login), Some(site)) if within(&current, site) => {
                    request = request.header(AUTHORIZATION, login.as_str());
                    true
                }
                _ => false,
            };
            let request = request
                .body(Bytes::from(body.clone().unwrap_or_default()))
                .map_err(|_| RemoteError::NotAllowed("that is not a web address".into()))?;
            let answer = self.transport.send(request, max_bytes).await?;
            let status = StatusCode::from_u16(answer.status).unwrap_or(StatusCode::BAD_GATEWAY);
            if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
                let location = answer
                    .headers
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(RemoteError::Status(answer.status))?;
                let next = current.join(location).map_err(|_| RemoteError::Status(answer.status))?;
                current = checked_url(next.as_str())?;
                if status == StatusCode::SEE_OTHER {
                    method = Method::GET;
                    body = None;
                }
                continue;
            }
            if status == StatusCode::UNAUTHORIZED && self.login.is_some() {
                return Err(if with_login { RemoteError::WrongPassword } else { RemoteError::RedirectedElsewhere });
            }
            return Ok(Reply { url: current, answer });
        }
        Err(RemoteError::Redirects)
    }

    /// A PROPFIND with the named properties (`<d:displayname/>` and the like, with the prefixes of
    /// [`xml::MULTISTATUS_START`]). `None` when the server does not answer it with a multistatus.
    pub async fn propfind(&mut self, url: &str, depth: u8, props: &str) -> Result<Option<Multistatus>, RemoteError> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:propfind xmlns:d=\"DAV:\" \
xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\" \
xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:ical=\"http://apple.com/ns/ical/\"><d:prop>{props}</d:prop></d:propfind>"
        );
        let reply = self
            .request(
                Method::from_bytes(b"PROPFIND").expect("a method name"),
                url,
                &[("depth", depth.to_string())],
                Some(body),
                4 * 1024 * 1024,
            )
            .await?;
        multistatus(reply)
    }

    /// A REPORT; `None` when the server does not answer it with a multistatus.
    pub async fn report(
        &mut self,
        url: &str,
        body: String,
        max_bytes: usize,
    ) -> Result<Option<Multistatus>, RemoteError> {
        let reply = self
            .request(
                Method::from_bytes(b"REPORT").expect("a method name"),
                url,
                &[("depth", "1".into())],
                Some(body),
                max_bytes,
            )
            .await?;
        multistatus(reply)
    }

    /// A plain GET of one resource.
    pub async fn get(&mut self, url: &str, accept: &str, max_bytes: usize) -> Result<Reply, RemoteError> {
        self.request(Method::GET, url, &[(ACCEPT.as_str(), accept.to_owned())], None, max_bytes).await
    }
}

/// One `<d:response>` of a multistatus: where, and the properties that came back with 200.
#[derive(Debug, Clone)]
pub struct Entry {
    pub href: Url,
    pub props: Vec<Element>,
    /// The status of the response itself, as multiget reports missing entries.
    pub missing: bool,
}

impl Entry {
    pub fn prop(&self, ns: &str, name: &str) -> Option<&Element> {
        self.props.iter().find(|prop| prop.is(ns, name))
    }

    /// The href inside a property, like `current-user-principal`.
    pub fn href_in(&self, ns: &str, name: &str, base: &Url) -> Option<Url> {
        let href = self.prop(ns, name)?.child(DAV, "href")?.all_text();
        base.join(href.trim()).ok()
    }

    pub fn text(&self, ns: &str, name: &str) -> Option<String> {
        let text = self.prop(ns, name)?.all_text();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    pub fn has_type(&self, ns: &str, name: &str) -> bool {
        self.prop(DAV, "resourcetype").is_some_and(|types| types.child(ns, name).is_some())
    }
}

#[derive(Debug, Clone)]
pub struct Multistatus {
    /// Where the answer came from, for hrefs relative to it.
    pub url: Url,
    pub entries: Vec<Entry>,
}

fn multistatus(reply: Reply) -> Result<Option<Multistatus>, RemoteError> {
    if reply.answer.status != 207 {
        return Ok(None);
    }
    let Some(root) = xml::parse_with(&reply.answer.body, MAX_XML_ELEMENTS).map_err(|_| RemoteError::Status(207))?
    else {
        return Ok(None);
    };
    if !root.is(DAV, "multistatus") {
        return Ok(None);
    }
    let mut entries = Vec::new();
    for response in root.children_named(DAV, "response") {
        let Some(href) = response.child(DAV, "href").map(Element::all_text) else { continue };
        let Ok(href) = reply.url.join(href.trim()) else { continue };
        let missing = response.child(DAV, "status").is_some_and(|status| !status.all_text().contains(" 200"));
        let mut props = Vec::new();
        for propstat in response.children_named(DAV, "propstat") {
            let ok = propstat.child(DAV, "status").is_none_or(|status| status.all_text().contains(" 200"));
            if let (true, Some(prop)) = (ok, propstat.child(DAV, "prop")) {
                props.extend(prop.children.iter().cloned());
            }
        }
        entries.push(Entry { href, props, missing });
    }
    Ok(Some(Multistatus { url: reply.url, entries }))
}

#[cfg(test)]
pub(crate) mod testing {
    //! A transport that hands requests to a router, as if it were a server on the internet.

    use std::sync::Mutex;

    use axum::Router;
    use axum::body::Body;
    use axum::http::HeaderValue;
    use tower::ServiceExt;

    use super::*;

    pub struct RouterTransport {
        pub router: Router,
        /// What was sent, with whether it carried a login.
        pub seen: Mutex<Vec<(String, String, bool)>>,
    }

    impl RouterTransport {
        pub fn new(router: Router) -> RouterTransport {
            RouterTransport { router, seen: Mutex::new(Vec::new()) }
        }
    }

    impl Transport for RouterTransport {
        fn send(&self, request: Request<Bytes>, max_bytes: usize) -> BoxFuture<'_, Result<Answer, RemoteError>> {
            Box::pin(async move {
                let uri = request.uri().clone();
                self.seen.lock().unwrap().push((
                    request.method().to_string(),
                    uri.to_string(),
                    request.headers().contains_key(AUTHORIZATION),
                ));
                // The router knows only paths; the host goes into the Host header.
                let (mut parts, body) = request.into_parts();
                let host = uri.authority().map(|a| a.to_string()).unwrap_or_default();
                parts.uri = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").parse().unwrap();
                parts.headers.insert("host", HeaderValue::from_str(&host).unwrap());
                let response = self.router.clone().oneshot(Request::from_parts(parts, Body::from(body))).await.unwrap();
                let status = response.status().as_u16();
                let headers = response.headers().clone();
                let body =
                    axum::body::to_bytes(response.into_body(), max_bytes).await.map_err(|_| RemoteError::TooLarge)?;
                Ok(Answer { status, headers, body })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::routing::any;

    use super::testing::RouterTransport;
    use super::*;

    #[test]
    fn sites_keep_logins_at_home() {
        let url = Url::parse("https://caldav.icloud.com/").unwrap();
        let site = site_of(&url).unwrap();
        assert_eq!(site, "icloud.com");
        assert!(within(&Url::parse("https://p42-caldav.icloud.com/x").unwrap(), &site));
        assert!(!within(&Url::parse("https://icloud.com.example.net/").unwrap(), &site));
        assert_eq!(site_of(&Url::parse("https://dav.example.co.uk/").unwrap()).unwrap(), "example.co.uk");
        assert!(checked_url("http://calendar.example.org/x.ics").is_err());
        assert!(checked_url("https://localhost/x.ics").is_err());
        assert!(checked_url("https://192.168.1.2/x.ics").is_err());
        assert!(checked_url("https://user:pw@calendar.example.org/").is_err());
        assert!(checked_url("https://calendar.example.org/x.ics").is_ok());
    }

    #[tokio::test]
    async fn logins_do_not_follow_redirects_elsewhere() {
        let router = axum::Router::new()
            .route(
                "/moved",
                any(|| async { (StatusCode::TEMPORARY_REDIRECT, [(LOCATION, "https://evil.example.net/steal")]) }),
            )
            .route(
                "/home",
                any(|| async { (StatusCode::MOVED_PERMANENTLY, [(LOCATION, "https://p1.example.org/dav/")]) }),
            )
            .route("/steal", any(|| async { StatusCode::UNAUTHORIZED }))
            .route("/dav/", any(|| async { StatusCode::UNAUTHORIZED }));
        let transport = RouterTransport::new(router);
        let mut remote = Remote::with_login(&transport, "mini", "geheim");
        let far = remote.request(Method::GET, "https://dav.example.org/moved", &[], None, 1000).await;
        assert_eq!(far.unwrap_err(), RemoteError::RedirectedElsewhere);
        let seen = transport.seen.lock().unwrap().clone();
        assert_eq!(seen.iter().map(|(_, _, login)| *login).collect::<Vec<_>>(), vec![true, false]);

        let mut remote = Remote::with_login(&transport, "mini", "falsch");
        let near = remote.request(Method::GET, "https://dav.example.org/home", &[], None, 1000).await;
        assert_eq!(near.unwrap_err(), RemoteError::WrongPassword, "the same site gets the login");
    }
}
