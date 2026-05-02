//! On-disk cache of the user's own locks, used by
//! `git lfs locks --local` to list cached records without contacting the
//! server. Updated as a side effect of successful `git lfs lock` /
//! `git lfs unlock`.
//!
//! Layout: a single JSON array at
//! `<lfs_dir>/cache/locks.json` (typically `.git/lfs/cache/locks.json`).
//! Upstream uses a per-remote+refspec sqlite db keyed off
//! `lockcache.db`; we collapse that to a single file because the only
//! consumer today is `--local` listing in t-locks tests 8/9 (no
//! remote/refspec scoping, no concurrent-process locking concerns).
//! Bigger surfaces (per-remote scoping, lock conflict resolution
//! across cached vs. remote) are tracked in NOTES.md.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use git_lfs_api::Lock;

/// Path of the cache file for `cwd`'s repo. `<lfs>/cache/locks.json`.
/// Bubbles up the same `git_lfs_git::Error` as `lfs_dir` when called
/// outside a repo — callers should route that into a soft warning,
/// since failing to update the cache shouldn't block a successful
/// `lock`/`unlock` round-trip.
pub fn cache_path(cwd: &Path) -> Result<PathBuf, git_lfs_git::Error> {
    Ok(git_lfs_git::lfs_dir(cwd)?.join("cache").join("locks.json"))
}

/// Read the cached locks, returning an empty `Vec` for a missing or
/// unparseable file (we'd rather lose visibility into stale entries
/// than break the command). Pass an absolute path or a path inside
/// the repo — anything `git_lfs_git::lfs_dir` understands.
pub fn read(cwd: &Path) -> Vec<Lock> {
    let Ok(path) = cache_path(cwd) else {
        return Vec::new();
    };
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    serde_json::from_slice::<Vec<Lock>>(&bytes).unwrap_or_default()
}

/// Add `lock` to the cache, replacing any existing entry with the
/// same id (defensive — `git lfs lock` shouldn't issue a duplicate id,
/// but a server reissuing the same id should still leave one entry).
/// Best-effort: on any I/O or path-resolution error we silently drop
/// the update so a real lock op isn't reported as failed.
pub fn add(cwd: &Path, lock: &Lock) {
    let Ok(path) = cache_path(cwd) else {
        return;
    };
    let mut locks = read(cwd);
    locks.retain(|l| l.id != lock.id);
    locks.push(lock.clone());
    let _ = write(&path, &locks);
}

/// Remove the cached entry with `id`, if present. Same best-effort
/// posture as [`add`].
pub fn remove_by_id(cwd: &Path, id: &str) {
    let Ok(path) = cache_path(cwd) else {
        return;
    };
    let mut locks = read(cwd);
    let before = locks.len();
    locks.retain(|l| l.id != id);
    if locks.len() == before {
        return;
    }
    let _ = write(&path, &locks);
}

/// Remove the first cached entry matching `path` (used when an unlock
/// path → id lookup found a matching server-side record but we want
/// the cache to also drop it, even if the id isn't in our cache for
/// some reason).
pub fn remove_by_path(cwd: &Path, repo_relative_path: &str) {
    let Ok(file) = cache_path(cwd) else {
        return;
    };
    let mut locks = read(cwd);
    let before = locks.len();
    locks.retain(|l| l.path != repo_relative_path);
    if locks.len() == before {
        return;
    }
    let _ = write(&file, &locks);
}

fn write(path: &Path, locks: &[Lock]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(locks).map_err(io::Error::other)?;
    fs::write(path, bytes)
}
