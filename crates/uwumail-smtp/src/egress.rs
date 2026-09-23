//! The way out for requests that tell a sender something about the people reading their mail: the remote
//! pictures in a message, and the logos shown next to it. Whoever serves such a picture learns when, from
//! where and how often it was looked at.
//!
//! Fetching them here instead of in the reader's browser already hides the reader behind the server. With a
//! proxy set, they leave through it too, so the sender sees the address of a VPN rather than the server's:
//! gluetun's HTTP proxy (OpenVPN, WireGuard, NordVPN and others), or a SOCKS5 proxy a VPN provider offers.
//!
//! Everything else — DNS, delivery, blocklists, list updates — keeps leaving directly: it says nothing about
//! who reads what. Names are resolved here, before the proxy sees them, so a picture can't reach an address
//! inside the network through it either.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::header::{ACCEPT, CONTENT_TYPE, LOCATION, USER_AGENT};
use hyper::{Request, StatusCode, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use url::Url;

use crate::fetch::{check_url, is_public};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 4;
/// Fetches at the same time, for everyone together: a newsletter with fifty pictures must not turn the
/// server into a flood.
const MAX_CONCURRENT: usize = 32;
/// Addresses tried per name before giving up.
const MAX_ADDRESSES: usize = 3;
/// What the request says it is. Nothing that singles out this server or its version.
const AGENT: &str = "Mozilla/5.0";

/// `[egress]` in the configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EgressConfig {
    /// `http://host:port` for a proxy that tunnels with CONNECT (gluetun: `http://gluetun:8888`), or
    /// `socks5://host:port`. Either may carry `user:password@`. Empty: straight from the server.
    pub proxy: String,
    /// What happens when the proxy can't be reached or refuses the tunnel.
    pub fallback: Fallback,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fallback {
    /// Nothing is fetched. Pictures stay away until the proxy is back.
    #[default]
    Block,
    /// Fetched straight from the server, which the sender then sees.
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Proxy {
    /// HTTP CONNECT; `auth` is the ready `Proxy-Authorization` value.
    Http {
        address: String,
        auth: Option<String>,
    },
    Socks5 {
        address: String,
        auth: Option<(String, String)>,
    },
}

impl Proxy {
    fn parse(proxy: &str) -> Result<Option<Proxy>, String> {
        let proxy = proxy.trim();
        if proxy.is_empty() {
            return Ok(None);
        }
        let url = Url::parse(proxy).map_err(|_| "egress.proxy is not a URL like http://gluetun:8888".to_owned())?;
        let host = url.host_str().ok_or("egress.proxy has no host")?;
        let decode = |part: &str| {
            percent_decode(part).ok_or_else(|| "egress.proxy has a login that is not valid UTF-8".to_owned())
        };
        let login = match (url.username(), url.password()) {
            ("", None) => None,
            (user, password) => Some((decode(user)?, decode(password.unwrap_or(""))?)),
        };
        let default_port = match url.scheme() {
            "http" => 8080,
            "socks5" | "socks5h" => 1080,
            other => return Err(format!("egress.proxy: {other}:// is not supported, only http:// and socks5://")),
        };
        let address = format!("{host}:{}", url.port().unwrap_or(default_port));
        Ok(Some(if url.scheme() == "http" {
            let auth = login.map(|(user, password)| {
                format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
            });
            Proxy::Http { address, auth }
        } else {
            if let Some((user, password)) = &login
                && (user.len() > 255 || password.len() > 255)
            {
                return Err("egress.proxy: a SOCKS5 login is at most 255 bytes each".into());
            }
            Proxy::Socks5 { address, auth: login }
        }))
    }

    /// A connection to `target` through the proxy.
    async fn open(&self, target: SocketAddr) -> std::io::Result<TcpStream> {
        let address = match self {
            Proxy::Http { address, .. } | Proxy::Socks5 { address, .. } => address,
        };
        let mut stream = timed(TcpStream::connect(address.as_str())).await?;
        match self {
            Proxy::Http { auth, .. } => timed(http_connect(&mut stream, target, auth.as_deref())).await?,
            Proxy::Socks5 { auth, .. } => timed(socks5_connect(&mut stream, target, auth.as_ref())).await?,
        }
        Ok(stream)
    }
}

fn percent_decode(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = part.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

async fn timed<T>(step: impl Future<Output = std::io::Result<T>>) -> std::io::Result<T> {
    tokio::time::timeout(CONNECT_TIMEOUT, step)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "the proxy did not answer in time"))?
}

fn refused(what: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionRefused, what.into())
}

