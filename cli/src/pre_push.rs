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

use crate::push::{PushCommandError, PushOutcome, remote_tracking_refs, upload_in_range_with_args};

/// Run the pre-push command. `stdin_lines` is typically `io::stdin().lock()`.
pub fn pre_push<R: BufRead>(
    cwd: &Path,
    remote: &str,
    stdin: R,
    dry_run: bool,
) -> Result<PushOutcome, PushCommandError> {
    if std::env::var_os("GIT_LFS_SKIP_PUSH").is_some_and(|v| v != "0" && !v.is_empty()) {
        return Ok(PushOutcome::default());
    }

    // Validate the remote upfront — `git lfs pre-push not-a-remote …`
    // (t-pre-push 15) wants the user-facing "Invalid remote name"
    // message before we try anything else. We accept anything that
    // looks like a URL, an SCP-style `host:path`, a local directory
    // (so `git push ../sibling-clone` works), or a configured remote.
    // Anything else is rejected.
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
            // Empty / malformed line — ignore. Git always sends 4 fields,
            // but `git push --delete` style operations can have edge
            // cases worth being lenient about.
            continue;
        }
        let local_sha = parts[1];
        let remote_ref = parts[2];
        let remote_sha = parts[3];

        if is_zero_oid(local_sha) {
            // Branch delete — no LFS work to do.
            continue;
        }

        includes.push(local_sha.to_owned());
        remote_refs.push(remote_ref.to_owned());
        if is_zero_oid(remote_sha) {
            // New branch — remote has nothing for this ref. Fall back
            // to "everything else the remote tracks" as the exclude
            // set, same as our `git lfs push` default.
            needs_remote_tracking = true;
        } else if object_exists(cwd, remote_sha) {
            excludes.push(remote_sha.to_owned());
        }
        // If the remote-side OID isn't in our local store (force-push
        // after local GC, t-pre-push 39), drop it from the exclude
        // set — `git rev-list --not <missing>` would error out.
    }

    if includes.is_empty() {
        return Ok(PushOutcome::default());
    }

    // Local-path push (`git push ../sibling`, `git push .`) — there's
    // no LFS server, so we can't go through the batch API. Copy each
    // reachable LFS object directly into the target repo's
    // `lfs/objects/` so the downstream checkout can smudge.
    if is_local_path_remote(cwd, remote) {
        return push_to_local_path(cwd, remote, &includes, &excludes);
    }

    // Resolve `remote` (which can be a URL when the user runs
    // `git push <url>`) to the configured remote name whose URL it
    // matches, if any. Drives the upstream `--not --remotes=<name>`
    // optimization below: when the push target *is* one of our
    // tracked remotes, we can hand its full ref namespace to
    // rev-list as one expression rather than enumerating each
    // `refs/remotes/<name>/<branch>` ourselves.
    let resolved_remote_name = matching_remote_name(cwd, remote);

    let mut extra_args: Vec<String> = Vec::new();
    if let Some(name) = &resolved_remote_name {
        // Pass on the cmdline (not via stdin) so the GIT_TRACE output
        // shows it verbatim — t-pre-push 37 greps the trace for
        // `rev-list.*--not --remotes=origin`.
        extra_args.push("--not".into());
        extra_args.push(format!("--remotes={name}"));
    } else if needs_remote_tracking {
        // Fallback for URL pushes that don't match any remote: still
        // exclude objects already in the (now name-pinned) remote
        // tracking branches by listing each ref explicitly.
        excludes.extend(remote_tracking_refs(cwd, remote)?);
    }

    // Branch-required servers reject batch requests without a refspec
    // matching the destination ref. Use the remote ref from stdin —
    // single-ref pushes are unambiguous; multi-ref pushes don't get a
    // refspec since one batch can only carry one. Drop duplicates so
    // `git push origin main main` doesn't look like a multi-ref push.
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

/// Find the configured remote whose `remote.<name>.url` equals
/// `value` (or matches `value` as a name directly). Used by pre-push
/// to enable the `--not --remotes=<name>` rev-list optimization on
/// URL-style pushes.
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

