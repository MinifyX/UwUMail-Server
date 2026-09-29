//! Remote pictures of a message, fetched by the server instead of the reader (docs/jmap-remote.md).
//!
//! A picture loaded by the webmail or an app tells its sender when the message was opened and from which
//! address. Asked for here, the sender sees the server — or, with `[egress] proxy` set, a VPN — and never
//! the person reading. Whether a message's pictures are shown at all stays the reader's choice: nothing
//! is rewritten in the message, the client asks for each picture once it may.

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::stream::FuturesUnordered;
use serde::Deserialize;
use serde_json::json;
use uwumail_smtp::egress::EgressError;
use uwumail_smtp::remote_images::{MAX_URL_LENGTH, PictureError};

use crate::auth::ClientInfo;
use crate::{Jmap, ids, pictures};

pub use uwumail_smtp::remote_images::MAX_IMAGE_BYTES;
/// Addresses one request for sizes may ask about.
pub const MAX_SIZES: usize = 200;

fn problem(status: StatusCode, detail: &str) -> Response {
    let body = json!({ "type": "about:blank", "status": status.as_u16(), "detail": detail });
    (status, [(header::CONTENT_TYPE, "application/problem+json")], body.to_string()).into_response()
}

#[derive(Deserialize)]
pub struct ImageQuery {
    url: String,
}

pub async fn image(
    State(jmap): State<Jmap>,
    Path(account): Path<String>,
    Query(query): Query<ImageQuery>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let owner = match jmap.inner.auth.account_for(&headers, client, false).await {
        Ok(owner) => owner,
        Err(err) => return err.into_response(),
    };
    if account != ids::account(owner.id) {
        return problem(StatusCode::NOT_FOUND, "Unknown account.");
    }
    if query.url.len() > MAX_URL_LENGTH {
        return problem(StatusCode::BAD_REQUEST, "The picture's address is too long.");
    }
    let picture = match jmap.inner.remote_images.get(owner.id, &query.url).await {
        Ok(picture) => picture,
        Err(PictureError::Egress(EgressError::NotAllowed(why))) => return problem(StatusCode::BAD_REQUEST, &why),
        Err(PictureError::Egress(EgressError::Timeout)) => {
            return problem(StatusCode::GATEWAY_TIMEOUT, "The picture did not come in time.");
        }
        Err(PictureError::NotAPicture) => return problem(StatusCode::BAD_GATEWAY, "That is not a picture."),
        Err(PictureError::Busy) => {
            return problem(StatusCode::TOO_MANY_REQUESTS, "Too many pictures at once; try again in a moment.");
        }
        Err(err) => return problem(StatusCode::BAD_GATEWAY, &format!("The picture could not be fetched: {err}.")),
    };
    let Ok(media_type) = HeaderValue::from_str(&picture.media_type) else {
        return problem(StatusCode::BAD_GATEWAY, "That is not a picture.");
    };
    let mut response = Response::new(Body::from(picture.bytes.clone()));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, media_type);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    if let Some(size) = picture.size {
        headers.insert("x-image-width", HeaderValue::from(size.width));
        headers.insert("x-image-height", HeaderValue::from(size.height));
    }
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    // An <img> ignores both; they are for someone who opens the address itself, so a picture — an SVG
    // above all — can never run anything on this origin.
    headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    response
}

#[derive(Deserialize)]
pub struct SizesRequest {
    urls: Vec<String>,
}

