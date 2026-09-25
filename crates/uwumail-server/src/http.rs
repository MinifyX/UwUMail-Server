//! HTTP(S): JMAP, the web portal, health checks and ACME challenges.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use uwumail_smtp::Language;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::IpNetwork;

use crate::acme::Challenges;
use crate::tls::{CertStore, CertificateInfo};

#[derive(Clone)]
pub struct HttpState {
    pub hostname: String,
    pub challenges: Arc<Challenges>,
    pub started: Instant,
}

/// The main site (HTTPS, or plain HTTP behind a reverse proxy), with JMAP and the web portal merged in.
pub fn app(state: HttpState, jmap: Router, web: Router, trusted_proxies: Arc<Vec<IpNetwork>>) -> Router {
    let mut router =
        Router::new().route("/healthz", get(health)).route("/.well-known/acme-challenge/{token}", get(acme_challenge));
    if !uwumail_web::Web::has_app() {
        // A build without the web app still greets visitors.
        router = router.route("/", get(landing));
    }
    router
        .fallback(not_found)
        .with_state(state)
        .merge(jmap)
        .merge(web)
        .layer(middleware::from_fn_with_state(trusted_proxies, client_info))
}

/// HSTS only makes sense with a certificate browsers trust. With a self-signed one (or none yet)
/// the header would lock people out of the portal for a year.
fn hsts_allowed(certificate: Option<CertificateInfo>) -> bool {
    certificate.is_some_and(|info| !info.self_signed)
}

/// Tells browsers to use HTTPS only, for requests the server answered over its own TLS.
pub async fn strict_transport_security(State(certs): State<Arc<CertStore>>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if hsts_allowed(certs.info()) {
        response.headers_mut().insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    response
}

/// The TCP connection a request arrived on.
#[derive(Debug, Clone, Copy)]
struct Peer {
    addr: SocketAddr,
    tls: bool,
}

/// Whether the log already named a reverse proxy that is missing in `http.trusted_proxies`.
static UNTRUSTED_PROXY_NAMED: AtomicBool = AtomicBool::new(false);

/// The address of a reverse proxy that forwards requests without being trusted. Only the proxy
/// listener is reached without TLS, and browsers do not send X-Forwarded-For themselves.
fn untrusted_proxy(peer: Option<Peer>, trusted: bool, headers: &HeaderMap) -> Option<IpAddr> {
    let peer = peer.filter(|peer| !peer.tls && !trusted)?;
    headers.contains_key("x-forwarded-for").then(|| peer.addr.ip().to_canonical())
}

/// Works out who the client is; behind a trusted reverse proxy that is the address it forwarded.
/// One `X-Forwarded-For` hop as an address: a bare IP, or the `ip:port` / `[v6]:port` form some
/// proxies write. `None` for anything else, which ends the walk.
fn parse_forwarded_hop(hop: &str) -> Option<IpAddr> {
    if let Ok(ip) = hop.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Some(rest) = hop.strip_prefix('[') {
        return rest[..rest.find(']')?].parse().ok();
    }
    hop.rsplit_once(':').and_then(|(host, _)| host.parse().ok())
}

/// HTTP/2 carries the host in the `:authority` pseudo-header, which hyper puts into the URI; there
/// is no `Host` header then. Everything that builds URLs for the client (the JMAP session, the
/// token answer) or compares origins (the WebSocket handshake) reads `Host`, and fell back to
/// `localhost` for every HTTP/2 client. So the header is filled in from the authority, as
/// RFC 9113 (section 8.3.1) describes for intermediaries.
fn host_from_authority(request: &mut Request) {
    if request.headers().contains_key(header::HOST) {
        return;
    }
    let Some(authority) = request.uri().authority() else { return };
    let host = match authority.port_u16() {
        Some(port) => format!("{}:{port}", authority.host()),
        None => authority.host().to_owned(),
    };
    if let Ok(value) = HeaderValue::from_str(&host) {
        request.headers_mut().insert(header::HOST, value);
    }
}

async fn client_info(State(trusted): State<Arc<Vec<IpNetwork>>>, mut request: Request, next: Next) -> Response {
    host_from_authority(&mut request);
    let peer = request.extensions().get::<Peer>().copied();
    let mut info =
        peer.map_or_else(ClientInfo::default, |p| ClientInfo { ip: p.addr.ip().to_canonical(), https: p.tls });
    let is_trusted = |ip: IpAddr| trusted.iter().any(|network| network.contains(ip));
    if let Some(proxy) = untrusted_proxy(peer, is_trusted(info.ip), request.headers())
        && !UNTRUSTED_PROXY_NAMED.swap(true, Ordering::Relaxed)
    {
        // Said once: it is the address to put into the configuration.
        tracing::warn!(
            %proxy,
            "requests with X-Forwarded-For arrive from an address that is not in http.trusted_proxies. If that is \
             your reverse proxy, add it: until then every visitor counts as this address and cookies are not \
             marked Secure"
        );
    }
    if is_trusted(info.ip) {
        let headers = request.headers();
        if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            // The rightmost address that is not one of our proxies is the real client. A hop that
            // cannot be read as an address ends the walk rather than being skipped over -- skipping
            // it let a client behind a proxy that appends `ip:port` pick its own address for the
            // login throttle (security-audit-0.5.2 S-20). A `:port` suffix is stripped so such
            // proxies work.
            let mut client = None;
            for hop in forwarded.split(',').rev() {
                let Some(ip) = parse_forwarded_hop(hop.trim()) else {
                    break;
                };
                let ip = ip.to_canonical();
                if !is_trusted(ip) {
                    client = Some(ip);
                    break;
                }
            }
            if let Some(client) = client {
                info.ip = client;
            }
        }
        if let Some(proto) = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) {
            info.https = proto.eq_ignore_ascii_case("https");
        }
    }
    request.extensions_mut().insert(info);
    next.run(request).await
}

