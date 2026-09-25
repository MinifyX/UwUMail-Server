//! All integration tests of uwumail-backup in one test binary: one binary per crate keeps
//! `target/` small and linking fast. Add new test files as modules here.

mod backup;
mod sftp;
