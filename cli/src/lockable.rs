//! Makes lockable files read-only unless the user holds their lock.
//!
//! A file is lockable if `.gitattributes` gives it the `lockable`
//! attribute. Other files are never changed here. On Unix, making a file
//! writable adds only the owner's write bit, and making it read-only
//! removes all write bits.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use git_lfs_git::AttrSet;

/// Paths the user holds locks for, relative to the repository root with
/// `/` separators.
///
/// `Empty` means no locks, whether because none are held or because the
/// server couldn't be asked.
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

    /// Ask the default remote's server which locks are ours, over HTTP.
    ///
    /// Any failure stops the query and keeps the locks found so far, so a
    /// hook never fails because of it. A failure on the first page gives
    /// `Empty`, which makes every lockable file read-only. An empty
    /// `next_cursor` is sent back as the cursor rather than ending the
    /// loop.
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

/// Make each lockable path in `paths` writable if its lock is held, and
/// read-only otherwise.
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

/// Make `path`, relative to `cwd`, writable whether or not it's lockable.
///
/// `git lfs lock` calls this after taking a lock, and `git lfs track
/// --not-lockable` to undo an earlier read-only mode.
pub fn force_writable(cwd: &Path, path: &str) -> io::Result<()> {
    chmod_writable(cwd, path, true)
}

/// Make `path` read-only if it's lockable. `git lfs unlock` calls this.
pub fn enforce_readonly_if_lockable(cwd: &Path, attrs: &AttrSet, path: &str) -> io::Result<()> {
    if !attrs.is_lockable(path) {
        return Ok(());
    }
    chmod_writable(cwd, path, false)
}

/// Set the mode of every lockable file in the index.
///
/// The post-checkout, post-commit, and post-merge hooks call this. The
/// server is only asked if some file is lockable, because asking can run
/// credential helpers, which may prompt or store credentials.
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

/// Whether lockable files should be made read-only.
///
/// True unless `GIT_LFS_SET_LOCKABLE_READONLY` or
/// `lfs.setlockablereadonly` is false, as in upstream's
/// `SetLockableFilesReadOnly`. The setting is read with `git config`, so
/// `.lfsconfig` isn't consulted.
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

/// List the paths in the index.
///
/// A submodule is listed as its own path, without its contents. A failed
/// `git ls-files` gives an empty list.
pub fn ls_files(cwd: &Path) -> io::Result<Vec<String>> {
    ls_files_inner(cwd, &[])
}

/// List the paths in the index that match the `.gitattributes` pattern
/// `pattern`, as a pathspec.
///
/// `git lfs track` uses this to find the files whose lockable state it
/// just changed.
pub fn ls_files_matching(cwd: &Path, pattern: &str) -> io::Result<Vec<String>> {
    // A leading `/` anchors a `.gitattributes` pattern at the root, but in
    // a pathspec it's an absolute path that git rejects, so it's dropped.
    let stripped = pattern
        .strip_prefix('/')
        .filter(|s| !s.is_empty())
        .unwrap_or(pattern);
    ls_files_inner(cwd, &["--", stripped])
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
