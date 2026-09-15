//! The web portal of the UwUMail server.
//!
//! - `/api/...` is a JSON API for the portal, logged in with a session cookie.
//!   Requests that change something need the session's CSRF token in the
//!   `X-CSRF-Token` header.
//! - `/`, `/login`, `/account/...`, `/admin/...` and `/setup` serve the React app
//!   from `web/`, embedded into the binary at build time.

mod assets;
mod error;
mod routes;
mod session;

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::routing::{get, patch, post};
use uwumail_smtp::AuthLimiter;
use uwumail_store::Store;

pub use error::{ApiError, ApiResult};
pub use session::{Admin, CSRF_HEADER, SESSION_LIFETIME_SECS, Session};

pub struct WebSettings {
    pub hostname: String,
    pub started: Instant,
}

#[derive(Clone)]
pub struct Web {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    settings: WebSettings,
    limiter: AuthLimiter,
}

impl Web {
    pub fn new(store: Store, settings: WebSettings) -> Web {
        Web { inner: Arc::new(Inner { store, settings, limiter: AuthLimiter::default() }) }
    }

    /// Whether this build contains the web app. Without it the server shows a simple landing page.
    pub fn has_app() -> bool {
        assets::index().is_some()
    }

    pub(crate) fn store(&self) -> &Store {
        &self.inner.store
    }

    pub(crate) fn settings(&self) -> &WebSettings {
        &self.inner.settings
    }

    pub(crate) fn limiter(&self) -> &AuthLimiter {
        &self.inner.limiter
    }

    pub fn router(&self) -> Router {
        let api = Router::new()
            .route("/api/info", get(routes::auth::info))
            .route("/api/session", get(routes::auth::session))
            .route("/api/auth/login", post(routes::auth::login))
            .route("/api/auth/logout", post(routes::auth::logout))
            .route("/api/account", get(routes::account::profile))
            .route("/api/account/preferences", patch(routes::account::update_preferences))
            .route("/api/admin/overview", get(routes::admin::overview))
            .route("/api", get(routes::not_found))
            .route("/api/{*rest}", get(routes::not_found).post(routes::not_found).patch(routes::not_found))
            .with_state(self.clone());

        let mut app = Router::new();
        if Web::has_app() {
            for path in ["/", "/login", "/setup", "/account", "/account/{*rest}", "/admin", "/admin/{*rest}"] {
                app = app.route(path, get(routes::app_page));
            }
            app = app.route("/assets/{*path}", get(routes::asset));
            for file in assets::root_files() {
                app = app.route(file.path, get(move || async move { assets::respond(file) }));
            }
        }
        api.merge(app)
    }
}
