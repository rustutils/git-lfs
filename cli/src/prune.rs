//! `git lfs prune` — delete local LFS objects that aren't reachable from
//! HEAD (or any unpushed commit). Reclaims disk for LFS-heavy repos
//! whose history has moved on past the objects.
//!
//! Algorithm:
//! 1. Walk `<lfs>/objects/` to enumerate every object the store has.
//! 2. Build a "retained" OID set from the union of:
//!    - pointers in HEAD's tree (everything currently checked-in)
//!    - pointers in unpushed history (`HEAD ^refs/remotes/<remote>/*`)
//! 3. Delete every store object whose OID isn't retained.
//!
//! Configurable via `lfs.pruneremotename` (default `origin`); falls back
//! to "no remote" if the remote doesn't exist, in which case the
//! unpushed-retain step is skipped (everything in HEAD's tree is still
//! safe).
//!
//! Out of scope (NOTES.md): `--verify-remote`, `--recent` / `--force`,
//! recent-refs / recent-commits retention windows, worktree + stash +
//! index walks. The current scope handles the most-common need
//! ("delete what nobody references anymore on this branch") without the
//! corner cases.

use std::collections::HashSet;
use std::path::Path;

use git_lfs_git::{scan_pointers, scan_tree};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;

use crate::push::remote_tracking_refs;

#[derive(Debug, thiserror::Error)]
pub enum PruneError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Push(#[from] crate::push::PushCommandError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct Options {
    pub dry_run: bool,
    pub verbose: bool,
}

pub fn run(cwd: &Path, opts: &Options) -> Result<(), PruneError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let local_objects = store.each_object()?;
    if local_objects.is_empty() {
        println!("No local LFS objects to prune.");
        return Ok(());
    }

    let retained = build_retain_set(cwd)?;

    // Partition: what stays, what goes.
    let mut prunable: Vec<(Oid, u64)> = Vec::new();
    let mut total_size: u64 = 0;
    for (oid, size) in &local_objects {
        if !retained.contains(oid) {
            prunable.push((*oid, *size));
            total_size += size;
        }
    }

    if prunable.is_empty() {
        println!(
            "Nothing to prune. {} local object(s) all retained.",
            local_objects.len(),
        );
        return Ok(());
    }

    if opts.dry_run {
        println!(
            "Would prune {} object(s) ({}).",
            prunable.len(),
            humanize(total_size),
        );
    } else {
        println!(
            "Pruning {} object(s) ({}).",
            prunable.len(),
            humanize(total_size),
        );
    }

    if opts.verbose {
        for (oid, size) in &prunable {
            println!(" * {oid} ({})", humanize(*size));
        }
    }

    if !opts.dry_run {
        let mut deleted = 0usize;
        let mut failed: Vec<(Oid, std::io::Error)> = Vec::new();
        for (oid, _) in &prunable {
            let path = store.object_path(*oid);
            match std::fs::remove_file(&path) {
                Ok(()) => deleted += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Raced with a concurrent prune or fsck; treat as
                    // already-done.
                }
                Err(e) => failed.push((*oid, e)),
            }
        }
        for (oid, e) in &failed {
            eprintln!("git-lfs: failed to remove {oid}: {e}");
        }
        println!(
            "Pruned {deleted} object(s){}.",
            if failed.is_empty() {
                String::new()
            } else {
                format!(" ({} failed)", failed.len())
            },
        );
    }

    Ok(())
}

/// Build the set of OIDs we must NOT delete: HEAD-tree + unpushed.
fn build_retain_set(cwd: &Path) -> Result<HashSet<Oid>, PruneError> {
    let mut retained: HashSet<Oid> = HashSet::new();

    // (a) HEAD's tree — current working tree state. If HEAD doesn't
    // exist (e.g. pre-first-commit repo), there's nothing to scan.
    if head_exists(cwd) {
        for entry in scan_tree(cwd, "HEAD")? {
            retained.insert(entry.oid);
        }
    }

    // (b) Unpushed: HEAD ^ refs/remotes/<remote>/*. Defaults to origin;
    // configurable via `lfs.pruneremotename`. If the remote isn't
    // configured we just skip this step — anything in HEAD's tree is
    // still retained, which is the safe fallback.
    let remote_name = git_lfs_git::config::get_effective(cwd, "lfs.pruneremotename")?
        .unwrap_or_else(|| "origin".into());
    let excludes = remote_tracking_refs(cwd, &remote_name).unwrap_or_default();
    if head_exists(cwd) {
        let exclude_refs: Vec<&str> = excludes.iter().map(String::as_str).collect();
        for entry in scan_pointers(cwd, &["HEAD"], &exclude_refs)? {
            retained.insert(entry.oid);
        }
    }

    Ok(retained)
}

fn head_exists(cwd: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Mirrors the humanizer in `ls_files.rs`. Tiny enough that a duplicate
/// is fine; if a third caller appears, hoist into a shared util module.
fn humanize(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut i = 0;
    while value >= 1024.0 && i + 1 < UNITS.len() {
        value /= 1024.0;
        i += 1;
    }
    format!("{value:.2} {}", UNITS[i])
}
