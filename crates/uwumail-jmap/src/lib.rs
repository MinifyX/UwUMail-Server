//! JMAP for the UwUMail server: RFC 8620 (core), RFC 8621 (mail, submission,
//! vacation responses), RFC 9661 (Sieve scripts), JMAP Calendars on the CalDAV calendars, RFC 9610
//! (JMAP Contacts) on the CardDAV address books and push over EventSource.
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
//! | `GET /jmap/ws` | Requests and push over a WebSocket (RFC 8887) |
//! | `POST /jmap/token` | A new app password for a program, to send as a bearer token |
//! | `GET /jmap/image/{accountId}?url=` | A message's remote picture, fetched by the server |
//! | `GET /jmap/picture/{accountId}?email=` | The logo or website icon of a company sender |

mod api;
pub mod auth;
mod blob;
pub mod dates;
mod email;
mod error;
mod ids;
mod jscal;
mod jscontact;
mod methods;
mod push;
mod remote;
mod scheduled;
pub mod safe_html;
mod session;
mod token;
mod ws;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{any, get, post};
use uwumail_smtp::Smtp;
use uwumail_smtp::egress::Egress;
use uwumail_smtp::pictures::SenderPictures;
use uwumail_store::Store;

pub use auth::{AuthError, Authenticator, ClientInfo};

/// Tells a person that a program created an app password for their account at `/jmap/token`:
/// account, the app password's name and the client's IP. The server hands in the portal's notice
/// (a mail and the activity entry); without one only the activity entry is written.
pub type AppPasswordNotice = Arc<
    dyn Fn(uwumail_store::Account, String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

pub const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_CALLS_IN_REQUEST: usize = 64;
pub const MAX_OBJECTS_IN_GET: usize = 500;
pub const MAX_OBJECTS_IN_SET: usize = 500;
/// The furthest ahead a message may be scheduled with EmailSubmission: 30 days (`maxDelayedSend`).
pub const MAX_DELAYED_SEND_SECS: i64 = 30 * 24 * 3600;

#[derive(Clone)]
pub struct Jmap {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub store: Store,
    pub smtp: Smtp,
    pub auth: auth::Authenticator,
    /// The way out for a message's remote pictures.
    pub egress: Egress,
    /// Logos and website icons of company senders, fetched the same way.
    pub pictures: Arc<SenderPictures>,
    /// How a person hears of an app password created at `/jmap/token`.
    pub notice: Option<AppPasswordNotice>,
    /// Wakes the sender of held submissions when one was added.
    pub wake: tokio::sync::Notify,
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
        let egress = Egress::direct();
        let pictures = Arc::new(SenderPictures::new(egress.clone()));
        Jmap { inner: Arc::new(Inner { auth, store, smtp, egress, pictures, notice: None, wake: tokio::sync::Notify::new() }) }
    }

    /// Remote pictures and sender pictures leave through `egress` instead of straight from the server.
    /// Called before the router is built.
    pub fn with_egress(self, egress: Egress) -> Jmap {
        let inner = Arc::into_inner(self.inner).expect("the egress is set before anything else holds the JMAP service");
        let pictures = Arc::new(SenderPictures::new(egress.clone()));
        Jmap { inner: Arc::new(Inner { egress, pictures, ..inner }) }
    }

    /// Hands in how people hear of app passwords created at `/jmap/token`. Called before the router
    /// is built.
    pub fn with_notice(self, notice: AppPasswordNotice) -> Jmap {
        let inner = Arc::into_inner(self.inner).expect("the notice is set before anything else holds the JMAP service");
        Jmap { inner: Arc::new(Inner { notice: Some(notice), ..inner }) }
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
            // `any`: HTTP/1.1 upgrades with GET, HTTP/2 WebSockets (RFC 8441) with CONNECT.
            .route("/jmap/ws", any(ws::handle))
            .route("/jmap/token", post(token::handle).layer(DefaultBodyLimit::max(16 * 1024)))
            .route("/jmap/image/{account}", get(remote::image))
            .route("/jmap/picture/{account}", get(remote::picture))
            .with_state(self.clone())
    }
}
