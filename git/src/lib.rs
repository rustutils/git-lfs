//! Git interop helpers for Git LFS: config, refs, scanners, and `.gitattributes` matching.
//!
//! Git LFS needs the user's git binary for a handful of things
//! with no LFS-specific equivalent: where the repo lives, what's
//! in its config, which objects each ref reaches, and how
//! `.gitattributes` applies to a given path. This crate collects
//! those helpers in one place. Everything runs by shelling out
//! to the `git` binary the user has installed; this crate does
//! not bundle its own git implementation.
//!
//! It sits at the bottom of the LFS workspace: every other crate
//! goes through it whenever it needs to know something about the
//! repo it's running against. The crate is intentionally a
//! collection of unrelated helpers rather than a single
//! abstraction, so the pieces are independent of each other and
//! you can pick what you need. See the per-module docs below for
//! the specific surfaces.
//!
//! [`Error`] is the shared error type for the few cases that
//! need to surface git's stderr verbatim.

use std::io;
use std::path::Path;
use std::process::Command;

pub mod aliases;
pub mod attr;
pub mod cat_file;
pub mod config;
pub mod diff_index;
pub mod endpoint;
pub mod extension;
pub mod fetch_prune;
pub mod http_options;
pub mod path;
pub mod pktline;
pub mod refs;
pub mod rev_list;
pub mod scanner;

// Top-level re-exports: a small set of widely-used helpers that
// feel natural as flat names. Everything else is accessed through
// its module — see the module list rendered below by rustdoc.
pub use attr::AttrSet;
pub use config::ConfigScope;
pub use http_options::{HttpOptions, extra_headers_for, lfs_url_bool};
pub use path::{git_common_dir, git_dir, lfs_alternate_dirs, lfs_dir, work_tree_root};

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

#[cfg(test)]
pub(crate) mod tests {
    /// Shared test helpers for setting up real git repos. Each helper
    /// function shells out to the `git` binary; tests using these are
    /// integration-level despite living next to their module.
    pub mod commit_helper {
        use std::path::Path;
        use std::process::Command;

        use tempfile::TempDir;

        /// Initialize a fresh repo with a deterministic identity + branch
        /// so tests don't depend on the developer's git config.
        pub fn init_repo() -> TempDir {
            // Fail loudly if the test process inherits GIT_DIR /
            // GIT_WORK_TREE. With those set, `git init <tempdir>`
            // ignores the path and operates on the inherited git-dir
            // instead — every subsequent assertion would silently
            // exercise the wrong repo. The `Justfile` pre-commit
            // recipe strips these; this is the canary if anything
            // else slips through.
            for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
                assert!(
                    std::env::var_os(var).is_none(),
                    "{var} is set in the test process — git subprocesses \
                     will ignore the per-test tempdir. Run via \
                     `just pre-commit` (which strips it) or \
                     `env -u {var} cargo test`."
                );
            }
            let tmp = TempDir::new().unwrap();
            run(tmp.path(), &["init", "--quiet", "--initial-branch=main"]);
            run(tmp.path(), &["config", "user.email", "test@example.com"]);
            run(tmp.path(), &["config", "user.name", "test"]);
            // Disable signing so contributor environments with sign.commit
            // configured globally don't fail tests.
            run(tmp.path(), &["config", "commit.gpgsign", "false"]);
            tmp
        }

        /// Add and commit `content` at `path` (relative to the repo root).
        /// Returns nothing — call `head_oid` if you need the resulting
        /// commit's SHA.
        pub fn commit_file(repo: &TempDir, path: &str, content: &[u8]) {
            std::fs::write(repo.path().join(path), content).unwrap();
            run(repo.path(), &["add", path]);
            run(
                repo.path(),
                &["commit", "--quiet", "-m", &format!("add {path}")],
            );
        }

        /// Hex OID of the commit currently at HEAD.
        pub fn head_oid(repo: &TempDir) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            assert!(out.status.success());
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        fn run(cwd: &Path, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }
    }
}
