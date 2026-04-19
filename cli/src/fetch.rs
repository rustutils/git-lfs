//! `git lfs fetch [<ref>...]` — download every LFS object reachable from
//! the named refs that isn't already in the local store.
//!
//! For each ref, the scanner walks history (matching upstream's `ScanRefs`
//! semantics — see `git/src/scanner.rs`), collects every LFS pointer,
//! dedupes by LFS OID, drops the ones we already have, and hands the
//! rest to the transfer queue.

use std::path::Path;

use git_lfs_api::ObjectSpec;
use git_lfs_git::scan_pointers;
use git_lfs_store::Store;
use git_lfs_transfer::Report;

use crate::LfsFetcher;

#[derive(Debug, thiserror::Error)]
pub enum FetchCommandError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("fetch failed: {0}")]
    Fetch(git_lfs_filter::FetchError),
}

/// Run the fetch command for `refs` (defaults to `["HEAD"]` if empty).
///
/// Prints a per-OID failure list on stderr and a one-line summary on
/// stdout. Returns the [`Report`] so callers can decide their own exit
/// code beyond just "fetch errored entirely".
pub fn fetch(cwd: &Path, refs: &[String]) -> Result<Report, FetchCommandError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);

    let default_refs = ["HEAD".to_string()];
    let effective: &[String] = if refs.is_empty() { &default_refs } else { refs };
    let ref_strs: Vec<&str> = effective.iter().map(String::as_str).collect();

    let pointers = scan_pointers(cwd, &ref_strs, &[])?;

    // Filter out anything already in the store with the right size. A
    // size mismatch is treated as missing — same recovery path as smudge.
    let to_fetch: Vec<ObjectSpec> = pointers
        .into_iter()
        .filter(|p| !store.contains_with_size(p.oid, p.size))
        .map(|p| ObjectSpec { oid: p.oid.to_string(), size: p.size })
        .collect();

    if to_fetch.is_empty() {
        println!("Nothing to fetch — all referenced LFS objects are already present.");
        return Ok(Report::default());
    }

    println!("Fetching {} object(s)", to_fetch.len());
    let fetcher = LfsFetcher::from_repo(cwd, &store)?;
    let report = fetcher
        .download_many(to_fetch)
        .map_err(FetchCommandError::Fetch)?;

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
