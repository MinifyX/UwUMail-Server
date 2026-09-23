//! JMAP for the UwUMail server: RFC 8620 (core), RFC 8621 (mail, submission,
//! vacation responses), RFC 9661 (Sieve scripts) and push over EventSource.
//!
//! [`Jmap::router`] serves:
//!
//! | Route | Purpose |
//! | --- | --- |
//! | `GET /.well-known/jmap`, `GET /jmap/session` | Session resource |
//! | `POST /jmap/api` | Method calls |
//! | `POST /jmap/upload/{accountId}` | Blob upload |
//! | `GET /jmap/download/{accountId}/{blobId}/{name}` | Blob download |
//! | `GET /jmap/eventsource` | Push |

mod api;
pub mod auth;
mod blob;
pub mod dates;
mod email;
mod error;
mod ids;
mod methods;
mod push;
pub mod safe_html;
mod session;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use uwumail_smtp::Smtp;
use uwumail_store::Store;

pub use auth::{AuthError, Authenticator, ClientInfo};

pub const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_CALLS_IN_REQUEST: usize = 64;
pub const MAX_OBJECTS_IN_GET: usize = 500;
pub const MAX_OBJECTS_IN_SET: usize = 500;

#[derive(Clone)]
pub struct Jmap {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub store: Store,
    pub smtp: Smtp,
    pub auth: auth::Authenticator,
}

impl Jmap {
    pub fn new(smtp: Smtp) -> Jmap {
        Jmap::with_webmail(smtp, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)))
    }

    /// The same, but told whether the webmail is switched on: only then does signing in with the
    /// portal's session work here.
    pub fn with_webmail(smtp: Smtp, webmail: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Jmap {
        let store = smtp.store().clone();
        let mut auth = auth::Authenticator::new(store.clone());
        auth.watch_webmail(webmail);
        Jmap { inner: Arc::new(Inner { auth, store, smtp }) }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/.well-known/jmap", get(session::handle))
            .route("/jmap/session", get(session::handle))
            .route("/jmap/api", post(api::handle).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES)))
            .route("/jmap/api/", post(api::handle).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES)))
            .route("/jmap/upload/{account}", post(blob::upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)))
            .route("/jmap/upload/{account}/", post(blob::upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)))
            .route("/jmap/download/{account}/{blob}/{name}", get(blob::download))
            .route("/jmap/eventsource", get(push::handle))
            .route("/jmap/eventsource/", get(push::handle))
            .with_state(self.clone())
    }
}
