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

use std::path::{Path, PathBuf};
use std::process::Command;

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
    let opts = fetch::FetchOptions {
        args: refs,
        stdin_lines: &[],
        dry_run: false,
        json: false,
        all: false,
        refetch: false,
        stdin: false,
        prune: false,
        include: &[],
        exclude: &[],
    };
    let outcome = fetch::fetch(cwd, &opts)?;
    if !outcome.report.failed.is_empty() {
        return Err(PullCommandError::FetchFailures(outcome.report.failed.len()));
    }

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let tracked = list_tracked_files(cwd)?;
    let mut rewritten = 0usize;
    for path in tracked {
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
        rewritten += 1;
    }
    if rewritten > 0 {
        println!("Materialized {rewritten} working-tree file(s)");
    }
    Ok(())
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
