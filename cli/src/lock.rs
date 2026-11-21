//! `git lfs lock`, `git lfs locks`, and `git lfs unlock`.
//!
//! Requests go through [`LockClient`], over HTTP or pure SSH. Paths are
//! sent relative to the repository root with `/` separators.
//!
//! `git lfs locks --local` reads the [`lock_cache`] that `lock` and `unlock`
//! keep up to date. There's no `--cached`
//! option.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use git_lfs_api::{ApiError, ListLocksFilter, Lock, Ref, VerifyLocksRequest};
use serde::Serialize;
use tokio::runtime::Runtime;

use crate::lock_cache;
use crate::lock_client::{LockClient, LockClientError, VerifyPage, build_lock_client};
use crate::lockable;

#[derive(Debug, thiserror::Error)]
pub enum LockCommandError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Build(String),
    #[error("lock api: {0}")]
    Api(String),
    #[error("could not serialize JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Default, Clone)]
pub struct LockOptions {
    pub remote: Option<String>,
    /// The ref to send. `None` means the current branch's
    /// `branch.<name>.merge`, else the branch itself.
    pub refspec: Option<String>,
    pub json: bool,
}

#[derive(Debug, Default, Clone)]
pub struct LocksOptions {
    pub remote: Option<String>,
    pub refspec: Option<String>,
    pub path: Option<String>,
    pub id: Option<String>,
    pub limit: Option<u32>,
    pub verify: bool,
    pub json: bool,
    /// List from the [`lock_cache`] without contacting the server.
    ///
    /// `--path`, `--id`, and `--limit` filter the cache. `--verify` is
    /// ignored.
    pub local: bool,
}

#[derive(Debug, Default, Clone)]
pub struct UnlockOptions {
    pub remote: Option<String>,
    pub refspec: Option<String>,
    pub id: Option<String>,
    pub force: bool,
    pub json: bool,
}

// --------------------------------------------------------------------------
// lock
// --------------------------------------------------------------------------

pub fn lock(cwd: &Path, paths: &[String], opts: &LockOptions) -> Result<bool, LockCommandError> {
    if paths.is_empty() {
        return Err(LockCommandError::Build(
            "git lfs lock requires at least one path".into(),
        ));
    }
    let runtime = Arc::new(build_runtime()?);
    let client = build_lock_client(cwd, opts.remote.as_deref(), runtime.clone())
        .map_err(LockCommandError::Build)?;
    let root = repo_root(cwd).map_err(LockCommandError::Build)?;
    let refspec = resolve_refspec(&root, opts.refspec.as_deref());

    let mut success = true;
    let mut locks: Vec<Lock> = Vec::new();
    for raw in paths {
        let path = match resolve_lock_path(cwd, &root, raw) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("git-lfs: {e}");
                success = false;
                continue;
            }
        };
        match client.create_lock(&path, refspec.as_deref()) {
            Ok(lock) => {
                if !opts.json {
                    println!("Locked {path} ({})", lock.id);
                }
                // A lockable file may have been made read-only by a hook.
                // A failed chmod doesn't fail the lock.
                let _ = lockable::force_writable(&root, &path);
                lock_cache::add(cwd, &lock);
                locks.push(lock);
            }
            Err(LockClientError::Conflict { existing, message }) => {
                // Upstream's wording, which `t-lock.sh` checks.
                eprintln!("Locking {path} failed: {message}");
                if let Some(owner) = existing.as_ref().and_then(|l| l.owner.as_ref()) {
                    eprintln!("  Lock owner: {}", owner.name);
                }
                success = false;
            }
            Err(e) => {
                eprintln!("Locking {path} failed: {}", lock_client_error_reason(&e));
                success = false;
            }
        }
    }

    if opts.json {
        println!("{}", serde_json::to_string(&locks)?);
    }
    Ok(success)
}

// --------------------------------------------------------------------------
// locks (list)
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct VerifyJsonOutput<'a> {
    ours: &'a [Lock],
    theirs: &'a [Lock],
}

