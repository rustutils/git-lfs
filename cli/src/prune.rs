//! `git lfs prune`: delete local LFS objects that nothing retains.
//!
//! An object is kept if any of these reference it:
//!
//! 1. The HEAD tree of this worktree and of every linked worktree.
//! 2. The tree of each ref whose tip is within `lfs.fetchrecentrefsdays`
//!    plus `lfs.pruneoffsetdays` of now.
//! 3. Earlier versions of files changed on HEAD or one of those refs
//!    within `lfs.fetchrecentcommitsdays` plus `lfs.pruneoffsetdays` of
//!    that ref's tip commit.
//! 4. The index of every worktree that has a commit checked out and
//!    still exists.
//! 5. The stash.
//! 6. Commits on local branches and tags that aren't on any
//!    `refs/remotes/<remote>/` ref, where the remote is
//!    `lfs.pruneremotetocheck`.
//!
//! `--recent` drops 2 and 3. `--force` also drops 1. Sources 1 to 4 skip
//! pointers whose paths are all filtered out by `lfs.fetchinclude` and
//! `lfs.fetchexclude`. Upstream applies only `lfs.fetchexclude` here.
//!
//! `--verify-remote` asks the server, with a download batch, which
//! prunable objects it still has. An object it doesn't have stops the
//! prune, or with `--when-unverified=continue` is kept. Objects that no
//! commit references are only checked with `--verify-unreachable`.

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
    /// The server is missing objects and `--when-unverified=continue`
    /// wasn't given.
    ///
    /// The OIDs are printed before this error is returned.
    #[error(
        "prune halted: objects missing on remote (re-run with --when-unverified=continue to drop them from the delete set)"
    )]
    UnverifiedHalt,
    /// The verify batch request failed.
    #[error("prune verify failed: {0}")]
    Verify(String),
}

#[derive(Debug, Clone)]
pub struct Options {
    pub dry_run: bool,
    pub verbose: bool,
    /// `--recent`: don't keep objects for recent refs and commits.
    pub recent: bool,
    /// `--force`: keep only unpushed, staged, and stashed objects.
    ///
    /// Unlike `--recent`, this also drops the objects in each worktree's
    /// HEAD tree.
    pub force: bool,
    /// `--verify-remote`, which `lfs.pruneverifyremotealways` also turns
    /// on and `--no-verify-remote` overrides.
    pub verify_remote: bool,
    pub no_verify_remote: bool,
    /// `--verify-unreachable`, which `lfs.pruneverifyunreachablealways`
    /// also turns on and `--no-verify-unreachable` overrides.
    pub verify_unreachable: bool,
    pub no_verify_unreachable: bool,
    /// `--when-unverified=continue`: keep objects the server is missing
    /// instead of stopping.
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

    // The `--no-` flags override both the positive flags and config.
    let cfg = FetchPruneConfig::from_repo(cwd);
    let verify_remote =
        !opts.no_verify_remote && (opts.verify_remote || cfg.prune_verify_remote_always);
    let verify_unreachable = !opts.no_verify_unreachable
        && (opts.verify_unreachable || cfg.prune_verify_unreachable_always);

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
        println!("These objects to be pruned are missing on remote:");
        for oid in &missing_on_remote {
            println!(" * {oid}");
        }
        if !opts.continue_when_unverified {
            return Err(PruneError::UnverifiedHalt);
        }
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
            // Another process deleted it first.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => deleted += 1,
            Err(e) => failed.push((*oid, e)),
        }
    }
    for (oid, e) in &failed {
        eprintln!("git-lfs: failed to remove {oid}: {e}");
    }
    println!("Deleting objects: 100% ({deleted}/{total}), done.");

    Ok(())
}

/// What [`verify_prunable`] returns: the objects to delete, the objects
/// the server is missing, and how many the server has.
type VerifyOutcome = (Vec<(Oid, u64)>, Vec<Oid>, usize);

/// Ask the server which of `prunable` it has, with a download batch.
///
/// Objects the server has go in the delete list. Objects it's missing go
/// in the missing list if some commit references them or
/// `verify_unreachable` is set, and in the delete list otherwise. Same
/// rules as upstream's `pruneGetVerifiedPrunableObjects`, but the batch
/// goes to the default remote, where upstream uses
/// `lfs.pruneremotetocheck`.
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

    // With `verify_unreachable`, every missing object counts, so there's
    // no need to know which ones are reachable.
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
            missing.push(*oid);
        } else {
            delete_list.push((*oid, *size));
        }
    }
    Ok((delete_list, missing, verify_count))
}

/// Every LFS object referenced from any ref, like upstream's
/// `gitscanner.ScanAll`.
fn scan_reachable_pointers(cwd: &Path) -> Result<HashSet<Oid>, PruneError> {
    let entries = scan_pointers_with_args(cwd, &[], &[], &["--all"])?;
    Ok(entries.into_iter().map(|e| e.oid).collect())
}

/// Collect the objects to keep, from the sources listed in the module
/// docs.
fn build_retain_set(cwd: &Path, opts: &Options) -> Result<HashSet<Oid>, PruneError> {
    let cfg = FetchPruneConfig::from_repo(cwd);
    let include_set = fetch_filter_set(cwd, "lfs.fetchinclude")?;
    let exclude_set = fetch_filter_set(cwd, "lfs.fetchexclude")?;

    // Pointers without a path always pass the filter.
    let mut retained: HashSet<Oid> = HashSet::new();
    let keep = |entry: git_lfs_git::scanner::PointerEntry, retained: &mut HashSet<Oid>| {
        if paths_pass_filter(&entry.paths, &include_set, &exclude_set) {
            retained.insert(entry.oid);
        }
    };

    // 1. The HEAD tree of each worktree, scanning each commit once. An
    // unborn branch in any worktree is listed with an all-zero HEAD,
    // which makes `scan_tree` fail.
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

    // 2 and 3: recent refs, and earlier versions on HEAD and on them.
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
        // Unlike fetch, keep recent refs of every remote.
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

    // 4. The index of each worktree, even with `--force`. Skip worktrees
    // marked prunable, whose directory is gone. Without a HEAD commit
    // the current worktree's index isn't scanned.
    if head_present {
        for entry in scan_index_pointers(cwd, "HEAD")? {
            keep(entry, &mut retained);
        }
    }
    for wt in &wts {
        if wt.prunable {
            continue;
        }
        if wt.dir == cwd {
            continue;
        }
        let Some(head) = wt.head.as_deref() else {
            continue;
        };
        // A worktree on an unborn branch has no commit to diff against.
        for entry in scan_index_pointers(&wt.dir, head).unwrap_or_default() {
            keep(entry, &mut retained);
        }
    }

    // 5. The stash, without the path filter, like upstream's
    // `scanStashed`.
    for entry in scan_stashed(cwd)? {
        retained.insert(entry.oid);
    }

    // 6. Unpushed commits, even with `--force` and without the path
    // filter, like upstream's `scanUnpushed`. Skipped without a HEAD
    // commit.
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

/// Every local branch and tag, as full ref names.
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

/// HEAD's commit SHA, or `None` if HEAD doesn't resolve.
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

/// The committer time of `reference`'s tip as a Unix timestamp, or `None`
/// if it doesn't resolve.
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

/// Format a byte count in binary units, like `humanize` in `ls_files.rs`.
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
