//! Library half of the `git-lfs` binary.
//!
//! It exposes the clap command tree in [`args`] and the documentation in
//! [`man`] so that `xtask` can render man pages and the manual from them.
//! The other modules are private to the binary.

pub mod args;
pub mod man;

/// The version of the `git-lfs` binary, as shown in its man pages.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
