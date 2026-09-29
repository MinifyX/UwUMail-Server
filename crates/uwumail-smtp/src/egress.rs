//! The way out for requests that tell a sender something about the people reading their mail: the remote
//! pictures in a message, and the logos shown next to it. Whoever serves such a picture learns when, from
//! where and how often it was looked at.
//!
//! Fetching them here instead of in the reader's browser already hides the reader behind the server. With a
//! proxy set, they leave through it too, so the sender sees the address of a VPN rather than the server's:
//! gluetun's HTTP proxy (OpenVPN, WireGuard, NordVPN and others), or a SOCKS5 proxy a VPN provider offers.
//!
//! One-click unsubscriptions (RFC 8058) take the same way as pictures: the POST tells a newsletter that
//! someone opened the message and wants out, and from where.
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
use http_body_util::{BodyExt, Empty, Full, Limited};
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
/// Connecting for a remote picture in a message, per address: a tracking pixel on a dead host must not
/// keep the rest of a message waiting (docs/jmap-remote.md). Through a proxy this covers the tunnel too.
const PICTURE_CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
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
    /// Remote pictures, sender logos and one-click unsubscriptions take the proxy. On unless switched off.
    pub pictures: bool,
    /// The check for new UwUMail versions takes the proxy.
    pub updates: bool,
    /// Fetching mail from mailboxes at other providers takes the proxy.
    pub fetch: bool,
    /// The most the cache of remote pictures in messages keeps on disk, in megabytes; 0 keeps none.
    pub image_cache_mb: u64,
    /// Requests to AI providers on the internet take the proxy (docs/llm.md). Off unless switched on:
    /// the provider knows who the key belongs to anyway. Providers in the local network never do.
    pub assist: bool,
}

impl Default for EgressConfig {
    fn default() -> Self {
        EgressConfig {
            proxy: String::new(),
            fallback: Fallback::Block,
            pictures: true,
            updates: false,
            fetch: false,
            image_cache_mb: 1024,
            assist: false,
        }
    }
}

/// What a request is for, which decides whether it takes the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Pictures,
    Updates,
    Fetch,
    Assist,
}

/// Which addresses a request may reach. Everything but the AI assistant reaches public addresses only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    /// Public addresses only.
    #[default]
    Public,
    /// Public addresses and the local network: private IPv4 (RFC 1918), shared address space (RFC 6598,
    /// e.g. Tailscale) and IPv6 unique local addresses. Never the machine itself, link-local addresses
    /// (cloud metadata services) or anything unroutable. For a provider a person set up while the admin
    /// allows it.
    Lan,
    /// Anywhere, the machine itself included: for a provider the admin set up.
    Any,
}

impl Reach {
    /// Whether `ip` may be connected to.
    pub fn allows(self, ip: IpAddr) -> bool {
        match self {
            Reach::Public => is_public(ip),
            Reach::Lan => is_public(ip) || is_local_network(ip),
            Reach::Any => !ip.is_unspecified() && !ip.is_multicast(),
        }
    }
}

/// An address of the local network, as [`Reach::Lan`] means it.
pub fn is_local_network(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || (a == 100 && (b & 0xc0) == 64)
        }
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
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

    /// A connection to `target` through the proxy, or why not: the proxy itself could not be reached
    /// (its name, its port, its login), or it could not open this one tunnel.
    async fn open(&self, target: SocketAddr, limit: Duration) -> Result<TcpStream, (std::io::Error, Fault)> {
        let address = match self {
            Proxy::Http { address, .. } | Proxy::Socks5 { address, .. } => address,
        };
        let mut stream = timed(limit, TcpStream::connect(address.as_str())).await.map_err(|err| (err, Fault::Proxy))?;
        let tunnel = match self {
            Proxy::Http { auth, .. } => timed(limit, http_connect(&mut stream, target, auth.as_deref())).await,
            Proxy::Socks5 { auth, .. } => timed(limit, socks5_connect(&mut stream, target, auth.as_ref())).await,
        };
        match tunnel {
            Ok(()) => Ok(stream),
            Err(err) if err.get_ref().is_some_and(|inner| inner.is::<LoginRefused>()) => Err((err, Fault::Proxy)),
            Err(err) => Err((err, Fault::Tunnel)),
        }
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

async fn timed<T>(limit: Duration, step: impl Future<Output = std::io::Result<T>>) -> std::io::Result<T> {
    tokio::time::timeout(limit, step)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "the proxy did not answer in time"))?
}

fn refused(what: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionRefused, what.into())
}

/// The proxy wants another login: nothing goes through it until the admin changes it.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct LoginRefused(&'static str);

fn login_refused(what: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionRefused, LoginRefused(what))
}

/// Whose fault it was that no connection came through the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The proxy itself: its name does not resolve, nothing listens, or it wants another login.
    Proxy,
    /// Only this tunnel: the proxy answered but could not reach the address, which may simply be down.
    Tunnel,
}

/// After the proxy failed, requests leave without trying it (or stay away, with `fallback = "block"`)
/// for this long; then one request tries it again.
const PROXY_REST: Duration = Duration::from_secs(30);
/// Tunnels that fail in a row, with nothing coming through in between, before the proxy is taken for
/// broken although it answers: a VPN whose tunnel is down lets its proxy refuse every address.
const FAILED_TUNNELS: u32 = 8;
/// ... and the different hosts they were for, so a few dead tracking hosts alone never trip it.
const FAILED_TUNNEL_HOSTS: usize = 3;
/// A request that tries the proxy again and never reports back (it was dropped) is not waited for longer.
const TRIAL_PATIENCE: Duration = Duration::from_secs(20);

/// Remembers that the proxy is down, so each picture of a message does not wait for it on its own and
/// the log says so once instead of once per picture (a circuit breaker). Shared by every request of one
/// configuration.
#[derive(Default)]
struct Breaker {
    state: Mutex<BreakerState>,
}

#[derive(Default)]
struct BreakerState {
    /// Resting until then; none while the proxy is in use.
    resting_until: Option<tokio::time::Instant>,
    /// Since when one request tries the proxy again after its rest.
    trial_since: Option<tokio::time::Instant>,
    /// Tunnels that failed since the last one that came through, and the hosts they were for.
    failed_tunnels: u32,
    failed_hosts: Vec<String>,
    /// The last address a tunnel reached: asked again before refused tunnels count as the proxy's
    /// fault, so dead hosts someone asks for do not take the proxy out of use for everyone.
    last_good: Option<SocketAddr>,
}

