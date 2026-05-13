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
//! `--verify-remote` (alias `-c`) sends prunable OIDs through a
//! download-direction batch and refuses to delete anything the server
//! can't serve back — protects against accidentally pruning the only
//! remaining copy of a not-yet-replicated object. With
//! `--verify-unreachable`, orphan objects (in the local store but not
//! reachable from any commit) get the same check; without it, orphans
//! pass through silently. `--when-unverified=continue` drops
//! unverified objects from the delete set instead of halting.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use git_lfs_git::fetch_prune::FetchPruneConfig;
use git_lfs_git::scanner::{
    scan_index_pointers, scan_pointers, scan_pointers_with_args, scan_previous_versions,
    scan_stashed, scan_tree,
};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;

use crate::fetch::{fetch_filter_set, paths_pass_filter};
use crate::fetcher::LfsFetcher;
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
    /// Verify-remote turned up unverified OIDs that the user didn't
    /// authorize to be silently dropped — refuse the delete.
    /// Wording is generic; the OID list is printed before the error
    /// surfaces so the user knows what's at stake.
    #[error(
        "prune halted: objects missing on remote (re-run with --when-unverified=continue to drop them from the delete set)"
    )]
    UnverifiedHalt,
    /// Wraps fetcher errors that hit the verify pass.
    #[error("prune verify failed: {0}")]
    Verify(String),
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
    /// `--verify-remote`: check each prunable OID against the remote
    /// before deleting. Combined with `lfs.pruneverifyremotealways`
    /// and `--no-verify-remote` to compute the effective decision.
    pub verify_remote: bool,
    pub no_verify_remote: bool,
    /// `--verify-unreachable`: also verify orphan objects (those not
    /// reachable from any commit). Combined with
    /// `lfs.pruneverifyunreachablealways` and `--no-verify-unreachable`.
    pub verify_unreachable: bool,
    pub no_verify_unreachable: bool,
    /// `--when-unverified=continue` (default `halt`). When `true`,
    /// drop unverified objects from the delete set instead of
    /// refusing the prune.
    pub continue_when_unverified: bool,
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
    for (oid, size) in &local_objects {
        if !retained.contains(oid) {
            prunable.push((*oid, *size));
        }
    }

    let local_count = local_objects.len();
    let retained_count = local_count - prunable.len();

    if prunable.is_empty() {
        println!("{local_count} local objects, {retained_count} retained, done.");
        return Ok(());
    }

    // Resolve the effective verify flags. Args win over config; the
    // explicit `--no-...` form is the override knob for users who
    // turned the corresponding `lfs.pruneverify*always` config on
    // globally and want to opt out for this invocation.
    let cfg = FetchPruneConfig::from_repo(cwd);
    let verify_remote =
        !opts.no_verify_remote && (opts.verify_remote || cfg.prune_verify_remote_always);
    let verify_unreachable = !opts.no_verify_unreachable
        && (opts.verify_unreachable || cfg.prune_verify_unreachable_always);

    // Verify-remote pass: classify each prunable OID into "verified"
    // (server can serve it back) or "not verified" (missing). With
    // `verify_unreachable=false`, orphan OIDs (not reachable from any
    // commit) are NOT considered failures even if the server doesn't
    // have them — we don't care about pruning bytes nothing in history
    // references. Without `verify_remote`, every prunable is treated as
    // verified.
    let (mut delete_list, missing_on_remote, verify_count) = if verify_remote {
        verify_prunable(cwd, &prunable, verify_unreachable)?
    } else {
        (prunable.clone(), Vec::new(), 0usize)
    };

    let mut summary = format!("{local_count} local objects, {retained_count} retained");
    if verify_count > 0 {
        summary.push_str(&format!(", {verify_count} verified with remote"));
    }
    if !missing_on_remote.is_empty() {
        summary.push_str(&format!(", {} not on remote", missing_on_remote.len()));
    }
    summary.push_str(", done.");
    println!("{summary}");

    if !missing_on_remote.is_empty() {
        // List the unverified objects so a halt or a continue both
        // give the user the OIDs they need to re-push or delete
        // by hand. Wording matches upstream's
        // `These objects to be pruned are missing on remote:`.
        println!("These objects to be pruned are missing on remote:");
        for oid in &missing_on_remote {
            println!(" * {oid}");
        }
        if !opts.continue_when_unverified {
            // Halt: refuse the delete entirely. No bytes removed.
            return Err(PruneError::UnverifiedHalt);
        }
        // Continue: drop the missing ones from the delete set.
        let missing: HashSet<&Oid> = missing_on_remote.iter().collect();
        delete_list.retain(|(oid, _)| !missing.contains(oid));
    }

    if delete_list.is_empty() {
        return Ok(());
    }

    let delete_total_size: u64 = delete_list.iter().map(|(_, s)| *s).sum();

    if opts.dry_run {
        println!(
            "{} files would be pruned ({})",
            delete_list.len(),
            humanize(delete_total_size),
        );
        if opts.verbose {
            for (oid, size) in &delete_list {
                println!(" * {oid} ({})", humanize(*size));
            }
        }
        return Ok(());
    }

    if opts.verbose {
        for (oid, size) in &delete_list {
            println!(" * {oid} ({})", humanize(*size));
        }
    }

    let total = delete_list.len();
    let mut deleted = 0usize;
    let mut failed: Vec<(Oid, std::io::Error)> = Vec::new();
    for (oid, _) in &delete_list {
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

/// `(delete_list, missing_on_remote, verify_count)` returned by
/// [`verify_prunable`]. Named because the tuple is wide enough that
/// clippy flags the bare `Result<...>` as a complex type.
type VerifyOutcome = (Vec<(Oid, u64)>, Vec<Oid>, usize);

/// Run a download-direction batch over `prunable` and apply the
/// decision matrix from upstream's `pruneGetVerifiedPrunableObjects`.
///
/// - `delete_list` — verified objects, plus orphan-and-unverified
///   objects when `verify_unreachable=false` (nothing to protect).
/// - `missing_on_remote` — reachable-but-unverified OIDs, or every
///   unverified OID when `verify_unreachable=true`. Caller either
///   halts or strips these from the delete list.
/// - `verify_count` — OIDs the server confirmed it can serve back.
fn verify_prunable(
    cwd: &Path,
    prunable: &[(Oid, u64)],
    verify_unreachable: bool,
) -> Result<VerifyOutcome, PruneError> {
    use git_lfs_api::ObjectSpec;

    let fetcher = LfsFetcher::from_repo(cwd, &Store::new(git_lfs_git::lfs_dir(cwd)?))?;
    let specs: Vec<ObjectSpec> = prunable
        .iter()
        .map(|(oid, size)| ObjectSpec {
            oid: oid.to_string(),
            size: *size,
        })
        .collect();
    let verified: HashSet<Oid> = fetcher
        .check_server_can_download(specs)
        .map_err(|e| PruneError::Verify(e.to_string()))?
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let verify_count = verified.len();

    // Reachable scan only matters when `verify_unreachable=false`:
    // an unverified-but-orphan OID is silently passed through (no
    // value), so we need to know who's an orphan. With
    // `verify_unreachable=true`, every unverified OID counts as
    // missing regardless of reachability — the scan is wasted work.
    let reachable: HashSet<Oid> = if verify_unreachable {
        HashSet::new()
    } else {
        scan_reachable_pointers(cwd)?
    };

    let mut delete_list: Vec<(Oid, u64)> = Vec::new();
    let mut missing: Vec<Oid> = Vec::new();
    for (oid, size) in prunable {
        if verified.contains(oid) {
            delete_list.push((*oid, *size));
        } else if verify_unreachable || reachable.contains(oid) {
            // Reachable + missing OR orphan-with-verify-unreachable:
            // we care about this one. Caller halts or strips.
            missing.push(*oid);
        } else {
            // Orphan + missing on remote and we weren't asked to
            // care about orphans: just prune.
            delete_list.push((*oid, *size));
        }
    }
    Ok((delete_list, missing, verify_count))
}

/// Every LFS pointer reachable from any ref. Mirrors upstream's
/// `gitscanner.ScanAll` — used by `--verify-remote` (without
/// `--verify-unreachable`) to distinguish "missing-on-remote object
/// I care about" from "orphan in the local store".
fn scan_reachable_pointers(cwd: &Path) -> Result<HashSet<Oid>, PruneError> {
    let entries = scan_pointers_with_args(cwd, &[], &[], &["--all"])?;
    Ok(entries.into_iter().map(|e| e.oid).collect())
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
    let keep = |entry: git_lfs_git::scanner::PointerEntry, retained: &mut HashSet<Oid>| {
        if paths_pass_filter(&entry.paths, &include_set, &exclude_set) {
            retained.insert(entry.oid);
        }
    };

    // (a) HEAD's tree across every worktree (this one + linked).
    // Each worktree may be on a different ref; retain the tip-state
    // of all of them so a checkout in another worktree isn't broken
    // by a prune in this one. Skipped under `--force` (the explicit
    // "purge everything pushed regardless" mode). Dedup by SHA so
    // a worktree pointing at the same commit as another one doesn't
    // re-walk the tree.
    let head_present = head_exists(cwd);
    let wts = git_lfs_git::refs::worktrees(cwd);
    let head_sha = head_present.then(|| current_head_sha(cwd)).flatten();
    if !opts.force {
        let mut seen_shas: HashSet<String> = HashSet::new();
        if head_present {
            for entry in scan_tree(cwd, "HEAD")? {
                keep(entry, &mut retained);
            }
            if let Some(sha) = &head_sha {
                seen_shas.insert(sha.clone());
            }
        }
        for wt in &wts {
            let Some(head) = wt.head.as_deref() else {
                continue;
            };
            if !seen_shas.insert(head.to_owned()) {
                continue;
            }
            for entry in scan_tree(cwd, head)? {
                keep(entry, &mut retained);
            }
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
        let recent = git_lfs_git::refs::recent_branches(
            cwd,
            since,
            cfg.fetch_recent_refs_include_remotes,
            None,
        )?;
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

    // (d') Index of every (non-prunable) worktree. Pointers staged
    // but not yet committed — including in linked worktrees — won't
    // appear in any tree-walk; the index is the only producer that
    // surfaces them. Mirrors upstream's `pruneTaskGetRetainedIndex`
    // per worktree. A worktree marked `prunable` (its directory was
    // removed but `git worktree prune` hasn't run) gets its index
    // walk skipped — the index file may already be inaccessible.
    // Always runs (even under `--force`): tests 13 + 14 rely on this.
    if head_present {
        for entry in scan_index_pointers(cwd, "HEAD")? {
            keep(entry, &mut retained);
        }
    }
    for wt in &wts {
        if wt.prunable {
            continue;
        }
        // Skip the current worktree — already scanned above.
        if wt.dir == cwd {
            continue;
        }
        let Some(head) = wt.head.as_deref() else {
            continue;
        };
        // best-effort — a worktree on an unborn branch has no HEAD
        // ref to diff against.
        for entry in scan_index_pointers(&wt.dir, head).unwrap_or_default() {
            keep(entry, &mut retained);
        }
    }

    // (d'') Stash. Stashed working-copy / index / untracked LFS
    // pointers must survive prune so a future `git stash pop` can
    // restore them. Mirrors upstream's `pruneTaskGetRetainedStashed`
    // → `gitscanner_log.go::scanStashed`. Filter is *not* applied —
    // upstream passes a nil filter through `parseScannerLogOutput`,
    // matching the unpushed walk.
    for entry in scan_stashed(cwd)? {
        retained.insert(entry.oid);
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

/// HEAD's commit SHA. Returns `None` when HEAD doesn't resolve. Used
/// to dedup the current worktree against the linked-worktree list.
fn current_head_sha(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
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
