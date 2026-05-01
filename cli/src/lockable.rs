//! Lockable-file enforcement: chmod working-tree files based on their
//! `lockable` `.gitattributes` status and the set of locks the current
//! user holds.
//!
//! Invariant for any file matching a `lockable` pattern:
//!   - we hold the lock → file is writable
//!   - we don't hold the lock → file is read-only
//!
//! Files not matching a `lockable` pattern are never touched by this
//! module; their permissions are the user's business.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use git_lfs_git::AttrSet;

/// Set of paths the current user holds locks for, relative to the repo
/// root with `/` separators. `Empty` means "no LFS endpoint configured
/// or the locks API was unavailable" — treated as "no held locks" by
/// `apply_modes`, so every lockable file ends up read-only.
pub enum HeldLocks {
    Empty,
    Paths(HashSet<String>),
}

impl HeldLocks {
    pub fn contains(&self, path: &str) -> bool {
        match self {
            Self::Empty => false,
            Self::Paths(set) => set.contains(path),
        }
    }

    /// Query the LFS server's `verify_locks` endpoint, paginating
    /// through and collecting the `ours` partition. Returns `Empty` on
    /// any failure (no endpoint, network error, server doesn't support
    /// locking) so we fall through to the strict "no lock = read-only"
    /// path rather than aborting a hook.
    pub fn from_server(cwd: &Path) -> Self {
        let Ok(api) = crate::fetcher::build_api_client(cwd, None) else {
            return Self::Empty;
        };
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return Self::Empty;
        };
        let mut held = HashSet::new();
        let mut req = git_lfs_api::VerifyLocksRequest::default();
        while let Ok(resp) = runtime.block_on(api.verify_locks(&req)) {
            for l in resp.ours {
                held.insert(l.path);
            }
            match resp.next_cursor {
                Some(c) => req.cursor = Some(c),
                None => break,
            }
        }
        if held.is_empty() {
            Self::Empty
        } else {
            Self::Paths(held)
        }
    }
}

/// For each path in `paths` that matches a `lockable` pattern in
/// `attrs`: chmod writable if `held.contains(path)`, read-only
/// otherwise. Non-lockable paths are left alone.
pub fn apply_modes<I>(cwd: &Path, paths: I, attrs: &AttrSet, held: &HeldLocks) -> io::Result<()>
where
    I: IntoIterator<Item = String>,
{
    for path in paths {
        if !attrs.is_lockable(&path) {
            continue;
        }
        chmod_writable(cwd, &path, held.contains(&path))?;
    }
    Ok(())
}

/// Force `path` (relative to `cwd`) writable, regardless of its
/// `lockable` status. Used by `git lfs lock` (after a successful lock,
/// the user needs to actually edit the file) and by
/// `track --not-lockable` (undo any earlier read-only state).
pub fn force_writable(cwd: &Path, path: &str) -> io::Result<()> {
    chmod_writable(cwd, path, true)
}

/// If `path` matches a lockable pattern in `attrs`, force it
/// read-only. Used by `git lfs unlock` after a successful release.
pub fn enforce_readonly_if_lockable(cwd: &Path, attrs: &AttrSet, path: &str) -> io::Result<()> {
    if !attrs.is_lockable(path) {
        return Ok(());
    }
    chmod_writable(cwd, path, false)
}

/// Walk the entire working tree (`git ls-files`) and apply the
/// lockable invariant. Wrapped by the post-checkout / post-commit /
/// post-merge hooks.
///
/// `verify_locks` is only invoked if at least one indexed file matches
/// a lockable pattern. The credential helper has visible side effects
/// (caching, reject) and we don't want a `.gitattributes`-only commit
/// to churn auth state on a server the user hasn't logged into yet.
pub fn enforce_workdir(cwd: &Path) -> io::Result<()> {
    if !lockable_readonly_enabled(cwd) {
        return Ok(());
    }
    let attrs = AttrSet::from_workdir(cwd)?;
    let files = ls_files(cwd)?;
    if !files.iter().any(|f| attrs.is_lockable(f)) {
        return Ok(());
    }
    let held = HeldLocks::from_server(cwd);
    apply_modes(cwd, files, &attrs, &held)
}

/// Returns whether the lockable read-only invariant should be
/// enforced. Defaults to `true`; either the env override
/// `GIT_LFS_SET_LOCKABLE_READONLY=0/false` or the git config
/// `lfs.setlockablereadonly=false` flips it off (matching upstream's
/// `Configuration.SetLockableFilesReadOnly()`).
pub fn lockable_readonly_enabled(cwd: &Path) -> bool {
    if let Some(v) = std::env::var_os("GIT_LFS_SET_LOCKABLE_READONLY") {
        let s = v.to_string_lossy().trim().to_lowercase();
        if matches!(s.as_str(), "false" | "0" | "no" | "off") {
            return false;
        }
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "lfs.setlockablereadonly"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_lowercase();
            !matches!(v.as_str(), "false" | "0" | "no" | "off")
        }
        _ => true,
    }
}

