//! `git lfs prune` — delete local LFS objects that aren't reachable
//! from any retention source. Reclaims disk for LFS-heavy repos whose
//! history has moved on past the objects.
//!
//! Retention sources, in priority order (matches upstream's
//! `pruneTaskGetRetainedCurrentAndRecentRefs`):
//!
//! 1. HEAD's tree.
//! 2. Recent refs whose tip lies within
//!    `lfs.fetchrecentrefsdays + lfs.pruneoffsetdays` of now (mirrors
//!    upstream — recent-tracked branches keep their tip-state pointers
//!    around in case the user checks them out).
//! 3. Per-anchor (HEAD + recent refs) pre-images modified within
//!    `lfs.fetchrecentcommitsdays + lfs.pruneoffsetdays` of *that
//!    anchor's tip date* (so a ref whose tip is itself old still keeps
//!    a recent-history slice).
//! 4. Unpushed history (`HEAD ^refs/remotes/<remote>/*`) — never delete
//!    LFS objects only the local working copy knows about.
//!
//! `--force` skips (2-4) — only HEAD's tree is retained. `--recent`
//! skips (2) and (3) but keeps unpushed retention. `lfs.fetchexclude` /
//! `lfs.fetchinclude` filter every retain producer's pointer paths
//! (matches upstream's `gitscanner.Filter` argument).
//!
//! Out of scope (NOTES.md / future slices): worktree + index + stash
//! walks (Slice 6); `--verify-remote` (later); `--when-unverified`.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use git_lfs_git::{FetchPruneConfig, scan_pointers, scan_previous_versions, scan_tree};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;

use crate::fetch::{fetch_filter_set, paths_pass_filter};
use crate::push::remote_tracking_refs;

#[derive(Debug, thiserror::Error)]
pub enum PruneError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Push(#[from] crate::push::PushCommandError),
    #[error(transparent)]
    Fetch(#[from] crate::fetch::FetchCommandError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct Options {
    pub dry_run: bool,
    pub verbose: bool,
    /// Skip the recent-refs / recent-commits retention windows
    /// (`--recent` flag). Unpushed retention still applies.
    pub recent: bool,
    /// Treat every pushed object as prunable regardless of recent /
    /// unpushed retention (`--force` flag). HEAD-tree retention still
    /// applies.
    pub force: bool,
}

pub fn run(cwd: &Path, opts: &Options) -> Result<(), PruneError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let local_objects = store.each_object()?;
    if local_objects.is_empty() {
        println!("No local LFS objects to prune.");
        return Ok(());
    }

    let retained = build_retain_set(cwd, opts)?;

    // Partition: what stays, what goes.
    let mut prunable: Vec<(Oid, u64)> = Vec::new();
    let mut total_size: u64 = 0;
    for (oid, size) in &local_objects {
        if !retained.contains(oid) {
            prunable.push((*oid, *size));
            total_size += size;
        }
    }

    let local_count = local_objects.len();
    let retained_count = local_count - prunable.len();
    let summary = format!("{local_count} local objects, {retained_count} retained, done.");

    if prunable.is_empty() {
        println!("{summary}");
        return Ok(());
    }

    if opts.dry_run {
        println!("{summary}");
        println!(
            "{} files would be pruned ({})",
            prunable.len(),
            humanize(total_size),
        );
        if opts.verbose {
            for (oid, size) in &prunable {
                println!(" * {oid} ({})", humanize(*size));
            }
        }
        return Ok(());
    }

    if opts.verbose {
        for (oid, size) in &prunable {
            println!(" * {oid} ({})", humanize(*size));
        }
    }

    println!("{summary}");

    let total = prunable.len();
    let mut deleted = 0usize;
    let mut failed: Vec<(Oid, std::io::Error)> = Vec::new();
    for (oid, _) in &prunable {
        let path = store.object_path(*oid);
        match std::fs::remove_file(&path) {
            Ok(()) => deleted += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Raced with a concurrent prune or fsck; treat as
                // already-done.
                deleted += 1;
            }
            Err(e) => failed.push((*oid, e)),
        }
    }
    for (oid, e) in &failed {
        eprintln!("git-lfs: failed to remove {oid}: {e}");
    }
    println!("Deleting objects: 100% ({deleted}/{total}), done.");

    Ok(())
}