async fn http_connect(stream: &mut TcpStream, target: SocketAddr, auth: Option<&str>) -> std::io::Result<()> {
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(auth) = auth {
        request.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    // The answer ends with an empty line; nothing of the tunnelled connection comes before we speak.
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while !answer.ends_with(b"\r\n\r\n") {
        if answer.len() > 8 * 1024 {
            return Err(refused("the proxy's answer is too long"));
        }
        if stream.read(&mut byte).await? == 0 {
            return Err(refused("the proxy closed the connection"));
        }
        answer.push(byte[0]);
    }
    let status = answer.split(|&b| b == b' ').nth(1).unwrap_or_default();
    if status != b"200" {
        let line = String::from_utf8_lossy(answer.split(|&b| b == b'\r').next().unwrap_or_default()).into_owned();
        return Err(refused(format!("the proxy refused the tunnel: {line}")));
    }
    Ok(())
}

/// RFC 1928, CONNECT to an address, with RFC 1929 username and password when set.
async fn socks5_connect(
    stream: &mut TcpStream,
    target: SocketAddr,
    auth: Option<&(String, String)>,
) -> std::io::Result<()> {
    let method = if auth.is_some() { 0x02 } else { 0x00 };
    stream.write_all(&[0x05, 0x01, method]).await?;
    let mut chosen = [0u8; 2];
    stream.read_exact(&mut chosen).await?;
    if chosen != [0x05, method] {
        return Err(refused("the SOCKS5 proxy wants a different login"));
    }
    if let Some((user, password)) = auth {
        let mut login = vec![0x01, user.len() as u8];
        login.extend_from_slice(user.as_bytes());
        login.push(password.len() as u8);
        login.extend_from_slice(password.as_bytes());
        stream.write_all(&login).await?;
        let mut verdict = [0u8; 2];
        stream.read_exact(&mut verdict).await?;
        if verdict[1] != 0x00 {
            return Err(refused("the SOCKS5 proxy turned the login down"));
        }
    }
    let mut request = vec![0x05, 0x01, 0x00];
    match target.ip() {
        IpAddr::V4(ip) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        }
    }
    request.extend_from_slice(&target.port().to_be_bytes());
    stream.write_all(&request).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(refused(format!("the SOCKS5 proxy could not connect (reply {})", head[1])));
    }
    let rest = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => stream.read_u8().await? as usize + 2,
        _ => return Err(refused("the SOCKS5 proxy answered in a way nobody speaks")),
    };
    let mut bound = vec![0u8; rest];
    stream.read_exact(&mut bound).await?;
    Ok(())
}

/// Resolves the name itself, keeps only public addresses, and connects to one of them — through the proxy
/// when there is one.
#[derive(Clone)]
struct Connector {
    proxy: Option<Arc<Proxy>>,
    fallback: Fallback,
    /// Every name leads here, in tests: the pictures then come from a server on this machine.
    #[cfg(test)]
    pinned: Option<SocketAddr>,
}

impl Connector {
    async fn addresses(&self, uri: &Uri) -> std::io::Result<Vec<SocketAddr>> {
        #[cfg(test)]
        if let Some(pinned) = self.pinned {
            return Ok(vec![pinned]);
        }
        let host = uri.host().ok_or_else(|| refused("no host"))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("http") { 80 } else { 443 });
        let found: Vec<SocketAddr> = match host.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::net::lookup_host((host, port)).await?.collect(),
        };
        let public: Vec<SocketAddr> =
            found.into_iter().filter(|address| is_public(address.ip())).take(MAX_ADDRESSES).collect();
        if public.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "not a public address"));
        }
        Ok(public)
    }

    async fn connect(&self, target: SocketAddr) -> std::io::Result<TcpStream> {
        let Some(proxy) = &self.proxy else {
            return timed(TcpStream::connect(target)).await;
        };
        match proxy.open(target).await {
            Ok(stream) => Ok(stream),
            Err(err) if self.fallback == Fallback::Direct => {
                tracing::warn!(%err, "the egress proxy failed, fetching directly as configured");
                timed(TcpStream::connect(target)).await
            }
            Err(err) => Err(err),
        }
    }
}

