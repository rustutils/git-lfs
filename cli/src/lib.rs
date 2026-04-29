//! Library surface for the `git-lfs` binary.
//!
//! This crate is primarily a binary (see `main.rs`). The library half
//! exists so the [`xtask`] member can reuse the clap command tree for
//! man-page generation, and so future tools (shell completions, docs
//! site, …) have a single source of truth.
//!
//! Only `cli_def` and `man` are intentionally public — the rest of the
//! binary's modules stay private to `main.rs`.
//!
//! [`xtask`]: ../xtask/index.html

pub mod args;
pub mod man;

/// The published version of the `git-lfs` binary. Re-exported so xtask
/// (and any future consumer) renders the correct banner / man-page
/// version without baking in a literal.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
