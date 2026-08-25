//! `ccm` — Contextual Commit Message.
//!
//! Library crate backing the `ccm` binary. Split out from `main.rs` so the pipeline's
//! logic is reachable both from the binary and from `tests/` integration tests, and so
//! the many pure helper functions (path normalization, diff-scope resolution, message
//! cleanup, editor-command splitting, config validation, ...) are unit-testable in
//! isolation from any repo/network/subprocess.

pub mod backend;
pub mod cleanup;
pub mod cli;
pub mod commit;
pub mod config;
pub mod diff;
pub mod editor;
pub mod env;
pub mod error;
pub mod generation;
pub mod picker;
pub mod pipeline;
pub mod progress;
pub mod prompt;
pub mod repo;
pub mod vcs;