pub fn locks(cwd: &Path, opts: &LocksOptions) -> Result<(), LockCommandError> {
    let root = repo_root(cwd).map_err(LockCommandError::Build)?;

    // This must not touch the remote: `t-locks.sh` removes `origin` to
    // check. `--verify` is ignored here, where upstream rejects it.
    if opts.local {
        let path_filter = match opts.path.as_deref() {
            Some(raw) => Some(resolve_lock_path(cwd, &root, raw).map_err(LockCommandError::Build)?),
            None => None,
        };
        let cached = lock_cache::read(cwd);
        let mut filtered: Vec<Lock> = cached
            .into_iter()
            .filter(|l| match (&path_filter, &opts.id) {
                (Some(p), _) if &l.path != p => false,
                (_, Some(id)) if &l.id != id => false,
                _ => true,
            })
            .collect();
        if let Some(limit) = opts.limit {
            filtered.truncate(limit as usize);
        }
        if opts.json {
            println!("{}", serde_json::to_string(&filtered)?);
        } else {
            print_lock_table(&filtered, None);
        }
        return Ok(());
    }

    let runtime = Arc::new(build_runtime()?);
    let client = build_lock_client(cwd, opts.remote.as_deref(), runtime.clone())
        .map_err(LockCommandError::Build)?;
    let refspec = resolve_refspec(&root, opts.refspec.as_deref());

    if opts.verify {
        let resp = verify_all(&client, opts.limit, refspec.clone())
            .map_err(|e| LockCommandError::Api(lock_client_error_reason(&e)))?;
        if opts.json {
            println!(
                "{}",
                serde_json::to_string(&VerifyJsonOutput {
                    ours: &resp.ours,
                    theirs: &resp.theirs,
                })?
            );
        } else {
            print_verify_table(&resp);
        }
        return Ok(());
    }

    // The server stores paths relative to the root, so `./data/x.bin`
    // must be resolved like `lock` does to match anything.
    let path_filter = match opts.path.as_deref() {
        Some(raw) => Some(resolve_lock_path(cwd, &root, raw).map_err(LockCommandError::Build)?),
        None => None,
    };

    let mut filter = ListLocksFilter {
        path: path_filter,
        id: opts.id.clone(),
        limit: opts.limit,
        refspec: refspec.clone(),
        ..Default::default()
    };
    let mut all_locks: Vec<Lock> = Vec::new();
    loop {
        let page = client
            .list_locks(&filter)
            .map_err(|e| LockCommandError::Api(lock_client_error_reason(&e)))?;
        all_locks.extend(page.locks);
        // Check the limit before the cursor, so a server that ignores the
        // limit and returns too many locks on the last page is still cut
        // off.
        if let Some(limit) = opts.limit
            && all_locks.len() >= limit as usize
        {
            all_locks.truncate(limit as usize);
            break;
        }
        match page.next_cursor {
            Some(c) if !c.is_empty() => filter.cursor = Some(c),
            _ => break,
        }
    }

    if opts.json {
        println!("{}", serde_json::to_string(&all_locks)?);
    } else {
        print_lock_table(&all_locks, None);
    }
    Ok(())
}

/// Fetch every page of ours and theirs.
///
/// `limit` is sent as the page size. Unlike the plain listing, the total
/// isn't cut to it.
fn verify_all(
    client: &LockClient,
    limit: Option<u32>,
    refspec: Option<String>,
) -> Result<VerifyPage, LockClientError> {
    let mut req = VerifyLocksRequest {
        limit,
        r#ref: refspec.map(Ref::new),
        ..Default::default()
    };
    let mut combined = VerifyPage::default();
    loop {
        let page = client.verify_locks(&req)?;
        combined.ours.extend(page.ours);
        combined.theirs.extend(page.theirs);
        match page.next_cursor {
            Some(c) if !c.is_empty() => req.cursor = Some(c),
            _ => break,
        }
    }
    Ok(combined)
}

// --------------------------------------------------------------------------
// unlock
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct UnlockJsonEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    unlocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