/// Port 80: answers ACME challenges and sends everyone else to HTTPS. A reverse proxy belongs at
/// the proxy listener instead and is told so.
pub fn redirect_app(state: HttpState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/.well-known/acme-challenge/{token}", get(acme_challenge))
        .fallback(redirect_to_https)
        .with_state(state)
}

async fn health(State(state): State<HttpState>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": state.started.elapsed().as_secs(),
    }))
}

async fn acme_challenge(State(state): State<HttpState>, Path(token): Path<String>) -> Response {
    match state.challenges.answer(&token) {
        Some(answer) => ([(header::CONTENT_TYPE, "application/octet-stream")], answer).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn redirect_to_https(State(state): State<HttpState>, uri: Uri, headers: HeaderMap) -> Response {
    // A visitor who already came over HTTPS can only have arrived through a reverse proxy that
    // points at this port. Redirecting again would go round in circles, so say what to change.
    // The header decides nothing else here, so it does not matter who sent it.
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"));
    if forwarded_https {
        return proxy_at_redirect_port(&headers, &state.hostname).into_response();
    }
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Redirect::permanent(&format!("https://{}{path}", state.hostname)).into_response()
}

fn proxy_at_redirect_port(headers: &HeaderMap, hostname: &str) -> (StatusCode, Html<String>) {
    // Meant for whoever set up the proxy: German or English, like the docs it points to.
    let (lang, title, text) = if language(headers) == Language::De {
        (
            "de",
            "Dieser Port leitet nur um (・_・;)",
            "Dein Reverse Proxy zeigt auf Port 80 von UwUMail. Der schickt alle zu HTTPS, und hinter einem Proxy \
             dreht sich das im Kreis. Richte ihn stattdessen auf den Proxy-Listener (listen.proxy). Wie das geht, \
             steht in docs/deployment.md unter „Behind a reverse proxy“.",
        )
    } else {
        (
            "en",
            "This port only redirects (・_・;)",
            "Your reverse proxy points at UwUMail's port 80. That one sends everyone to HTTPS, and behind a proxy \
             this goes round in circles. Point it at the proxy listener (listen.proxy) instead. \
             docs/deployment.md, “Behind a reverse proxy”, shows how.",
        )
    };
    (StatusCode::MISDIRECTED_REQUEST, Html(page(lang, title, text, hostname)))
}

/// The visitor's language as far as the server speaks it; German without a hint.
fn language(headers: &HeaderMap) -> Language {
    headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .and_then(Language::from_accept_language)
        .unwrap_or(Language::De)
}

async fn landing(State(state): State<HttpState>, headers: HeaderMap) -> Html<String> {
    let language = language(&headers);
    let (title, text) = match language {
        Language::De => ("Hier wohnt ein Mailserver", "Das Portal und die Web-Mail sind über HTTPS erreichbar."),
        Language::En => ("A mail server lives here", "The portal and the web mail are reachable over HTTPS."),
        Language::Fr => ("Ici habite un serveur mail", "Le portail et le webmail sont accessibles en HTTPS."),
        Language::Nl => ("Hier woont een mailserver", "Het portaal en de webmail zijn bereikbaar via HTTPS."),
        Language::Ja => ("ここはメールサーバーです", "ポータルとウェブメールは HTTPS で利用できます。"),
        Language::Zh => ("这里是一台邮件服务器", "门户和网页邮箱可通过 HTTPS 访问。"),
    };
    Html(page(language.code(), title, text, &state.hostname))
}

async fn not_found(headers: HeaderMap) -> (StatusCode, Html<String>) {
    let language = language(&headers);
    let (title, text) = match language {
        Language::De => ("Hier ist nichts", "Diese Seite gibt es nicht."),
        Language::En => ("Nothing here", "This page does not exist."),
        Language::Fr => ("Rien ici", "Cette page n'existe pas."),
        Language::Nl => ("Hier is niets", "Deze pagina bestaat niet."),
        Language::Ja => ("ページがありません", "このページは存在しません。"),
        Language::Zh => ("这里什么也没有", "此页面不存在。"),
    };
    (StatusCode::NOT_FOUND, Html(page(language.code(), title, text, "")))
}

fn page(lang: &str, title: &str, text: &str, hostname: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  :root {{ color-scheme: light dark; --pink: #ff4d8d; --bg: #fff7fa; --card: #ffffff; --text: #2b1d24; --muted: #7a6470; }}
  @media (prefers-color-scheme: dark) {{ :root {{ --pink: #ff7fac; --bg: #1c1418; --card: #271c21; --text: #f6e9ef; --muted: #b69aa8; }} }}
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: var(--bg); color: var(--text);
         font: 16px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; padding: 0 16px; }}
  main {{ background: var(--card); border-radius: 24px; padding: 40px 32px; max-width: 420px; text-align: center;
         box-shadow: 0 12px 40px rgba(255, 77, 141, .15); }}
  h1 {{ font-size: 22px; margin: 0 0 8px; }}
  p {{ margin: 0; color: var(--muted); }}
  small {{ display: block; margin-top: 20px; color: var(--muted); }}
</style>
</head>
<body>
<main>
  <h1>{title}</h1>
  <p>{text}</p>
  <small>{hostname}</small>
</main>
</body>
</html>"#
    )
}

// Before anything is logged in, an HTTP connection costs a task, a descriptor and buffers. Neither
// the internet as a whole nor one network may hold them open without end: that would also leave
// SMTP and IMAP without descriptors (security-audit-0.8.0 W-2). JMAP's event stream keeps a
// connection open on purpose, so the limits count connections, not how long they last.
/// Connections all HTTP listeners and the gateway hold at once.
const MAX_CONNECTIONS: usize = 4096;
/// Connections one network (an IPv4 address, an IPv6 /64) holds at once. Not counted behind a
/// reverse proxy, where every connection comes from the proxy.
const MAX_PER_NETWORK: usize = 128;
/// How long a request's headers may take, and how long an HTTP/1 connection may wait for the next
/// request. The first bytes, which tell HTTP/1 from HTTP/2, have to arrive within it too.
const HEADER_TIMEOUT: Duration = Duration::from_secs(20);
/// An HTTP/2 connection is pinged this often, and closed when a ping goes unanswered this long.
const H2_KEEP_ALIVE: Duration = Duration::from_secs(60);
const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the HTTP connections hold, in all and per network.
pub struct Connections {
    total: Arc<Semaphore>,
    per_network: Mutex<HashMap<IpAddr, usize>>,
    per_network_max: usize,
    header_timeout: Duration,
}

/// One connection's place, given back when it closes.
pub struct Admitted {
    _permit: OwnedSemaphorePermit,
    network: Option<IpAddr>,
    connections: Arc<Connections>,
}

impl Drop for Admitted {
    fn drop(&mut self) {
        let Some(network) = self.network else { return };
        let mut counts = self.connections.per_network.lock().expect("connection counts poisoned");
        if let Some(count) = counts.get_mut(&network) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&network);
            }
        }
    }
}

/// IPv6 users usually own a whole /64, so connections count per /64, like failed logins do.
fn network(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        v4 @ IpAddr::V4(_) => v4,
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            IpAddr::V6(segments.into())
        }
    }
}

