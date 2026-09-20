//! Fetching lists from the web: subscribed word lists and the built-in lists. Only public addresses are
//! reached, even when a name points somewhere inside the network; redirects are not followed; the body is
//! capped, also after unpacking zstd; and a list that did not change is not fetched again.

use std::future::Future;
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::Request;
use hyper::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::dns::Name;
use hyper_util::rt::TokioExecutor;
use url::{Host, Url};

const TIMEOUT: Duration = Duration::from_secs(60);
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Whether an address is on the open internet: not this machine, not the local network, not reserved.
pub fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (b & 0xc0) == 64)
                || (a == 198 && (b & 0xfe) == 18)
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first == 0x2001 && v6.segments()[1] == 0x0db8))
        }
    }
}

/// Resolves names like the system does, but keeps only public addresses.
#[derive(Clone)]
struct PublicResolver;

impl tower::Service<Name> for PublicResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        Box::pin(async move {
            let found: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> = found.into_iter().filter(|addr| is_public(addr.ip())).collect();
            if public.is_empty() {
                return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "not a public address"));
            }
            Ok(public.into_iter())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// The list is as it was when `validator` was taken.
    Unchanged,
    /// The list's bytes, unpacked, and what to ask with next time.
    Fresh { body: Vec<u8>, validator: Option<String> },
}

#[derive(Clone)]
pub struct Fetcher {
    client: Client<HttpsConnector<HttpConnector<PublicResolver>>, Empty<Bytes>>,
}

impl Fetcher {
    pub fn new() -> Fetcher {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut http = HttpConnector::new_with_resolver(PublicResolver);
        http.enforce_http(false);
        http.set_connect_timeout(Some(Duration::from_secs(15)));
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .wrap_connector(http);
        Fetcher { client: Client::builder(TokioExecutor::new()).build(connector) }
    }

    /// GETs a list. `allow_http` is for built-in lists that exist only without TLS; everything a person types
    /// in has to be https. `validator` is what an earlier fetch returned. `shown` is how errors name the link,
    /// so a secret in it never reaches a log or the portal.
    pub async fn get(
        &self,
        url: &str,
        allow_http: bool,
        validator: Option<&str>,
        max_bytes: usize,
        shown: &str,
    ) -> Result<Fetched, String> {
        let parsed = check_url(url, allow_http)?;
        let mut request = Request::get(parsed.as_str())
            .header("User-Agent", concat!("UwUMail/", env!("CARGO_PKG_VERSION"), " (list update)"));
        match validator.and_then(|validator| validator.split_once(':')) {
            Some(("etag", tag)) => request = request.header(IF_NONE_MATCH, tag),
            Some(("modified", date)) => request = request.header(IF_MODIFIED_SINCE, date),
            _ => {}
        }
        let request = request.body(Empty::new()).map_err(|err| err.to_string())?;
        let fetch = async {
            let response = self.client.request(request).await.map_err(|err| describe(&err, shown))?;
            let status = response.status();
            if status == hyper::StatusCode::NOT_MODIFIED {
                return Ok(Fetched::Unchanged);
            }
            if status.is_redirection() {
                return Err(format!("{shown} redirects elsewhere; subscribe to the address it leads to"));
            }
            if status != hyper::StatusCode::OK {
                return Err(format!("{shown} answered {status}"));
            }
            let header =
                |name| response.headers().get(name).and_then(|value: &hyper::header::HeaderValue| value.to_str().ok());
            let validator = header(ETAG)
                .map(|tag| format!("etag:{tag}"))
                .or_else(|| header(LAST_MODIFIED).map(|date| format!("modified:{date}")));
            let body = Limited::new(response.into_body(), max_bytes)
                .collect()
                .await
                .map_err(|_| format!("{shown} is bigger than {} KB or broke off", max_bytes / 1024))?
                .to_bytes();
            Ok(Fetched::Fresh { body: unpack(&body, max_bytes)?, validator })
        };
        tokio::time::timeout(TIMEOUT, fetch).await.map_err(|_| format!("{shown} did not answer in time"))?
    }
}

impl Default for Fetcher {
    fn default() -> Self {
        Fetcher::new()
    }
}

/// A link a list may be fetched from: http(s) only, no credentials, and not an address inside the network.
pub(crate) fn check_url(url: &str, allow_http: bool) -> Result<Url, String> {
    let parsed = Url::parse(url.trim()).map_err(|_| "that is not a web address".to_owned())?;
    match parsed.scheme() {
        "https" => {}
        "http" if allow_http => {}
        _ => return Err("only https links can be subscribed to".into()),
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the link may not contain a login".into());
    }
    match parsed.host() {
        None => Err("the link has no host".into()),
        Some(Host::Ipv4(ip)) if !is_public(IpAddr::V4(ip)) => Err("the link points into a private network".into()),
        Some(Host::Ipv6(ip)) if !is_public(IpAddr::V6(ip)) => Err("the link points into a private network".into()),
        Some(Host::Domain(domain)) if domain.eq_ignore_ascii_case("localhost") || !domain.contains('.') => {
            Err("the link points into a private network".into())
        }
        Some(_) => Ok(parsed),
    }
}

/// zstd-packed lists come unpacked, within the same limit.
fn unpack(body: &[u8], max_bytes: usize) -> Result<Vec<u8>, String> {
    if !body.starts_with(&ZSTD_MAGIC) {
        return Ok(body.to_vec());
    }
    let decoder = ruzstd::decoding::StreamingDecoder::new(body).map_err(|err| format!("unpacking failed: {err}"))?;
    let mut unpacked = Vec::new();
    decoder.take(max_bytes as u64 + 1).read_to_end(&mut unpacked).map_err(|err| format!("unpacking failed: {err}"))?;
    if unpacked.len() > max_bytes {
        return Err(format!("the list is bigger than {} KB unpacked", max_bytes / 1024));
    }
    Ok(unpacked)
}

fn describe(err: &(dyn std::error::Error + 'static), shown: &str) -> String {
    let mut source = Some(err);
    while let Some(inner) = source {
        if let Some(io) = inner.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::PermissionDenied
        {
            return format!("{shown} does not lead to a public address");
        }
        source = inner.source();
    }
    format!("{shown} could not be reached")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_and_web_links_are_fetched() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.20",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "192.0.2.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "9.9.9.9", "2a01:4f8::1"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        assert!(check_url("https://maps.example.org/list.txt", false).is_ok());
        assert!(check_url("http://maps.example.org/list.txt", false).is_err(), "people subscribe over https");
        assert!(check_url("http://maps.example.org/list.txt", true).is_ok());
        assert!(check_url("file:///etc/passwd", true).is_err());
        assert!(check_url("https://127.0.0.1/list", false).is_err());
        assert!(check_url("https://[::1]/list", false).is_err());
        assert!(check_url("https://localhost/list", false).is_err());
        assert!(check_url("https://nas/list", false).is_err());
        assert!(check_url("https://user:pw@maps.example.org/list", false).is_err());
    }

    #[test]
    fn zstd_lists_are_unpacked_within_the_limit() {
        // "spam.example\n" packed with zstd (Node's zlib.zstdCompressSync).
        let packed = [
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x0d, 0x69, 0x00, 0x00, 0x73, 0x70, 0x61, 0x6d, 0x2e, 0x65, 0x78, 0x61, 0x6d,
            0x70, 0x6c, 0x65, 0x0a,
        ];
        assert_eq!(unpack(&packed, 1024).unwrap(), b"spam.example\n");
        assert!(unpack(&packed, 5).is_err());
        assert_eq!(unpack(b"plain\n", 1024).unwrap(), b"plain\n");
    }
}