pub fn unlock(
    cwd: &Path,
    paths: &[String],
    opts: &UnlockOptions,
) -> Result<bool, LockCommandError> {
    let has_path = !paths.is_empty();
    let has_id = opts.id.is_some();
    if has_path == has_id {
        // Upstream's wording, which the shell tests grep for.
        return Err(LockCommandError::Build(
            "Exactly one of --id or a set of paths must be provided".into(),
        ));
    }

    let runtime = Arc::new(build_runtime()?);
    let client = build_lock_client(cwd, opts.remote.as_deref(), runtime.clone())
        .map_err(LockCommandError::Build)?;
    let root = repo_root(cwd).map_err(LockCommandError::Build)?;
    let refspec = resolve_refspec(&root, opts.refspec.as_deref());
    let lockable_readonly = crate::lockable::lockable_readonly_enabled(&root);
    let mut success = true;
    let mut report: Vec<UnlockJsonEntry> = Vec::new();

    if has_id {
        let id = opts.id.clone().expect("checked above");
        let attrs = git_lfs_git::AttrSet::from_workdir(&root).ok();
        match client.delete_lock(&id, refspec.as_deref(), opts.force) {
            Ok(lock) => {
                if !opts.json {
                    println!("Unlocked Lock {id}");
                } else {
                    report.push(UnlockJsonEntry {
                        id: Some(id.clone()),
                        path: None,
                        unlocked: true,
                        reason: None,
                    });
                }
                // The response has the lock's path, so the file can be
                // made read-only again, as for a path-based unlock.
                if lockable_readonly && let Some(attrs) = attrs.as_ref() {
                    let _ = lockable::enforce_readonly_if_lockable(&root, attrs, &lock.path);
                }
                lock_cache::remove_by_id(cwd, &id);
            }
            Err(e) => {
                eprintln!("Unlocking {id} failed: {}", lock_client_error_reason(&e));
                success = false;
                if opts.json {
                    report.push(UnlockJsonEntry {
                        id: Some(id),
                        path: None,
                        unlocked: false,
                        reason: Some(lock_client_error_reason(&e)),
                    });
                }
            }
        }
    } else {
        let attrs = git_lfs_git::AttrSet::from_workdir(&root).ok();
        for raw in paths {
            let path = match resolve_lock_path(cwd, &root, raw) {
                Ok(p) => p,
                Err(e) if opts.force => {
                    // With `--force`, upstream sends the path as given.
                    eprintln!("git-lfs: warning: {e} (continuing because --force)");
                    raw.replace('\\', "/").trim_start_matches("./").to_owned()
                }
                Err(e) => {
                    eprintln!("git-lfs: {e}");
                    success = false;
                    if opts.json {
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(raw.clone()),
                            unlocked: false,
                            reason: Some(e),
                        });
                    }
                    continue;
                }
            };

            if has_uncommitted_changes(&root, &path) {
                if opts.force {
                    eprintln!("warning: unlocking with uncommitted changes");
                } else {
                    let msg = "Cannot unlock file with uncommitted changes";
                    eprintln!("{msg}");
                    success = false;
                    if opts.json {
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(path.clone()),
                            unlocked: false,
                            reason: Some(msg.into()),
                        });
                    }
                    continue;
                }
            }

            // Only the first page is read. A path has at most one lock.
            let lookup = ListLocksFilter {
                path: Some(path.clone()),
                refspec: refspec.clone(),
                ..Default::default()
            };
            let id = match client.list_locks(&lookup) {
                Ok(list) => list
                    .locks
                    .iter()
                    .find(|l| l.path == path)
                    .map(|l| l.id.clone()),
                Err(e) => {
                    eprintln!(
                        "git-lfs: lookup failed for {path}: {}",
                        lock_client_error_reason(&e)
                    );
                    success = false;
                    if opts.json {
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(path.clone()),
                            unlocked: false,
                            reason: Some(lock_client_error_reason(&e)),
                        });
                    }
                    continue;
                }
            };
            let Some(id) = id else {
                eprintln!("git-lfs: {path} is not locked");
                success = false;
                if opts.json {
                    report.push(UnlockJsonEntry {
                        id: None,
                        path: Some(path.clone()),
                        unlocked: false,
                        reason: Some("not locked".into()),
                    });
                }
                continue;
            };
            match client.delete_lock(&id, refspec.as_deref(), opts.force) {
                Ok(_) => {
                    if !opts.json {
                        println!("Unlocked {path}");
                    }
                    if lockable_readonly && let Some(attrs) = attrs.as_ref() {
                        let _ = lockable::enforce_readonly_if_lockable(&root, attrs, &path);
                    }
                    lock_cache::remove_by_id(cwd, &id);
                    lock_cache::remove_by_path(cwd, &path);
                    if opts.json {
                        // Upstream leaves out `id` for path-based unlocks.
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(path),
                            unlocked: true,
                            reason: None,
                        });
                    }
                }
                Err(e) => {
                    eprintln!("Unlocking {path} failed: {}", lock_client_error_reason(&e));
                    success = false;
                    if opts.json {
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(path),
                            unlocked: false,
                            reason: Some(lock_client_error_reason(&e)),
                        });
                    }
                }
            }
        }
    }

    if opts.json {
        println!("{}", serde_json::to_string(&report)?);
    }
    Ok(success)
}

