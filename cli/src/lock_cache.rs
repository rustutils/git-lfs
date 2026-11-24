//! Local record of the user's own locks, for `git lfs locks --local`.
//!
//! `git lfs lock` adds each lock it takes and `git lfs unlock` removes it.
//! The record is one JSON array in `<lfs dir>/cache/locks.json`, shared by
//! all remotes and refs. Concurrent updates can lose entries, since nothing
//! locks the file.
//!
//! The results of the last full `git lfs locks` search are saved under
//! `<lfs dir>/cache/locks/<ref>/`, for `git lfs locks --cached`, in the same
//! place and format as upstream.
//!
//! Every update here is best-effort: errors are ignored, so a failed
//! update never fails the lock command.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use git_lfs_api::Lock;
use serde::Serialize;

/// Path of the cache file for the repository at `cwd`.
pub fn cache_path(cwd: &Path) -> Result<PathBuf, git_lfs_git::Error> {
    Ok(git_lfs_git::lfs_dir(cwd)?.join("cache").join("locks.json"))
}

/// Read the cached locks, or none if the file is missing or invalid.
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

/// Read the cached locks, telling a cache that can't be read apart from
/// a missing one.
///
/// A missing cache gives no locks. `None` means the file exists but can't
/// be read or parsed.
pub fn read_checked(cwd: &Path) -> Option<Vec<Lock>> {
    let path = cache_path(cwd).ok()?;
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).ok(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => None,
    }
}

/// Add `lock` to the cache, replacing an entry with the same id.
pub fn add(cwd: &Path, lock: &Lock) {
    let Ok(path) = cache_path(cwd) else {
        return;
    };
    let mut locks = read(cwd);
    locks.retain(|l| l.id != lock.id);
    locks.push(lock.clone());
    let _ = write(&path, &locks);
}

/// Remove the entry with `id`, if there is one.
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

/// Remove every entry for `repo_relative_path`.
///
/// Path-based unlock calls this as well as [`remove_by_id`], in case the
/// cache has the path under a different id.
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

/// Path of the saved search results of `kind` for `refspec`.
///
/// `kind` is `remote` for a plain search, or `verifiable` for one with
/// `--verify`.
pub fn search_path(
    cwd: &Path,
    refspec: Option<&str>,
    kind: &str,
) -> Result<PathBuf, git_lfs_git::Error> {
    let mut dir = git_lfs_git::lfs_dir(cwd)?.join("cache").join("locks");
    if let Some(r) = refspec {
        dir.push(r);
    }
    Ok(dir.join(kind))
}

/// Save search results to `path`.
pub fn save_search<T: Serialize + ?Sized>(path: &Path, results: &T) {
    let Ok(bytes) = serde_json::to_vec(results) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, bytes);
}

fn write(path: &Path, locks: &[Lock]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(locks).map_err(io::Error::other)?;
    fs::write(path, bytes)
}