impl tower::Service<Uri> for Connector {
    type Response = TokioIo<TcpStream>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connector = self.clone();
        Box::pin(async move {
            let mut last = None;
            for target in connector.addresses(&uri).await? {
                match connector.connect(target).await {
                    Ok(stream) => {
                        let _ = stream.set_nodelay(true);
                        return Ok(TokioIo::new(stream));
                    }
                    Err(err) => last = Some(err),
                }
            }
            Err(last.unwrap_or_else(|| refused("no address to connect to")))
        })
    }
}

/// A fetched picture.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The `Content-Type` as sent, without parameters and in lower case; empty when there was none.
    pub media_type: String,
    pub body: Bytes,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EgressError {
    #[error("{0}")]
    NotAllowed(String),
    #[error("the address could not be reached")]
    Unreachable,
    #[error("the answer was {0}")]
    Status(u16),
    #[error("too many redirects")]
    Redirects,
    #[error("bigger than allowed")]
    TooLarge,
    #[error("no answer in time")]
    Timeout,
}

#[derive(Clone)]
pub struct Egress {
    client: Client<HttpsConnector<Connector>, Empty<Bytes>>,
    permits: Arc<Semaphore>,
    proxied: bool,
}

impl Egress {
    pub fn new(config: &EgressConfig) -> Result<Egress, String> {
        let proxy = Proxy::parse(&config.proxy)?;
        Ok(Egress::build(Connector {
            proxy: proxy.map(Arc::new),
            fallback: config.fallback,
            #[cfg(test)]
            pinned: None,
        }))
    }

    /// Straight from the server, for tools and tests that have no configuration.
    pub fn direct() -> Egress {
        Egress::new(&EgressConfig::default()).expect("no proxy to get wrong")
    }

    fn build(connector: Connector) -> Egress {
        let proxied = connector.proxy.is_some();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .wrap_connector(connector);
        Egress {
            client: Client::builder(TokioExecutor::new()).build(https),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            proxied,
        }
    }

    /// Whether requests leave through a proxy.
    pub fn proxied(&self) -> bool {
        self.proxied
    }

    /// GETs `url`, following a few redirects, and gives up past `max_bytes`. No cookies, no referrer, and an
    /// agent string that says nothing about this server.
    pub async fn get(&self, url: &str, accept: &str, max_bytes: usize) -> Result<Fetched, EgressError> {
        let _permit = self.permits.acquire().await.map_err(|_| EgressError::Unreachable)?;
        tokio::time::timeout(TIMEOUT, self.follow(url, accept, max_bytes)).await.map_err(|_| EgressError::Timeout)?
    }

    async fn follow(&self, url: &str, accept: &str, max_bytes: usize) -> Result<Fetched, EgressError> {
        let mut current = check_url(url, true).map_err(EgressError::NotAllowed)?;
        for _ in 0..=MAX_REDIRECTS {
            let request = Request::get(current.as_str())
                .header(USER_AGENT, AGENT)
                .header(ACCEPT, accept)
                .body(Empty::new())
                .map_err(|_| EgressError::NotAllowed("that is not a web address".into()))?;
            let response = self.client.request(request).await.map_err(|err| reason(&err))?;
            let status = response.status();
            if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(EgressError::Status(status.as_u16()))?;
                let next = current.join(location).map_err(|_| EgressError::Status(status.as_u16()))?;
                current = check_url(next.as_str(), true).map_err(EgressError::NotAllowed)?;
                continue;
            }
            if status != StatusCode::OK {
                return Err(EgressError::Status(status.as_u16()));
            }
            let media_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(|value| value.trim().to_ascii_lowercase())
                .unwrap_or_default();
            let body = Limited::new(response.into_body(), max_bytes)
                .collect()
                .await
                .map_err(|err| {
                    if err.is::<http_body_util::LengthLimitError>() {
                        EgressError::TooLarge
                    } else {
                        EgressError::Unreachable
                    }
                })?
                .to_bytes();
            return Ok(Fetched { media_type, body });
        }
        Err(EgressError::Redirects)
    }
}