/// Build the union of retention sources. Pointers whose paths are all
/// excluded by `lfs.fetchexclude` / `lfs.fetchinclude` are dropped from
/// each source — matches upstream's `gitscanner.Filter`.
fn build_retain_set(cwd: &Path, opts: &Options) -> Result<HashSet<Oid>, PruneError> {
    let cfg = FetchPruneConfig::from_repo(cwd);
    let include_set = fetch_filter_set(cwd, "lfs.fetchinclude")?;
    let exclude_set = fetch_filter_set(cwd, "lfs.fetchexclude")?;

    // Closure: insert `entry` into `retained` if its path-set passes
    // the user's exclude/include filter. Pointers without any path
    // (orphan blobs) always pass.
    let mut retained: HashSet<Oid> = HashSet::new();
    let keep = |entry: git_lfs_git::PointerEntry, retained: &mut HashSet<Oid>| {
        if paths_pass_filter(&entry.paths, &include_set, &exclude_set) {
            retained.insert(entry.oid);
        }
    };

    // (a) HEAD's tree. Skipped if HEAD doesn't resolve
    // (pre-first-commit) or under `--force` (the explicit "purge
    // everything pushed regardless" mode — HEAD content that's also
    // pushed is a legitimate target).
    let head_present = head_exists(cwd);
    if head_present && !opts.force {
        for entry in scan_tree(cwd, "HEAD")? {
            keep(entry, &mut retained);
        }
    }

    // (b) Recent refs + (c) per-anchor pre-images. Skipped under
    // `--force` (purge everything reachable only via recent windows)
    // or `--recent` (only HEAD-tree + unpushed retention).
    let do_recent = !opts.force && !opts.recent;
    let mut anchors: Vec<String> = if head_present {
        vec!["HEAD".to_owned()]
    } else {
        Vec::new()
    };

    if do_recent && cfg.fetch_recent_refs_days > 0 {
        let day = Duration::from_secs(86_400);
        let prune_ref_days = cfg.fetch_recent_refs_days + cfg.prune_offset_days;
        let since = SystemTime::now() - day * prune_ref_days as u32;
        // No `only_remote` filter: prune retains across ALL recent
        // remote refs the user has tracked, not just one fetch source.
        let recent =
            git_lfs_git::recent_branches(cwd, since, cfg.fetch_recent_refs_include_remotes, None)?;
        for r in recent {
            if !anchors.contains(&r.full) {
                anchors.push(r.full.clone());
            }
            for entry in scan_tree(cwd, &r.full)? {
                keep(entry, &mut retained);
            }
        }
    }

    if do_recent && cfg.fetch_recent_commits_days > 0 {
        let day = Duration::from_secs(86_400);
        let prune_commit_days = cfg.fetch_recent_commits_days + cfg.prune_offset_days;
        for r in &anchors {
            let Some(tip_unix) = ref_tip_unix(cwd, r) else {
                continue;
            };
            let commits_since = SystemTime::UNIX_EPOCH + Duration::from_secs(tip_unix as u64)
                - day * prune_commit_days as u32;
            for entry in scan_previous_versions(cwd, r, commits_since)? {
                keep(entry, &mut retained);
            }
        }
    }

    // (d) Unpushed history across every local branch + tag. Mirrors
    // upstream's `git log --branches --tags --not --remotes=<remote>`
    // (`lfs/gitscanner_log.go::scanUnpushed`). Always runs, even under
    // `--force` — branches that exist only locally and tags pointing at
    // deleted branches rely on this to not be deleted. The path filter
    // is *not* applied here: upstream's `scanUnpushed` passes a nil
    // filter to `parseScannerLogOutput`, so a pointer under an excluded
    // path that's only reachable via an unpushed branch still gets
    // retained (test 3 `prune keep unpushed` is the smoke test).
    if head_present {
        let excludes = remote_tracking_refs(cwd, &cfg.prune_remote_name).unwrap_or_default();
        let includes = local_branches_and_tags(cwd).unwrap_or_else(|_| vec!["HEAD".to_owned()]);
        if !includes.is_empty() {
            let include_refs: Vec<&str> = includes.iter().map(String::as_str).collect();
            let exclude_refs: Vec<&str> = excludes.iter().map(String::as_str).collect();
            for entry in scan_pointers(cwd, &include_refs, &exclude_refs)? {
                retained.insert(entry.oid);
            }
        }
    }

    Ok(retained)
}

/// Every local branch and tag (`refs/heads/*` + `refs/tags/*`).
/// Mirrors upstream's `git log --branches --tags` reachability seed.
fn local_branches_and_tags(cwd: &Path) -> std::io::Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/",
            "refs/tags/",
        ])
        .output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
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

/// Tip commit's Unix timestamp for `reference`, or `None` if the ref
/// doesn't resolve. Used as the anchor for the per-ref `commits_days +
/// prune_offset_days` window — matches upstream's
/// `summ.CommitDate.AddDate(0, 0, -pruneCommitDays)`.
fn ref_tip_unix(cwd: &Path, reference: &str) -> Option<i64> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["log", "-1", "--format=%ct", reference])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
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
