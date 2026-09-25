//! The web portal of the UwUMail server.
//!
//! - `/api/...` is a JSON API for the portal, logged in with a session cookie.
//!   Requests that change something need the session's CSRF token in the
//!   `X-CSRF-Token` header.
//! - `/`, `/login`, `/account/...`, `/admin/...`, `/setup`, `/password/{token}` and
//!   `/forwarding/{token}` serve the React app from `web/`, embedded into the binary at build time.
//! - `/mail` serves the webmail, built from its own repository, when the build has one and it is on.

mod assets;
mod cloudflare;
mod error;
pub mod gateway;
mod health;
pub mod host;
mod login;
mod logs;
pub mod loki;
mod notices;
pub mod profile_signing;
mod routes;
mod session;
pub mod settings;
mod updates;
pub mod vpn;
mod webauthn;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use uwumail_smtp::dnscheck::{DnsChecker, DomainReport};
use uwumail_smtp::{AuthLimiter, Smtp};
use uwumail_store::Store;

pub use error::{ApiError, ApiResult};
pub use health::{CertificateSource, CertificateStatus};
pub use logs::{LogBuffer, LogLine, LogSource};
pub use loki::{Loki, LokiConfig};
pub use routes::settings::OVERLAY_KEY as SETTINGS_OVERLAY_KEY;
pub use session::{Admin, CSRF_HEADER, SESSION_LIFETIME_SECS, Session};

use std::sync::atomic::{AtomicBool, Ordering};

pub struct WebSettings {
    pub hostname: String,
    pub started: Instant,
    /// The newest server log lines, when the server keeps them.
    pub logs: Option<Arc<LogBuffer>>,
    /// Sending the log to Grafana Loki, when the server can.
    pub loki: Option<Arc<Loki>>,
    /// Changing server settings from the admin panel, when the server allows it.
    pub config: Option<Arc<dyn settings::SettingsBackend>>,
    /// The certificate in use, for the health overview.
    pub certificate: Option<CertificateSource>,
    /// Whether the webmail is served. Shared with the settings backend, so changing it in the
    /// admin panel takes effect without a restart.
    pub webmail: Arc<AtomicBool>,
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
    /// When DNS checks and the delivery probe last ran.
    last_health_check: Mutex<Option<i64>>,
    /// Held while an admin-requested check runs, so clicks do not pile up.
    health_check: tokio::sync::Mutex<()>,
    login: login::LoginState,
    /// The one-time code of the setup assistant while the server has no admin.
    setup_code: Mutex<Option<String>>,
    /// The latest run of the setup checks.
    server_check: Mutex<Option<uwumail_smtp::servercheck::ServerCheck>>,
    /// Apple configuration profiles waiting for their one download, by token.
    apple_profiles: Mutex<HashMap<String, routes::apps::PendingProfile>>,
    /// The UwUMail Gateway, once the server plugged it in.
    gateway: std::sync::OnceLock<Arc<dyn gateway::GatewayBackend>>,
    /// The helper on the machine, once the server plugged it in.
    host: std::sync::OnceLock<Arc<dyn host::HostBackend>>,
    /// Backups, once the server plugged them in.
    backups: std::sync::OnceLock<uwumail_backup::Backups>,
    /// The way out for a message's remote pictures, once the server plugged it in.
    egress: std::sync::OnceLock<uwumail_smtp::egress::Egress>,
    /// The certificate and key Apple configuration profiles are signed with, once plugged in.
    profile_key: std::sync::OnceLock<profile_signing::ProfileKeySource>,
}

impl Web {
    pub fn new(smtp: Smtp, settings: WebSettings) -> Web {
        let dns =
            DnsChecker::new().inspect_err(|err| tracing::warn!(%err, "DNS checks of domains are not available")).ok();
        Web {
            inner: Arc::new(Inner {
                smtp,
                settings,
                limiter: AuthLimiter::default(),
                dns,
                reports: Mutex::default(),
                last_health_check: Mutex::default(),
                health_check: tokio::sync::Mutex::default(),
                login: login::LoginState::default(),
                setup_code: Mutex::default(),
                server_check: Mutex::default(),
                apple_profiles: Mutex::default(),
                gateway: std::sync::OnceLock::new(),
                host: std::sync::OnceLock::new(),
                backups: std::sync::OnceLock::new(),
                egress: std::sync::OnceLock::new(),
                profile_key: std::sync::OnceLock::new(),
            }),
        }
    }

