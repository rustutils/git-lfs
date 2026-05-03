//! Refspec resolution for the LFS lock APIs and ref enumeration
//! helpers used by fetch-recent / prune retention.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Error;

/// Resolve the refspec to send with lock-API requests, or `None` if
/// the working tree is on a detached HEAD.
pub fn current_refspec(cwd: &Path) -> Option<String> {
    let branch = current_branch(cwd)?;
    if let Some(tracked) = tracked_upstream(cwd, &branch) {
        return Some(tracked);
    }
    Some(format!("refs/heads/{branch}"))
}

/// Short name of the current branch (`git symbolic-ref --short HEAD`),
/// or `None` for detached HEAD.
fn current_branch(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

/// `branch.<branch>.merge` if set — the upstream branch that pushes /
/// pulls of the current branch are routed to. When set, locks should
/// be scoped to this ref rather than the local branch's ref.
fn tracked_upstream(cwd: &Path, branch: &str) -> Option<String> {
    let key = format!("branch.{branch}.merge");
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", &key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

/// One ref returned by [`recent_branches`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentRef {
    /// Full ref name (`refs/heads/main`, `refs/remotes/origin/feature`,
    /// `refs/tags/v1`, ...).
    pub full: String,
    /// Hex commit OID the ref points at.
    pub oid: String,
    pub kind: RefKind,
    /// Committer date as Unix epoch seconds. Useful for the per-ref
    /// `commits_days` window calculation in prune.
    pub committer_unix: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// Under `refs/heads/`.
    LocalBranch,
    /// Under `refs/remotes/`.
    RemoteBranch,
    /// Under `refs/tags/`.
    Tag,
    /// Anything else (`refs/notes/`, `refs/stash`, custom namespaces).
    Other,
}

/// Refs whose tip commit was authored on or after `since`. Mirrors
/// upstream's `git.RecentBranches` (`git/git.go::RecentBranches`).
///
/// Output is filtered:
/// - if `include_remote_branches` is false, refs under `refs/remotes/`
///   are dropped entirely.
/// - if `only_remote` is `Some(name)`, remote refs not under
///   `refs/remotes/<name>/` are dropped (local refs and tags pass
///   through regardless).
///
/// `git for-each-ref` is asked to sort newest-first; the iteration
/// stops at the first ref older than `since` so large repos don't
/// pay for refs they'd discard anyway.
pub fn recent_branches(
    cwd: &Path,
    since: SystemTime,
    include_remote_branches: bool,
    only_remote: Option<&str>,
) -> Result<Vec<RecentRef>, Error> {
    let since_unix: i64 = since
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname) %(objectname) %(committerdate:unix)",
            "refs",
        ])
        .output()?;
    if !out.status.success() {
        return Err(Error::Failed(format!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let mut result = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(full), Some(oid), Some(unix_str)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(committer_unix) = unix_str.trim().parse::<i64>() else {
            continue;
        };
        // Sorted newest-first → first ref older than the cutoff means
        // every remaining one is too.
        if committer_unix < since_unix {
            break;
        }
        let kind = classify_ref(full);
        if matches!(kind, RefKind::RemoteBranch) {
            if !include_remote_branches {
                continue;
            }
            if let Some(remote) = only_remote {
                let prefix = format!("refs/remotes/{remote}/");
                if !full.starts_with(&prefix) {
                    continue;
                }
            }
        }
        result.push(RecentRef {
            full: full.to_owned(),
            oid: oid.to_owned(),
            kind,
            committer_unix,
        });
    }
    Ok(result)
}

/// One entry from `git worktree list --porcelain`. Includes the main
/// working copy plus every linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// Absolute path to the worktree's directory.
    pub dir: std::path::PathBuf,
    /// Tip commit SHA, or `None` for bare worktrees.
    pub head: Option<String>,
    /// `true` when git considers this entry prunable — typically the
    /// worktree's directory has been deleted but `git worktree prune`
    /// hasn't run. Mirrors upstream's `Prunable` field; prune retention
    /// keeps the HEAD-state but skips the index scan for prunable
    /// entries (the index file may still be inaccessible / removed).
    pub prunable: bool,
}

/// Every worktree attached to this repo (`git worktree list
/// --porcelain -z`). Includes the main working copy. Returns an empty
/// vec when `git worktree` exits non-zero (older git versions, bare
/// repos with no linked worktrees, etc.).
pub fn worktrees(cwd: &Path) -> Vec<WorktreeEntry> {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_worktree_list(&out.stdout)
}

