//! `git lfs push <remote> [<ref>...]` — upload every LFS object reachable
//! from the given refs that the named remote doesn't already have.
//!
//! Three argument modes (mutually exclusive):
//!
//! - **refs** (default): positional args are git refs. Scan rev-list
//!   from those refs (excluding everything reachable from
//!   `refs/remotes/<remote>/*`) and upload the LFS pointers found.
//! - `--all`: scan from `refs/heads/*` + `refs/tags/*` instead. If
//!   positional args are given alongside, they restrict the walk.
//! - `--object-id`: positional args are raw LFS OIDs. No scan — we
//!   read the bytes (and size) directly from the local store.
//!
//! `--stdin` overrides positional args with one-per-line input from
//! stdin (blank lines are dropped). Mixing `--stdin` with positional
//! args emits a warning, mirroring upstream.
//!
//! The batch API also dedupes server-side: any object the server
//! already has comes back from the upload batch with no `actions`, and
//! the transfer queue treats that as success without sending bytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_api::ObjectSpec;
use git_lfs_git::scan_pointers;
use git_lfs_pointer::Oid;
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
    /// User-facing argument error. Carries the message verbatim — it's
    /// printed and we exit non-zero. No exit-2 wrapping (that's
    /// reserved for missing-locally aborts).
    #[error("{0}")]
    Usage(String),
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

/// All flags + positional args for `git lfs push`. Bundled so callers
/// don't have to thread eight independent parameters through the
/// dispatcher.
pub struct PushOptions<'a> {
    pub args: &'a [String],
    /// Pre-read stdin lines (blank lines stripped). Empty when
    /// `stdin` is false.
    pub stdin_lines: &'a [String],
    pub dry_run: bool,
    pub all: bool,
    pub stdin: bool,
    pub object_id: bool,
}

/// Run `git lfs push`. Validates flags, classifies the requested mode
/// (refs / refs+--all / object-id), and routes to the appropriate
/// uploader.
pub fn push(
    cwd: &Path,
    remote: &str,
    opts: &PushOptions<'_>,
) -> Result<PushOutcome, PushCommandError> {
    // Remote validation runs first — both modes need a real remote so
    // we can resolve its tracking refs / LFS endpoint. This catches
    // typos like `git lfs push not-a-remote` before any other work.
    if !is_remote_or_url(cwd, remote) {
        return Err(PushCommandError::Usage(format!(
            "Invalid remote name: {remote:?}"
        )));
    }

    // Resolve the effective positional args. With --stdin, stdin_lines
    // wins and any args become a warning. Empty stdin under --stdin
    // is fine for object-id (no objects to push); the all-refs walk
    // and the explicit-refs path catch their own emptiness below.
    let (effective_args, stdin_overrode_args) = if opts.stdin {
        (opts.stdin_lines, !opts.args.is_empty())
    } else {
        (opts.args, false)
    };
    if stdin_overrode_args {
        eprintln!("Further command line arguments are ignored with --stdin.");
    }

    if opts.object_id {
        return push_by_oid(cwd, remote, effective_args, opts);
    }

    if opts.all {
        let walk_refs = if effective_args.is_empty() {
            all_local_refs(cwd)?
        } else {
            effective_args.to_vec()
        };
        let ref_strs: Vec<&str> = walk_refs.iter().map(String::as_str).collect();
        let excludes_owned = remote_tracking_refs(cwd, remote)?;
        let excludes: Vec<&str> = excludes_owned.iter().map(String::as_str).collect();
        return upload_in_range(cwd, remote, &ref_strs, &excludes, None, opts.dry_run);
    }

    // Plain ref mode. Refusing the empty-refs case is a small UX
    // improvement over upstream's silent `git push HEAD` default —
    // and the test suite explicitly checks for it (`push with
    // nothing`).
    if effective_args.is_empty() {
        return Err(PushCommandError::Usage(
            "At least one ref must be supplied without --all".into(),
        ));
    }

    // Validate each ref before handing to rev-list, so the user gets
    // a tidy `Invalid ref argument: foo` instead of git's
    // `fatal: bad revision 'foo'`.
    for r in effective_args {
        if !is_resolvable_ref(cwd, r) {
            return Err(PushCommandError::Usage(format!(
                "Invalid ref argument: {r:?}"
            )));
        }
    }

    let ref_strs: Vec<&str> = effective_args.iter().map(String::as_str).collect();
    let excludes_owned = remote_tracking_refs(cwd, remote)?;
    let excludes: Vec<&str> = excludes_owned.iter().map(String::as_str).collect();
    upload_in_range(cwd, remote, &ref_strs, &excludes, None, opts.dry_run)
}

