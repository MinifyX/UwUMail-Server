//! JMAP for the UwUMail server: RFC 8620 (core), RFC 8621 (mail, submission,
//! vacation responses), RFC 9661 (Sieve scripts), JMAP Calendars on the CalDAV calendars, RFC 9610
//! (JMAP Contacts) on the CardDAV address books, push over EventSource and Web Push (RFC 8030).
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
//! | `GET /jmap/picture/{accountId}?email=` | The picture of a sender: a person's, or a company's logo |
//! | `GET /avatar/{hash}` | Libravatar: the public pictures of this server's addresses |
//!
//! `Email/unsubscribe` has the server send a newsletter's one-click unsubscription (RFC 8058).
//!
//! [`Jmap::run_web_push`] pushes changes to the push subscriptions (RFC 8620, 7.2) over Web Push.

mod api;
pub mod auth;
pub mod availability;
mod blob;
pub mod calendar_alerts;
pub mod dates;
mod email;
mod error;
mod ids;
mod jscal;
mod jscontact;
mod methods;
mod pictures;
mod push;
mod remote;
pub mod safe_html;
mod scheduled;
mod session;
mod sharing;
mod timezones;
mod token;
mod webpush;
mod ws;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{any, get, post};
use uwumail_smtp::Smtp;
use uwumail_smtp::avatars::{AvatarNet, Avatars, LiveNet};
use uwumail_smtp::egress::Egress;
use uwumail_smtp::pictures::SenderPictures;
use uwumail_store::Store;

pub use auth::{AuthError, Authenticator, ClientInfo, Login};
pub use methods::unsubscribe::UnsubscribeTransport;
pub use webpush::{PushMessage, PushTiming, PushTransport};

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
    /// Pictures of people from elsewhere: linked contact photos and Libravatar.
    pub avatars: Arc<Avatars>,
    /// The addresses this server answers Libravatar for, by their hashes.
    pub libravatar: pictures::Provider,
    /// How a person hears of an app password created at `/jmap/token`.
    pub notice: Option<AppPasswordNotice>,
    /// Wakes the sender of held submissions when one was added.
    pub wake: tokio::sync::Notify,
    /// Push subscriptions: the VAPID key and the way out to push services.
    pub push: webpush::WebPush,
    /// One-click unsubscriptions sent lately, for their limits.
    pub unsubscribes: methods::unsubscribe::Unsubscribes,
    /// Where they go instead of the egress, in tests.
    pub unsubscribe_transport: Option<Arc<dyn UnsubscribeTransport>>,
    /// Reminder mails of calendar alerts sent lately, for their limits.
    pub reminders: calendar_alerts::ReminderLimits,
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
        let avatars = Arc::new(Avatars::new(Arc::new(LiveNet::new(egress.clone()))));
        let push = webpush::WebPush::new(store.clone(), egress.clone(), smtp.hostname());
        Jmap {
            inner: Arc::new(Inner {
                auth,
                libravatar: pictures::Provider::new(store.clone()),
                store,
                smtp,
                egress,
                pictures,
                avatars,
                notice: None,
                wake: tokio::sync::Notify::new(),
                push,
                unsubscribes: Default::default(),
                unsubscribe_transport: None,
                reminders: Default::default(),
            }),
        }
    }

    /// Remote pictures and sender pictures leave through `egress` instead of straight from the server.
    /// Called before the router is built.
    pub fn with_egress(self, egress: Egress) -> Jmap {
        let inner = Arc::into_inner(self.inner).expect("the egress is set before anything else holds the JMAP service");
        let pictures = Arc::new(SenderPictures::new(egress.clone()));
        let avatars = Arc::new(Avatars::new(Arc::new(LiveNet::new(egress.clone()))));
        let push = inner.push.clone().with_egress(egress.clone());
        Jmap { inner: Arc::new(Inner { egress, pictures, avatars, push, ..inner }) }
    }

    /// Pictures of people from elsewhere (linked contact photos, Libravatar) come through `net`
    /// instead of the egress and DNS: for tests. Called after [`Jmap::with_egress`] and before the
    /// router is built.
    pub fn with_avatar_net(self, net: Arc<dyn AvatarNet>) -> Jmap {
        let inner =
            Arc::into_inner(self.inner).expect("the network is set before anything else holds the JMAP service");
        Jmap { inner: Arc::new(Inner { avatars: Arc::new(Avatars::new(net)), ..inner }) }
    }

    /// Push messages (docs/jmap-push.md) leave through `transport` instead of the egress, which
    /// reaches public https addresses only. For tests with a push service on the same machine;
    /// called after [`Jmap::with_egress`] and before the router is built.
    pub fn with_push_transport(self, transport: Arc<dyn PushTransport>) -> Jmap {
        let inner =
            Arc::into_inner(self.inner).expect("the transport is set before anything else holds the JMAP service");
        let push = inner.push.clone().with_transport(transport);
        Jmap { inner: Arc::new(Inner { push, ..inner }) }
    }

    /// One-click unsubscriptions (docs/jmap-unsubscribe.md) are sent through `transport` instead of the
    /// egress, which reaches public https addresses only. For tests with a newsletter on the same
    /// machine; called before the router is built.
    pub fn with_unsubscribe_transport(self, transport: Arc<dyn UnsubscribeTransport>) -> Jmap {
        let inner =
            Arc::into_inner(self.inner).expect("the transport is set before anything else holds the JMAP service");
        Jmap { inner: Arc::new(Inner { unsubscribe_transport: Some(transport), ..inner }) }
    }

    /// Bundles pushes with other waits than the usual two seconds and five seconds between pushes:
    /// for tests. Called before the router is built.
    pub fn with_push_timing(self, timing: PushTiming) -> Jmap {
        let inner = Arc::into_inner(self.inner).expect("the timing is set before anything else holds the JMAP service");
        let push = inner.push.clone().with_timing(timing);
        Jmap { inner: Arc::new(Inner { push, ..inner }) }
    }

    /// Hands in how people hear of app passwords created at `/jmap/token`. Called before the router
    /// is built.
    pub fn with_notice(self, notice: AppPasswordNotice) -> Jmap {
        let inner = Arc::into_inner(self.inner).expect("the notice is set before anything else holds the JMAP service");
        Jmap { inner: Arc::new(Inner { notice: Some(notice), ..inner }) }
    }

    /// The API and uploads read their bodies themselves, after the login and up to
    /// [`MAX_REQUEST_BYTES`] and [`MAX_UPLOAD_BYTES`].
    pub fn router(&self) -> Router {
        Router::new()
            .route("/.well-known/jmap", get(session::handle))
            .route("/jmap/session", get(session::handle))
            .route("/jmap/api", post(api::handle))
            .route("/jmap/api/", post(api::handle))
            .route("/jmap/upload/{account}", post(blob::upload))
            .route("/jmap/upload/{account}/", post(blob::upload))
            .route("/jmap/download/{account}/{blob}/{name}", get(blob::download))
            .route("/jmap/eventsource", get(push::handle))
            .route("/jmap/eventsource/", get(push::handle))
            // `any`: HTTP/1.1 upgrades with GET, HTTP/2 WebSockets (RFC 8441) with CONNECT.
            .route("/jmap/ws", any(ws::handle))
            .route("/jmap/token", post(token::handle).layer(DefaultBodyLimit::max(16 * 1024)))
            .route("/jmap/image/{account}", get(remote::image))
            .route("/jmap/picture/{account}", get(remote::picture))
            .route("/avatar/{hash}", get(pictures::libravatar))
            .with_state(self.clone())
    }
}
