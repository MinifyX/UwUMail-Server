//! All integration tests of uwumail-web in one test binary: one binary per crate keeps
//! `target/` small and linking fast. Add new test files as modules here.

mod admin_alerts;
mod api;
mod apps;
mod assist_costs;
mod backups;
mod branding;
mod calendar_import;
mod calendars;
mod directory;
mod domains;
mod egress;
mod fetch_oauth;
mod gateway;
mod health;
mod ldap;
mod loki;
mod mailbox;
mod moving;
mod oauth;
mod oidc;
mod people;
mod pictures;
mod security;
mod settings;
mod setup;
mod sharing;
mod spam;
mod vpn;
