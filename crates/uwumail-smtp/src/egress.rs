//! The way out for requests that tell a sender something about the people reading their mail: the remote
//! pictures in a message, and the logos shown next to it. Whoever serves such a picture learns when, from
//! where and how often it was looked at.
//!
//! Fetching them here instead of in the reader's browser already hides the reader behind the server. With a
//! proxy set, they leave through it too, so the sender sees the address of a VPN rather than the server's:
//! gluetun's HTTP proxy (OpenVPN, WireGuard, NordVPN and others), or a SOCKS5 proxy a VPN provider offers.
//!
//! The admin can send two more kinds of request the same way: the check for new UwUMail versions, and
//! fetching mail from mailboxes at other providers. Everything else — DNS, delivery, blocklists, list
//! updates — keeps leaving directly. Names are resolved here, before the proxy sees them, so a request can't
//! reach an address inside the network through it either.
//!
//! The proxy and the ways that take it can change while the server runs: the admin panel sets them, and every
//! copy of an [`Egress`] sees the change with its next request.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
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
use serde::{Deserialize, Serialize};
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
/// Answers with the address a request came from. Asked only when an admin tests the way out.
const ADDRESS_ECHO: &str = "https://api.ipify.org/";

/// `[egress]` in the configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EgressConfig {
    /// `http://host:port` for a proxy that tunnels with CONNECT (gluetun: `http://gluetun:8888`), or
    /// `socks5://host:port`. Either may carry `user:password@`. Empty: straight from the server.
    pub proxy: String,
    /// What happens when the proxy can't be reached or refuses the tunnel.
    pub fallback: Fallback,
    /// Remote pictures and sender logos take the proxy. On unless switched off.
    pub pictures: bool,
    /// The check for new UwUMail versions takes the proxy.
    pub updates: bool,
    /// Fetching mail from mailboxes at other providers takes the proxy.
    pub fetch: bool,
}

impl Default for EgressConfig {
    fn default() -> Self {
        EgressConfig { proxy: String::new(), fallback: Fallback::Block, pictures: true, updates: false, fetch: false }
    }
}

/// What a request is for, which decides whether it takes the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Pictures,
    Updates,
    Fetch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
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

    /// The proxy without its login, for showing.
    fn shown(&self) -> String {
        match self {
            Proxy::Http { address, .. } => format!("http://{address}"),
            Proxy::Socks5 { address, .. } => format!("socks5://{address}"),
        }
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
/// Counted since the server started.
#[derive(Default)]
struct Stats {
    fetched: AtomicU64,
    failed: AtomicU64,
    proxy_failures: AtomicU64,
    fallbacks: AtomicU64,
    last_proxy_failure: Mutex<Option<ProxyFailure>>,
}

/// The latest time the proxy could not be used.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProxyFailure {
    /// Unix seconds.
    pub at: i64,
    pub error: String,
}

/// What the admin panel shows about the way out.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressStatus {
    /// The proxy without its login, e.g. `http://gluetun:8888`; none when pictures leave straight.
    pub proxy: Option<String>,
    pub fallback: Fallback,
    /// Pictures fetched and pictures that could not be, since the server started.
    pub fetched: u64,
    pub failed: u64,
    /// Connections the proxy could not carry, and how many of those went out directly instead.
    pub proxy_failures: u64,
    pub fallbacks: u64,
    pub last_proxy_failure: Option<ProxyFailure>,
    /// Which kinds of request take the proxy while one is set.
    pub routes: Routes,
}

/// Which kinds of request take the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Routes {
    pub pictures: bool,
    pub updates: bool,
    pub fetch: bool,
}

impl Routes {
    fn of(config: &EgressConfig) -> Routes {
        Routes { pictures: config.pictures, updates: config.updates, fetch: config.fetch }
    }

    fn takes(&self, purpose: Purpose) -> bool {
        match purpose {
            Purpose::Pictures => self.pictures,
            Purpose::Updates => self.updates,
            Purpose::Fetch => self.fetch,
        }
    }
}

#[derive(Clone)]
struct Connector {
    proxy: Option<Arc<Proxy>>,
    fallback: Fallback,
    stats: Arc<Stats>,
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
        let err = match proxy.open(target).await {
            Ok(stream) => return Ok(stream),
            Err(err) => err,
        };
        self.stats.proxy_failures.fetch_add(1, Ordering::Relaxed);
        *self.stats.last_proxy_failure.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(ProxyFailure { at: crate::now(), error: err.to_string() });
        if self.fallback == Fallback::Direct {
            tracing::warn!(%err, "the egress proxy failed, fetching directly as configured");
            self.stats.fallbacks.fetch_add(1, Ordering::Relaxed);
            return timed(TcpStream::connect(target)).await;
        }
        tracing::warn!(%err, "the egress proxy failed, the picture stays away");
        Err(err)
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
    /// Where it came from in the end, after redirects.
    pub url: Url,
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
    #[error("the answer was not an address")]
    Garbled,
}

