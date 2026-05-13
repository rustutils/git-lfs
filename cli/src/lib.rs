//! Library surface for the `git-lfs` binary.
//!
//! This crate is primarily a binary (see `main.rs`). The library
//! half exists so other tools in the workspace can reuse the clap
//! command tree for man-page generation, shell completions, and
//! the like. Only [`args`] and [`man`] are intentionally public;
//! the rest of the binary's modules stay private to `main.rs`.

pub mod args;
pub mod man;

/// The published version of the `git-lfs` binary.
///
/// Re-exported so xtask (and any future consumer) renders the
/// correct banner and man-page version without baking in a literal.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
