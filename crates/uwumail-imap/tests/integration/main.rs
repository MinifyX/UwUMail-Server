//! All integration tests of uwumail-imap in one test binary: one binary per crate keeps
//! `target/` small and linking fast. Add new test files as modules here.

mod imap;
mod managesieve;
mod robustness;
mod sharing;
