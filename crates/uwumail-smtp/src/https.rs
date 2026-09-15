//! A small HTTPS client for what other domains publish on the web, like MTA-STS policies.
//! Certificates must be valid, redirects are not followed, and bodies are capped.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::Request;
use hyper::header::CONTENT_TYPE;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

#[derive(Clone)]
pub struct Https {
    client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub content_type: String,
    pub body: String,
}

impl Https {
    pub fn new() -> Https {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector =
            hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(tls).https_only().enable_http1().build();
        Https { client: Client::builder(TokioExecutor::new()).build(connector) }
    }

    /// GETs `url`. Anything but 200 is an error, and so is a body over `max_bytes`.
    pub async fn get(&self, url: &str, max_bytes: usize, timeout: Duration) -> Result<Fetched, String> {
        let request = Request::get(url)
            .header("User-Agent", concat!("UwUMail/", env!("CARGO_PKG_VERSION")))
            .body(Empty::new())
            .map_err(|err| err.to_string())?;
        let fetch = async {
            let response = self.client.request(request).await.map_err(|err| error_chain(&err))?;
            let status = response.status();
            if status != hyper::StatusCode::OK {
                return Err(format!("the server answered {status}"));
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let body = Limited::new(response.into_body(), max_bytes)
                .collect()
                .await
                .map_err(|err| format!("reading the answer failed: {err}"))?
                .to_bytes();
            let body = String::from_utf8(body.to_vec()).map_err(|_| "the answer is not UTF-8 text".to_owned())?;
            Ok(Fetched { content_type, body })
        };
        tokio::time::timeout(timeout, fetch).await.map_err(|_| "no answer in time".to_owned())?
    }
}

impl Default for Https {
    fn default() -> Self {
        Https::new()
    }
}

/// hyper's errors hide the interesting part (like a certificate problem) in their sources.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        let text = inner.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = inner.source();
    }
    message
}
