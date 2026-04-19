//! Repository path discovery.

use std::path::{Path, PathBuf};

use crate::{Error, run_git};

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
    use std::process::Command;
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
        let err = git_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::Failed(_)), "got {err:?}");
    }
}
