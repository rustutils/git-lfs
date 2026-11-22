//! `git lfs pre-push`: the pre-push hook, which uploads LFS objects before
//! `git push` sends commits.
//!
//! Git passes the remote as an argument and one line per ref on stdin:
//!
//! ```text
//! <local ref> <local sha> <remote ref> <remote sha>
//! ```
//!
//! The objects uploaded are those in commits reachable from the local
//! SHAs but not from the remote SHAs. When the remote names a configured
//! remote or matches one's URL, commits on that remote's tracking refs
//! are excluded too. Deleted refs are skipped.
//!
//! A remote that is a local path gets the objects through the
//! `lfs-standalone-file` adapter, like any `file://` remote.

use std::io::BufRead;
use std::path::Path;

use crate::push::{PushCommandError, PushOutcome, remote_tracking_refs, upload_in_range_with_args};

/// Run `git lfs pre-push`, reading the ref lines from `stdin`.
///
/// Any value of `GIT_LFS_SKIP_PUSH` other than empty or `0` skips the
/// upload. Upstream parses it as a boolean, so `false` doesn't skip there.
pub fn pre_push<R: BufRead>(
    cwd: &Path,
    remote: &str,
    stdin: R,
    dry_run: bool,
) -> Result<PushOutcome, PushCommandError> {
    if std::env::var_os("GIT_LFS_SKIP_PUSH").is_some_and(|v| v != "0" && !v.is_empty()) {
        return Ok(PushOutcome::default());
    }

    if !is_acceptable_remote(cwd, remote) {
        return Err(PushCommandError::Usage(format!(
            "Invalid remote name {remote:?}"
        )));
    }

    let mut includes: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut remote_refs: Vec<String> = Vec::new();
    let mut needs_remote_tracking = false;

    for line in stdin.lines() {
        let line = line.map_err(PushCommandError::Io)?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let local_sha = parts[1];
        let remote_ref = parts[2];
        let remote_sha = parts[3];

        // A deleted ref.
        if is_zero_oid(local_sha) {
            continue;
        }

        includes.push(local_sha.to_owned());
        remote_refs.push(remote_ref.to_owned());
        // A remote SHA we don't have, as after a force push, would make
        // `git rev-list` fail. t-pre-push.sh checks this.
        if is_zero_oid(remote_sha) {
            needs_remote_tracking = true;
        } else if object_exists(cwd, remote_sha) {
            excludes.push(remote_sha.to_owned());
        }
    }

    if includes.is_empty() {
        return Ok(PushOutcome::default());
    }

    let resolved_remote_name = matching_remote_name(cwd, remote);

    let mut extra_args: Vec<String> = Vec::new();
    if let Some(name) = &resolved_remote_name {
        // On the command line, not stdin, because t-pre-push.sh looks
        // for `--not --remotes=origin` in the trace.
        extra_args.push("--not".into());
        extra_args.push(format!("--remotes={name}"));
    } else if needs_remote_tracking {
        // `remote` isn't a configured remote here, so this rarely finds
        // any refs.
        excludes.extend(remote_tracking_refs(cwd, remote)?);
    }

    // Some servers need the destination ref in the batch request. A
    // batch carries one ref, so send it only when one ref is pushed.
    remote_refs.sort();
    remote_refs.dedup();
    let refspec = if remote_refs.len() == 1 {
        remote_refs.pop()
    } else {
        None
    };

    let inc: Vec<&str> = includes.iter().map(String::as_str).collect();
    let exc: Vec<&str> = excludes.iter().map(String::as_str).collect();
    let extra: Vec<&str> = extra_args.iter().map(String::as_str).collect();
    upload_in_range_with_args(cwd, remote, &inc, &exc, &extra, refspec, dry_run)
}

/// The configured remote named `value`, or whose `remote.<name>.url` is
/// `value`.
fn matching_remote_name(cwd: &Path, value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    for name in String::from_utf8_lossy(&out.stdout).lines() {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if name == value {
            return Some(name.to_owned());
        }
        let url_out = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["config", "--get", &format!("remote.{name}.url")])
            .output()
            .ok()?;
        if url_out.status.success() && String::from_utf8_lossy(&url_out.stdout).trim() == value {
            return Some(name.to_owned());
        }
    }
    None
}

/// Whether `s` is Git's all-zero object ID, for SHA-1 or SHA-256.
fn is_zero_oid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}

/// Whether the Git object `oid` exists locally.
fn object_exists(cwd: &Path, oid: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["cat-file", "-e", oid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether `remote` is a URL, an SCP-style `host:path`, a name with an LFS
/// endpoint, or a local directory.
fn is_acceptable_remote(cwd: &Path, remote: &str) -> bool {
    if remote.is_empty() {
        return false;
    }
    if git_lfs_git::endpoint::looks_like_url(remote) {
        return true;
    }
    if remote.contains(':') {
        // SCP-style `host:path`, which upstream's `ValidateRemoteURL`
        // allows.
        return true;
    }
    if git_lfs_git::endpoint::endpoint_for_remote(cwd, Some(remote)).is_ok() {
        return true;
    }
    std::path::Path::new(remote).is_dir()
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
