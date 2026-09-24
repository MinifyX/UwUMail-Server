//! The server's own look: a stylesheet with the chosen colours, the logo, and what the apps need to
//! know about both.
//!
//! Name, colour and the mascot are ordinary settings (`brand.*`). The logo is a file and lives in
//! its own settings row, so it is in every backup and needs nothing on disk.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use data_encoding::{BASE64, HEXLOWER};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uwumail_smtp::palette;

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;

const LOGO_KEY: &str = "brand.logo";
/// Big enough for any sensible logo, small enough to send along with every page.
pub const MAX_LOGO_BYTES: usize = 512 * 1024;

#[derive(Serialize, Deserialize)]
struct StoredLogo {
    #[serde(rename = "type")]
    content_type: String,
    /// Base64 of the file.
    data: String,
    /// The start of its SHA-256, so browsers fetch it again only when it changed.
    version: String,
}

/// What kind of picture `bytes` is, judged by its content and never by what the browser claimed.
fn logo_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let start = text.trim_start_matches('\u{feff}').trim_start();
    let head: String = start.chars().take(1024).collect::<String>().to_ascii_lowercase();
    (head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg"))).then_some("image/svg+xml")
}

async fn stored_logo(web: &Web) -> Option<StoredLogo> {
    let raw = web.store().setting(LOGO_KEY).await.ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Name, colour, mascot and logo, for the login page, the portal and the webmail.
pub async fn brand_json(web: &Web) -> Value {
    let brand = web.smtp().brand();
    let logo = stored_logo(web).await.map(|logo| format!("/branding/logo?v={}", logo.version));
    json!({
        "name": brand.name(),
        "custom": brand.is_custom() || logo.is_some(),
        "color": (!brand.color.trim().is_empty()).then(|| brand.color.trim().to_ascii_lowercase()),
        "mascot": brand.mascot,
        "logo": logo,
    })
}

/// `/branding.css`: the chosen colours over the built-in ones; empty for the UwUMail pink. The
/// portal and the webmail both link it, so they change together.
pub async fn stylesheet(State(web): State<Web>) -> Response {
    let brand = web.smtp().brand();
    let css = palette::parse_hex(&brand.color).map(|accent| palette::palette(accent).css()).unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/css; charset=utf-8")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        css,
    )
        .into_response()
}

/// `/branding/logo`: the uploaded logo. An SVG could carry scripts, so it is served in a sandbox
/// where none of them run, even when someone opens the file on its own.
pub async fn logo(State(web): State<Web>) -> Response {
    let Some(logo) = stored_logo(&web).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(bytes) = BASE64.decode(logo.data.as_bytes()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&logo.content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; img-src data:; sandbox"),
    );
    (headers, bytes).into_response()
}

/// Puts a new logo in place. The body is the file itself.
pub async fn upload_logo(State(web): State<Web>, Admin(session): Admin, body: Bytes) -> ApiResult<Json<Value>> {
    if body.is_empty() {
        return Err(ApiError::Rule("logoEmpty", "the file is empty".into()));
    }
    if body.len() > MAX_LOGO_BYTES {
        return Err(ApiError::Rule("logoTooLarge", format!("at most {} KB", MAX_LOGO_BYTES / 1024)));
    }
    let Some(content_type) = logo_type(&body) else {
        return Err(ApiError::Rule("logoType", "PNG, JPEG, WebP or SVG".into()));
    };
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &body);
    let stored = StoredLogo {
        content_type: content_type.into(),
        data: BASE64.encode(&body),
        version: HEXLOWER.encode(&digest.as_ref()[..8]),
    };
    let raw = serde_json::to_string(&stored).map_err(|_| ApiError::Internal)?;
    web.store().set_setting(LOGO_KEY, &raw).await?;
    audit(&web, &session, "brand.logo", "", json!({ "type": content_type, "bytes": body.len() })).await;
    Ok(Json(brand_json(&web).await))
}

/// Goes back to the UwUMail logo.
pub async fn remove_logo(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    if web.store().delete_setting(LOGO_KEY).await? {
        audit(&web, &session, "brand.logoRemoved", "", json!({})).await;
    }
    Ok(Json(brand_json(&web).await))
}

#[derive(Deserialize)]
pub struct PaletteQuery {
    color: String,
}

/// The colours a choice would give, so the settings page can show them before saving.
pub async fn preview(_admin: Admin, Query(query): Query<PaletteQuery>) -> ApiResult<Json<Value>> {
    let accent =
        palette::parse_hex(&query.color).ok_or_else(|| ApiError::Rule("brandColor", "a colour like #ff4d8d".into()))?;
    let palette = palette::palette(accent);
    let tokens = |tokens: &palette::Tokens| {
        Value::Object(tokens.iter().map(|(name, value)| ((*name).to_owned(), Value::from(value.as_str()))).collect())
    };
    Ok(Json(json!({ "light": tokens(&palette.light), "dark": tokens(&palette.dark) })))
}

#[cfg(test)]
mod tests {
    use super::logo_type;

    #[test]
    fn logos_are_known_by_their_content() {
        assert_eq!(logo_type(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(logo_type(&[0xff, 0xd8, 0xff, 0xe0, 1]), Some("image/jpeg"));
        assert_eq!(logo_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(logo_type(b"  <svg xmlns=\"http://www.w3.org/2000/svg\"/>"), Some("image/svg+xml"));
        assert_eq!(logo_type(b"<?xml version=\"1.0\"?>\n<svg/>"), Some("image/svg+xml"));
        assert_eq!(logo_type(b"<html><script>alert(1)</script>"), None);
        assert_eq!(logo_type(b"GIF89a"), None);
    }
}