    /// Whether this build contains the web app. Without it the server shows a simple landing page.
    pub fn has_app() -> bool {
        assets::index().is_some()
    }

    /// Whether the webmail is served right now: built into this binary and switched on.
    pub(crate) fn webmail_enabled(&self) -> bool {
        assets::has_webmail() && self.inner.settings.webmail.load(Ordering::Relaxed)
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

    pub(crate) fn last_health_check(&self) -> Option<i64> {
        *self.inner.last_health_check.lock().expect("health check time poisoned")
    }

    pub(crate) fn mark_health_checked(&self) {
        *self.inner.last_health_check.lock().expect("health check time poisoned") = Some(health::unix_now());
    }

    /// Lets the portal show and pair the UwUMail Gateway. Only the first call counts.
    pub fn set_gateway(&self, gateway: Arc<dyn gateway::GatewayBackend>) {
        let _ = self.inner.gateway.set(gateway);
    }

    /// Lets the portal set up and run backups. Only the first call counts.
    pub fn set_backups(&self, backups: uwumail_backup::Backups) {
        let _ = self.inner.backups.set(backups);
    }

    pub(crate) fn backups(&self) -> Option<&uwumail_backup::Backups> {
        self.inner.backups.get()
    }

    /// Lets the portal show how remote pictures leave the server. Only the first call counts.
    pub fn set_egress(&self, egress: uwumail_smtp::egress::Egress) {
        let _ = self.inner.egress.set(egress);
    }

    /// Lets Apple configuration profiles be signed with the server's certificate (docs/calendars.md
    /// explains why it matters). Only the first call counts.
    pub fn set_profile_key(&self, source: profile_signing::ProfileKeySource) {
        let _ = self.inner.profile_key.set(source);
    }

    pub(crate) fn profile_key(&self) -> Option<&profile_signing::ProfileKeySource> {
        self.inner.profile_key.get()
    }

    pub(crate) fn egress(&self) -> Option<&uwumail_smtp::egress::Egress> {
        self.inner.egress.get()
    }

    pub(crate) fn gateway(&self) -> Option<&Arc<dyn gateway::GatewayBackend>> {
        self.inner.gateway.get()
    }

    /// Lets the portal look after the machine the server runs on. Only the first call counts.
    pub fn set_host(&self, host: Arc<dyn host::HostBackend>) {
        let _ = self.inner.host.set(host);
    }

    pub(crate) fn host(&self) -> Option<&Arc<dyn host::HostBackend>> {
        self.inner.host.get()
    }

    pub(crate) fn settings(&self) -> &WebSettings {
        &self.inner.settings
    }

    pub(crate) fn login_state(&self) -> &login::LoginState {
        &self.inner.login
    }

    pub(crate) fn limiter(&self) -> &AuthLimiter {
        &self.inner.limiter
    }

    /// Hands every network the portal turns away to `reporter` as well, so it can be kept out at
    /// the UwUMail Gateway instead of arriving here again.
    pub fn report_blocks_to(&self, reporter: Option<uwumail_smtp::Reporter>) {
        self.inner.limiter.report_to(reporter);
    }

    pub fn router(&self) -> Router {
        let api = Router::new()
            .route("/api/info", get(routes::auth::info))
            // The look of the portal and the webmail, for everyone, logged in or not.
            .route("/branding.css", get(routes::branding::stylesheet))
            .route("/branding/logo", get(routes::branding::logo))
            .route("/api/session", get(routes::auth::session))
            .route("/api/auth/login", post(routes::auth::login))
            .route("/api/auth/logout", post(routes::auth::logout))
            .route("/api/auth/second-factor", post(routes::auth::second_factor))
            .route("/api/setup", get(routes::setup::status).post(routes::setup::complete))
            .route("/api/setup/code", post(routes::setup::verify_code))
            .route("/api/setup/backup/look", post(routes::setup::backup_look))
            .route("/api/setup/backup/restore", post(routes::setup::backup_restore))
            .route("/api/admin/setup/check", get(routes::setup::last_check).post(routes::setup::run_check))
            .route("/api/admin/setup/reachability", post(routes::gateway::reachability))
            .route("/api/admin/host", get(routes::host::show))
            .route("/api/admin/host/jobs", post(routes::host::ask))
            .route(
                "/api/admin/gateway",
                get(routes::gateway::show).post(routes::gateway::pair).delete(routes::gateway::forget),
            )
            .route("/api/admin/gateway/jobs", post(routes::gateway::ask))
            .route("/api/admin/setup/test-mail", post(routes::setup::send_test_mail))
            .route("/api/admin/setup/test-mail/{id}", get(routes::setup::test_mail_status))
            .route("/api/auth/passkey/options", post(routes::auth::passkey_options))
            .route("/api/auth/passkey", post(routes::auth::passkey_login))
            .route("/api/account", get(routes::account::profile))
            .route("/api/account/preferences", patch(routes::account::update_preferences))
            .route("/api/account/webmail", get(routes::webmail::access))
            .route("/api/account/security", get(routes::security::overview))
            .route("/api/account/password", post(routes::security::change_password))
            .route("/api/account/totp", post(routes::security::start_totp).delete(routes::security::disable_totp))
            .route("/api/account/totp/confirm", post(routes::security::confirm_totp))
            .route("/api/account/recovery-codes", post(routes::security::new_recovery_codes))
            .route("/api/account/apps-need-app-password", put(routes::security::set_apps_need_app_password))
            .route("/api/account/app-passwords", post(routes::security::create_app_password))
            .route("/api/account/app-passwords/{id}", delete(routes::security::revoke_app_password))
            .route("/api/account/apple-profiles", post(routes::apps::create_apple_profile))
            .route("/api/apple-profiles/{token}", get(routes::apps::download_apple_profile))
            .route("/mail/config-v1.1.xml", get(routes::apps::autoconfig))
            .route("/.well-known/autoconfig/mail/config-v1.1.xml", get(routes::apps::autoconfig))
            .route("/autodiscover/autodiscover.xml", post(routes::apps::autodiscover))
            .route("/Autodiscover/Autodiscover.xml", post(routes::apps::autodiscover))
            .route("/api/account/sessions/{id}", delete(routes::security::end_session))
            .route("/api/account/sessions/end-others", post(routes::security::end_other_sessions))
            .route("/api/account/passkeys/options", post(routes::security::passkey_options))
            .route("/api/account/forwarding", get(routes::mailbox::forwarding))
            .route("/api/account/forwarding/targets", post(routes::mailbox::add_target))
            .route("/api/account/forwarding/targets/{id}", delete(routes::mailbox::remove_target))
            .route("/api/account/forwarding/keep-copy", put(routes::mailbox::set_keep_copy))
            .route("/api/account/vacation", get(routes::mailbox::vacation).put(routes::mailbox::set_vacation))
            .route("/api/account/fetch", get(routes::fetch::list).post(routes::fetch::create))
            .route("/api/account/fetch/discover", post(routes::fetch::discover))
            .route("/api/account/fetch/{id}", patch(routes::fetch::update).delete(routes::fetch::delete))
            .route("/api/account/fetch/{id}/run", post(routes::fetch::fetch_now))
            .route("/api/account/fetch/{id}/existing", post(routes::fetch::take_existing))
            .route("/api/account/calendars", get(routes::calendars::list))
            .route("/api/account/calendars/{id}/shares", put(routes::calendars::share))
            .route("/api/account/calendars/{id}/shares/{account}", delete(routes::calendars::unshare))
            .route("/api/account/shared-calendars/{id}", delete(routes::calendars::leave))
            .route("/api/account/addresses", get(routes::own::addresses))
            .route("/api/account/aliases", post(routes::own::create_alias))
            .route("/api/account/aliases/{address}", delete(routes::own::delete_alias))
            .route("/api/account/storage", get(routes::own::storage))
            .route("/api/account/greylist", get(routes::greylist::waiting))
            .route("/api/account/greylist/{id}", post(routes::greylist::decide))
            .route("/api/account/spam", get(routes::spam::account_overview))
            .route("/api/account/spam/learn-folders", post(routes::spam::account_learn))
            .route("/api/account/spam/limits", put(routes::spam::account_set_limits))
            .route(
                "/api/account/spam/senders",
                get(routes::spam::account_senders).post(routes::spam::account_add_sender),
            )
            .route("/api/account/spam/senders/{id}", delete(routes::spam::account_remove_sender))
            .route("/api/account/spam/rules", get(routes::rules::account_list).post(routes::rules::account_create))
            .route("/api/account/spam/rules/bulk", post(routes::rules::account_bulk))
            .route("/api/account/spam/rules/import", post(routes::rules::account_import))
            .route("/api/account/spam/rules/export", get(routes::rules::account_export))
            .route("/api/account/spam/rules/{type}/{id}", patch(routes::rules::account_change))
            .route("/api/account/spam/words", get(routes::words::account_words).post(routes::words::account_add_words))
            .route("/api/account/spam/words/{id}", delete(routes::words::account_remove_word))
            .route("/api/account/spam/word-sources", post(routes::words::account_subscribe))
            .route("/api/account/spam/word-sources/{id}", delete(routes::words::account_unsubscribe))
            .route("/api/account/spam/word-sources/{id}/refresh", post(routes::words::account_refresh_source))
            .route("/api/account/mailboxes/{role}/empty", post(routes::own::empty_mailbox))
            .route("/api/forwarding-links/{token}", get(routes::mailbox::show_link))
            .route("/api/forwarding-links/{token}/confirm", post(routes::mailbox::confirm_link))
            .route("/api/forwarding-links/{token}/decline", post(routes::mailbox::decline_link))
            .route("/api/account/passkeys", post(routes::security::add_passkey))
            .route("/api/account/passkeys/{id}", delete(routes::security::remove_passkey))
            .route("/api/password-links/{token}", get(routes::links::show).post(routes::links::choose))
            .route("/api/admin/overview", get(routes::admin::overview))
            .route("/api/admin/health", get(routes::admin::health))
            .route("/api/admin/health/check", post(routes::admin::check_health))
            .route("/api/admin/updates", get(routes::updates::show).put(routes::updates::save))
            .route("/api/admin/updates/check", post(routes::updates::check))
            .route("/api/admin/backups", get(routes::backups::show).put(routes::backups::save))
            .route("/api/admin/backups/test", post(routes::backups::test))
            .route("/api/admin/backups/forget-host-key", post(routes::backups::forget_host_key))
            .route("/api/admin/backups/run", post(routes::backups::run))
            .route("/api/admin/backups/snapshots", get(routes::backups::snapshots))
            .route("/api/admin/backups/restore", post(routes::backups::restore).delete(routes::backups::forget_restore))
            .route("/api/admin/backups/recovery-key", post(routes::backups::recovery_key))
            .route("/api/admin/domains", get(routes::domains::list).post(routes::domains::create))
            .route("/api/admin/domains/{name}", get(routes::domains::detail).delete(routes::domains::remove))
            .route("/api/admin/domains/{name}/catch-all", put(routes::domains::set_catch_all))
            .route("/api/admin/domains/{name}/forwards", put(routes::domains::set_forward_address))
            .route("/api/admin/domains/{name}/forwards/{local}", delete(routes::domains::remove_forward_address))
            .route("/api/admin/domains/{name}/self-service", put(routes::own::set_domain_self_service))
            .route("/api/admin/domains/{name}/check", post(routes::domains::check))
            .route("/api/admin/domains/{name}/mta-sts", put(routes::reports::set_mode))
            .route("/api/admin/domains/{name}/reports", get(routes::reports::domain_reports))
            .route("/api/admin/reports", get(routes::reports::overview))
            .route("/api/admin/domains/{name}/reports/{kind}", get(routes::reports::list))
            .route("/api/admin/domains/{name}/reports/{kind}/{id}", get(routes::reports::detail))
            .route("/.well-known/mta-sts.txt", get(routes::reports::policy))
            .route("/api/admin/domains/{name}/dns/cloudflare", post(routes::domains::cloudflare))
            .route("/api/admin/domains/{name}/dkim/rotate", post(routes::domains::rotate_keys))
            .route("/api/admin/domains/{name}/dkim/activate", post(routes::domains::activate_keys))
            .route("/api/admin/domains/{name}/dkim/{selector}", delete(routes::domains::remove_key))
            .route("/api/admin/audit", get(routes::admin::audit))
            .route("/api/admin/queue", get(routes::queue::list))
            .route("/api/admin/queue/{id}/retry", post(routes::queue::retry))
            .route("/api/admin/queue/{id}", delete(routes::queue::drop))
            .route("/api/admin/logs", get(routes::queue::logs))
            .route("/api/admin/logs/loki", get(routes::settings::loki_status))
            .route("/api/admin/logs/loki/test", post(routes::settings::loki_test))
            .route("/api/admin/settings", get(routes::settings::show).patch(routes::settings::update))
            .route(
                "/api/admin/branding/logo",
                axum::routing::put(routes::branding::upload_logo).delete(routes::branding::remove_logo),
            )
            .route("/api/admin/branding/palette", get(routes::branding::preview))
            .route("/api/admin/egress", get(routes::egress::show))
            .route("/api/admin/egress/test", post(routes::egress::test))
            .route("/api/admin/vpn", get(routes::vpn::show).put(routes::vpn::save))
            .route("/api/admin/vpn/apply", post(routes::vpn::apply))
            .route("/api/admin/vpn/stop", post(routes::vpn::stop))
            .route("/api/admin/vpn/remove", post(routes::vpn::remove))
            .route("/api/admin/vpn/files", post(routes::vpn::files))
            .route("/api/admin/vpn/use-gluetun", post(routes::vpn::use_gluetun))
            .route("/api/admin/spam", get(routes::spam::admin_overview))
            .route("/api/admin/spam/learn-folders", post(routes::spam::admin_learn))
            .route("/api/admin/spam/log", get(routes::spam::admin_log).delete(routes::spam::admin_clear_log))
            .route(
                "/api/admin/spam/senders",
                get(routes::spam::admin_senders_view).post(routes::spam::admin_add_sender),
            )
            .route("/api/admin/spam/senders/{id}", delete(routes::spam::admin_remove_sender))
            .route("/api/admin/spam/rules", get(routes::rules::admin_list).post(routes::rules::admin_create))
            .route("/api/admin/spam/rules/bulk", post(routes::rules::admin_bulk))
            .route("/api/admin/spam/rules/import", post(routes::rules::admin_import))
            .route("/api/admin/spam/rules/export", get(routes::rules::admin_export))
            .route("/api/admin/spam/rules/{type}/{id}", patch(routes::rules::admin_change))
            .route("/api/admin/spam/scopes", get(routes::rules::admin_scopes))
            .route("/api/admin/spam/words", get(routes::words::admin_words).post(routes::words::admin_add_words))
            .route("/api/admin/spam/words/{id}", delete(routes::words::admin_remove_word))
            .route("/api/admin/spam/word-sources", post(routes::words::admin_subscribe))
            .route("/api/admin/spam/word-sources/{id}", delete(routes::words::admin_unsubscribe))
            .route("/api/admin/spam/word-sources/{id}/refresh", post(routes::words::admin_refresh_source))
            .route("/api/admin/spam/antivirus", get(routes::spam::admin_antivirus))
            .route("/api/admin/spam/antivirus/test", post(routes::spam::admin_antivirus_test))
            .route("/api/admin/spam/feeds", get(routes::words::admin_feeds))
            .route("/api/admin/spam/feeds/{key}/refresh", post(routes::words::admin_refresh_feed))
            .route("/api/admin/people", get(routes::people::list).post(routes::people::create))
            .route(
                "/api/admin/people/{login}",
                get(routes::people::detail).patch(routes::people::update).delete(routes::people::trash),
            )
            .route("/api/admin/people/{login}/restore", post(routes::people::restore))
            .route("/api/admin/people/{login}/purge", post(routes::people::purge))
            .route("/api/admin/people/{login}/password-link", post(routes::people::password_link))
            .route("/api/admin/people/{login}/password", put(routes::people::set_password))
            .route("/api/admin/people/{login}/reset-second-factors", post(routes::people::reset_second_factors))
            .route("/api/admin/people/{login}/external-forwarding", put(routes::mailbox::set_external_forwarding))
            .route("/api/admin/people/{login}/alias-limit", put(routes::own::set_alias_limit))
            .route("/api/admin/people/{login}/send-as-domains", put(routes::people::set_send_as_domains))
            .route("/api/admin/people/{login}/app-passwords", post(routes::people::create_app_password))
            .route("/api/admin/people/{login}/app-passwords/{id}", delete(routes::people::revoke_app_password))
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
                "/forwarding/{token}",
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
        // The webmail is a second app under /mail, built from its own repository. It routes
        // itself, so every path below hands out the same page.
        if assets::has_webmail() {
            app = app.route("/mail", get(routes::webmail::page));
            // With the slash as well. It is the address the webmail is built with, so it is what a
            // bookmark and a link from the page itself hold — and a wildcard wants at least one
            // character after the slash, so neither route would have caught it on its own.
            app = app.route("/mail/", get(routes::webmail::page));
            // One wildcard for everything below: the handler tells a file of the build from a
            // path the webmail routes itself. Two overlapping wildcards would not be allowed here.
            app = app.route("/mail/{*rest}", get(routes::webmail::below));
        }
        // The webmail routes need the state; the portal ones do not, so it is applied once here.
        api.merge(app.with_state(self.clone()))
    }
}
