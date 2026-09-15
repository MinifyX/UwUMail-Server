//! The web portal of the UwUMail server.
//!
//! - `/api/...` is a JSON API for the portal, logged in with a session cookie.
//!   Requests that change something need the session's CSRF token in the
//!   `X-CSRF-Token` header.
//! - `/`, `/login`, `/account/...`, `/admin/...` and `/setup` serve the React app
//!   from `web/`, embedded into the binary at build time.

mod assets;
mod error;
mod logs;
mod routes;
mod session;
pub mod settings;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use uwumail_smtp::dnscheck::{DnsChecker, DomainReport};
use uwumail_smtp::{AuthLimiter, Smtp};
use uwumail_store::Store;

pub use error::{ApiError, ApiResult};
pub use logs::{LogBuffer, LogLine};
pub use routes::settings::OVERLAY_KEY as SETTINGS_OVERLAY_KEY;
pub use session::{Admin, CSRF_HEADER, SESSION_LIFETIME_SECS, Session};

pub struct WebSettings {
    pub hostname: String,
    pub started: Instant,
    /// The newest server log lines, when the server keeps them.
    pub logs: Option<Arc<LogBuffer>>,
    /// Changing server settings from the admin panel, when the server allows it.
    pub config: Option<Arc<dyn settings::SettingsBackend>>,
}

#[derive(Clone)]
pub struct Web {
    inner: Arc<Inner>,
}

struct Inner {
    smtp: Smtp,
    settings: WebSettings,
    limiter: AuthLimiter,
    dns: Option<DnsChecker>,
    /// The latest DNS check of each domain.
    reports: Mutex<HashMap<String, DomainReport>>,
}

impl Web {
    pub fn new(smtp: Smtp, settings: WebSettings) -> Web {
        let dns =
            DnsChecker::new().inspect_err(|err| tracing::warn!(%err, "DNS checks of domains are not available")).ok();
        Web {
            inner: Arc::new(Inner { smtp, settings, limiter: AuthLimiter::default(), dns, reports: Mutex::default() }),
        }
    }

    /// Whether this build contains the web app. Without it the server shows a simple landing page.
    pub fn has_app() -> bool {
        assets::index().is_some()
    }

    pub(crate) fn store(&self) -> &Store {
        self.inner.smtp.store()
    }

    pub(crate) fn smtp(&self) -> &Smtp {
        &self.inner.smtp
    }

    pub(crate) fn dns(&self) -> Option<&DnsChecker> {
        self.inner.dns.as_ref()
    }

    pub(crate) fn report(&self, domain: &str) -> Option<DomainReport> {
        self.inner.reports.lock().expect("reports poisoned").get(domain).cloned()
    }

    pub(crate) fn keep_report(&self, report: DomainReport) {
        self.inner.reports.lock().expect("reports poisoned").insert(report.domain.clone(), report);
    }

    pub(crate) fn forget_report(&self, domain: &str) {
        self.inner.reports.lock().expect("reports poisoned").remove(domain);
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
            .route("/api/password-links/{token}", get(routes::links::show).post(routes::links::choose))
            .route("/api/admin/overview", get(routes::admin::overview))
            .route("/api/admin/domains", get(routes::domains::list).post(routes::domains::create))
            .route("/api/admin/domains/{name}", get(routes::domains::detail).delete(routes::domains::remove))
            .route("/api/admin/domains/{name}/catch-all", put(routes::domains::set_catch_all))
            .route("/api/admin/domains/{name}/check", post(routes::domains::check))
            .route("/api/admin/domains/{name}/dkim/rotate", post(routes::domains::rotate_keys))
            .route("/api/admin/domains/{name}/dkim/activate", post(routes::domains::activate_keys))
            .route("/api/admin/domains/{name}/dkim/{selector}", delete(routes::domains::remove_key))
            .route("/api/admin/audit", get(routes::admin::audit))
            .route("/api/admin/queue", get(routes::queue::list))
            .route("/api/admin/queue/{id}/retry", post(routes::queue::retry))
            .route("/api/admin/queue/{id}", delete(routes::queue::drop))
            .route("/api/admin/logs", get(routes::queue::logs))
            .route("/api/admin/settings", get(routes::settings::show).patch(routes::settings::update))
            .route("/api/admin/people", get(routes::people::list).post(routes::people::create))
            .route(
                "/api/admin/people/{login}",
                get(routes::people::detail).patch(routes::people::update).delete(routes::people::trash),
            )
            .route("/api/admin/people/{login}/restore", post(routes::people::restore))
            .route("/api/admin/people/{login}/purge", post(routes::people::purge))
            .route("/api/admin/people/{login}/password-link", post(routes::people::password_link))
            .route("/api/admin/people/{login}/password", put(routes::people::set_password))
            .route("/api/admin/people/{login}/aliases", post(routes::people::add_alias))
            .route("/api/admin/people/{login}/aliases/{address}", delete(routes::people::remove_alias))
            .route("/api", get(routes::not_found))
            .route(
                "/api/{*rest}",
                get(routes::not_found)
                    .post(routes::not_found)
                    .patch(routes::not_found)
                    .put(routes::not_found)
                    .delete(routes::not_found),
            )
            .with_state(self.clone());

        let mut app = Router::new();
        if Web::has_app() {
            for path in [
                "/",
                "/login",
                "/setup",
                "/password/{token}",
                "/account",
                "/account/{*rest}",
                "/admin",
                "/admin/{*rest}",
            ] {
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