type HttpClient = Client<HttpsConnector<Connector>, Empty<Bytes>>;

/// One configuration of the way out, replaced as a whole when the admin panel changes it.
struct Setup {
    proxy: Option<Arc<Proxy>>,
    fallback: Fallback,
    routes: Routes,
    /// For pictures: through the proxy when they take it.
    pictures: HttpClient,
    /// Through the proxy whenever there is one, to test it.
    probe: HttpClient,
}

struct Shared {
    setup: RwLock<Arc<Setup>>,
    permits: Semaphore,
    stats: Arc<Stats>,
    roots: rustls::RootCertStore,
    #[cfg(test)]
    pinned: Option<SocketAddr>,
}

/// The way out. Cheap to clone; every clone sees a new configuration at once.
#[derive(Clone)]
pub struct Egress {
    shared: Arc<Shared>,
}

/// Opens connections the way one kind of request leaves, for code that speaks its own protocol (IMAP) or
/// has its own HTTP client.
#[derive(Clone)]
pub struct Dialer {
    connector: Connector,
}

impl Dialer {
    /// A connection to `host:port`, resolved here and only to a public address; through the proxy when the
    /// dialer has one.
    pub async fn connect(&self, host: &str, port: u16) -> std::io::Result<TcpStream> {
        let uri: Uri =
            format!("tcp://{}:{port}", if host.contains(':') { format!("[{host}]") } else { host.to_owned() })
                .parse()
                .map_err(|_| refused("not a host name"))?;
        let mut last = None;
        for target in self.connector.addresses(&uri).await? {
            match self.connector.connect(target).await {
                Ok(stream) => return Ok(stream),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| refused("no address to connect to")))
    }

    /// Whether this leaves through a proxy.
    pub fn proxied(&self) -> bool {
        self.connector.proxy.is_some()
    }

    /// The same connections as a hyper connector, for an HTTPS client.
    pub fn https_connector(&self, tls: rustls::ClientConfig) -> HttpsConnector<DialerConnector> {
        hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_only()
            .enable_http1()
            .wrap_connector(DialerConnector(self.connector.clone()))
    }
}

/// What [`Dialer::https_connector`] wraps.
#[derive(Clone)]
pub struct DialerConnector(Connector);

impl tower::Service<Uri> for DialerConnector {
    type Response = TokioIo<TcpStream>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        self.0.call(uri)
    }
}

impl Egress {
    pub fn new(config: &EgressConfig) -> Result<Egress, String> {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let egress = Egress::empty(
            roots,
            #[cfg(test)]
            None,
        );
        egress.reconfigure(config)?;
        Ok(egress)
    }

    /// Checks a configuration without putting it into effect.
    pub fn check(config: &EgressConfig) -> Result<(), String> {
        Proxy::parse(&config.proxy).map(|_| ())
    }

    /// Straight from the server, for tools and tests that have no configuration.
    pub fn direct() -> Egress {
        Egress::new(&EgressConfig::default()).expect("no proxy to get wrong")
    }

    fn empty(roots: rustls::RootCertStore, #[cfg(test)] pinned: Option<SocketAddr>) -> Egress {
        let stats: Arc<Stats> = Arc::default();
        let direct = Connector {
            proxy: None,
            fallback: Fallback::Block,
            stats: stats.clone(),
            #[cfg(test)]
            pinned,
        };
        let client = build_client(direct, &roots);
        let setup = Setup {
            proxy: None,
            fallback: Fallback::Block,
            routes: Routes::of(&EgressConfig::default()),
            pictures: client.clone(),
            probe: client,
        };
        Egress {
            shared: Arc::new(Shared {
                setup: RwLock::new(Arc::new(setup)),
                permits: Semaphore::new(MAX_CONCURRENT),
                stats,
                roots,
                #[cfg(test)]
                pinned,
            }),
        }
    }