fn parse_worktree_list(bytes: &[u8]) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    // Records are NUL-separated lines; an empty record terminates an
    // entry. With `-z` git emits `\0` between every field, including
    // an extra `\0` between entries — easiest to split + match by
    // line-prefix and treat empty splits as separators.
    for record in bytes.split(|&b| b == 0) {
        let Ok(line) = std::str::from_utf8(record) else {
            continue;
        };
        if line.is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            // Starting a new entry; flush whatever was being built.
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                dir: std::path::PathBuf::from(rest),
                head: None,
                prunable: false,
            });
        } else if let Some(rest) = line.strip_prefix("HEAD ")
            && let Some(c) = current.as_mut()
        {
            c.head = Some(rest.to_owned());
        } else if line.starts_with("prunable")
            && let Some(c) = current.as_mut()
        {
            c.prunable = true;
        } else if line == "bare" {
            // Bare worktree — ignore the entire entry (no index, no
            // HEAD content to retain).
            current = None;
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    entries
}

fn classify_ref(full: &str) -> RefKind {
    if full.starts_with("refs/heads/") {
        RefKind::LocalBranch
    } else if full.starts_with("refs/remotes/") {
        RefKind::RemoteBranch
    } else if full.starts_with("refs/tags/") {
        RefKind::Tag
    } else {
        RefKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::commit_helper;

    #[test]
    fn refspec_falls_back_to_current_branch() {
        let tmp = commit_helper::init_repo();
        commit_helper::commit_file(&tmp, "a.txt", b"hi");
        // init_repo uses --initial-branch=main.
        assert_eq!(
            current_refspec(tmp.path()).as_deref(),
            Some("refs/heads/main"),
        );
    }

    #[test]
    fn refspec_prefers_tracked_upstream() {
        let tmp = commit_helper::init_repo();
        commit_helper::commit_file(&tmp, "a.txt", b"hi");
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "branch.main.merge", "refs/heads/tracked"])
            .status()
            .unwrap();
        assert_eq!(
            current_refspec(tmp.path()).as_deref(),
            Some("refs/heads/tracked"),
        );
    }

    #[test]
    fn recent_branches_returns_main_for_fresh_repo() {
        let tmp = commit_helper::init_repo();
        commit_helper::commit_file(&tmp, "a.txt", b"hi");
        let refs = recent_branches(tmp.path(), UNIX_EPOCH, true, None).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].full, "refs/heads/main");
        assert_eq!(refs[0].kind, RefKind::LocalBranch);
    }

    #[test]
    fn recent_branches_drops_remotes_when_excluded() {
        let tmp = commit_helper::init_repo();
        commit_helper::commit_file(&tmp, "a.txt", b"hi");
        // Synthesize a remote-tracking ref by pointing it at HEAD.
        let head = commit_helper::head_oid(&tmp);
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["update-ref", "refs/remotes/origin/main", &head])
            .status()
            .unwrap();
        let with_remotes = recent_branches(tmp.path(), UNIX_EPOCH, true, None).unwrap();
        let without = recent_branches(tmp.path(), UNIX_EPOCH, false, None).unwrap();
        assert!(
            with_remotes
                .iter()
                .any(|r| r.full == "refs/remotes/origin/main")
        );
        assert!(!without.iter().any(|r| r.full == "refs/remotes/origin/main"));
        // Local branch always survives.
        assert!(without.iter().any(|r| r.full == "refs/heads/main"));
    }

    #[test]
    fn recent_branches_only_remote_filter() {
        let tmp = commit_helper::init_repo();
        commit_helper::commit_file(&tmp, "a.txt", b"hi");
        let head = commit_helper::head_oid(&tmp);
        for r in ["refs/remotes/origin/main", "refs/remotes/upstream/main"] {
            Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(["update-ref", r, &head])
                .status()
                .unwrap();
        }
        let only_origin = recent_branches(tmp.path(), UNIX_EPOCH, true, Some("origin")).unwrap();
        assert!(
            only_origin
                .iter()
                .any(|r| r.full == "refs/remotes/origin/main")
        );
        assert!(
            !only_origin
                .iter()
                .any(|r| r.full == "refs/remotes/upstream/main")
        );
    }

    #[test]
    fn recent_branches_skips_old_refs() {
        let tmp = commit_helper::init_repo();
        commit_helper::commit_file(&tmp, "a.txt", b"hi");
        // Cutoff strictly in the future → no refs qualify.
        let future = SystemTime::now() + std::time::Duration::from_secs(86400);
        let refs = recent_branches(tmp.path(), future, true, None).unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn refspec_none_on_detached_head() {
        let tmp = commit_helper::init_repo();
        commit_helper::commit_file(&tmp, "a.txt", b"hi");
        let head = commit_helper::head_oid(&tmp);
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["checkout", "--quiet", &head])
            .status()
            .unwrap();
        assert_eq!(current_refspec(tmp.path()), None);
    }
}