// --------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------

fn build_runtime() -> std::io::Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// The ref to send with lock requests: the override, else the current
/// branch's.
///
/// `None`, on a detached HEAD without an override, sends no ref.
fn resolve_refspec(repo_root: &Path, override_ref: Option<&str>) -> Option<String> {
    if let Some(s) = override_ref {
        return Some(s.to_owned());
    }
    git_lfs_git::refs::current_refspec(repo_root)
}

/// The reason to print for an API error.
///
/// If the server sent an error body, it's just the server's message,
/// without the status prefix. `t-lock.sh` greps for lines like
/// `Locking a.dat failed: Expected ref ...`.
fn api_error_reason(e: &ApiError) -> String {
    match e {
        ApiError::Status { body: Some(b), .. } => b.message.clone(),
        _ => e.to_string(),
    }
}

/// The reason to print for a [`LockClientError`].
///
/// Like [`api_error_reason`], it's the server's message where there is one.
fn lock_client_error_reason(e: &LockClientError) -> String {
    match e {
        LockClientError::Api(inner) => api_error_reason(inner),
        LockClientError::Conflict { message, .. } => message.clone(),
        LockClientError::SshStatus { message, status } => {
            if message.is_empty() {
                format!("ssh status {status}")
            } else {
                message.clone()
            }
        }
        LockClientError::Ssh(s) => s.clone(),
    }
}

/// Whether `git status --porcelain` prints anything for `path`.
///
/// That includes staged and unstaged changes and an untracked file, as in
/// upstream's `IsFileModified`. If `git status` fails, the file counts as
/// unchanged.
fn has_uncommitted_changes(root: &Path, path: &str) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "--", path])
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => false,
    }
}

fn repo_root(cwd: &Path) -> Result<PathBuf, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("invoking git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        return Err("not in a git repository".into());
    }
    Ok(PathBuf::from(s))
}

/// Turn `file` into a path relative to the repository root, with `/`
/// separators, for the locking API.
///
/// Like upstream's `lockPath`, symlinks aren't followed: with
/// `folder1/folder2` a symlink, `folder1/folder2/a.dat` is sent as typed,
/// which `t-lock.sh` checks. Directories, the root, and paths outside the
/// repository are rejected.
fn resolve_lock_path(cwd: &Path, repo_root: &Path, file: &str) -> Result<String, String> {
    let file_path = Path::new(file);
    let abs = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        cwd.join(file_path)
    };
    let cleaned = lexical_clean(&abs);
    let root_cleaned = lexical_clean(repo_root);

    let rel = cleaned
        .strip_prefix(&root_cleaned)
        .map_err(|_| format!("path is outside the repository: {file}"))?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() || s == "." {
        return Err(format!("cannot lock the repository root: {file}"));
    }

    // `t-lock.sh` greps for this message.
    if cleaned.is_dir() {
        return Err(format!("cannot lock directory: {file}"));
    }

    Ok(s)
}

