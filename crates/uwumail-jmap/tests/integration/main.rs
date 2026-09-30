//! All integration tests of uwumail-jmap in one test binary: one binary per crate keeps
//! `target/` small and linking fast. Add new test files as modules here.

mod api;
mod assist;
mod birthdays;
mod calendar_features;
mod calendar_sharing;
mod calendars;
mod common;
mod conformance;
mod contacts;
mod image_text;
mod limits;
mod masked_email;
mod masked_scope;
mod pictures;
mod profile;
mod query_changes;
mod remote_images;
mod sharing;
mod sieve;
mod signatures;
mod submission;
mod suggestions;
mod tnef;
mod tokens;
mod unsubscribe;
mod web_push;
mod websocket;
