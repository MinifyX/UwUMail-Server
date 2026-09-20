//! The webmail: its files under `/mail`, and who may open it.
//!
//! The interface lives in its own repository (MinifyX/UwUMail-Webmail) and talks to this server
//! over JMAP with the portal's session, so there is almost nothing to do here: hand out the files,
//! and answer whether this account may see them.

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::{ApiError, ApiResult};
use crate::session::Session;
use crate::{Web, assets};

/// The page itself. Every path below `/mail` hands out the same file; the interface routes itself.
pub async fn page(State(web): State<Web>) -> Response {
    if !web.webmail_enabled() {
        return ApiError::NotFound("the webmail".into()).into_response();
    }
    match assets::webmail_index() {
        Some(index) => assets::webmail_respond(index),
        None => ApiError::NotFound("the webmail".into()).into_response(),
    }
}

/// Everything below `/mail`: a file of the build if there is one, otherwise the page again.
///
/// The webmail routes itself in the browser, so `/mail/whatever` has to answer with the same
/// page. Only files that were built into the binary are ever handed out; nothing here reads
/// from disk, and a path that isn't a file simply becomes the page.
pub async fn below(State(web): State<Web>, UrlPath(rest): UrlPath<String>) -> Response {
    if !web.webmail_enabled() {
        return ApiError::NotFound("the webmail".into()).into_response();
    }
    match assets::webmail_find(&format!("/{rest}")) {
        Some(asset) if asset.path != "/index.html" => assets::webmail_respond(asset),
        _ => page(State(web)).await,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// Whether this account may use the webmail: the server's switch, then the account's own.
pub async fn access(State(web): State<Web>, session: Session) -> ApiResult<Json<Access>> {
    if !web.webmail_enabled() || !assets::has_webmail() {
        return Ok(Json(Access { allowed: false, reason: Some("server") }));
    }
    if !session.account.webmail || !session.account.has_mailbox() {
        return Ok(Json(Access { allowed: false, reason: Some("account") }));
    }
    Ok(Json(Access { allowed: true, reason: None }))
}

/// Whether this account should land in its mailbox after signing in to the portal.
///
/// The portal does the sending itself, in the browser: signing in is a JSON call, not a form, so
/// there is no redirect to follow here. Where a `?next=` may point is decided there too.
pub fn allowed_for(web: &Web, account: &uwumail_store::Account) -> bool {
    web.webmail_enabled() && account.webmail && account.has_mailbox()
}
