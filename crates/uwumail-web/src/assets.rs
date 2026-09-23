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

/// The webmail's policy.
///
/// A message is shown in a `srcdoc` frame without scripts, and such a frame inherits the policy of
/// the page around it. Its remote pictures come through this server (`/jmap/image`, see
/// docs/jmap-remote.md), so `img-src` allows no other host here either: a picture that did not go
/// through the server can't reach its sender, not even by mistake. Whether a mail's pictures load at
/// all stays with the reader; until they ask, the frame carries its own policy that allows none.
/// `form-action` is `'none'`, because nothing in the webmail ever submits a form — everything goes
/// through fetch. PDF previews live in `blob:` frames.
const WEBMAIL_CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; \
     base-uri 'none'; form-action 'none'; frame-src 'self' blob:; frame-ancestors 'none'";

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

/// The webmail's files, built from its own repository and served under `/mail`.
pub fn webmail_find(path: &str) -> Option<&'static Asset> {
    WEBMAIL_ASSETS.binary_search_by(|asset| asset.path.cmp(path)).ok().map(|index| &WEBMAIL_ASSETS[index])
}

pub fn webmail_index() -> Option<&'static Asset> {
    webmail_find("/index.html")
}

/// Whether a webmail was built into this binary at all.
pub fn has_webmail() -> bool {
    webmail_index().is_some()
}

pub fn webmail_respond(asset: &'static Asset) -> Response {
    let mut response = respond(asset);
    if asset.path == "/index.html" {
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(WEBMAIL_CONTENT_SECURITY_POLICY));
    }
    response
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_load_only_from_here() {
        for policy in [CONTENT_SECURITY_POLICY, WEBMAIL_CONTENT_SECURITY_POLICY] {
            let images = policy.split(';').map(str::trim).find(|part| part.starts_with("img-src")).unwrap();
            assert_eq!(images, "img-src 'self' data: blob:", "{policy}");
        }
    }
}
