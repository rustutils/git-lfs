//! Git interop for git-lfs.
//!
//! Everything in this crate shells out to the `git` binary — see CLAUDE.md
//! for the rationale.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error invoking git: {0}")]
    Io(#[from] io::Error),
    #[error("git: {0}")]
    Failed(String),
}

/// Run `git -C <cwd> <args>` and return its trimmed stdout on success.
fn run_git(cwd: &Path, args: &[&str]) -> Result<String, Error> {
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

/// Path to the `.git` directory of the repository containing `cwd`. Always
/// returns an absolute path. Errors if `cwd` isn't inside a git repository.
pub fn git_dir(cwd: &Path) -> Result<PathBuf, Error> {
    run_git(cwd, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

/// Path to the LFS storage directory for the repository (`<git-dir>/lfs`).
/// The directory is not created.
pub fn lfs_dir(cwd: &Path) -> Result<PathBuf, Error> {
    Ok(git_dir(cwd)?.join("lfs"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .arg(tmp.path())
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");
        tmp
    }

    #[test]
    fn git_dir_is_absolute() {
        let tmp = init_repo();
        let dir = git_dir(tmp.path()).unwrap();
        assert!(dir.is_absolute(), "{dir:?}");
        assert_eq!(dir.file_name().unwrap(), ".git");
    }

    #[test]
    fn lfs_dir_under_git_dir() {
        let tmp = init_repo();
        let dir = lfs_dir(tmp.path()).unwrap();
        assert!(dir.ends_with(".git/lfs"));
    }

    #[test]
    fn outside_repo_errors() {
        let tmp = TempDir::new().unwrap();
        // Brand-new dir, not a git repo.
        let err = git_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::Failed(_)), "got {err:?}");
    }
}