impl Connections {
    pub fn new() -> Arc<Connections> {
        Connections::with_limits(MAX_CONNECTIONS, MAX_PER_NETWORK, HEADER_TIMEOUT)
    }

    fn with_limits(total: usize, per_network: usize, header_timeout: Duration) -> Arc<Connections> {
        Arc::new(Connections {
            total: Arc::new(Semaphore::new(total)),
            per_network: Mutex::default(),
            per_network_max: per_network,
            header_timeout,
        })
    }

    /// A place for a connection from `ip`, unless all are taken. `count_network` is false where
    /// every connection comes from the same reverse proxy.
    pub fn admit(self: &Arc<Self>, ip: IpAddr, count_network: bool) -> Option<Admitted> {
        let permit = self.total.clone().try_acquire_owned().ok()?;
        let network = count_network.then(|| network(ip));
        if let Some(network) = network {
            let mut counts = self.per_network.lock().expect("connection counts poisoned");
            let count = counts.entry(network).or_default();
            if *count >= self.per_network_max {
                return None;
            }
            *count += 1;
        }
        Some(Admitted { _permit: permit, network, connections: self.clone() })
    }
}

/// A connection that has to start talking within [`HEADER_TIMEOUT`]. hyper's header timeout only
/// starts once it knows which HTTP version it speaks, and it learns that from the first bytes.
struct FirstBytesDue<S> {
    inner: S,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<S> FirstBytesDue<S> {
    fn new(inner: S, due: Duration) -> FirstBytesDue<S> {
        FirstBytesDue { inner, deadline: Some(Box::pin(tokio::time::sleep(due))) }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for FirstBytesDue<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if buf.filled().len() > before {
                    this.deadline = None;
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                if let Some(deadline) = this.deadline.as_mut()
                    && deadline.as_mut().poll(cx).is_ready()
                {
                    return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no request in time")));
                }
                Poll::Pending
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for FirstBytesDue<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// Serves HTTP/1 and HTTP/2 on a listener, with TLS when `tls` is given. `count_network` is false
/// on the listener behind a reverse proxy.
pub async fn serve(
    listener: TcpListener,
    tls: Option<Arc<rustls::ServerConfig>>,
    app: Router,
    connections: Arc<Connections>,
    count_network: bool,
    mut shutdown: watch::Receiver<bool>,
) {
    let acceptor = tls.map(TlsAcceptor::from);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((socket, addr)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                // A connection beyond the limits is closed right away, before it costs anything.
                let Some(admitted) = connections.admit(addr.ip(), count_network) else { continue };
                tokio::spawn(serve_connection(socket, addr, acceptor.clone(), app.clone(), admitted));
            }
            _ = shutdown.changed() => break,
        }
    }
}

/// Serves one connection from `addr`, which arrived on a listener or through the UwUMail Gateway,
/// and holds its place in [`Connections`] until it closes.
pub async fn serve_connection<S>(
    stream: S,
    addr: SocketAddr,
    acceptor: Option<TlsAcceptor>,
    app: Router,
    admitted: Admitted,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let header_timeout = admitted.connections.header_timeout;
    let _admitted = admitted;
    let peer = Peer { addr, tls: acceptor.is_some() };
    let service = app.map_request(move |mut request: axum::http::Request<hyper::body::Incoming>| {
        request.extensions_mut().insert(peer);
        request
    });
    let service = TowerToHyperService::new(service);
    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    // Without a timer hyper has no header timeout at all, whatever its defaults say.
    builder.http1().timer(TokioTimer::new()).header_read_timeout(header_timeout);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(H2_KEEP_ALIVE)
        .keep_alive_timeout(H2_KEEP_ALIVE_TIMEOUT);
    match acceptor {
        Some(acceptor) => {
            let Ok(Ok(stream)) = tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await else {
                return;
            };
            let io = TokioIo::new(FirstBytesDue::new(stream, header_timeout));
            let _ = builder.serve_connection_with_upgrades(io, service).await;
        }
        None => {
            let io = TokioIo::new(FirstBytesDue::new(stream, header_timeout));
            let _ = builder.serve_connection_with_upgrades(io, service).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsts_needs_a_trusted_certificate() {
        let info = |self_signed| Some(CertificateInfo { not_after: 0, names: vec![], self_signed });
        assert!(hsts_allowed(info(false)));
        assert!(!hsts_allowed(info(true)));
        assert!(!hsts_allowed(None));
    }

    #[test]
    fn a_forwarded_hop_reads_bare_and_port_forms_and_nothing_else() {
        let ip = |s: &str| parse_forwarded_hop(s);
        assert_eq!(ip("203.0.113.9"), "203.0.113.9".parse().ok());
        assert_eq!(ip("203.0.113.9:5555"), "203.0.113.9".parse().ok(), "a :port suffix is stripped");
        assert_eq!(ip("[2001:db8::1]:443"), "2001:db8::1".parse().ok());
        assert_eq!(ip("2001:db8::1"), "2001:db8::1".parse().ok());
        assert_eq!(ip("for=1.2.3.4"), None, "an unreadable hop ends the walk, it is not skipped");
        assert_eq!(ip("garbage"), None);
    }
    use axum::body::Body;
    use axum::http::Request;

    fn state() -> HttpState {
        HttpState { hostname: "mail.example.org".into(), challenges: Arc::default(), started: Instant::now() }
    }

    fn app(state: HttpState) -> Router {
        super::app(state, Router::new(), Router::new(), Arc::default())
    }

    #[tokio::test]
    async fn an_http2_authority_becomes_the_host_header() {
        let echo = Router::new().route(
            "/host",
            get(|headers: HeaderMap| async move {
                headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("none").to_owned()
            }),
        );
        let app = super::app(state(), echo, Router::new(), Arc::default());
        let host = |request: Request<Body>| {
            let app = app.clone();
            async move {
                let response = app.oneshot(request).await.unwrap();
                let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
                String::from_utf8(body.to_vec()).unwrap()
            }
        };
        // How hyper hands over an HTTP/2 request: the authority in the URI, no Host header.
        assert_eq!(
            host(Request::get("https://mail.example.org:8443/host").body(Body::empty()).unwrap()).await,
            "mail.example.org:8443"
        );
        assert_eq!(
            host(Request::get("https://mail.example.org/host").body(Body::empty()).unwrap()).await,
            "mail.example.org"
        );
        assert_eq!(
            host(Request::get("https://[2001:db8::1]:8443/host").body(Body::empty()).unwrap()).await,
            "[2001:db8::1]:8443"
        );
        // HTTP/1.1 sends Host, which stays as it is.
        let http1 = Request::get("/host").header(header::HOST, "mail.example.net").body(Body::empty()).unwrap();
        assert_eq!(host(http1).await, "mail.example.net");
    }

    #[tokio::test]
    async fn health_and_redirects() {
        let response = app(state()).oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response =
            redirect_app(state()).oneshot(Request::get("/login?x=1").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers()[header::LOCATION], "https://mail.example.org/login?x=1");

        let response = app(state())
            .oneshot(Request::get("/.well-known/acme-challenge/unknown").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_reverse_proxy_at_the_redirect_port_is_told_instead_of_looping() {
        let through_proxy = |path: &str| {
            Request::get(path)
                .header("x-forwarded-proto", "https")
                .header(header::ACCEPT_LANGUAGE, "en")
                .body(Body::empty())
                .unwrap()
        };
        let response = redirect_app(state()).oneshot(through_proxy("/login")).await.unwrap();
        assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
        assert!(!response.headers().contains_key(header::LOCATION));
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("listen.proxy"));

        // Certificate challenges and health checks work through a proxy as well.
        let response = redirect_app(state()).oneshot(through_proxy("/healthz")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response =
            redirect_app(state()).oneshot(through_proxy("/.well-known/acme-challenge/unknown")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // A proxy that forwards plain HTTP visitors is redirected like everyone else.
        let plain = Request::get("/login").header("x-forwarded-proto", "http").body(Body::empty()).unwrap();
        let response = redirect_app(state()).oneshot(plain).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    }

    #[test]
    fn only_a_forwarding_peer_of_the_proxy_listener_is_named() {
        let peer = |tls| Some(Peer { addr: "172.30.25.2:40000".parse().unwrap(), tls });
        let mut forwarded = HeaderMap::new();
        forwarded.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));

        assert_eq!(untrusted_proxy(peer(false), false, &forwarded), Some("172.30.25.2".parse().unwrap()));
        assert_eq!(untrusted_proxy(peer(false), true, &forwarded), None, "trusted already");
        assert_eq!(untrusted_proxy(peer(true), false, &forwarded), None, "HTTPS is not the proxy listener");
        assert_eq!(untrusted_proxy(peer(false), false, &HeaderMap::new()), None, "a browser, not a proxy");
        assert_eq!(untrusted_proxy(None, false, &forwarded), None);
    }

    #[test]
    fn connections_are_limited_in_all_and_per_network() {
        let connections = Connections::with_limits(4, 2, HEADER_TIMEOUT);
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        let first = connections.admit(ip("192.0.2.1"), true).unwrap();
        let _second = connections.admit(ip("192.0.2.1"), true).unwrap();
        assert!(connections.admit(ip("192.0.2.1"), true).is_none(), "two per network");
        assert!(connections.admit(ip("::ffff:192.0.2.1"), true).is_none(), "the same address, mapped");
        let _v6 = connections.admit(ip("2001:db8::1"), true).unwrap();
        let _v6_neighbour = connections.admit(ip("2001:db8::2"), true).expect("the same /64, second place");
        assert!(connections.admit(ip("2001:db8::3"), true).is_none(), "the same /64, no third");
        assert!(connections.admit(ip("198.51.100.1"), false).is_none(), "four in all, also behind a proxy");
        drop(first);
        assert!(connections.admit(ip("192.0.2.1"), true).is_some(), "a closed connection gives its place back");
    }

    #[tokio::test]
    async fn a_connection_that_sends_nothing_is_closed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Connections::with_limits(3, 3, Duration::from_millis(300));
        let (_stop, shutdown) = watch::channel(false);
        tokio::spawn(serve(listener, None, redirect_app(state()), connections.clone(), true, shutdown));

        // Silent from the start: closed although hyper has not even learnt the HTTP version.
        let mut silent = tokio::net::TcpStream::connect(addr).await.unwrap();
        let started = Instant::now();
        let read = tokio::time::timeout(Duration::from_secs(10), silent.read(&mut [0u8; 64])).await;
        assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "closed");
        assert!(started.elapsed() < Duration::from_secs(5));

        // Half a request: closed by the header timeout.
        let mut slow = tokio::net::TcpStream::connect(addr).await.unwrap();
        slow.write_all(
            b"GET /healthz HTTP/1.1
Host: x
",
        )
        .await
        .unwrap();
        let mut answer = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(10), slow.read_to_end(&mut answer)).await;
        assert!(read.is_ok(), "closed");
        assert!(!String::from_utf8_lossy(&answer).contains("200 OK"));

        // A whole request is answered, and the places come back once the connections are gone.
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"GET /healthz HTTP/1.1
Host: x
Connection: close

",
            )
            .await
            .unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut answer)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 200"));
    }
}