/// True if `s` is a non-empty hex string of all zeros — git's marker for
/// "no commit" in pre-push input. Length-agnostic so SHA-1 (40 chars)
/// and SHA-256 (64 chars) both work.
fn is_zero_oid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}

/// Is `remote` something we should accept as a destination? Mirrors
/// upstream's `git.ValidateRemote` + `RewriteLocalPathAsURL`:
/// configured remote names, URL-shaped strings, SCP-style `host:path`,
/// and local directories all pass.
/// Copy every LFS object reachable from `includes` (minus those
/// reachable from `excludes`) into `remote`'s LFS object store.
/// Hardlinks where possible, falls back to copy on cross-device or
/// unsupported errors. Used by pre-push when the user is pushing to
/// a local-path remote (`git push ../sibling`, `git push .`).
fn push_to_local_path(
    cwd: &Path,
    remote: &str,
    includes: &[String],
    excludes: &[String],
) -> Result<PushOutcome, PushCommandError> {
    let target = std::path::Path::new(remote);
    let target_lfs_dir = git_lfs_git::lfs_dir(target).map_err(|e| {
        PushCommandError::Usage(format!("local-path remote {remote:?} has no git dir: {e}"))
    })?;
    if target_lfs_dir == git_lfs_git::lfs_dir(cwd).unwrap_or_default() {
        // `git push . main:foo` — source and target share an LFS
        // store. Nothing to copy.
        return Ok(PushOutcome::default());
    }

    let inc: Vec<&str> = includes.iter().map(String::as_str).collect();
    let exc: Vec<&str> = excludes.iter().map(String::as_str).collect();
    let pointers = git_lfs_git::scan_pointers_with_args(cwd, &inc, &exc, &[])?;

    let local_store = git_lfs_store::Store::new(git_lfs_git::lfs_dir(cwd)?);
    let target_objects_root = target_lfs_dir.join("objects");

    for entry in &pointers {
        let oid = entry.oid;
        let src = local_store.object_path(oid);
        if !src.is_file() {
            // Object isn't in our store — same situation as a missing
            // upload to a regular remote. Quietly skip; the downstream
            // smudge will surface the gap if it matters.
            continue;
        }
        let hex = oid.to_string();
        let dst = target_objects_root
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(&hex);
        if dst.is_file() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::hard_link(&src, &dst).is_err() {
            std::fs::copy(&src, &dst)?;
        }
    }

    Ok(PushOutcome::default())
}

/// `true` if `git cat-file -e <oid>` succeeds — i.e. the object is
/// present in the repo's object database (or any borrowed alternate).
/// Used to filter excludes that point at GC'd objects.
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

/// `true` when `remote` resolves to a local directory and isn't also a
/// configured remote name or a URL we can derive an LFS endpoint from.
/// Used by pre-push to short-circuit `git push ../sibling-clone`
/// style invocations that have no LFS server.
fn is_local_path_remote(cwd: &Path, remote: &str) -> bool {
    if git_lfs_git::looks_like_url(remote) {
        return false;
    }
    if remote.contains(':') {
        return false;
    }
    if git_lfs_git::endpoint_for_remote(cwd, Some(remote)).is_ok() {
        return false;
    }
    std::path::Path::new(remote).is_dir()
}

fn is_acceptable_remote(cwd: &Path, remote: &str) -> bool {
    if remote.is_empty() {
        return false;
    }
    if git_lfs_git::looks_like_url(remote) {
        return true;
    }
    if remote.contains(':') {
        // SCP-style `git@host:path/to/repo`. `looks_like_url` already
        // catches anything with `@`, so this picks up the colon-only
        // forms upstream's `ValidateRemoteURL` allows.
        return true;
    }
    if git_lfs_git::endpoint_for_remote(cwd, Some(remote)).is_ok() {
        return true;
    }
    // Local path push (`git push ../sibling`, `git push .`). Accept if
    // it's a directory we can read; the actual git/LFS push semantics
    // happen further down — the remote-name layer just needs to know
    // this isn't a typo.
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