/// Remove `.` and `..` components without touching the filesystem.
///
/// Like Go's `filepath.Clean` for absolute paths.
fn lexical_clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn print_lock_table(locks: &[Lock], owned: Option<&std::collections::HashSet<String>>) {
    let mut sorted: Vec<&Lock> = locks.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));

    let max_path = sorted.iter().map(|l| l.path.len()).max().unwrap_or(0);
    let max_owner = sorted
        .iter()
        .map(|l| l.owner.as_ref().map(|o| o.name.len()).unwrap_or(0))
        .max()
        .unwrap_or(0);

    for lock in sorted {
        let owner_name = lock.owner.as_ref().map(|o| o.name.as_str()).unwrap_or("");
        let path_pad = " ".repeat(max_path.saturating_sub(lock.path.len()));
        let owner_pad = " ".repeat(max_owner.saturating_sub(owner_name.len()));
        let kind = match owned {
            Some(set) if set.contains(&lock.id) => "O ",
            Some(_) => "  ",
            None => "",
        };
        println!(
            "{kind}{}{path_pad}\t{}{owner_pad}\tID:{}",
            lock.path, owner_name, lock.id,
        );
    }
}

fn print_verify_table(resp: &VerifyPage) {
    let mut combined = Vec::with_capacity(resp.ours.len() + resp.theirs.len());
    combined.extend(resp.ours.iter().cloned());
    combined.extend(resp.theirs.iter().cloned());
    let owned: std::collections::HashSet<String> = resp.ours.iter().map(|l| l.id.clone()).collect();
    print_lock_table(&combined, Some(&owned));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        tmp
    }

    #[test]
    fn resolve_lock_path_relative_under_root() {
        let tmp = init_repo();
        std::fs::write(tmp.path().join("a.bin"), b"x").unwrap();
        let path = resolve_lock_path(tmp.path(), tmp.path(), "a.bin").unwrap();
        assert_eq!(path, "a.bin");
    }

    #[test]
    fn resolve_lock_path_absolute() {
        let tmp = init_repo();
        std::fs::write(tmp.path().join("a.bin"), b"x").unwrap();
        let abs = tmp.path().join("a.bin");
        let path = resolve_lock_path(tmp.path(), tmp.path(), abs.to_str().unwrap()).unwrap();
        assert_eq!(path, "a.bin");
    }

    #[test]
    fn resolve_lock_path_subdir_uses_forward_slashes() {
        let tmp = init_repo();
        std::fs::create_dir(tmp.path().join("data")).unwrap();
        std::fs::write(tmp.path().join("data/blob.bin"), b"x").unwrap();
        let path = resolve_lock_path(tmp.path(), tmp.path(), "data/blob.bin").unwrap();
        assert_eq!(path, "data/blob.bin");
    }

    #[test]
    fn resolve_lock_path_rejects_directory() {
        let tmp = init_repo();
        std::fs::create_dir(tmp.path().join("data")).unwrap();
        let err = resolve_lock_path(tmp.path(), tmp.path(), "data").unwrap_err();
        assert!(err.contains("directory"), "{err}");
    }

    #[test]
    fn resolve_lock_path_rejects_outside_repo() {
        let tmp_repo = init_repo();
        let tmp_other = TempDir::new().unwrap();
        std::fs::write(tmp_other.path().join("x.bin"), b"x").unwrap();
        let outside = tmp_other.path().join("x.bin");
        let err = resolve_lock_path(tmp_repo.path(), tmp_repo.path(), outside.to_str().unwrap())
            .unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn resolve_lock_path_allows_nonexistent_leaf() {
        // Locking a file before creating it is allowed.
        let tmp = init_repo();
        let path = resolve_lock_path(tmp.path(), tmp.path(), "nope.bin").unwrap();
        assert_eq!(path, "nope.bin");
    }
}
