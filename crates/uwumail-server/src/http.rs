//! HTTP(S): JMAP, the web portal, health checks and ACME challenges.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::IpNetwork;

use crate::acme::Challenges;

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

/// The TCP connection a request arrived on.
#[derive(Debug, Clone, Copy)]
struct Peer {
    addr: SocketAddr,
    tls: bool,
}

/// Works out who the client is; behind a trusted reverse proxy that is the address it forwarded.
async fn client_info(State(trusted): State<Arc<Vec<IpNetwork>>>, mut request: Request, next: Next) -> Response {
    let peer = request.extensions().get::<Peer>().copied();
    let mut info =
        peer.map_or_else(ClientInfo::default, |p| ClientInfo { ip: p.addr.ip().to_canonical(), https: p.tls });
    let is_trusted = |ip: IpAddr| trusted.iter().any(|network| network.contains(ip));
    if is_trusted(info.ip) {
        let headers = request.headers();
        if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            // The last address that is not one of our proxies is the real client.
            let client = forwarded
                .split(',')
                .rev()
                .filter_map(|part| part.trim().parse::<IpAddr>().ok())
                .map(|ip| ip.to_canonical())
                .find(|ip| !is_trusted(*ip));
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

/// Port 80: answers ACME challenges and sends everyone else to HTTPS.
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

async fn redirect_to_https(State(state): State<HttpState>, uri: Uri) -> Redirect {
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Redirect::permanent(&format!("https://{}{path}", state.hostname))
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
                let (acceptor, app) = (acceptor.clone(), app.clone());
                tokio::spawn(async move {
                    let peer = Peer { addr, tls: acceptor.is_some() };
                    let service = app.map_request(move |mut request: axum::http::Request<hyper::body::Incoming>| {
                        request.extensions_mut().insert(peer);
                        request
                    });
                    let service = TowerToHyperService::new(service);
                    let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                    match acceptor {
                        Some(acceptor) => {
                            let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(socket)).await else {
                                return;
                            };
                            let _ = builder.serve_connection_with_upgrades(TokioIo::new(stream), service).await;
                        }
                        None => {
                            let _ = builder.serve_connection_with_upgrades(TokioIo::new(socket), service).await;
                        }
                    }
                });
            }
            _ = shutdown.changed() => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