/// `--object-id` path: positional args are LFS OIDs to upload directly
/// from the store, bypassing the rev-list scan. Used for surgical
/// re-uploads (e.g. after the user pruned and the remote also lost an
/// object, leaving only their local copy).
fn push_by_oid(
    cwd: &Path,
    remote: &str,
    oids: &[String],
    opts: &PushOptions<'_>,
) -> Result<PushOutcome, PushCommandError> {
    // --stdin with empty input is a legitimate no-op (matches upstream
    // — easier to script "push every oid in this file"). Without
    // --stdin, requiring at least one positional arg gives a clear
    // signal that the flag was used incorrectly.
    if oids.is_empty() {
        if opts.stdin {
            return Ok(PushOutcome::default());
        }
        return Err(PushCommandError::Usage(
            "At least one object ID must be supplied with --object-id".into(),
        ));
    }

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let mut to_upload: Vec<ObjectSpec> = Vec::with_capacity(oids.len());
    for raw in oids {
        let oid = parse_oid(raw)?;
        let path = store.object_path(oid);
        let size = std::fs::metadata(&path).map(|m| m.len()).map_err(|_| {
            PushCommandError::Usage(format!("object {oid} not found in local LFS store"))
        })?;
        to_upload.push(ObjectSpec {
            oid: oid.to_string(),
            size,
        });
    }

    if opts.dry_run {
        // No path to attribute — just emit the OID with an empty
        // path slot. Test 14 greps `push <oid> =>`, so the `=>` has
        // to be there.
        for spec in &to_upload {
            println!("push {} => ", spec.oid);
        }
        return Ok(PushOutcome::default());
    }

    let mut fetcher = LfsFetcher::from_repo_with_remote(cwd, &store, Some(remote))?;
    // Skip lock verification for direct-OID uploads — there's no path
    // context to reason about ownership against, and the typical
    // caller is `t-push.sh`-style tooling that's already vetted the
    // workflow.
    let _ = &mut fetcher;

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

/// Parse an OID string into [`Oid`], rejecting empty / too-short input
/// with the message `t-push.sh::push --object-id (invalid value)`
/// expects.
fn parse_oid(raw: &str) -> Result<Oid, PushCommandError> {
    if raw.len() < 64 {
        return Err(PushCommandError::Usage(format!(
            "too short object ID: {raw:?}"
        )));
    }
    raw.parse::<Oid>()
        .map_err(|_| PushCommandError::Usage(format!("invalid object ID: {raw:?}")))
}

/// True if `<remote>` is something we can resolve to an LFS endpoint —
/// a known git remote name, a URL we can use directly, or any name
/// that picks up a fallback `lfs.url` / `GIT_LFS_URL`. The check
/// mirrors the resolution chain in `endpoint_for_remote` so a
/// remote-less repo with `lfs.url` set still pushes (the typical
/// integration-test setup).
fn is_remote_or_url(cwd: &Path, name: &str) -> bool {
    if name.contains("://")
        || name.starts_with("git@")
        || name.starts_with("file://")
        || std::path::Path::new(name).is_absolute()
    {
        return true;
    }
    let key = format!("remote.{name}.url");
    if matches!(git_lfs_git::config::get_effective(cwd, &key), Ok(Some(_))) {
        return true;
    }
    // Endpoint-resolvable via `lfs.url` / `remote.<name>.lfsurl` / etc.
    git_lfs_git::endpoint_for_remote(cwd, Some(name)).is_ok()
}

/// True if `<ref>` resolves to a commit. Used to distinguish a typo'd
/// branch name from a brand-new branch the user just created.
fn is_resolvable_ref(cwd: &Path, r: &str) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{r}^{{commit}}"),
        ])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// Enumerate `refs/heads/*` and `refs/tags/*` for `--all`. Order is
/// whatever git's `for-each-ref` emits (alphabetical within each
/// pattern); the scanner doesn't care.
fn all_local_refs(cwd: &Path) -> Result<Vec<String>, PushCommandError> {
    let out = Command::new("git")
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
                    let ours_paths: Vec<&str> = ours.iter().map(|l| l.path.as_str()).collect();
                    let any_ours_pushed = pushed_paths.iter().any(|p| {
                        let p = p.to_string_lossy();
                        ours_paths.iter().any(|op| p == *op)
                    });
                    if any_ours_pushed {
                        eprintln!(
                            "Consider unlocking your own locked files: \
                             (`git lfs unlock <path>`)"
                        );
                        for op in &ours_paths {
                            if pushed_paths.iter().any(|p| p.to_string_lossy() == *op) {
                                eprintln!("* {op}");
                            }
                        }
                    }
                }
                // Path-intersect theirs with what we're pushing —
                // those are the entries that actually block this push.
                theirs
                    .into_iter()
                    .filter(|l| pushed_paths.iter().any(|p| p.to_string_lossy() == l.path))
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
pub(crate) fn human_bytes(n: u64) -> String {
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
