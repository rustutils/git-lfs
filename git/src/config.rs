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
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        // `git config --get` exits 1 when the key isn't set.
        Some(1) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Read a single config value from a specific file (e.g. `.lfsconfig`).
/// Returns `Ok(None)` if the file doesn't exist or the key isn't set.
pub fn get_from_file(cwd: &Path, file: &Path, key: &str) -> Result<Option<String>, Error> {
    if !cwd.join(file).is_file() {
        // `git config --file` errors loudly on a missing file. The common
        // case for `.lfsconfig` is "no file" — treat that as "no value".
        return Ok(None);
    }
    let file_arg = format!("--file={}", file.display());
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", &file_arg, "--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Look up `key` across `.lfsconfig` (committed; lowest priority) and
/// the standard git config scopes (local → global → system). Returns the
/// first match.
///
/// Mirrors upstream's effective config: settings written to `.lfsconfig`
/// at the repo root are visible without overriding anything explicitly
/// set in the user's git config.
pub fn get_effective(cwd: &Path, key: &str) -> Result<Option<String>, Error> {
    if let Some(v) = get(cwd, ConfigScope::Local, key)? {
        return Ok(Some(v));
    }
    if let Some(v) = get(cwd, ConfigScope::Global, key)? {
        return Ok(Some(v));
    }
    if let Some(v) = get(cwd, ConfigScope::System, key)? {
        return Ok(Some(v));
    }
    get_from_file(cwd, std::path::Path::new(".lfsconfig"), key)
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
        set(
            tmp.path(),
            ConfigScope::Local,
            "filter.lfs.clean",
            "git-lfs clean -- %f",
        )
        .unwrap();
        let v = get(tmp.path(), ConfigScope::Local, "filter.lfs.clean").unwrap();
        assert_eq!(v.as_deref(), Some("git-lfs clean -- %f"));
    }

    #[test]
    fn unset_removes_key() {
        let tmp = init_repo();
        set(
            tmp.path(),
            ConfigScope::Local,
            "filter.lfs.required",
            "true",
        )
        .unwrap();
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
