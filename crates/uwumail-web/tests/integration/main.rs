//! All integration tests of uwumail-web in one test binary: one binary per crate keeps
//! `target/` small and linking fast. Add new test files as modules here.

mod api;
mod apps;
mod backups;
mod branding;
mod calendars;
mod domains;
mod egress;
mod gateway;
mod health;
mod loki;
mod mailbox;
mod people;
mod security;
mod settings;
mod setup;
mod sharing;
mod spam;
mod vpn;
