//! `git lfs lock`, `git lfs locks`, `git lfs unlock` — the file-lock
//! command surface. All three speak the locking API in `api/src/locks.rs`;
//! this module is mostly path-resolution + flag dispatch + display logic.
//!
//! Deferred (see NOTES.md): `--local` / `--cached` for `locks` (require
//! an on-disk lock cache we don't have).

use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_api::{
    ApiError, Client as ApiClient, CreateLockError, CreateLockRequest, DeleteLockRequest,
    ListLocksFilter, Lock, LockList, Ref, VerifyLocksRequest, VerifyLocksResponse,
};
use serde::Serialize;
use tokio::runtime::Runtime;

use crate::fetcher::build_api_client;
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
    /// Override the auto-detected refspec (`branch.<current>.merge` or
    /// the current branch). `None` means "auto".
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
    let api = build_api_client(cwd, opts.remote.as_deref())
        .map_err(LockCommandError::Build)?;
    let runtime = build_runtime()?;
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
        let mut req = CreateLockRequest::new(path.clone());
        if let Some(name) = &refspec {
            req = req.with_ref(Ref::new(name.clone()));
        }
        match runtime.block_on(api.create_lock(&req)) {
            Ok(lock) => {
                if !opts.json {
                    println!("Locked {path} ({})", lock.id);
                }
                // The user took the lock to edit the file; if the
                // file is currently read-only (because it matches a
                // lockable pattern and the previous post-* hook
                // chmod'd it), they need it writable. Best-effort —
                // a chmod failure shouldn't fail the lock op.
                let _ = lockable::force_writable(&root, &path);
                locks.push(lock);
            }
            Err(CreateLockError::Conflict { existing, message }) => {
                // Match upstream's "Locking <path> failed: <reason>"
                // shape (see `t-lock.sh::locking a previously locked
                // file`). Owner goes on its own follow-up line.
                eprintln!("Locking {path} failed: {message}");
                if let Some(owner) = existing.as_ref().and_then(|l| l.owner.as_ref()) {
                    eprintln!("  Lock owner: {}", owner.name);
                }
                success = false;
            }
            Err(CreateLockError::Api(e)) => {
                eprintln!("Locking {path} failed: {}", api_error_reason(&e));
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
    let api = build_api_client(cwd, opts.remote.as_deref())
        .map_err(LockCommandError::Build)?;
    let runtime = build_runtime()?;
    let root = repo_root(cwd).map_err(LockCommandError::Build)?;
    let refspec = resolve_refspec(&root, opts.refspec.as_deref());

    if opts.verify {
        let resp = runtime
            .block_on(verify_all(&api, opts.limit, refspec.clone()))
            .map_err(|e| format_api_error(&e))
            .map_err(LockCommandError::Api)?;
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

    // Path filter must be relativized just like `lock` does — the server
    // stores repo-relative paths, so a user-supplied `./data/x.bin`
    // wouldn't match anything otherwise.
    let path_filter = match opts.path.as_deref() {
        Some(raw) => {
            Some(resolve_lock_path(cwd, &root, raw).map_err(LockCommandError::Build)?)
        }
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
        let page: LockList = runtime
            .block_on(api.list_locks(&filter))
            .map_err(|e| LockCommandError::Api(format_api_error(&e)))?;
        all_locks.extend(page.locks);
        // Check the limit BEFORE the cursor — a server that ignores
        // `?limit=N` and returns more on the last page would otherwise
        // sneak past us (we'd see no next_cursor and exit before the
        // truncate would have fired).
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

/// Drain `verify_locks` across all pages, since the API paginates the same
/// way `list_locks` does.
async fn verify_all(
    api: &ApiClient,
    limit: Option<u32>,
    refspec: Option<String>,
) -> Result<VerifyLocksResponse, git_lfs_api::ApiError> {
    let mut req = VerifyLocksRequest {
        limit,
        r#ref: refspec.map(Ref::new),
        ..Default::default()
    };
    let mut combined = VerifyLocksResponse {
        ours: Vec::new(),
        theirs: Vec::new(),
        next_cursor: None,
    };
    loop {
        let page = api.verify_locks(&req).await?;
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
        // Capital "E" matches the upstream test grep.
        return Err(LockCommandError::Build(
            "Exactly one of --id or a set of paths must be provided".into(),
        ));
    }

    let api = build_api_client(cwd, opts.remote.as_deref())
        .map_err(LockCommandError::Build)?;
    let runtime = build_runtime()?;
    let root = repo_root(cwd).map_err(LockCommandError::Build)?;
    let refspec = resolve_refspec(&root, opts.refspec.as_deref());
    let lockable_readonly = crate::lockable::lockable_readonly_enabled(&root);
    let mut success = true;
    let mut report: Vec<UnlockJsonEntry> = Vec::new();

    if has_id {
        let id = opts.id.clone().expect("checked above");
        let req = build_delete_request(opts.force, refspec.as_deref());
        let attrs = git_lfs_git::AttrSet::from_workdir(&root).ok();
        match runtime.block_on(api.delete_lock(&id, &req)) {
            Ok(lock) => {
                if !opts.json {
                    println!("Unlocked Lock {id}");
                } else {
                    report.push(UnlockJsonEntry {
                        id: Some(id),
                        path: None,
                        unlocked: true,
                        reason: None,
                    });
                }
                // The server hands back the unlocked lock's path in
                // the response; use it to restore the read-only
                // invariant for lockable patterns. Without this,
                // `unlock --id=<id>` doesn't chmod even though
                // path-based unlock does.
                if lockable_readonly && let Some(attrs) = attrs.as_ref() {
                    let _ = lockable::enforce_readonly_if_lockable(
                        &root, attrs, &lock.path,
                    );
                }
            }
            Err(e) => {
                eprintln!("Unlocking {id} failed: {}", api_error_reason(&e));
                success = false;
                if opts.json {
                    report.push(UnlockJsonEntry {
                        id: Some(id),
                        path: None,
                        unlocked: false,
                        reason: Some(api_error_reason(&e)),
                    });
                }
            }
        }
    } else {
        // Built once so we can flip a successfully-released lockable
        // path back to read-only without re-parsing `.gitattributes`
        // for every path.
        let attrs = git_lfs_git::AttrSet::from_workdir(&root).ok();
        for raw in paths {
            let path = match resolve_lock_path(cwd, &root, raw) {
                Ok(p) => p,
                Err(e) if opts.force => {
                    // Match upstream: with --force, fall back to the
                    // raw path. The server may still reject it.
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

            // Refuse to unlock a path with uncommitted edits unless
            // `--force` is given (in which case we warn and continue,
            // matching upstream).
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

            // Look up the lock id by path. Use a bounded list — we want
            // exact-path matches, of which there's at most one.
            let lookup = ListLocksFilter {
                path: Some(path.clone()),
                refspec: refspec.clone(),
                ..Default::default()
            };
            let id = match runtime.block_on(api.list_locks(&lookup)) {
                Ok(list) => list
                    .locks
                    .iter()
                    .find(|l| l.path == path)
                    .map(|l| l.id.clone()),
                Err(e) => {
                    eprintln!("git-lfs: lookup failed for {path}: {}", format_api_error(&e));
                    success = false;
                    if opts.json {
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(path.clone()),
                            unlocked: false,
                            reason: Some(format_api_error(&e)),
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
            let req = build_delete_request(opts.force, refspec.as_deref());
            match runtime.block_on(api.delete_lock(&id, &req)) {
                Ok(_) => {
                    if !opts.json {
                        println!("Unlocked {path}");
                    }
                    // Restore the read-only invariant for lockable
                    // paths now that we no longer hold the lock —
                    // unless `lfs.setlockablereadonly=false` opts out.
                    if lockable_readonly {
                        if let Some(attrs) = attrs.as_ref() {
                            let _ = lockable::enforce_readonly_if_lockable(
                                &root, attrs, &path,
                            );
                        }
                    }
                    if opts.json {
                        // Path-based unlocks emit only `path` and
                        // `unlocked` per upstream's JSON schema; the
                        // id field is reserved for `--id`-keyed
                        // unlocks.
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(path),
                            unlocked: true,
                            reason: None,
                        });
                    }
                }
                Err(e) => {
                    eprintln!("Unlocking {path} failed: {}", api_error_reason(&e));
                    success = false;
                    if opts.json {
                        report.push(UnlockJsonEntry {
                            id: None,
                            path: Some(path),
                            unlocked: false,
                            reason: Some(api_error_reason(&e)),
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

fn build_delete_request(force: bool, refspec: Option<&str>) -> DeleteLockRequest {
    DeleteLockRequest {
        force,
        r#ref: refspec.map(|n| Ref::new(n.to_string())),
    }
}

// --------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------

fn build_runtime() -> std::io::Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// Resolve the refspec to send with lock-API requests: caller-supplied
/// override wins, else `git_lfs_git::refs::current_refspec`. `None`
/// means "send no ref" (detached HEAD with no override).
fn resolve_refspec(repo_root: &Path, override_ref: Option<&str>) -> Option<String> {
    if let Some(s) = override_ref {
        return Some(s.to_owned());
    }
    git_lfs_git::refs::current_refspec(repo_root)
}

/// Pull just the human-readable reason out of an `ApiError`. For
/// `Status` with a server-supplied error body, use the body's
/// `message` directly so the user sees what the LFS server said
/// without our "server returned status N:" wrapper. The wrapper makes
/// the test grep brittle (e.g. `grep 'Locking a.dat failed: Expected
/// ref ...'`).
fn api_error_reason(e: &ApiError) -> String {
    match e {
        ApiError::Status {
            body: Some(b),
            ..
        } => b.message.clone(),
        _ => e.to_string(),
    }
}

/// Same idea as [`api_error_reason`], used at call sites where the
/// command is wrapping the error as a string for upper layers (e.g.
/// `lookup failed for {path}: {…}`).
fn format_api_error(e: &ApiError) -> String {
    api_error_reason(e)
}

/// True if `path` (relative to `root`) has staged or unstaged
/// modifications. `git status --porcelain -- <path>` prints a line
/// only for dirty paths; an empty result means clean. Errors fall
/// back to "clean" so a `git status` failure doesn't block unlock.
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

/// Resolve `file` to a repo-relative POSIX path suitable for the locking
/// API. Mirrors upstream's `lockPath`.
fn resolve_lock_path(cwd: &Path, repo_root: &Path, file: &str) -> Result<String, String> {
    let file_path = Path::new(file);
    let abs = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        cwd.join(file_path)
    };

    // Canonicalize parents but allow non-existent leaves (locking a file
    // that doesn't exist yet should still work — server stores the path
    // verbatim). We canonicalize the parent, then re-attach the file
    // name.
    let canonical = match abs.canonicalize() {
        Ok(p) => p,
        Err(_) => match abs.parent() {
            Some(parent) => {
                let parent_canon = parent
                    .canonicalize()
                    .map_err(|e| format!("canonicalizing {}: {e}", parent.display()))?;
                if let Some(name) = abs.file_name() {
                    parent_canon.join(name)
                } else {
                    return Err(format!("invalid path: {file}"));
                }
            }
            None => return Err(format!("invalid path: {file}")),
        },
    };

    let root_canon = repo_root
        .canonicalize()
        .map_err(|e| format!("canonicalizing repo root: {e}"))?;

    let rel = canonical.strip_prefix(&root_canon).map_err(|_| {
        format!("path is outside the repository: {file}")
    })?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() || s == "." {
        return Err(format!("cannot lock the repository root: {file}"));
    }

    if canonical.is_dir() {
        // Test grep expects "cannot lock directory" verbatim
        // (`t-lock.sh::locking a directory`).
        return Err(format!("cannot lock directory: {file}"));
    }

    Ok(s)
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

fn print_verify_table(resp: &VerifyLocksResponse) {
    let mut combined = Vec::with_capacity(resp.ours.len() + resp.theirs.len());
    combined.extend(resp.ours.iter().cloned());
    combined.extend(resp.theirs.iter().cloned());
    let owned: std::collections::HashSet<String> =
        resp.ours.iter().map(|l| l.id.clone()).collect();
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
        let err = resolve_lock_path(
            tmp_repo.path(),
            tmp_repo.path(),
            outside.to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn resolve_lock_path_allows_nonexistent_leaf() {
        // Locking a file that doesn't exist yet should be permitted —
        // the server stores the path string verbatim, and locking
        // ahead of file creation is a legitimate workflow.
        let tmp = init_repo();
        let path = resolve_lock_path(tmp.path(), tmp.path(), "nope.bin").unwrap();
        assert_eq!(path, "nope.bin");
    }
}