    /// Every name leads to `pinned`, whose certificate is trusted: websites for tests elsewhere in this crate.
    #[cfg(test)]
    pub(crate) fn pinned_trusting(
        pinned: SocketAddr,
        certificate: rustls_pki_types::CertificateDer<'static>,
    ) -> Egress {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).expect("a test certificate");
        Egress::empty(roots, Some(pinned))
    }

    fn connector(&self, proxy: Option<Arc<Proxy>>, fallback: Fallback) -> Connector {
        Connector {
            proxy,
            fallback,
            stats: self.shared.stats.clone(),
            #[cfg(test)]
            pinned: self.shared.pinned,
        }
    }

    /// Puts a new configuration into effect for every copy of this egress. Connections already open finish
    /// the way they started.
    pub fn reconfigure(&self, config: &EgressConfig) -> Result<(), String> {
        let proxy = Proxy::parse(&config.proxy)?.map(Arc::new);
        let routes = Routes::of(config);
        let through = |takes: bool| if takes { proxy.clone() } else { None };
        let setup = Setup {
            pictures: build_client(self.connector(through(routes.pictures), config.fallback), &self.shared.roots),
            probe: build_client(self.connector(proxy.clone(), config.fallback), &self.shared.roots),
            proxy,
            fallback: config.fallback,
            routes,
        };
        *self.shared.setup.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(setup);
        Ok(())
    }

    fn setup(&self) -> Arc<Setup> {
        self.shared.setup.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether requests leave through a proxy.
    pub fn proxied(&self) -> bool {
        self.setup().proxy.is_some()
    }

    /// How requests for `purpose` leave right now: through the proxy when one is set and this kind of request
    /// takes it, otherwise straight from the server.
    pub fn dialer(&self, purpose: Purpose) -> Dialer {
        let setup = self.setup();
        let proxy = setup.proxy.clone().filter(|_| setup.routes.takes(purpose));
        Dialer { connector: self.connector(proxy, setup.fallback) }
    }

    pub fn status(&self) -> EgressStatus {
        let stats = &self.shared.stats;
        let setup = self.setup();
        EgressStatus {
            proxy: setup.proxy.as_ref().map(|proxy| proxy.shown()),
            fallback: setup.fallback,
            fetched: stats.fetched.load(Ordering::Relaxed),
            failed: stats.failed.load(Ordering::Relaxed),
            proxy_failures: stats.proxy_failures.load(Ordering::Relaxed),
            fallbacks: stats.fallbacks.load(Ordering::Relaxed),
            last_proxy_failure: stats.last_proxy_failure.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            routes: setup.routes,
        }
    }

    /// GETs `url`, following a few redirects, and gives up past `max_bytes`. No cookies, no referrer, and an
    /// agent string that says nothing about this server.
    pub async fn get(&self, url: &str, accept: &str, max_bytes: usize) -> Result<Fetched, EgressError> {
        let client = self.setup().pictures.clone();
        let result = self.fetch(&client, url, accept, max_bytes).await;
        let counter = if result.is_ok() { &self.shared.stats.fetched } else { &self.shared.stats.failed };
        counter.fetch_add(1, Ordering::Relaxed);
        result
    }

    /// The address the other side sees through the proxy (or of the server, without one), asked of a public
    /// service.
    pub async fn public_address(&self) -> Result<IpAddr, EgressError> {
        self.public_address_from(ADDRESS_ECHO).await
    }

    async fn public_address_from(&self, echo: &str) -> Result<IpAddr, EgressError> {
        let client = self.setup().probe.clone();
        let answer = self.fetch(&client, echo, "text/plain", 256).await?;
        std::str::from_utf8(&answer.body).ok().and_then(|text| text.trim().parse().ok()).ok_or(EgressError::Garbled)
    }

    async fn fetch(
        &self,
        client: &HttpClient,
        url: &str,
        accept: &str,
        max_bytes: usize,
    ) -> Result<Fetched, EgressError> {
        let _permit = self.shared.permits.acquire().await.map_err(|_| EgressError::Unreachable)?;
        tokio::time::timeout(TIMEOUT, follow(client, url, accept, max_bytes)).await.map_err(|_| EgressError::Timeout)?
    }
}

fn build_client(connector: Connector, roots: &rustls::RootCertStore) -> HttpClient {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the default TLS versions")
        .with_root_certificates(roots.clone())
        .with_no_client_auth();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .wrap_connector(connector);
    Client::builder(TokioExecutor::new()).build(https)
}

