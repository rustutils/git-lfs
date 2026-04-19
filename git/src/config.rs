//! Git config get/set/unset, scoped to one of git's config files.

use std::path::Path;
use std::process::Command;

use crate::Error;

/// Which config file `git config` operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// `~/.gitconfig` (or `~/.config/git/config`). The default for upstream
    /// `git lfs install`.
    Global,
    /// The current repository's `.git/config`.
    Local,
    /// `/etc/gitconfig`. Usually requires root.
    System,
}

impl ConfigScope {
    fn flag(self) -> &'static str {
        match self {
            Self::Global => "--global",
            Self::Local => "--local",
            Self::System => "--system",
        }
    }
}

/// Read a single config value from the given scope. Returns `Ok(None)` if
/// the key isn't set.
pub fn get(cwd: &Path, scope: ConfigScope, key: &str) -> Result<Option<String>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", scope.flag(), "--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        )),
        // `git config --get` exits 1 when the key isn't set.
        Some(1) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Set `key = value` in the given scope.
pub fn set(cwd: &Path, scope: ConfigScope, key: &str, value: &str) -> Result<(), Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", scope.flag(), key, value])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

/// Unset `key` in the given scope. Idempotent: if the key isn't there,
/// returns `Ok(())` rather than erroring.
pub fn unset(cwd: &Path, scope: ConfigScope, key: &str) -> Result<(), Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", scope.flag(), "--unset", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(()),
        // git config --unset exits 5 when the key isn't set.
        Some(5) => Ok(()),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
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
        assert!(status.success());
        tmp
    }

    #[test]
    fn get_unset_key_returns_none() {
        let tmp = init_repo();
        let v = get(tmp.path(), ConfigScope::Local, "filter.lfs.clean").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let tmp = init_repo();
        set(tmp.path(), ConfigScope::Local, "filter.lfs.clean", "git-lfs clean -- %f").unwrap();
        let v = get(tmp.path(), ConfigScope::Local, "filter.lfs.clean").unwrap();
        assert_eq!(v.as_deref(), Some("git-lfs clean -- %f"));
    }

    #[test]
    fn unset_removes_key() {
        let tmp = init_repo();
        set(tmp.path(), ConfigScope::Local, "filter.lfs.required", "true").unwrap();
        unset(tmp.path(), ConfigScope::Local, "filter.lfs.required").unwrap();
        let v = get(tmp.path(), ConfigScope::Local, "filter.lfs.required").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn unset_missing_key_is_ok() {
        let tmp = init_repo();
        unset(tmp.path(), ConfigScope::Local, "never.was.set").unwrap();
    }
}
