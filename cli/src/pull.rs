//! `git lfs pull [<ref>...]` — `fetch` + materialize LFS files in the
//! working tree.
//!
//! After `fetch` populates the store, the working tree may still hold
//! pointer text (this is the "fresh clone, smudge skipped" state). We
//! enumerate every tracked file, and for any whose contents parse as an
//! LFS pointer that we have locally, rewrite the file with the bytes
//! from the store.
//!
//! Doing the rewrite ourselves (rather than `git checkout HEAD -- .`)
//! is deliberate: `git checkout` skips files it considers "unchanged"
//! relative to the index — and a pointer text that's also what's in
//! the index counts as unchanged. We'd never re-trigger the smudge
//! filter that way.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git_lfs_pointer::Pointer;
use git_lfs_store::Store;

use crate::fetch::{self, FetchCommandError};

#[derive(Debug, thiserror::Error)]
pub enum PullCommandError {
    #[error(transparent)]
    Fetch(#[from] FetchCommandError),
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("git ls-files failed: {0}")]
    LsFiles(String),
    #[error("partial pull: {0} object(s) failed to fetch — working tree not updated")]
    FetchFailures(usize),
}

pub fn pull(cwd: &Path, refs: &[String]) -> Result<(), PullCommandError> {
    pull_with_filter(cwd, refs, &[], &[])
}

pub fn pull_with_filter(
    cwd: &Path,
    refs: &[String],
    include: &[String],
    exclude: &[String],
) -> Result<(), PullCommandError> {
    let opts = fetch::FetchOptions {
        args: refs,
        stdin_lines: &[],
        dry_run: false,
        json: false,
        all: false,
        refetch: false,
        stdin: false,
        prune: false,
        include,
        exclude,
    };
    let outcome = fetch::fetch(cwd, &opts)?;
    if !outcome.report.failed.is_empty() {
        return Err(PullCommandError::FetchFailures(outcome.report.failed.len()));
    }

    // Match upstream `newSingleCheckout`: if the smudge filter isn't
    // installed (no `filter.lfs.clean` config), skip the working-
    // tree materialize step and tell the user how to fix it. The
    // fetch above still ran, so objects land in `.git/lfs/objects/`
    // and `git lfs install` later will smudge them in.
    if !smudge_filter_installed(cwd) {
        println!(
            "Skipping object checkout, Git LFS is not installed for this repository.\n\
             Consider installing it with 'git lfs install'."
        );
        return Ok(());
    }

    // Build the same include/exclude filter `fetch` used so the
    // working-tree rewrite respects -I / -X (or `lfs.fetchinclude` /
    // `lfs.fetchexclude`). Without this an LFS object that fetch
    // skipped would still be rewritten in-place if it happened to be
    // present locally already.
    let include_set = fetch::build_pattern_set(cwd, include, "lfs.fetchinclude")?;
    let exclude_set = fetch::build_pattern_set(cwd, exclude, "lfs.fetchexclude")?;

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let tracked = list_tracked_files(cwd)?;
    let mut rewritten_paths: Vec<String> = Vec::new();
    for path in tracked {
        if !fetch::path_passes_filter(Some(&path), &include_set, &exclude_set) {
            continue;
        }
        let full = cwd.join(&path);
        let Ok(meta) = std::fs::metadata(&full) else { continue };
        // Skip anything that can't possibly be a pointer (too big to
        // bother reading) or anything that isn't a regular file.
        if !meta.is_file() || meta.len() >= git_lfs_pointer::MAX_POINTER_SIZE as u64 {
            continue;
        }
        let content = std::fs::read(&full)?;
        let Ok(pointer) = Pointer::parse(&content) else { continue };
        if !store.contains_with_size(pointer.oid, pointer.size) {
            // We don't have it (fetch didn't bring it; maybe a hash
            // mismatch landed in `failed`, or the user pulled a ref we
            // didn't fetch). Leave the pointer text in place.
            continue;
        }
        let mut src = store.open(pointer.oid)?;
        let mut dst = std::fs::File::create(&full)?;
        std::io::copy(&mut src, &mut dst)?;
        rewritten_paths.push(path.to_string_lossy().into_owned());
    }
    if !rewritten_paths.is_empty() {
        // After overwriting working-tree files, the stat info in the
        // index is stale; `git diff-index HEAD` would report each as
        // modified even though `clean(content)` hashes back to the
        // original blob. `git update-index -q --refresh --stdin`
        // re-stats each path and runs the clean filter to confirm
        // the content blob matches; matching paths get fresh stat
        // info and drop out of subsequent diff-index walks.
        refresh_index(cwd, &rewritten_paths)?;
        println!("Materialized {} working-tree file(s)", rewritten_paths.len());
    }
    Ok(())
}

fn refresh_index(cwd: &Path, paths: &[String]) -> Result<(), PullCommandError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["update-index", "-q", "--refresh", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        for p in paths {
            stdin.write_all(p.as_bytes())?;
            stdin.write_all(b"\n")?;
        }
    }
    // Don't surface failures: `update-index --refresh` exits non-zero
    // when *some* path is still considered dirty (e.g. genuine local
    // edits we didn't rewrite), and treating that as a hard error
    // would break the legitimate "clean partial pull" case.
    let _ = child.wait()?;
    Ok(())
}

fn smudge_filter_installed(cwd: &Path) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "filter.lfs.clean"])
        .output();
    matches!(out, Ok(o) if o.status.success() && !o.stdout.trim_ascii().is_empty())
}

/// `git ls-files -z` enumerates every tracked file in the working tree
/// (NUL-separated to survive paths with newlines).
fn list_tracked_files(cwd: &Path) -> Result<Vec<PathBuf>, PullCommandError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-files", "-z"])
        .output()?;
    if !out.status.success() {
        return Err(PullCommandError::LsFiles(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|bytes| PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
        .collect())
}
