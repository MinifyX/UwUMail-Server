//! Remote pictures of a message, fetched by the server instead of the reader (docs/jmap-remote.md).
//!
//! A picture loaded by the webmail or an app tells its sender when the message was opened and from which
//! address. Asked for here, the sender sees the server — or, with `[egress] proxy` set, a VPN — and never
//! the person reading. Whether a message's pictures are shown at all stays the reader's choice: nothing
//! is rewritten in the message, the client asks for each picture once it may.

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use uwumail_smtp::egress::EgressError;

use crate::auth::ClientInfo;
use crate::{Jmap, ids};

/// Bigger pictures than this are not passed on. Newsletters stay far below it.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_URL_LENGTH: usize = 4096;
const ACCEPT_IMAGES: &str = "image/avif,image/webp,image/apng,image/svg+xml,image/*;q=0.8";

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
    let fetched = match jmap.inner.egress.get(&query.url, ACCEPT_IMAGES, MAX_IMAGE_BYTES).await {
        Ok(fetched) => fetched,
        Err(EgressError::NotAllowed(why)) => return problem(StatusCode::BAD_REQUEST, &why),
        Err(EgressError::Timeout) => return problem(StatusCode::GATEWAY_TIMEOUT, "The picture did not come in time."),
        Err(err) => return problem(StatusCode::BAD_GATEWAY, &format!("The picture could not be fetched: {err}.")),
    };
    let Some(media_type) = picture_type(&fetched.media_type, &fetched.body) else {
        return problem(StatusCode::BAD_GATEWAY, "That is not a picture.");
    };
    let mut response = Response::new(Body::from(fetched.body));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, media_type);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
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

/// The type to hand the picture on with: the one it was sent with when that is a picture, otherwise what
/// its first bytes say. Anything else is not passed on.
fn picture_type(sent: &str, body: &[u8]) -> Option<HeaderValue> {
    if let Some(subtype) = sent.strip_prefix("image/")
        && !subtype.is_empty()
        && subtype.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    {
        return HeaderValue::from_str(sent).ok();
    }
    let sniffed = match body {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [0, 0, 1, 0, ..] => "image/x-icon",
        [b'B', b'M', ..] => "image/bmp",
        _ => return None,
    };
    Some(HeaderValue::from_static(sniffed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pictures_are_passed_on() {
        assert_eq!(picture_type("image/png", b"whatever").unwrap(), "image/png");
        assert_eq!(picture_type("image/svg+xml", b"<svg/>").unwrap(), "image/svg+xml");
        assert_eq!(picture_type("", b"GIF89a").unwrap(), "image/gif");
        assert_eq!(picture_type("application/octet-stream", b"\xff\xd8\xff\xe0").unwrap(), "image/jpeg");
        assert_eq!(picture_type("text/html", b"<html>").map(|_| ()), None);
        assert_eq!(picture_type("image/", b"<html>").map(|_| ()), None);
        assert_eq!(picture_type("image/png\r\nx: y", b"<html>").map(|_| ()), None);
    }
}
