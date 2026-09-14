//! HTTP(S): health checks, ACME challenges and (soon) the admin panel, web mail and JMAP.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use crate::acme::Challenges;

#[derive(Clone)]
pub struct HttpState {
    pub hostname: String,
    pub challenges: Arc<Challenges>,
    pub started: Instant,
}

/// The main site (HTTPS, or plain HTTP behind a reverse proxy).
pub fn app(state: HttpState) -> Router {
    Router::new()
        .route("/", get(landing))
        .route("/healthz", get(health))
        .route("/.well-known/acme-challenge/{token}", get(acme_challenge))
        .fallback(not_found)
        .with_state(state)
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

pub async fn serve_plain(listener: TcpListener, app: Router, mut shutdown: watch::Receiver<bool>) {
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.changed().await;
        })
        .await;
    if let Err(err) = result {
        tracing::error!(%err, "the HTTP listener stopped");
    }
}

pub async fn serve_https(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    app: Router,
    mut shutdown: watch::Receiver<bool>,
) {
    let acceptor = TlsAcceptor::from(tls);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((socket, _peer)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let (acceptor, app) = (acceptor.clone(), app.clone());
                tokio::spawn(async move {
                    let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(socket)).await else {
                        return;
                    };
                    let service = TowerToHyperService::new(app);
                    let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(TokioIo::new(stream), service)
                        .await;
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
    use tower::ServiceExt;

    fn state() -> HttpState {
        HttpState { hostname: "mail.example.de".into(), challenges: Arc::default(), started: Instant::now() }
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
