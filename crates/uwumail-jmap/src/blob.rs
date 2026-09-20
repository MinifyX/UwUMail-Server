//! Blob upload and download (RFC 8620, section 6).

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;

use crate::auth::ClientInfo;
use crate::{Jmap, email, ids};

fn problem(status: StatusCode, detail: &str) -> Response {
    let body = json!({ "type": "about:blank", "status": status.as_u16(), "detail": detail });
    (status, [(header::CONTENT_TYPE, "application/problem+json")], body.to_string()).into_response()
}

pub async fn upload(
    State(jmap): State<Jmap>,
    Path(account): Path<String>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let owner = match jmap.inner.auth.account_for(&headers, client, true).await {
        Ok(owner) => owner,
        Err(err) => return err.into_response(),
    };
    if account != ids::account(owner.id) {
        return problem(StatusCode::NOT_FOUND, "Unknown account.");
    }
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("application/octet-stream")
        .to_owned();
    match jmap.inner.store.upload(owner.id, &body, &media_type).await {
        Ok(hash) => (
            StatusCode::CREATED,
            Json(json!({ "accountId": account, "blobId": ids::blob(&hash), "type": media_type, "size": body.len() })),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(%err, "storing an upload failed");
            problem(StatusCode::INTERNAL_SERVER_ERROR, "The upload could not be stored.")
        }
    }
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    accept: Option<String>,
}

/// `filename*` value per RFC 5987.
fn encode_filename(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub async fn download(
    State(jmap): State<Jmap>,
    Path((account, blob_id, name)): Path<(String, String, String)>,
    Query(query): Query<DownloadQuery>,
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
    let Some(reference) = ids::parse_blob(&blob_id) else {
        return problem(StatusCode::NOT_FOUND, "Unknown blob.");
    };
    let store = &jmap.inner.store;
    if !store.blob_accessible(owner.id, reference.hash()).await.unwrap_or(false) {
        return problem(StatusCode::NOT_FOUND, "Unknown blob.");
    }
    let Ok(bytes) = store.blob(reference.hash()).await else {
        return problem(StatusCode::NOT_FOUND, "Unknown blob.");
    };
    let (bytes, detected) = match reference {
        ids::BlobRef::Whole(ref hash) => {
            let uploaded = store.upload_media_type(owner.id, hash).await.ok().flatten();
            (bytes, uploaded.unwrap_or_else(|| "message/rfc822".into()))
        }
        ids::BlobRef::Part(_, index) => match email::part_content(&bytes, index) {
            Some(part) => part,
            None => return problem(StatusCode::NOT_FOUND, "Unknown blob."),
        },
    };
    let content_type = query.accept.filter(|t| t.contains('/')).unwrap_or(detected);
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    // Always set, even when the name will not go into a header: this is what keeps a blob whose
    // content type the caller chose from being rendered on the portal own origin, and it must not
    // fall away quietly with the file name.
    let disposition = HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{}", encode_filename(&name)))
        .unwrap_or(HeaderValue::from_static("attachment"));
    headers.insert(header::CONTENT_DISPOSITION, disposition);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, immutable, max-age=31536000"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    response
}
