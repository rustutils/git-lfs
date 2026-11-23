//! Ask the user's installed `git` about a repository.
//!
//! Git LFS needs to know things only Git can answer: where the repository
//! lives, what its config says, which objects each ref reaches, and which
//! attributes apply to a path. This crate answers them by running the `git`
//! binary, so behavior matches whatever Git the user has. Parsing stable file
//! formats like `.gitattributes` is done in-process.
//!
//! The modules are independent, so use only what you need. Most functions
//! take a directory inside the repository as their first argument and run
//! `git` there.
//!
//! Functions that run `git` return [`Error`] when it can't be started or
//! exits with an error.

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

pub use attr::AttrSet;
pub use config::ConfigScope;
pub use http_options::{ExtraHeaders, HttpOptions, lfs_url_bool, lfs_url_value, url_value};
pub use path::{
    expand_tilde, git_common_dir, git_dir, lfs_alternate_dirs, lfs_dir, work_tree_root,
};

/// An error from running `git`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O error, such as `git` not being installed.
    #[error("io error invoking git: {0}")]
    Io(#[from] io::Error),
    /// `git` failed or printed something unexpected.
    ///
    /// Holds git's stderr, or a description of the unexpected output.
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
    /// Shared test helpers for setting up real git repos.
    ///
    /// Each helper runs the `git` binary, so tests using them are
    /// integration-level despite living next to their module.
    pub mod commit_helper {
        use std::path::Path;
        use std::process::Command;

        use tempfile::TempDir;

        /// Initialize a fresh repo with a deterministic identity and branch
        /// so tests don't depend on the developer's git config.
        pub fn init_repo() -> TempDir {
            // Fail loudly if the test process inherits GIT_DIR or
            // GIT_WORK_TREE. With those set, `git init <tempdir>`
            // ignores the path and operates on the inherited git-dir
            // instead, and every subsequent assertion would silently
            // exercise the wrong repo. The `Justfile` pre-commit
            // recipe strips these. This is the canary if anything
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
        /// Call `head_oid` for the resulting commit's SHA.
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
