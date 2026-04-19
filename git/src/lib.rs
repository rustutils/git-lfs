//! Git interop for git-lfs.
//!
//! Everything in this crate shells out to the `git` binary — see CLAUDE.md
//! for the rationale.

use std::io;
use std::path::Path;
use std::process::Command;

pub mod config;
pub mod path;
pub mod pktline;

pub use config::ConfigScope;
pub use path::{git_dir, lfs_dir};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error invoking git: {0}")]
    Io(#[from] io::Error),
    #[error("git: {0}")]
    Failed(String),
}

/// Run `git -C <cwd> <args>` and return its trimmed stdout on success.
pub(crate) fn run_git(cwd: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
    if !out.status.success() {
        return Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8(out.stdout)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .trim()
        .to_owned())
}
