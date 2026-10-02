//! All integration tests of uwumail-smtp in one test binary: one binary per crate keeps
//! `target/` small and linking fast. Add new test files as modules here.

mod corpus;
mod dane;
mod directory;
mod faces;
mod flow;
mod gateway;
mod imip;
mod microsoft;
mod mta_sts;
