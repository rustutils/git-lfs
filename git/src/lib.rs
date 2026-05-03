//! Git interop for git-lfs.
//!
//! Everything in this crate shells out to the `git` binary — see CLAUDE.md
//! for the rationale.

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
pub use cat_file::{BlobContent, CatFileBatch, CatFileBatchCheck, CatFileHeader};
pub use config::ConfigScope;
pub use diff_index::{DiffEntry, diff_index};
pub use endpoint::{
    EndpointError, EndpointInfo, SshInfo, derive_lfs_url, endpoint_for_remote, looks_like_url,
    parse_ssh_url, resolve_endpoint,
};
pub use extension::{ExtensionConfig, list_extensions};
pub use fetch_prune::FetchPruneConfig;
pub use http_options::HttpOptions;
pub use path::{git_common_dir, git_dir, lfs_alternate_dirs, lfs_dir, work_tree_root};
pub use refs::{RecentRef, RefKind, WorktreeEntry, recent_branches, worktrees};
pub use rev_list::{RevListEntry, rev_list, rev_list_with_args};
pub use scanner::{
    PointerEntry, TreeBlob, scan_index_lfs, scan_index_pointers, scan_pointers,
    scan_pointers_with_args, scan_previous_versions, scan_stashed, scan_tree, scan_tree_blobs,
};

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