/// The sizes of a message's remote pictures, each as soon as it is known (docs/jmap-remote.md): one
/// line of JSON per address, in the order they come. Asking fetches the pictures into the cache, so the
/// pictures themselves are there right after.
pub async fn sizes(
    State(jmap): State<Jmap>,
    Path(account): Path<String>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    // A POST: the webmail's session also has to send its CSRF token, as for every other POST.
    let owner = match jmap.inner.auth.account_for(&headers, client, true).await {
        Ok(owner) => owner,
        Err(err) => return err.into_response(),
    };
    if account != ids::account(owner.id) {
        return problem(StatusCode::NOT_FOUND, "Unknown account.");
    }
    let Ok(request) = serde_json::from_slice::<SizesRequest>(&body) else {
        return problem(StatusCode::BAD_REQUEST, "Expected {\"urls\": [...]}.");
    };
    if request.urls.len() > MAX_SIZES {
        return problem(StatusCode::BAD_REQUEST, &format!("At most {MAX_SIZES} addresses at once."));
    }
    let mut urls = request.urls;
    urls.sort();
    urls.dedup();
    let images = jmap.inner.remote_images.clone();
    let asking: FuturesUnordered<_> = urls
        .into_iter()
        .map(|url| {
            let images = images.clone();
            async move {
                let line = match images.size(owner.id, &url).await {
                    Ok(Some(size)) => json!({ "url": url, "width": size.width, "height": size.height }),
                    Ok(None) => json!({ "url": url, "width": null, "height": null }),
                    Err(_) => json!({ "url": url, "failed": true }),
                };
                Ok::<_, std::convert::Infallible>(Bytes::from(format!("{line}\n")))
            }
        })
        .collect();
    let mut response = Response::new(Body::from_stream(asking));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Reverse proxies pass each line on as it comes rather than all at the end.
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

#[derive(Deserialize)]
pub struct PictureQuery {
    email: String,
    /// `logo`: only the logo of the sender's company or domain.
    source: Option<String>,
    /// `1`: ask nobody outside, only what is known already.
    local: Option<String>,
}

/// The picture of a sender (docs/jmap-remote.md): a person's picture when the reader has one for the
/// address, or a company's logo or website icon. `404` when there is none.
pub async fn picture(
    State(jmap): State<Jmap>,
    Path(account): Path<String>,
    Query(query): Query<PictureQuery>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let owner = match jmap.inner.auth.account_for(&headers, client, false).await {
        Ok(owner) => owner,
        Err(err) => return err.into_response(),
    };
    if account != ids::account(owner.id) {
        return problem(StatusCode::NOT_FOUND, "Unknown account.");
    }
    if query.email.len() > 320 || !query.email.contains('@') {
        return problem(StatusCode::BAD_REQUEST, "That is not an address.");
    }
    let logo_only = query.source.as_deref() == Some("logo");
    let offline = matches!(query.local.as_deref(), Some("1" | "true"));
    let Some(found) = pictures::resolve(&jmap.inner, owner.id, query.email.trim(), logo_only, offline).await else {
        let mut response = problem(StatusCode::NOT_FOUND, "No picture for this sender.");
        // A contact photo or a profile picture can come any time; a company logo is asked for once
        // a week anyway.
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
        return response;
    };
    let mut response = match found {
        pictures::Found::Person { media_type, bytes } => {
            let etag = pictures::etag(&bytes);
            let unchanged = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.split(',').any(|tag| tag.trim() == etag || tag.trim() == "*"));
            let mut response =
                if unchanged { StatusCode::NOT_MODIFIED.into_response() } else { Response::new(Body::from(bytes)) };
            let headers = response.headers_mut();
            if !unchanged {
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(&media_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
                );
            }
            headers.insert("x-picture-kind", HeaderValue::from_static("photo"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
            if let Ok(etag) = HeaderValue::from_str(&etag) {
                headers.insert(header::ETAG, etag);
            }
            response
        }
        pictures::Found::Logo { media_type, bytes, kind, domain } => {
            let mut response = Response::new(Body::from(bytes));
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&media_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert("x-picture-kind", HeaderValue::from_static(kind));
            if let Some(domain) = domain.and_then(|domain| HeaderValue::from_str(&domain).ok()) {
                headers.insert("x-picture-domain", domain);
            }
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
            response
        }
    };
    let headers = response.headers_mut();
    pictures::sandbox(headers);
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    response
}
