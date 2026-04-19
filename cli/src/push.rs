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

/// Run the push command for `remote` + `refs`. Defaults `refs` to `["HEAD"]`.
///
/// Returns the [`Report`] so callers (i.e. `main`) can decide their own
/// exit code based on per-object success/failure.
pub fn push(cwd: &Path, remote: &str, refs: &[String]) -> Result<Report, PushCommandError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);

    let default_refs = ["HEAD".to_string()];
    let effective: &[String] = if refs.is_empty() { &default_refs } else { refs };
    let ref_strs: Vec<&str> = effective.iter().map(String::as_str).collect();

    // Enumerate refs/remotes/<remote>/* — these are the OIDs we assume
    // the server already has. Empty list (e.g. brand-new remote with no
    // tracking refs yet) means we upload everything reachable.
    let excludes_owned = remote_tracking_refs(cwd, remote)?;
    let excludes: Vec<&str> = excludes_owned.iter().map(String::as_str).collect();

    let pointers = scan_pointers(cwd, &ref_strs, &excludes)?;

    // Filter to OIDs we actually have locally. Pushing without local
    // bytes is impossible — warn so the user knows something's missing
    // (probably from history they never fetched themselves).
    let mut to_push: Vec<ObjectSpec> = Vec::with_capacity(pointers.len());
    let mut missing: Vec<PathBuf> = Vec::new();
    for entry in pointers {
        if store.contains_with_size(entry.oid, entry.size) {
            to_push.push(ObjectSpec {
                oid: entry.oid.to_string(),
                size: entry.size,
            });
        } else if let Some(p) = entry.path {
            missing.push(p);
        }
    }

    if !missing.is_empty() {
        eprintln!(
            "warning: {} pointer(s) reference objects not present locally; skipping:",
            missing.len(),
        );
        for p in &missing {
            eprintln!("  {}", p.display());
        }
    }

    if to_push.is_empty() {
        println!("Nothing to push — remote already has every reachable LFS object.");
        return Ok(Report::default());
    }

    println!("Pushing {} object(s)", to_push.len());
    let fetcher = LfsFetcher::from_repo(cwd, &store)?;
    let report = fetcher
        .upload_many(to_push)
        .map_err(PushCommandError::Fetch)?;

    for (oid, err) in &report.failed {
        eprintln!("  {oid}: {err}");
    }
    println!(
        "{} succeeded, {} failed",
        report.succeeded.len(),
        report.failed.len(),
    );
    Ok(report)
}

/// Enumerate every ref under `refs/remotes/<remote>/`. Returns the
/// fully-qualified ref names so they can go straight into rev-list's
/// exclude set.
fn remote_tracking_refs(cwd: &Path, remote: &str) -> Result<Vec<String>, PushCommandError> {
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
