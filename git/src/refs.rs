//! Refspec resolution for the LFS lock APIs.
//!
//! LFS lock create/delete/list endpoints all carry a `ref.name` field
//! that the server can use to enforce per-branch lock scoping. Upstream
//! resolves this once per command and ships it on every request: the
//! tracked upstream branch (from `branch.<current>.merge`) takes
//! precedence, falling back to the current branch's full ref. A
//! detached HEAD has no resolvable refspec, in which case the field
//! is omitted from the request.

use std::path::Path;
use std::process::Command;

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
