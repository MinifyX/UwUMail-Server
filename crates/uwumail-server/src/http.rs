//! HTTP(S): JMAP, the web portal, health checks and ACME challenges.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::watch;
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

async fn client_info(State(trusted): State<Arc<Vec<IpNetwork>>>, mut request: Request, next: Next) -> Response {
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
    let (lang, title, text) = if prefers_german(headers) {
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

fn prefers_german(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .is_none_or(|first| first.trim().to_ascii_lowercase().starts_with("de"))
}

async fn landing(State(state): State<HttpState>, headers: HeaderMap) -> Html<String> {
    let (lang, title, text) = if prefers_german(&headers) {
        ("de", "Hier wohnt ein UwUMail-Server", "Er schnurrt schon. Die Verwaltung und die Web-Mail ziehen bald ein.")
    } else {
        ("en", "A UwUMail server lives here", "It's already purring. The admin panel and web mail are moving in soon.")
    };
    Html(page(lang, title, text, &state.hostname))
}

async fn not_found(headers: HeaderMap) -> (StatusCode, Html<String>) {
    let (lang, title, text) = if prefers_german(&headers) {
        ("de", "Hier ist nichts (・_・;)", "Diese Seite gibt es nicht. Vielleicht hat sie sich unterm Sofa versteckt.")
    } else {
        ("en", "Nothing here (・_・;)", "This page does not exist. Maybe it is hiding under the sofa.")
    };
    (StatusCode::NOT_FOUND, Html(page(lang, title, text, "")))
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
  .face {{ font-size: 44px; color: var(--pink); margin-bottom: 8px; }}
  h1 {{ font-size: 22px; margin: 0 0 8px; }}
  p {{ margin: 0; color: var(--muted); }}
  small {{ display: block; margin-top: 20px; color: var(--muted); }}
</style>
</head>
<body>
<main>
  <div class="face" aria-hidden="true">(=^･ω･^=)</div>
  <h1>{title}</h1>
  <p>{text}</p>
  <small>{hostname}</small>
</main>
</body>
</html>"#
    )
}

/// Serves HTTP/1 and HTTP/2 on a listener, with TLS when `tls` is given.
pub async fn serve(
    listener: TcpListener,
    tls: Option<Arc<rustls::ServerConfig>>,
    app: Router,
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
                tokio::spawn(serve_connection(socket, addr, acceptor.clone(), app.clone()));
            }
            _ = shutdown.changed() => break,
        }
    }
}

/// Serves one connection from `addr`, which arrived on a listener or through the UwUMail Gateway.
pub async fn serve_connection<S>(stream: S, addr: SocketAddr, acceptor: Option<TlsAcceptor>, app: Router)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let peer = Peer { addr, tls: acceptor.is_some() };
    let service = app.map_request(move |mut request: axum::http::Request<hyper::body::Incoming>| {
        request.extensions_mut().insert(peer);
        request
    });
    let service = TowerToHyperService::new(service);
    let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    match acceptor {
        Some(acceptor) => {
            let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await else {
                return;
            };
            let _ = builder.serve_connection_with_upgrades(TokioIo::new(stream), service).await;
        }
        None => {
            let _ = builder.serve_connection_with_upgrades(TokioIo::new(stream), service).await;
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
        HttpState { hostname: "mail.example.de".into(), challenges: Arc::default(), started: Instant::now() }
    }

    fn app(state: HttpState) -> Router {
        super::app(state, Router::new(), Router::new(), Arc::default())
    }

    #[tokio::test]
    async fn health_and_redirects() {
        let response = app(state()).oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response =
            redirect_app(state()).oneshot(Request::get("/login?x=1").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers()[header::LOCATION], "https://mail.example.de/login?x=1");

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
}