fn reason(err: &(dyn std::error::Error + 'static)) -> EgressError {
    let mut source = Some(err);
    while let Some(inner) = source {
        if let Some(io) = inner.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::PermissionDenied
        {
            return EgressError::NotAllowed("the link does not lead to a public address".into());
        }
        source = inner.source();
    }
    EgressError::Unreachable
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::net::TcpListener;

    use super::*;

    fn egress(proxy: &str, fallback: Fallback, pinned: SocketAddr) -> Egress {
        let proxy = Proxy::parse(proxy).unwrap();
        Egress::build(Connector { proxy: proxy.map(Arc::new), fallback, pinned: Some(pinned) })
    }

    async fn read_head(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() == 1 {
            head.push(byte[0]);
        }
        String::from_utf8(head).unwrap()
    }

    /// A web server with a few pictures; remembers the requests it saw.
    async fn pictures() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let log = log.clone();
                tokio::spawn(async move {
                    let head = read_head(&mut stream).await;
                    let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                    log.lock().unwrap().push(head);
                    let answer = match path.as_str() {
                        "/pixel.gif" => {
                            "HTTP/1.1 200 OK\r\nContent-Type: image/GIF; x=y\r\nContent-Length: 6\r\n\r\nGIF89a"
                                .to_owned()
                        }
                        "/moved" => {
                            "HTTP/1.1 302 Found\r\nLocation: /pixel.gif\r\nContent-Length: 0\r\n\r\n".to_owned()
                        }
                        "/inside" => {
                            "HTTP/1.1 302 Found\r\nLocation: http://10.0.0.1/x\r\nContent-Length: 0\r\n\r\n".to_owned()
                        }
                        "/big" => format!("HTTP/1.1 200 OK\r\nContent-Length: 2000\r\n\r\n{}", "x".repeat(2000)),
                        _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_owned(),
                    };
                    stream.write_all(answer.as_bytes()).await.unwrap();
                });
            }
        });
        (address, seen)
    }

    /// An HTTP proxy that tunnels with CONNECT; remembers what it was asked.
    async fn http_proxy() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let log = log.clone();
                tokio::spawn(async move {
                    let head = read_head(&mut stream).await;
                    let target = head.split(' ').nth(1).unwrap().to_owned();
                    log.lock().unwrap().push(head);
                    let mut upstream = TcpStream::connect(target).await.unwrap();
                    stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                });
            }
        });
        (address, seen)
    }

    /// A SOCKS5 proxy that wants `user` / `secret`.
    async fn socks_proxy() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut greeting = [0u8; 3];
                    stream.read_exact(&mut greeting).await.unwrap();
                    assert_eq!(greeting, [5, 1, 2], "only the login method is offered");
                    stream.write_all(&[5, 2]).await.unwrap();
                    let mut version_and_length = [0u8; 2];
                    stream.read_exact(&mut version_and_length).await.unwrap();
                    let mut user = vec![0u8; version_and_length[1] as usize];
                    stream.read_exact(&mut user).await.unwrap();
                    let mut password = vec![0u8; stream.read_u8().await.unwrap() as usize];
                    stream.read_exact(&mut password).await.unwrap();
                    let good = user == b"user" && password == b"secret";
                    stream.write_all(&[1, if good { 0 } else { 1 }]).await.unwrap();
                    if !good {
                        return;
                    }
                    let mut head = [0u8; 4];
                    stream.read_exact(&mut head).await.unwrap();
                    assert_eq!(head, [5, 1, 0, 1], "CONNECT to an IPv4 address, never a name");
                    let mut ip = [0u8; 4];
                    stream.read_exact(&mut ip).await.unwrap();
                    let port = stream.read_u16().await.unwrap();
                    let mut upstream = TcpStream::connect((IpAddr::from(ip), port)).await.unwrap();
                    stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                });
            }
        });
        address
    }

    async fn closed_port() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    }

    #[test]
    fn proxies_are_read_from_the_configuration() {
        assert_eq!(Proxy::parse(" ").unwrap(), None);
        assert_eq!(
            Proxy::parse("http://gluetun:8888").unwrap(),
            Some(Proxy::Http { address: "gluetun:8888".into(), auth: None })
        );
        assert_eq!(
            Proxy::parse("http://me:p%40ss@proxy.example").unwrap(),
            Some(Proxy::Http { address: "proxy.example:8080".into(), auth: Some("Basic bWU6cEBzcw==".into()) })
        );
        assert_eq!(
            Proxy::parse("socks5://me:secret@[2001:db8::1]:1080").unwrap(),
            Some(Proxy::Socks5 { address: "[2001:db8::1]:1080".into(), auth: Some(("me".into(), "secret".into())) })
        );
        assert!(Proxy::parse("gluetun:8888").is_err());
        assert!(Proxy::parse("https://proxy.example").is_err(), "a TLS proxy is not spoken");
        assert!(Proxy::parse(&format!("socks5://{}:x@proxy.example", "u".repeat(256))).is_err());
    }

    #[tokio::test]
    async fn pictures_come_without_anything_that_points_at_the_reader() {
        let (server, seen) = pictures().await;
        let egress = egress("", Fallback::Block, server);
        let picture = egress.get("http://pictures.example/pixel.gif", "image/*", 1024).await.unwrap();
        assert_eq!((picture.media_type.as_str(), &picture.body[..]), ("image/gif", &b"GIF89a"[..]));
        let head = seen.lock().unwrap()[0].to_ascii_lowercase();
        assert!(head.contains("user-agent: mozilla/5.0\r\n"), "{head}");
        assert!(!head.contains("cookie") && !head.contains("referer") && !head.contains("uwumail"), "{head}");
    }

    #[tokio::test]
    async fn redirects_are_followed_but_never_into_the_network() {
        let (server, _) = pictures().await;
        let egress = egress("", Fallback::Block, server);
        let picture = egress.get("http://pictures.example/moved", "image/*", 1024).await.unwrap();
        assert_eq!(&picture.body[..], b"GIF89a");
        let inside = egress.get("http://pictures.example/inside", "image/*", 1024).await;
        assert!(matches!(inside, Err(EgressError::NotAllowed(_))), "{inside:?}");
        assert!(matches!(egress.get("http://10.1.2.3/x", "image/*", 1024).await, Err(EgressError::NotAllowed(_))));
        assert!(matches!(egress.get("file:///etc/passwd", "image/*", 1024).await, Err(EgressError::NotAllowed(_))));
        let big = egress.get("http://pictures.example/big", "image/*", 1024).await;
        assert_eq!(big.unwrap_err(), EgressError::TooLarge);
        let gone = egress.get("http://pictures.example/gone", "image/*", 1024).await;
        assert_eq!(gone.unwrap_err(), EgressError::Status(404));
    }

    #[tokio::test]
    async fn an_http_proxy_tunnels_to_the_address_resolved_here() {
        let (server, _) = pictures().await;
        let (proxy, asked) = http_proxy().await;
        let egress = egress(&format!("http://me:secret@{proxy}"), Fallback::Block, server);
        assert!(egress.proxied());
        let picture = egress.get("http://pictures.example/pixel.gif", "image/*", 1024).await.unwrap();
        assert_eq!(&picture.body[..], b"GIF89a");
        let head = asked.lock().unwrap()[0].clone();
        assert!(head.starts_with(&format!("CONNECT {server} HTTP/1.1\r\n")), "{head}");
        assert!(head.contains("Proxy-Authorization: Basic bWU6c2VjcmV0\r\n"), "{head}");
    }

    #[tokio::test]
    async fn a_socks5_proxy_with_a_login_carries_the_request() {
        let (server, _) = pictures().await;
        let proxy = socks_proxy().await;
        let picture = egress(&format!("socks5://user:secret@{proxy}"), Fallback::Block, server)
            .get("http://pictures.example/pixel.gif", "image/*", 1024)
            .await
            .unwrap();
        assert_eq!(&picture.body[..], b"GIF89a");
        let wrong = egress(&format!("socks5://user:wrong@{proxy}"), Fallback::Block, server)
            .get("http://pictures.example/pixel.gif", "image/*", 1024)
            .await;
        assert_eq!(wrong.unwrap_err(), EgressError::Unreachable);
    }

    #[tokio::test]
    async fn without_its_proxy_the_server_blocks_or_goes_direct_as_told() {
        let (server, seen) = pictures().await;
        let gone = closed_port().await;
        let blocked = egress(&format!("http://{gone}"), Fallback::Block, server)
            .get("http://pictures.example/pixel.gif", "image/*", 1024)
            .await;
        assert_eq!(blocked.unwrap_err(), EgressError::Unreachable);
        assert!(seen.lock().unwrap().is_empty(), "nothing reached the sender");
        let direct = egress(&format!("socks5://{gone}"), Fallback::Direct, server)
            .get("http://pictures.example/pixel.gif", "image/*", 1024)
            .await
            .unwrap();
        assert_eq!(&direct.body[..], b"GIF89a");
    }
}
