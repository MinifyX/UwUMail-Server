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

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// The three paths the webmail is served on, exactly as `lib.rs` registers them.
    ///
    /// Two things can go wrong here and neither shows up in a normal test run, because a build
    /// without a webmail in it never registers these routes at all. Overlapping paths make axum
    /// panic while the router is built, which takes the server down at startup rather than failing
    /// one request. And `/mail/` fell between an exact `/mail` and a `/mail/{*rest}` that wants at
    /// least one character after the slash — which is how the address the webmail is built with
    /// came to answer with a not-found in 0.5.0.
    #[tokio::test]
    async fn the_webmail_answers_with_the_slash_as_well_as_without() {
        let app = Router::new()
            .route("/mail", get(async || "page"))
            .route("/mail/", get(async || "page"))
            .route("/mail/{*rest}", get(async || "below"));
        for path in ["/mail", "/mail/", "/mail/inbox", "/mail/assets/app.js"] {
            let request = Request::builder().uri(path).body(Body::empty()).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path} is served by the webmail");
        }
    }
}
