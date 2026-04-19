//! `git lfs pre-push <remote> <url>` — git hook entry point.
//!
//! Git's `pre-push` hook runs before `git push` actually contacts the
//! remote. It receives `<remote>` and `<url>` as args, plus one stdin
//! line per ref being pushed in the form:
//!
//! ```text
//! <local ref> <local sha> <remote ref> <remote sha>
//! ```
//!
//! For each line, we want to upload every LFS object reachable from
//! `<local sha>` that the remote doesn't already have. The "doesn't
//! have" boundary is `<remote sha>` for existing branches, or all of
//! `refs/remotes/<remote>/*` for brand-new branches (`<remote sha>` is
//! all-zeros in that case). Branch deletes (`<local sha>` all-zeros)
//! are skipped — nothing to upload.
//!
//! Honors `GIT_LFS_SKIP_PUSH=1` as an early no-op (matches upstream).

use std::io::BufRead;
use std::path::Path;

use git_lfs_transfer::Report;

use crate::push::{PushCommandError, remote_tracking_refs, upload_in_range};

/// Run the pre-push command. `stdin_lines` is typically `io::stdin().lock()`.
pub fn pre_push<R: BufRead>(
    cwd: &Path,
    remote: &str,
    stdin: R,
) -> Result<Report, PushCommandError> {
    if std::env::var_os("GIT_LFS_SKIP_PUSH").is_some_and(|v| v != "0" && !v.is_empty()) {
        return Ok(Report::default());
    }

    let mut includes: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut needs_remote_tracking = false;

    for line in stdin.lines() {
        let line = line.map_err(PushCommandError::Io)?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            // Empty / malformed line — ignore. Git always sends 4 fields,
            // but `git push --delete` style operations can have edge
            // cases worth being lenient about.
            continue;
        }
        let local_sha = parts[1];
        let remote_sha = parts[3];

        if is_zero_oid(local_sha) {
            // Branch delete — no LFS work to do.
            continue;
        }

        includes.push(local_sha.to_owned());
        if is_zero_oid(remote_sha) {
            // New branch — remote has nothing for this ref. Fall back
            // to "everything else the remote tracks" as the exclude
            // set, same as our `git lfs push` default.
            needs_remote_tracking = true;
        } else {
            excludes.push(remote_sha.to_owned());
        }
    }

    if includes.is_empty() {
        return Ok(Report::default());
    }

    if needs_remote_tracking {
        excludes.extend(remote_tracking_refs(cwd, remote)?);
    }

    let inc: Vec<&str> = includes.iter().map(String::as_str).collect();
    let exc: Vec<&str> = excludes.iter().map(String::as_str).collect();
    upload_in_range(cwd, &inc, &exc)
}

/// True if `s` is a non-empty hex string of all zeros — git's marker for
/// "no commit" in pre-push input. Length-agnostic so SHA-1 (40 chars)
/// and SHA-256 (64 chars) both work.
fn is_zero_oid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_zero_oids() {
        assert!(is_zero_oid("0000000000000000000000000000000000000000"));
        assert!(is_zero_oid(
            "0000000000000000000000000000000000000000000000000000000000000000"
        ));
        assert!(!is_zero_oid("0000000000000000000000000000000000000001"));
        assert!(!is_zero_oid(""));
    }
}