/// What [`Breaker::admits`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// The proxy is in use.
    Yes,
    /// The proxy rested and this request tries it again; its outcome alone decides.
    Trial,
    /// The proxy rests.
    No,
}

impl Breaker {
    /// Whether a request may try the proxy now.
    fn admits(&self) -> Admission {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = tokio::time::Instant::now();
        match state.resting_until {
            None => Admission::Yes,
            Some(until) if now < until => Admission::No,
            Some(_) => {
                if state.trial_since.is_some_and(|since| now.duration_since(since) < TRIAL_PATIENCE) {
                    return Admission::No;
                }
                state.trial_since = Some(now);
                Admission::Trial
            }
        }
    }

    /// A tunnel came through to `target`. Answers whether the proxy had been resting.
    fn succeeded(&self, target: SocketAddr) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.failed_tunnels = 0;
        state.failed_hosts.clear();
        state.trial_since = None;
        state.last_good = Some(target);
        state.resting_until.take().is_some()
    }

    /// Where to look whether the proxy still reaches anything, when a refused tunnel for `host` would
    /// make it rest: the last address it reached.
    fn check_before_rest(&self, fault: Fault, host: &str, trial: bool) -> Option<SocketAddr> {
        if fault != Fault::Tunnel {
            return None;
        }
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let hosts = state.failed_hosts.len() + usize::from(!state.failed_hosts.iter().any(|seen| seen == host));
        let trips = trial || (state.failed_tunnels + 1 >= FAILED_TUNNELS && hosts >= FAILED_TUNNEL_HOSTS);
        if trips { state.last_good } else { None }
    }

    /// Nothing came through for `host`. `trial`: this was the request that tried the proxy after its
    /// rest; the failures of others that were still under way do not end the rest's trial. Answers
    /// whether the proxy starts resting because of this (and was not resting before), which is logged
    /// once.
    fn failed(&self, fault: Fault, host: &str, trial: bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let was_resting = state.resting_until.is_some();
        let trial = trial && state.trial_since.take().is_some();
        let broken = match fault {
            Fault::Proxy => true,
            Fault::Tunnel => {
                state.failed_tunnels += 1;
                if !state.failed_hosts.iter().any(|seen| seen == host) && state.failed_hosts.len() < 16 {
                    state.failed_hosts.push(host.to_owned());
                }
                trial || (state.failed_tunnels >= FAILED_TUNNELS && state.failed_hosts.len() >= FAILED_TUNNEL_HOSTS)
            }
        };
        if broken {
            state.resting_until = Some(tokio::time::Instant::now() + PROXY_REST);
            state.failed_tunnels = 0;
            state.failed_hosts.clear();
        }
        broken && !was_resting
    }

    fn resting(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.resting_until.is_some_and(|until| tokio::time::Instant::now() < until)
    }
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
        if status == b"407" {
            return Err(login_refused("the proxy turned the login down (407)"));
        }
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
        return Err(login_refused("the SOCKS5 proxy wants a different login"));
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
            return Err(login_refused("the SOCKS5 proxy turned the login down"));
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
    /// The proxy failed a moment ago and is not tried for a few seconds.
    pub proxy_resting: bool,
    /// Which kinds of request take the proxy while one is set.
    pub routes: Routes,
}

/// Which kinds of request take the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Routes {
    pub pictures: bool,
    pub updates: bool,
    pub fetch: bool,
    pub assist: bool,
}

impl Routes {
    fn of(config: &EgressConfig) -> Routes {
        Routes { pictures: config.pictures, updates: config.updates, fetch: config.fetch, assist: config.assist }
    }

    fn takes(&self, purpose: Purpose) -> bool {
        match purpose {
            Purpose::Pictures => self.pictures,
            Purpose::Updates => self.updates,
            Purpose::Fetch => self.fetch,
            Purpose::Assist => self.assist,
        }
    }
}

#[derive(Clone)]
struct Connector {
    proxy: Option<Arc<Proxy>>,
    fallback: Fallback,
    /// Which addresses it connects to. Plain `http://` is only allowed to addresses that are not public
    /// when this is more than [`Reach::Public`]: a key must not cross the internet unencrypted.
    reach: Reach,
    stats: Arc<Stats>,
    /// For each address, and for each step with the proxy.
    connect_timeout: Duration,
    /// Whether the proxy is down; shared by every connector of one configuration.
    breaker: Arc<Breaker>,
    /// Tries the proxy even while it rests: the admin panel's test of the way out.
    ignores_breaker: bool,
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
        let mut public = candidates(found, self.proxy.is_some(), self.reach);
        if self.reach != Reach::Public && uri.scheme_str() == Some("http") {
            public.retain(|address| !is_public(address.ip()));
        }
        if public.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "not a public address"));
        }
        Ok(public)
    }

    /// A connection to one of the addresses of `uri`: through the proxy when there is one and it is not
    /// resting, otherwise straight or not at all, as `fallback` says.
    async fn open(&self, uri: &Uri) -> std::io::Result<TcpStream> {
        let targets = self.addresses(uri).await?;
        let Some(proxy) = &self.proxy else {
            return self.direct(&targets).await;
        };
        // A proxy (a VPN) cannot reach into the local network; such addresses are only ever allowed on
        // purpose (a reach beyond public, for the AI assistant's providers), and are reached directly.
        if self.reach != Reach::Public {
            let local: Vec<SocketAddr> = targets.iter().copied().filter(|target| !is_public(target.ip())).collect();
            if !local.is_empty() {
                return self.direct(&local).await;
            }
        }
        let admission = if self.ignores_breaker { Admission::Yes } else { self.breaker.admits() };
        if admission == Admission::No {
            tracing::debug!("the egress proxy is resting after it failed");
            return self.without_proxy(&targets, None).await;
        }
        let trial = admission == Admission::Trial;
        let mut last = None;
        let mut fault = Fault::Tunnel;
        for target in &targets {
            match proxy.open(*target, self.connect_timeout).await {
                Ok(stream) => {
                    if self.breaker.succeeded(*target) {
                        tracing::info!("the egress proxy works again");
                    }
                    return Ok(stream);
                }
                Err((err, kind)) => {
                    self.stats.proxy_failures.fetch_add(1, Ordering::Relaxed);
                    *self.stats.last_proxy_failure.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(ProxyFailure { at: crate::now(), error: err.to_string() });
                    last = Some(err);
                    fault = kind;
                    if kind == Fault::Proxy {
                        // Every other address would go the same way.
                        break;
                    }
                }
            }
        }
        let err = last.unwrap_or_else(|| refused("no address to connect to"));
        let host = uri.host().unwrap_or_default();
        // Refused tunnels are the proxy's fault only when it reaches nothing any more: the last address
        // it reached is asked first (EGRESS-1 of the 0.18.0 audit).
        if let Some(good) = self.breaker.check_before_rest(fault, host, trial)
            && let Ok(check) = proxy.open(good, self.connect_timeout).await
        {
            drop(check);
            if self.breaker.succeeded(good) {
                tracing::info!("the egress proxy works again");
            }
            tracing::debug!(%err, "no tunnel to this host, but the egress proxy reaches others");
            return self.without_proxy(&targets, Some(err)).await;
        }
        if self.breaker.failed(fault, host, trial) {
            let rest = PROXY_REST.as_secs();
            match self.fallback {
                Fallback::Direct => tracing::warn!(
                    %err,
                    "the egress proxy failed; requests leave directly for the next {rest} s, as configured"
                ),
                Fallback::Block => tracing::warn!(
                    %err,
                    "the egress proxy failed; pictures and other requests that take it stay away for the next {rest} s"
                ),
            }
        } else {
            tracing::debug!(%err, ?fault, "no tunnel through the egress proxy");
        }
        self.without_proxy(&targets, Some(err)).await
    }

    /// What happens without the proxy: straight from the server when the admin allowed it.
    async fn without_proxy(&self, targets: &[SocketAddr], err: Option<std::io::Error>) -> std::io::Result<TcpStream> {
        if self.fallback == Fallback::Direct {
            self.stats.fallbacks.fetch_add(1, Ordering::Relaxed);
            return self.direct(targets).await;
        }
        Err(err.unwrap_or_else(|| refused("the egress proxy failed a moment ago; nothing leaves without it")))
    }

    async fn direct(&self, targets: &[SocketAddr]) -> std::io::Result<TcpStream> {
        let mut last = None;
        for target in targets {
            match timed(self.connect_timeout, TcpStream::connect(*target)).await {
                Ok(stream) => return Ok(stream),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| refused("no address to connect to")))
    }
}

