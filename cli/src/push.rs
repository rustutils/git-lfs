//! `git lfs push <remote> [<ref>...]` — upload every LFS object reachable
//! from the given refs that the named remote doesn't already track.
//!
//! "Doesn't already track" is computed as: rev-list of `<refs>` minus
//! everything reachable from `refs/remotes/<remote>/*`. The list of
//! remote-tracking refs is enumerated up-front via `git for-each-ref`
//! so we can hand a flat exclude set to the scanner.
//!
//! The batch API also dedupes server-side: any object the server already
//! has comes back from the upload batch with no `actions`, and the
//! transfer queue treats that as success without sending bytes. So even
//! if our local exclude set misses something (because the user pushed
//! manually with another tool, say), we don't waste an upload.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_api::ObjectSpec;
use git_lfs_git::scan_pointers;
use git_lfs_store::Store;
use git_lfs_transfer::Report;

use crate::LfsFetcher;

#[derive(Debug, thiserror::Error)]
pub enum PushCommandError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("git for-each-ref failed: {0}")]
    ForEachRef(String),
    #[error("upload failed: {0}")]
    Fetch(git_lfs_filter::FetchError),
}

/// Outcome of a push attempt — carries the per-object [`Report`] plus
/// whether we aborted before uploading because of locally-and-remotely
/// missing objects (the `lfs.allowincompletepush=false` failure mode).
/// Callers map `aborted` to exit code 2 to match upstream.
#[derive(Debug, Default)]
pub struct PushOutcome {
    pub report: Report,
    pub aborted: bool,
}

/// Run the push command for `remote` + `refs`. Defaults `refs` to `["HEAD"]`.
pub fn push(
    cwd: &Path,
    remote: &str,
    refs: &[String],
    dry_run: bool,
) -> Result<PushOutcome, PushCommandError> {
    let default_refs = ["HEAD".to_string()];
    let effective: &[String] = if refs.is_empty() { &default_refs } else { refs };
    let ref_strs: Vec<&str> = effective.iter().map(String::as_str).collect();

    // Enumerate refs/remotes/<remote>/* — these are the OIDs we assume
    // the server already has. Empty list (e.g. brand-new remote with no
    // tracking refs yet) means we upload everything reachable.
    let excludes_owned = remote_tracking_refs(cwd, remote)?;
    let excludes: Vec<&str> = excludes_owned.iter().map(String::as_str).collect();

    upload_in_range(cwd, remote, &ref_strs, &excludes, None, dry_run)
}

