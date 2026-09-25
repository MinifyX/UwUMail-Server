//! All integration tests of uwumail-smtp in one test binary: one binary per crate keeps
//! `target/` small and linking fast. Add new test files as modules here.

mod flow;
mod gateway;
mod imip;