/// The public addresses of a name, in the order they are tried. Through a proxy, IPv4 comes first: a VPN
/// container usually has no IPv6 route, and gluetun's kill switch drops such a tunnel silently, so every
/// IPv6 address the system put first would wait out [`CONNECT_TIMEOUT`] and the picture never came.
fn candidates(found: Vec<SocketAddr>, proxied: bool, reach: Reach) -> Vec<SocketAddr> {
    let mut public: Vec<SocketAddr> = found.into_iter().filter(|address| reach.allows(address.ip())).collect();
    if proxied {
        public.sort_by_key(SocketAddr::is_ipv6);
    }
    public.truncate(MAX_ADDRESSES);
    public
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
            let stream = connector.open(&uri).await?;
            let _ = stream.set_nodelay(true);
            Ok(TokioIo::new(stream))
        })
    }
}

/// What [`Egress::assist_client`] hands out.
#[derive(Clone)]
pub struct AssistClient {
    client: PostClient,
}

impl AssistClient {
    /// Sends `request` and answers the response with its body still to be read, so the caller can read
    /// it as it streams in and stop at its own limits. Connecting counts against no timeout but the
    /// connection's own; the caller bounds the whole.
    pub async fn send(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, EgressError> {
        self.client.request(request).await.map_err(|err| reason(&err))
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

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
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
/// For POSTs: https only.
type PostClient = Client<HttpsConnector<Connector>, Full<Bytes>>;

/// One configuration of the way out, replaced as a whole when the admin panel changes it.
struct Setup {
    proxy: Option<Arc<Proxy>>,
    fallback: Fallback,
    routes: Routes,
    /// For pictures: through the proxy when they take it.
    pictures: HttpClient,
    /// The same for the remote pictures in messages, which give up on a host much sooner.
    message_pictures: HttpClient,
    /// Through the proxy whenever there is one, to test it.
    probe: HttpClient,
    /// One-click unsubscriptions: the way pictures go.
    unsubscribe: PostClient,
    /// Whether the proxy is down, for every request of this configuration.
    breaker: Arc<Breaker>,
    /// The most the cache of remote pictures may hold on disk, in bytes; 0 keeps nothing.
    image_cache_bytes: u64,
}

/// How long connecting may take; shorter in tests.
#[derive(Debug, Clone, Copy)]
struct ConnectTimeouts {
    usual: Duration,
    pictures: Duration,
}

struct Shared {
    setup: RwLock<Arc<Setup>>,
    /// Web Push messages to the push services of browsers and phones.
    post: PostClient,
    permits: Semaphore,
    stats: Arc<Stats>,
    roots: rustls::RootCertStore,
    timeouts: ConnectTimeouts,
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
        self.connector.open(&uri).await
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
        let timeouts = ConnectTimeouts { usual: CONNECT_TIMEOUT, pictures: PICTURE_CONNECT_TIMEOUT };
        Egress::build(
            roots,
            timeouts,
            #[cfg(test)]
            pinned,
        )
    }

    fn build(
        roots: rustls::RootCertStore,
        timeouts: ConnectTimeouts,
        #[cfg(test)] pinned: Option<SocketAddr>,
    ) -> Egress {
        let stats: Arc<Stats> = Arc::default();
        let breaker: Arc<Breaker> = Arc::default();
        let direct = Connector {
            proxy: None,
            fallback: Fallback::Block,
            reach: Reach::Public,
            stats: stats.clone(),
            connect_timeout: timeouts.usual,
            breaker: breaker.clone(),
            ignores_breaker: false,
            #[cfg(test)]
            pinned,
        };
        let post = build_post_client(direct.clone(), &roots);
        let client = build_client(direct, &roots);
        let setup = Setup {
            proxy: None,
            fallback: Fallback::Block,
            routes: Routes::of(&EgressConfig::default()),
            pictures: client.clone(),
            message_pictures: client.clone(),
            probe: client,
            unsubscribe: post.clone(),
            breaker,
            image_cache_bytes: EgressConfig::default().image_cache_mb * 1024 * 1024,
        };
        Egress {
            shared: Arc::new(Shared {
                setup: RwLock::new(Arc::new(setup)),
                post,
                permits: Semaphore::new(MAX_CONCURRENT),
                stats,
                roots,
                timeouts,
                #[cfg(test)]
                pinned,
            }),
        }
    }

    /// Every name leads to `pinned`: for tests elsewhere in this crate. Connecting keeps the real limits,
    /// so a busy machine does not fail a test that expects the connection.
    #[cfg(test)]
    pub(crate) fn pinned_to(pinned: SocketAddr) -> Egress {
        let egress = Egress::empty(rustls::RootCertStore::empty(), Some(pinned));
        egress.reconfigure(&EgressConfig::default()).expect("no proxy to get wrong");
        egress
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

    fn connector(&self, proxy: Option<Arc<Proxy>>, fallback: Fallback, breaker: &Arc<Breaker>) -> Connector {
        Connector {
            proxy,
            fallback,
            reach: Reach::Public,
            stats: self.shared.stats.clone(),
            connect_timeout: self.shared.timeouts.usual,
            breaker: breaker.clone(),
            ignores_breaker: false,
            #[cfg(test)]
            pinned: self.shared.pinned,
        }
    }

    /// Puts a new configuration into effect for every copy of this egress. Connections already open finish
    /// the way they started.
    pub fn reconfigure(&self, config: &EgressConfig) -> Result<(), String> {
        let proxy = Proxy::parse(&config.proxy)?.map(Arc::new);
        let routes = Routes::of(config);
        let breaker: Arc<Breaker> = Arc::default();
        let through = |takes: bool| if takes { proxy.clone() } else { None };
        let pictures = self.connector(through(routes.pictures), config.fallback, &breaker);
        let setup = Setup {
            pictures: build_client(pictures.clone(), &self.shared.roots),
            message_pictures: build_message_picture_client(
                Connector { connect_timeout: self.shared.timeouts.pictures, ..pictures.clone() },
                &self.shared.roots,
            ),
            // The admin's test tries the proxy even while it rests.
            probe: build_client(
                Connector { ignores_breaker: true, ..self.connector(proxy.clone(), config.fallback, &breaker) },
                &self.shared.roots,
            ),
            unsubscribe: build_post_client(pictures, &self.shared.roots),
            proxy,
            fallback: config.fallback,
            routes,
            breaker,
            image_cache_bytes: config.image_cache_mb.saturating_mul(1024 * 1024),
        };
        *self.shared.setup.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(setup);
        Ok(())
    }

    fn setup(&self) -> Arc<Setup> {
        self.shared.setup.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The most the cache of remote pictures may hold on disk, in bytes (`egress.image_cache_mb`).
    pub fn image_cache_limit(&self) -> u64 {
        self.setup().image_cache_bytes
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
        Dialer { connector: self.connector(proxy, setup.fallback, &setup.breaker) }
    }

    /// Straight from the server, whatever the proxy says: for requests between servers that name this one
    /// anyway, like the TLS reports it posts. Still only to public addresses.
    pub fn direct_dialer(&self) -> Dialer {
        Dialer { connector: self.connector(None, Fallback::Block, &Arc::default()) }
    }

    /// An HTTP client for AI providers (docs/llm.md): https to public addresses, through the proxy when
    /// `[egress] assist` says so, and with `reach` beyond that also into the local network or anywhere
    /// (plain http then only to addresses that are not public). Names are resolved and checked on every
    /// connection, so a name that later points elsewhere does not get around `reach`. Redirects are the
    /// caller's to refuse.
    pub fn assist_client(&self, reach: Reach) -> AssistClient {
        let setup = self.setup();
        let proxy = setup.proxy.clone().filter(|_| setup.routes.assist);
        let mut connector = self.connector(proxy, setup.fallback, &setup.breaker);
        connector.reach = reach;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default TLS versions")
            .with_root_certificates(self.shared.roots.clone())
            .with_no_client_auth();
        let builder = hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(tls);
        let https = if reach == Reach::Public { builder.https_only() } else { builder.https_or_http() };
        let client = Client::builder(TokioExecutor::new()).build(https.enable_http1().wrap_connector(connector));
        AssistClient { client }
    }

    /// The certificate authorities requests through this egress trust.
    pub(crate) fn roots(&self) -> rustls::RootCertStore {
        self.shared.roots.clone()
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
            proxy_resting: setup.breaker.resting(),
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

    /// POSTs `body` to a public https address and answers the status, for Web Push (docs/jmap-push.md).
    /// Straight from the server, never through the proxy: a push service learns nothing about readers, and
    /// the address is checked the same way as a picture's, name and resolved address alike. Redirects are
    /// not followed, and the answer's body is not read beyond a few kilobytes.
    pub async fn post(&self, url: &str, headers: &[(String, String)], body: Vec<u8>) -> Result<u16, EgressError> {
        let url = check_url(url, false).map_err(EgressError::NotAllowed)?;
        let mut request = Request::post(url.as_str()).header(USER_AGENT, AGENT);
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| EgressError::NotAllowed("that is not a web address".into()))?;
        self.send(&self.shared.post, request).await
    }

    /// POSTs a form to a public https address the way requests for `purpose` leave -- through the proxy
    /// when they take it, with the configured fallback -- and answers the status and the body, read up to
    /// `max_bytes`. For the token endpoints of OAuth providers (docs/fetch.md). Redirects are answered, not
    /// followed: a token endpoint never sends one, and a form with a secret in it does not go on to
    /// wherever a redirect points.
    pub async fn post_form(
        &self,
        purpose: Purpose,
        url: &str,
        form: String,
        max_bytes: usize,
    ) -> Result<(u16, Bytes), EgressError> {
        let url = check_url(url, false).map_err(EgressError::NotAllowed)?;
        let request = Request::post(url.as_str())
            .header(USER_AGENT, AGENT)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Full::new(Bytes::from(form)))
            .map_err(|_| EgressError::NotAllowed("that is not a web address".into()))?;
        // Made for each request from the way out as it is now, so a proxy the admin changes a moment
        // ago is already the one taken.
        let client = build_post_client(self.dialer(purpose).connector, &self.shared.roots);
        let _permit = self.shared.permits.acquire().await.map_err(|_| EgressError::Unreachable)?;
        let result = tokio::time::timeout(TIMEOUT, async move {
            let response = client.request(request).await.map_err(|err| reason(&err))?;
            let status = response.status().as_u16();
            let body = Limited::new(response.into_body(), max_bytes)
                .collect()
                .await
                .map_err(|_| EgressError::TooLarge)?
                .to_bytes();
            Ok((status, body))
        })
        .await
        .map_err(|_| EgressError::Timeout)?;
        let counter = if result.is_ok() { &self.shared.stats.fetched } else { &self.shared.stats.failed };
        counter.fetch_add(1, Ordering::Relaxed);
        result
    }

    /// Unsubscribes with one click (RFC 8058): POSTs `List-Unsubscribe=One-Click` as a form to a public
    /// https address and answers the status. It leaves the way pictures do, through the proxy when they
    /// take it and never around it while `fallback` is `block`. No cookies, no referrer, the same agent
    /// string as a picture. A redirect is answered, not followed: RFC 8058 forbids the sender to send one,
    /// and a POST that follows it may arrive somewhere as a GET.
    pub async fn unsubscribe(&self, url: &str) -> Result<u16, EgressError> {
        let url = check_url(url, false).map_err(EgressError::NotAllowed)?;
        let request = Request::post(url.as_str())
            .header(USER_AGENT, AGENT)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Full::new(Bytes::from_static(b"List-Unsubscribe=One-Click")))
            .map_err(|_| EgressError::NotAllowed("that is not a web address".into()))?;
        let client = self.setup().unsubscribe.clone();
        self.send(&client, request).await
    }

    /// Sends a POST within [`TIMEOUT`] and answers the status; the body of the answer is not read beyond a
    /// few kilobytes.
    async fn send(&self, client: &PostClient, request: Request<Full<Bytes>>) -> Result<u16, EgressError> {
        let _permit = self.shared.permits.acquire().await.map_err(|_| EgressError::Unreachable)?;
        let client = client.clone();
        tokio::time::timeout(TIMEOUT, async move {
            let response = client.request(request).await.map_err(|err| reason(&err))?;
            let status = response.status().as_u16();
            // Read what little there is, so the connection can be used again.
            let _ = Limited::new(response.into_body(), 16 * 1024).collect().await;
            Ok(status)
        })
        .await
        .map_err(|_| EgressError::Timeout)?
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
        let patience = Patience { answer: TIMEOUT, stall: None };
        tokio::time::timeout(TIMEOUT, follow(client, url, accept, max_bytes, patience, &mut |_| {}))
            .await
            .map_err(|_| EgressError::Timeout)?
    }

    /// GETs a remote picture of a message like [`Egress::get`], but within `limits`, which give up on a
    /// dead host within seconds, and without waiting for the permits every other request shares: the
    /// caller keeps these fair itself (`remote_images`). `progress` sees everything that came so far
    /// after each piece of the body, so a picture's size can be told before all of it is there.
    pub async fn get_message_picture(
        &self,
        url: &str,
        accept: &str,
        max_bytes: usize,
        limits: PictureLimits,
        progress: &mut (dyn FnMut(&[u8]) + Send),
    ) -> Result<Fetched, EgressError> {
        let client = self.setup().message_pictures.clone();
        let patience = Patience { answer: limits.answer, stall: Some(limits.stall) };
        let result = tokio::time::timeout(limits.total, follow(&client, url, accept, max_bytes, patience, progress))
            .await
            .map_err(|_| EgressError::Timeout)
            .and_then(|result| result);
        let counter = if result.is_ok() { &self.shared.stats.fetched } else { &self.shared.stats.failed };
        counter.fetch_add(1, Ordering::Relaxed);
        result
    }
}

/// How long a remote picture in a message may take (docs/jmap-remote.md). Connecting is limited to
/// four seconds per address as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureLimits {
    /// From the first connection to the answer's headers, redirects included.
    pub answer: Duration,
    /// The longest pause between two pieces of the body.
    pub stall: Duration,
    /// Everything together; a big picture on a slow line still makes it as long as bytes keep coming.
    pub total: Duration,
}

impl Default for PictureLimits {
    fn default() -> Self {
        PictureLimits { answer: Duration::from_secs(6), stall: Duration::from_secs(5), total: Duration::from_secs(20) }
    }
}

/// Limits of one GET besides its size.
#[derive(Debug, Clone, Copy)]
struct Patience {
    /// Until the headers of the last answer.
    answer: Duration,
    /// Between two pieces of the body.
    stall: Option<Duration>,
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

/// Like [`build_client`], but speaks HTTP/2 where the other side does, so all pictures of a message from
/// one host share a connection (and a single tunnel through the proxy).
fn build_message_picture_client(connector: Connector, roots: &rustls::RootCertStore) -> HttpClient {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the default TLS versions")
        .with_root_certificates(roots.clone())
        .with_no_client_auth();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_all_versions()
        .wrap_connector(connector);
    Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(60)).build(https)
}

fn build_post_client(connector: Connector, roots: &rustls::RootCertStore) -> PostClient {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the default TLS versions")
        .with_root_certificates(roots.clone())
        .with_no_client_auth();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_only()
        .enable_http1()
        .wrap_connector(connector);
    Client::builder(TokioExecutor::new()).build(https)
}

async fn follow(
    client: &HttpClient,
    url: &str,
    accept: &str,
    max_bytes: usize,
    patience: Patience,
    progress: &mut (dyn FnMut(&[u8]) + Send),
) -> Result<Fetched, EgressError> {
    let answer_by = tokio::time::Instant::now() + patience.answer;
    let mut current = check_url(url, true).map_err(EgressError::NotAllowed)?;
    for _ in 0..=MAX_REDIRECTS {
        let request = Request::get(current.as_str())
            .header(USER_AGENT, AGENT)
            .header(ACCEPT, accept)
            .body(Empty::new())
            .map_err(|_| EgressError::NotAllowed("that is not a web address".into()))?;
        let response = tokio::time::timeout_at(answer_by, client.request(request))
            .await
            .map_err(|_| EgressError::Timeout)?
            .map_err(|err| reason(&err))?;
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
        let announced = response
            .headers()
            .get(hyper::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if announced.is_some_and(|length| length > max_bytes as u64) {
            return Err(EgressError::TooLarge);
        }
        let mut body = response.into_body();
        let mut collected = Vec::with_capacity(announced.map_or(0, |length| length as usize));
        loop {
            let next = match patience.stall {
                Some(stall) => tokio::time::timeout(stall, body.frame()).await.map_err(|_| EgressError::Timeout)?,
                None => body.frame().await,
            };
            let Some(frame) = next else { break };
            let frame = frame.map_err(|_| EgressError::Unreachable)?;
            let Ok(data) = frame.into_data() else { continue };
            if collected.len() + data.len() > max_bytes {
                return Err(EgressError::TooLarge);
            }
            collected.extend_from_slice(&data);
            progress(&collected);
        }
        return Ok(Fetched { media_type, body: Bytes::from(collected), url: current });
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

    #[test]
    fn reach_says_where_the_assistant_may_connect() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        for public in ["8.8.8.8", "2a00:1450::1"] {
            assert!(Reach::Public.allows(ip(public)) && Reach::Lan.allows(ip(public)) && Reach::Any.allows(ip(public)));
        }
        for lan in ["192.168.1.20", "10.0.0.5", "172.16.3.4", "100.64.1.1", "fd12:3456::1"] {
            assert!(!Reach::Public.allows(ip(lan)), "{lan}");
            assert!(Reach::Lan.allows(ip(lan)), "{lan}");
            assert!(Reach::Any.allows(ip(lan)), "{lan}");
        }
        // The machine itself and cloud metadata stay out of reach of what people set up.
        for inside in ["127.0.0.1", "::1", "169.254.169.254", "fe80::1", "0.0.0.0", "::ffff:127.0.0.1"] {
            assert!(!Reach::Lan.allows(ip(inside)), "{inside}");
        }
        assert!(Reach::Any.allows(ip("127.0.0.1")) && Reach::Any.allows(ip("::1")));
        assert!(!Reach::Any.allows(ip("0.0.0.0")) && !Reach::Any.allows(ip("224.0.0.1")));
    }

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

    #[test]
    fn through_a_proxy_ipv4_is_tried_first() {
        let address = |text: &str| text.parse::<SocketAddr>().unwrap();
        // How a CDN answers on a machine with IPv6: more AAAA than the three addresses that are tried.
        let found = vec![
            address("[2600:9000:1::1]:443"),
            address("[2600:9000:2::1]:443"),
            address("[2600:9000:3::1]:443"),
            address("10.0.0.1:443"),
            address("93.184.215.14:443"),
            address("93.184.215.15:443"),
        ];
        assert_eq!(
            candidates(found.clone(), true, Reach::Public),
            [address("93.184.215.14:443"), address("93.184.215.15:443"), address("[2600:9000:1::1]:443")],
            "the VPN reaches IPv4; IPv6 only once that failed"
        );
        assert_eq!(
            candidates(found, false, Reach::Public),
            [address("[2600:9000:1::1]:443"), address("[2600:9000:2::1]:443"), address("[2600:9000:3::1]:443")]
        );
        assert!(candidates(vec![address("10.0.0.1:443")], true, Reach::Public).is_empty(), "never into the network");
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

    /// The AI assistant's providers in the local network are reached directly, even with `egress.assist`
    /// on and the proxy resting: a VPN could not reach them anyway.
    #[tokio::test]
    async fn local_ai_providers_are_reached_past_the_proxy() {
        let (server, _) = pictures().await;
        let (proxy, asked) = http_proxy().await;
        let egress = egress("", Fallback::Block, server);
        let config = EgressConfig { proxy: format!("http://{proxy}"), assist: true, ..EgressConfig::default() };
        egress.reconfigure(&config).unwrap();
        let request = || Request::get("http://ollama.test/pixel.gif").body(Full::new(Bytes::new())).unwrap();
        let response = egress.assist_client(Reach::Lan).send(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(asked.lock().unwrap().is_empty(), "the proxy was not asked");
        // Pictures still take it.
        egress.get("http://pictures.example/pixel.gif", "image/*", 1024).await.unwrap();
        assert_eq!(asked.lock().unwrap().len(), 1);
        // For the internet it stays https only.
        assert!(egress.assist_client(Reach::Public).send(request()).await.is_err());
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
        assert_eq!(egress.status().routes, Routes { pictures: false, updates: false, fetch: true, assist: false });
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

    /// A push service over TLS that answers 201 and remembers what it got.
    async fn push_service() -> (Egress, Arc<Mutex<Vec<String>>>) {
        tls_site(|_| "HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n").await
    }

    /// A website over TLS that answers each path with `answer` and remembers the requests it got, bodies
    /// included.
    async fn tls_site(answer: fn(&str) -> &'static str) -> (Egress, Arc<Mutex<Vec<String>>>) {
        let generated = rcgen::generate_simple_self_signed(vec!["push.example".into(), "news.example".into()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let (acceptor, log) = (acceptor.clone(), log.clone());
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(socket).await else { return };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() == 1 {
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8(head).unwrap();
                    let length: usize = head
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(str::to_owned))
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0u8; length];
                    stream.read_exact(&mut body).await.unwrap();
                    let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                    log.lock().unwrap().push(format!("{head}{}", String::from_utf8_lossy(&body)));
                    stream.write_all(answer(&path).as_bytes()).await.unwrap();
                    let _ = stream.shutdown().await;
                });
            }
        });
        (Egress::pinned_trusting(address, generated.cert.der().clone()), seen)
    }

    /// A newsletter's unsubscribe page.
    async fn newsletter() -> (Egress, Arc<Mutex<Vec<String>>>) {
        tls_site(|path| match path {
            "/moved" => "HTTP/1.1 302 Found\r\nLocation: /u/abc\r\nContent-Length: 0\r\n\r\n",
            "/broken" => "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
            _ => "HTTP/1.1 200 OK\r\nSet-Cookie: seen=1\r\nContent-Length: 4\r\n\r\nbye!",
        })
        .await
    }

    #[tokio::test]
    async fn one_click_unsubscribing_posts_the_form_and_nothing_about_the_reader() {
        let (egress, seen) = newsletter().await;
        let status = egress.unsubscribe("https://news.example/u/abc?token=secret").await.unwrap();
        assert_eq!(status, 200);
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("POST /u/abc?token=secret HTTP/1.1\r\n"), "{request}");
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("content-type: application/x-www-form-urlencoded\r\n"), "{request}");
        assert!(lower.contains("user-agent: mozilla/5.0\r\n"), "{request}");
        assert!(!lower.contains("cookie") && !lower.contains("referer") && !lower.contains("uwumail"), "{request}");
        assert!(request.ends_with("\r\n\r\nList-Unsubscribe=One-Click"), "{request}");

