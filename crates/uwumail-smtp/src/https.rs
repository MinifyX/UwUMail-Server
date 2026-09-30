//! A small HTTPS client for what other domains publish on the web, like MTA-STS policies.
//! Certificates must be valid, redirects are not followed, bodies are capped, and only public
//! addresses are reached: the names asked for come from other people's DNS, which may point them at
//! this host or the local network.

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

use crate::fetch::{PublicResolver, is_public};

#[derive(Clone)]
enum Inner {
    Direct(Client<HttpsConnector<HttpConnector<PublicResolver>>, Empty<Bytes>>),
    /// Through the egress, for requests the admin wants to leave through the VPN.
    Egress(Client<HttpsConnector<crate::egress::DialerConnector>, Empty<Bytes>>),
}

#[derive(Clone)]
pub struct Https {
    client: Inner,
}

fn tls_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub content_type: String,
    pub body: String,
}

impl Https {
    pub fn new() -> Https {
        let mut http = HttpConnector::new_with_resolver(PublicResolver);
        // The TLS layer around it insists on https; this one only has to let it through.
        http.enforce_http(false);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config())
            .https_only()
            .enable_http1()
            .wrap_connector(http);
        Https { client: Inner::Direct(Client::builder(TokioExecutor::new()).build(connector)) }
    }

    /// Leaves the way the dialer does: through the proxy, when it has one, and only to public addresses.
    pub fn through(dialer: &crate::egress::Dialer) -> Https {
        if !dialer.proxied() {
            return Https::new();
        }
        let connector = dialer.https_connector(tls_config());
        Https { client: Inner::Egress(Client::builder(TokioExecutor::new()).build(connector)) }
    }

    /// GETs `url`. Anything but 200 is an error, and so is a body over `max_bytes`.
    pub async fn get(&self, url: &str, max_bytes: usize, timeout: Duration) -> Result<Fetched, String> {
        // An address in the link is never looked up, so the resolver cannot refuse it.
        let uri: hyper::Uri = url.parse().map_err(|_| format!("{url} is not a web address"))?;
        let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
        if host.parse::<std::net::IpAddr>().is_ok_and(|ip| !is_public(ip)) {
            return Err("not a public address".to_owned());
        }
        let request = Request::get(uri)
            .header("User-Agent", concat!("UwUMail/", env!("CARGO_PKG_VERSION")))
            .body(Empty::new())
            .map_err(|err| err.to_string())?;
        let fetch = async {
            let response = match &self.client {
                Inner::Direct(client) => client.request(request).await,
                Inner::Egress(client) => client.request(request).await,
            }
            .map_err(|err| error_chain(&err))?;
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