async fn follow(client: &HttpClient, url: &str, accept: &str, max_bytes: usize) -> Result<Fetched, EgressError> {
    let mut current = check_url(url, true).map_err(EgressError::NotAllowed)?;
    for _ in 0..=MAX_REDIRECTS {
        let request = Request::get(current.as_str())
            .header(USER_AGENT, AGENT)
            .header(ACCEPT, accept)
            .body(Empty::new())
            .map_err(|_| EgressError::NotAllowed("that is not a web address".into()))?;
        let response = client.request(request).await.map_err(|err| reason(&err))?;
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
        return Ok(Fetched { media_type, body, url: current });
    }
    Err(EgressError::Redirects)
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
        let egress = Egress::empty(rustls::RootCertStore::empty(), Some(pinned));
        egress.reconfigure(&EgressConfig { proxy: proxy.into(), fallback, ..EgressConfig::default() }).unwrap();
        egress
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
                        "/ip" => "HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n203.0.113.7\n".to_owned(),
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
        let blocking = egress(&format!("http://me:secret@{gone}"), Fallback::Block, server);
        let blocked = blocking.get("http://pictures.example/pixel.gif", "image/*", 1024).await;
        assert_eq!(blocked.unwrap_err(), EgressError::Unreachable);
        assert!(seen.lock().unwrap().is_empty(), "nothing reached the sender");
        let status = blocking.status();
        assert_eq!(status.proxy, Some(format!("http://{gone}")), "shown without its login");
        assert_eq!((status.fetched, status.failed, status.fallbacks), (0, 1, 0));
        assert!(status.proxy_failures >= 1 && status.last_proxy_failure.is_some(), "{status:?}");

        let direct = egress(&format!("socks5://{gone}"), Fallback::Direct, server);
        let picture = direct.get("http://pictures.example/pixel.gif", "image/*", 1024).await.unwrap();
        assert_eq!(&picture.body[..], b"GIF89a");
        let status = direct.status();
        assert_eq!((status.fetched, status.failed, status.fallbacks), (1, 0, 1));
    }

    #[tokio::test]
    async fn the_address_senders_see_is_asked_the_way_pictures_go() {
        let (server, _) = pictures().await;
        let (proxy, asked) = http_proxy().await;
        let egress = egress(&format!("http://{proxy}"), Fallback::Block, server);
        let address = egress.public_address_from("http://echo.example/ip").await.unwrap();
        assert_eq!(address, "203.0.113.7".parse::<IpAddr>().unwrap());
        assert_eq!(asked.lock().unwrap().len(), 1, "through the proxy");
        assert_eq!(egress.status().fetched, 0, "not a picture");
        let garbled = egress.public_address_from("http://echo.example/pixel.gif").await;
        assert_eq!(garbled.unwrap_err(), EgressError::Garbled);
    }

    #[tokio::test]
    async fn a_new_configuration_reaches_every_copy_at_once() {
        let (server, _) = pictures().await;
        let (proxy, asked) = http_proxy().await;
        let egress = egress("", Fallback::Block, server);
        let copy = egress.clone();
        copy.get("http://pictures.example/pixel.gif", "image/*", 1024).await.unwrap();
        assert!(asked.lock().unwrap().is_empty(), "straight out without a proxy");

        let config = EgressConfig { proxy: format!("http://{proxy}"), ..EgressConfig::default() };
        egress.reconfigure(&config).unwrap();
        assert!(copy.proxied());
        copy.get("http://pictures.example/pixel.gif", "image/*", 1024).await.unwrap();
        assert_eq!(asked.lock().unwrap().len(), 1, "the copy took the new proxy");

        assert!(egress.reconfigure(&EgressConfig { proxy: "ftp://x".into(), ..EgressConfig::default() }).is_err());
        assert!(copy.proxied(), "a configuration that does not parse changes nothing");
    }

    #[tokio::test]
    async fn each_kind_of_request_takes_the_proxy_only_when_told() {
        let (server, _) = pictures().await;
        let (proxy, asked) = http_proxy().await;
        let egress = egress("", Fallback::Block, server);
        let config = EgressConfig {
            proxy: format!("http://{proxy}"),
            pictures: false,
            updates: false,
            fetch: true,
            ..EgressConfig::default()
        };
        egress.reconfigure(&config).unwrap();
        assert_eq!(egress.status().routes, Routes { pictures: false, updates: false, fetch: true });
        egress.get("http://pictures.example/pixel.gif", "image/*", 1024).await.unwrap();
        assert!(asked.lock().unwrap().is_empty(), "pictures leave directly now");
        assert!(!egress.dialer(Purpose::Updates).proxied());

        let dialer = egress.dialer(Purpose::Fetch);
        assert!(dialer.proxied());
        let mut stream = dialer.connect("imap.example", 993).await.unwrap();
        stream.write_all(b"GET /pixel.gif HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        assert_eq!(asked.lock().unwrap().len(), 1, "fetching took the proxy");
        assert!(asked.lock().unwrap()[0].starts_with(&format!("CONNECT {server} ")));
    }

    #[tokio::test]
    async fn a_dialer_never_reaches_into_the_network() {
        let egress = Egress::direct();
        let err = egress.dialer(Purpose::Fetch).connect("127.0.0.1", 993).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