        // RFC 8058 forbids the redirect; it is answered, not followed.
        assert_eq!(egress.unsubscribe("https://news.example/moved").await.unwrap(), 302);
        assert_eq!(egress.unsubscribe("https://news.example/broken").await.unwrap(), 500);
        assert_eq!(seen.lock().unwrap().len(), 3, "one request each");

        for refused in ["http://news.example/u", "https://127.0.0.1/u", "https://192.0.2.1/u", "https://localhost/u"] {
            let err = egress.unsubscribe(refused).await.unwrap_err();
            assert!(matches!(err, EgressError::NotAllowed(_)), "{refused}: {err:?}");
        }
        assert_eq!(egress.status().fetched + egress.status().failed, 0, "not counted as pictures");
    }

    #[tokio::test]
    async fn one_click_unsubscribing_takes_the_way_pictures_take() {
        let (egress, seen) = newsletter().await;
        let (proxy, asked) = http_proxy().await;
        egress.reconfigure(&EgressConfig { proxy: format!("http://{proxy}"), ..EgressConfig::default() }).unwrap();
        assert_eq!(egress.unsubscribe("https://news.example/u/abc").await.unwrap(), 200);
        assert_eq!(asked.lock().unwrap().len(), 1, "through the proxy");

        // The proxy is away and fallback is block: nothing reaches the newsletter.
        let gone = closed_port().await;
        egress.reconfigure(&EgressConfig { proxy: format!("http://{gone}"), ..EgressConfig::default() }).unwrap();
        assert_eq!(egress.unsubscribe("https://news.example/u/abc").await.unwrap_err(), EgressError::Unreachable);
        assert_eq!(seen.lock().unwrap().len(), 1);

        // Pictures leave directly, and so does this.
        let direct = EgressConfig { proxy: format!("http://{gone}"), pictures: false, ..EgressConfig::default() };
        egress.reconfigure(&direct).unwrap();
        assert_eq!(egress.unsubscribe("https://news.example/u/abc").await.unwrap(), 200);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn posts_go_to_https_addresses_only() {
        let (egress, seen) = push_service().await;
        let headers = vec![("ttl".to_owned(), "60".to_owned())];
        let status = egress.post("https://push.example/send/abc", &headers, b"{}".to_vec()).await.unwrap();
        assert_eq!(status, 201);
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("POST /send/abc HTTP/1.1"), "{request}");
        assert!(request.to_ascii_lowercase().contains("ttl: 60"), "{request}");
        assert!(request.ends_with("{}"), "{request}");

        for refused in ["http://push.example/send", "https://127.0.0.1/send", "https://localhost/send"] {
            let err = egress.post(refused, &headers, Vec::new()).await.unwrap_err();
            assert!(matches!(err, EgressError::NotAllowed(_)), "{refused}: {err:?}");
        }
    }

    #[tokio::test]
    async fn a_dialer_never_reaches_into_the_network() {
        let egress = Egress::direct();
        let err = egress.dialer(Purpose::Fetch).connect("127.0.0.1", 993).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// Gives up connecting after 300 ms: only for tests that expect a failure either way, where the limit
    /// keeps a hanging proxy or a slow resolver short. The others connect with the real limits and fail
    /// by refused ports and refused tunnels, which come at once, so a busy machine can not turn a slow
    /// connect into a failure they do not expect.
    fn hasty_egress(proxy: &str, fallback: Fallback, pinned: SocketAddr) -> Egress {
        let quick = Duration::from_millis(300);
        let egress = Egress::build(
            rustls::RootCertStore::empty(),
            ConnectTimeouts { usual: quick, pictures: quick },
            Some(pinned),
        );
        egress.reconfigure(&EgressConfig { proxy: proxy.into(), fallback, ..EgressConfig::default() }).unwrap();
        egress
    }

    /// A CONNECT proxy that answers every tunnel with `answer`, or never answers at all; counts the tunnels
    /// it was asked for.
    async fn broken_proxy(answer: Option<&'static str>) -> (SocketAddr, Arc<AtomicU64>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let asked = Arc::new(AtomicU64::new(0));
        let count = asked.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let count = count.clone();
                tokio::spawn(async move {
                    read_head(&mut stream).await;
                    count.fetch_add(1, Ordering::Relaxed);
                    match answer {
                        Some(answer) => {
                            let _ = stream.write_all(answer.as_bytes()).await;
                        }
                        // Holds the connection open without a word, like a VPN container whose tunnel hangs.
                        None => {
                            let _ = stream.read(&mut [0u8; 1]).await;
                        }
                    }
                });
            }
        });
        (address, asked)
    }

    #[tokio::test]
    async fn a_proxy_that_is_gone_is_tried_once_and_then_rests() {
        let (server, seen) = pictures().await;
        let gone = closed_port().await;
        let blocking = egress(&format!("http://{gone}"), Fallback::Block, server);
        let url = "http://pictures.example/pixel.gif";
        assert_eq!(blocking.get(url, "image/*", 1024).await.unwrap_err(), EgressError::Unreachable);
        let status = blocking.status();
        assert!(status.proxy_resting, "the proxy rests after it failed");
        assert_eq!(status.proxy_failures, 1);
        for _ in 0..5 {
            assert_eq!(blocking.get(url, "image/*", 1024).await.unwrap_err(), EgressError::Unreachable);
        }
        assert_eq!(blocking.status().proxy_failures, 1, "not tried again while it rests");
        assert!(seen.lock().unwrap().is_empty(), "and nothing went around it");

        let direct = egress(&format!("http://{gone}"), Fallback::Direct, server);
        for _ in 0..4 {
            assert_eq!(&direct.get(url, "image/*", 1024).await.unwrap().body[..], b"GIF89a");
        }
        let status = direct.status();
        assert_eq!((status.proxy_failures, status.fallbacks), (1, 4), "tried once, then straight away");
    }

    #[tokio::test]
    async fn a_proxy_whose_name_does_not_resolve_fails_at_once() {
        let (server, _) = pictures().await;
        let egress = hasty_egress("http://proxy.invalid:8888", Fallback::Block, server);
        let started = tokio::time::Instant::now();
        for _ in 0..10 {
            assert!(egress.get("http://pictures.example/pixel.gif", "image/*", 1024).await.is_err());
        }
        assert_eq!(egress.status().proxy_failures, 1, "the name is looked up once, not for every picture");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_refused_tunnel_is_only_the_address_s_fault_until_many_hosts_fail() {
        let (server, _) = pictures().await;
        let (proxy, asked) = broken_proxy(Some("HTTP/1.1 503 Service Unavailable\r\n\r\n")).await;
        let egress = egress(&format!("http://{proxy}"), Fallback::Block, server);
        // One dead tracking host, asked again and again, never takes the proxy out of use.
        for _ in 0..12 {
            assert!(egress.get("http://tracker.example/pixel.gif", "image/*", 1024).await.is_err());
        }
        assert!(!egress.status().proxy_resting);
        assert_eq!(asked.load(Ordering::Relaxed), 12);
        // Every host refused, one after the other: the VPN behind the proxy is down.
        for host in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            assert!(egress.get(&format!("http://{host}.example/pixel.gif"), "image/*", 1024).await.is_err());
        }
        assert!(egress.status().proxy_resting);
        let before = asked.load(Ordering::Relaxed);
        assert!(egress.get("http://i.example/pixel.gif", "image/*", 1024).await.is_err());
        assert_eq!(asked.load(Ordering::Relaxed), before, "not asked while it rests");
    }

    #[tokio::test]
    async fn a_proxy_that_hangs_costs_a_message_picture_its_connect_limit_only() {
        let (server, _) = pictures().await;
        let (proxy, _) = broken_proxy(None).await;
        let egress = hasty_egress(&format!("http://{proxy}"), Fallback::Block, server);
        let started = tokio::time::Instant::now();
        let failed = egress
            .get_message_picture(
                "http://pictures.example/pixel.gif",
                "image/*",
                1024,
                PictureLimits::default(),
                &mut |_| {},
            )
            .await;
        assert!(failed.is_err());
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }

    #[tokio::test(start_paused = true)]
    async fn the_proxy_is_tried_again_after_its_rest() {
        let breaker = Breaker::default();
        let good: SocketAddr = "192.0.2.1:443".parse().unwrap();
        assert_eq!(breaker.admits(), Admission::Yes);
        assert!(breaker.failed(Fault::Proxy, "a.example", false), "the first failure is logged");
        assert!(breaker.admits() == Admission::No && breaker.resting());
        tokio::time::advance(PROXY_REST + Duration::from_secs(1)).await;
        assert_eq!(breaker.admits(), Admission::Trial, "one request tries it again");
        assert_eq!(breaker.admits(), Admission::No, "the others wait for that one");
        // A request that was still under way before the rest does not decide the trial (EGRESS-1).
        assert!(!breaker.failed(Fault::Tunnel, "old.example", false));
        assert_eq!(breaker.admits(), Admission::No, "the trial is still going");
        assert!(!breaker.failed(Fault::Tunnel, "b.example", true), "a failed trial rests again, without a new warning");
        assert_eq!(breaker.admits(), Admission::No);
        tokio::time::advance(PROXY_REST + Duration::from_secs(1)).await;
        assert_eq!(breaker.admits(), Admission::Trial);
        assert!(breaker.succeeded(good), "it works again");
        assert!(breaker.admits() == Admission::Yes && breaker.admits() == Admission::Yes, "everyone takes it again");
        tokio::time::advance(PROXY_REST).await;
        assert!(breaker.failed(Fault::Proxy, "a.example", false));
        tokio::time::advance(PROXY_REST + Duration::from_secs(1)).await;
        assert_eq!(breaker.admits(), Admission::Trial);
        tokio::time::advance(TRIAL_PATIENCE + Duration::from_secs(1)).await;
        assert_eq!(breaker.admits(), Admission::Trial, "a trial that never reported back is not waited for forever");
    }

    /// EGRESS-1 of the 0.18.0 audit: refused tunnels to many hosts rest the proxy only when it does not
    /// reach the last address it reached either, so someone asking for dead hosts does not take the
    /// proxy out of use for everyone.
    #[tokio::test]
    async fn dead_hosts_do_not_rest_a_proxy_that_reaches_others() {
        let (server, _) = pictures().await;
        // The first tunnel and every one after the ninth come through; the eight between are refused.
        let (proxy, asked) = scripted_proxy(|n| n == 0 || n >= 9).await;
        let egress = egress(&format!("http://{proxy}"), Fallback::Block, server);
        assert!(egress.get("http://good.example/pixel.gif", "image/*", 1024).await.is_ok());
        for host in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            assert!(egress.get(&format!("http://{host}.example/pixel.gif"), "image/*", 1024).await.is_err());
        }
        assert_eq!(asked.load(Ordering::Relaxed), 10, "the last address was asked once more");
        assert!(!egress.status().proxy_resting);
        let again = egress.get("http://good.example/pixel.gif", "image/*", 1024).await;
        assert!(again.is_ok(), "{again:?} {}", asked.load(Ordering::Relaxed));
    }

    /// A CONNECT proxy that lets the `n`th tunnel through when `open(n)` says so and refuses it with a
    /// 503 otherwise; counts the tunnels it was asked for.
    async fn scripted_proxy(open: fn(u64) -> bool) -> (SocketAddr, Arc<AtomicU64>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let asked = Arc::new(AtomicU64::new(0));
        let count = asked.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let count = count.clone();
                tokio::spawn(async move {
                    let head = read_head(&mut stream).await;
                    let n = count.fetch_add(1, Ordering::Relaxed);
                    if !open(n) {
                        let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\n\r\n").await;
                        return;
                    }
                    let target = head.split(' ').nth(1).unwrap().to_owned();
                    let mut upstream = TcpStream::connect(target).await.unwrap();
                    stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                });
            }
        });
        (address, asked)
    }

    /// Answers `/slow.png` with the start of a PNG, and the rest once `go` is told, or never.
    async fn slow_picture(go: Arc<tokio::sync::Notify>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let go = go.clone();
                tokio::spawn(async move {
                    read_head(&mut stream).await;
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 64\r\n\r\n")
                        .await;
                    let _ = stream.write_all(&[7u8; 32]).await;
                    go.notified().await;
                    let _ = stream.write_all(&[8u8; 32]).await;
                });
            }
        });
        address
    }

    #[tokio::test]
    async fn a_message_picture_shows_what_came_so_far_and_gives_up_when_it_stalls() {
        let go = Arc::new(tokio::sync::Notify::new());
        let server = slow_picture(go.clone()).await;
        let slow = egress("", Fallback::Block, server);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let fetch = tokio::spawn(async move {
            slow.get_message_picture(
                "http://pictures.example/slow.png",
                "image/*",
                1024,
                PictureLimits::default(),
                &mut |so_far: &[u8]| log.lock().unwrap().push(so_far.len()),
            )
            .await
        });
        while seen.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        assert_eq!(seen.lock().unwrap()[..], [32], "the first half is seen before the rest is there");
        go.notify_one();
        let fetched = fetch.await.unwrap().unwrap();
        assert_eq!(fetched.body.len(), 64);

        let stalled = egress("", Fallback::Block, slow_picture(Arc::default()).await);
        let limits = PictureLimits { stall: Duration::from_millis(200), ..PictureLimits::default() };
        let result =
            stalled.get_message_picture("http://pictures.example/slow.png", "image/*", 1024, limits, &mut |_| {}).await;
        assert_eq!(result.unwrap_err(), EgressError::Timeout);
    }
}
