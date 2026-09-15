//! The web app's files, embedded at build time by `build.rs`.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

pub struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// Scripts and styles only from here; the app needs no inline scripts and no other hosts.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; \
     form-action 'self'; frame-ancestors 'none'";

pub fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.binary_search_by(|asset| asset.path.cmp(path)).ok().map(|index| &ASSETS[index])
}

pub fn index() -> Option<&'static Asset> {
    find("/index.html")
}

/// Files at the top of the build (favicon and friends), which need their own routes.
pub fn root_files() -> impl Iterator<Item = &'static Asset> {
    ASSETS.iter().filter(|asset| asset.path != "/index.html" && asset.path[1..].find('/').is_none())
}

pub fn respond(asset: &'static Asset) -> Response {
    let cache = if asset.path.starts_with("/assets/") {
        // Vite puts a content hash into these names.
        "public, max-age=31536000, immutable"
    } else if asset.path == "/index.html" {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    let mut response = (StatusCode::OK, asset.bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if asset.path == "/index.html" {
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CONTENT_SECURITY_POLICY));
        headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    response
}