/// `git ls-files -z` listing of cached (in-index) paths. Submodules
/// appear as their gitlink path, not their contents — exactly what we
/// want; we never want to descend into a submodule.
pub fn ls_files(cwd: &Path) -> io::Result<Vec<String>> {
    ls_files_inner(cwd, &[])
}

/// `git ls-files -z -- <pattern>`. Used by track-time chmod, which
/// only wants to walk files matching the pattern that just changed
/// lockable state.
pub fn ls_files_matching(cwd: &Path, pattern: &str) -> io::Result<Vec<String>> {
    ls_files_inner(cwd, &["--", pattern])
}

fn ls_files_inner(cwd: &Path, extra: &[&str]) -> io::Result<Vec<String>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args(["ls-files", "-z"]);
    for a in extra {
        cmd.arg(a);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|c| !c.is_empty())
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect())
}

#[cfg(unix)]
fn chmod_writable(cwd: &Path, path: &str, writable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let full = cwd.join(path);
    let meta = match fs::metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Ok(());
    }
    let mut perms = meta.permissions();
    let mode = perms.mode();
    // Owner-write only — matches `core.sharedRepository=false` (the
    // default). Stripping write strips for owner/group/other.
    let new_mode = if writable {
        mode | 0o200
    } else {
        mode & !0o222
    };
    if new_mode != mode {
        perms.set_mode(new_mode);
        fs::set_permissions(&full, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn chmod_writable(cwd: &Path, path: &str, writable: bool) -> io::Result<()> {
    let full = cwd.join(path);
    let meta = match fs::metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Ok(());
    }
    let mut perms = meta.permissions();
    perms.set_readonly(!writable);
    fs::set_permissions(&full, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn held_locks_contains() {
        let mut set = HashSet::new();
        set.insert("a.dat".to_string());
        let h = HeldLocks::Paths(set);
        assert!(h.contains("a.dat"));
        assert!(!h.contains("b.dat"));

        let empty = HeldLocks::Empty;
        assert!(!empty.contains("a.dat"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_modes_strips_write_for_lockable_unheld() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("foo.dat"), "x").unwrap();
        std::fs::write(tmp.path().join(".gitattributes"), "*.dat lockable\n").unwrap();
        let attrs = AttrSet::from_workdir(tmp.path()).unwrap();
        apply_modes(
            tmp.path(),
            ["foo.dat".to_string()],
            &attrs,
            &HeldLocks::Empty,
        )
        .unwrap();
        let mode = std::fs::metadata(tmp.path().join("foo.dat"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & 0o222, 0, "no write bits expected; got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn apply_modes_keeps_writable_for_held_lockable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("foo.dat"), "x").unwrap();
        std::fs::write(tmp.path().join(".gitattributes"), "*.dat lockable\n").unwrap();
        let attrs = AttrSet::from_workdir(tmp.path()).unwrap();
        let mut held_set = HashSet::new();
        held_set.insert("foo.dat".to_string());
        apply_modes(
            tmp.path(),
            ["foo.dat".to_string()],
            &attrs,
            &HeldLocks::Paths(held_set),
        )
        .unwrap();
        let mode = std::fs::metadata(tmp.path().join("foo.dat"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_ne!(mode & 0o200, 0, "owner write expected; got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn apply_modes_leaves_non_lockable_alone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("foo.bin");
        std::fs::write(&f, "x").unwrap();
        // Start writable.
        let mut p = std::fs::metadata(&f).unwrap().permissions();
        p.set_mode(0o644);
        std::fs::set_permissions(&f, p).unwrap();

        let attrs = AttrSet::from_workdir(tmp.path()).unwrap(); // empty
        apply_modes(
            tmp.path(),
            ["foo.bin".to_string()],
            &attrs,
            &HeldLocks::Empty,
        )
        .unwrap();
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "non-lockable file should be untouched");
    }

    #[cfg(unix)]
    #[test]
    fn force_writable_adds_owner_write() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("foo.dat");
        std::fs::write(&f, "x").unwrap();
        let mut p = std::fs::metadata(&f).unwrap().permissions();
        p.set_mode(0o444);
        std::fs::set_permissions(&f, p).unwrap();

        force_writable(tmp.path(), "foo.dat").unwrap();
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_ne!(mode & 0o200, 0, "owner write expected after force_writable");
    }
}