/// Shared core: scan for pointers reachable from `includes` minus
/// `excludes`, partition by local availability, ask the server about
/// the missing ones, and either upload or fail per upstream's rules.
///
/// Used by both [`push`] (CLI-driven) and
/// [`crate::pre_push::pre_push`] (git-hook-driven). Progress output goes
/// to stderr so it doesn't pollute scripts piping the command's stdout.
pub(crate) fn upload_in_range(
    cwd: &Path,
    remote: &str,
    includes: &[&str],
    excludes: &[&str],
    refspec: Option<String>,
    dry_run: bool,
) -> Result<PushOutcome, PushCommandError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let pointers = scan_pointers(cwd, includes, excludes)?;

    // Partition pointers: present locally vs. only-as-pointer. We need
    // the missing ones to ask the server about — if the server holds
    // them, the push silently succeeds (test 11 territory); if not, it
    // fails unless `lfs.allowincompletepush=true`.
    let mut to_upload: Vec<ObjectSpec> = Vec::new();
    let mut paths: HashMap<String, PathBuf> = HashMap::new();
    let mut missing: Vec<(ObjectSpec, Option<PathBuf>)> = Vec::new();
    for entry in pointers {
        let oid_str = entry.oid.to_string();
        if let Some(p) = entry.path.clone() {
            paths.entry(oid_str.clone()).or_insert(p);
        }
        let spec = ObjectSpec {
            oid: oid_str,
            size: entry.size,
        };
        if store.contains_with_size(entry.oid, entry.size) {
            to_upload.push(spec);
        } else {
            missing.push((spec, entry.path));
        }
    }

    if to_upload.is_empty() && missing.is_empty() {
        return Ok(PushOutcome::default());
    }

    if dry_run {
        // Dry-run lists all objects that would be considered — present
        // and missing alike (matches upstream: it has nothing local to
        // verify either, so the list is the same).
        for spec in &to_upload {
            if let Some(p) = paths.get(&spec.oid) {
                println!("push {} => {}", spec.oid, p.display());
            }
        }
        for (spec, _) in &missing {
            if let Some(p) = paths.get(&spec.oid) {
                println!("push {} => {}", spec.oid, p.display());
            }
        }
        return Ok(PushOutcome::default());
    }

    // For "have a pointer but not the bytes", check whether the server
    // already holds the object. If yes, treat as a silent no-op
    // (functionally equivalent to having it locally). If no, it's a
    // truly-missing object — fail hard or warn, depending on
    // `lfs.allowincompletepush`.
    let mut fetcher = LfsFetcher::from_repo_with_remote(cwd, &store, Some(remote))?;
    if refspec.is_some() {
        fetcher = fetcher.with_refspec(refspec);
    }

    // Pre-flight `/locks/verify`. Comes before any byte transfer (and
    // before the missing-object batch) so a "verification required"
    // failure aborts the push without touching the upload endpoint.
    let endpoint = git_lfs_git::endpoint_for_remote(cwd, Some(remote))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let pushed_paths: Vec<&PathBuf> = paths.values().collect();
    let theirs_blockers: Vec<git_lfs_api::Lock> =
        match fetcher.preflight_verify_locks(cwd, remote, &endpoint)? {
            crate::locks_verify::Outcome::Aborted => {
                return Ok(PushOutcome {
                    report: Report::default(),
                    aborted: true,
                });
            }
            crate::locks_verify::Outcome::Skipped => Vec::new(),
            crate::locks_verify::Outcome::Verified { ours, theirs } => {
                if !ours.is_empty() {
                    let ours_paths: Vec<&str> =
                        ours.iter().map(|l| l.path.as_str()).collect();
                    let any_ours_pushed = pushed_paths
                        .iter()
                        .any(|p| {
                            let p = p.to_string_lossy();
                            ours_paths.iter().any(|op| p == *op)
                        });
                    if any_ours_pushed {
                        eprintln!(
                            "Consider unlocking your own locked files: \
                             (`git lfs unlock <path>`)"
                        );
                        for op in &ours_paths {
                            if pushed_paths
                                .iter()
                                .any(|p| p.to_string_lossy() == *op)
                            {
                                eprintln!("* {op}");
                            }
                        }
                    }
                }
                // Path-intersect theirs with what we're pushing —
                // those are the entries that actually block this push.
                theirs
                    .into_iter()
                    .filter(|l| {
                        pushed_paths
                            .iter()
                            .any(|p| p.to_string_lossy() == l.path)
                    })
                    .collect()
            }
        };
    if !theirs_blockers.is_empty() {
        eprintln!("Unable to push locked files:");
        for l in &theirs_blockers {
            let owner = l
                .owner
                .as_ref()
                .map(|o| o.name.as_str())
                .unwrap_or("unknown user");
            eprintln!("* {} - {}", l.path, owner);
        }
        eprintln!("Cannot update locked files.");
        return Ok(PushOutcome {
            report: Report::default(),
            aborted: true,
        });
    }

    let truly_missing: Vec<(ObjectSpec, Option<PathBuf>)> = if missing.is_empty() {
        Vec::new()
    } else {
        let server_has = fetcher
            .check_server_has(missing.iter().map(|(s, _)| s.clone()).collect())
            .map_err(PushCommandError::Fetch)?;
        missing
            .into_iter()
            .filter(|(s, _)| !server_has.contains(&s.oid))
            .collect()
    };

    if !truly_missing.is_empty() {
        let allow = allow_incomplete_push(cwd);
        if !allow {
            // Trace line + summary + per-object listing, all to stderr
            // (test grep checks combined-output but stderr keeps stdout
            // clean for users redirecting).
            for (spec, _) in &truly_missing {
                eprintln!(
                    "tq: stopping batched queue, object \"{}\" missing locally and on remote",
                    spec.oid,
                );
            }
            eprintln!("LFS upload failed:");
            for (spec, path) in &truly_missing {
                let path_str = path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                eprintln!("  (missing) {path_str} ({})", spec.oid);
            }
            return Ok(PushOutcome {
                report: Report::default(),
                aborted: true,
            });
        } else {
            // allowincompletepush=true: warn, then proceed with the
            // present-locally subset.
            eprintln!("LFS upload missing objects");
            for (spec, path) in &truly_missing {
                let path_str = path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                eprintln!("  (missing) {path_str} ({})", spec.oid);
            }
        }
    }

    if to_upload.is_empty() {
        return Ok(PushOutcome::default());
    }

    let total = to_upload.len();
    let total_bytes: u64 = to_upload.iter().map(|s| s.size).sum();
    let succeeded_bytes_lookup: HashMap<String, u64> =
        to_upload.iter().map(|s| (s.oid.clone(), s.size)).collect();

    let report = fetcher
        .upload_many(to_upload)
        .map_err(PushCommandError::Fetch)?;

    let succeeded = report.succeeded.len();
    let succeeded_bytes: u64 = report
        .succeeded
        .iter()
        .filter_map(|oid| succeeded_bytes_lookup.get(oid).copied())
        .sum();
    let percent = if total_bytes == 0 {
        100
    } else {
        ((succeeded_bytes as u128 * 100) / total_bytes as u128) as u32
    };
    eprintln!(
        "Uploading LFS objects: {percent}% ({succeeded}/{total}), {}",
        human_bytes(succeeded_bytes),
    );

    for (oid, err) in &report.failed {
        eprintln!("  {oid}: {err}");
    }
    Ok(PushOutcome {
        report,
        aborted: false,
    })
}

/// Effective value of `lfs.allowincompletepush`. Defaults to `false`
/// (matches upstream — incomplete pushes fail by default).
fn allow_incomplete_push(cwd: &Path) -> bool {
    git_lfs_git::config::get_effective(cwd, "lfs.allowincompletepush")
        .ok()
        .flatten()
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

/// Decimal (SI) byte humanizer matching `dustin/go-humanize`'s `Bytes()`,
/// which upstream uses in its progress meter. 1000-base, single decimal
/// digit, units `B / kB / MB / GB / TB / PB / EB`.
fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "kB", "MB", "GB", "TB", "PB", "EB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut idx = 0;
    while value >= 1000.0 && idx < UNITS.len() - 1 {
        value /= 1000.0;
        idx += 1;
    }
    format!("{value:.1} {}", UNITS[idx])
}

/// Enumerate every ref under `refs/remotes/<remote>/`. Returns the
/// fully-qualified ref names so they can go straight into rev-list's
/// exclude set.
pub(crate) fn remote_tracking_refs(
    cwd: &Path,
    remote: &str,
) -> Result<Vec<String>, PushCommandError> {
    let pattern = format!("refs/remotes/{remote}/");
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["for-each-ref", "--format=%(refname)", &pattern])
        .output()?;
    if !out.status.success() {
        return Err(PushCommandError::ForEachRef(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}
