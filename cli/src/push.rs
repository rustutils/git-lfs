//! `git lfs push`: upload LFS objects to a remote.
//!
//! The arguments after the remote are refs, or OIDs with `--object-id`.
//! For refs, the objects uploaded are those in commits reachable from
//! the refs but not from any `refs/remotes/<remote>/` ref. `--all`
//! without refs uses all local branches and tags. With `--stdin`, the
//! arguments are read from stdin instead.
//!
//! [`upload_in_range`] does the work for both this command and
//! `pre-push`. Objects the server already has come back from the batch
//! without actions and aren't sent again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_api::ObjectSpec;
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
    /// An error whose message is shown as is.
    #[error("{0}")]
    Usage(String),
}

/// The result of a push.
///
/// `aborted` is set when the push failed after its own error output:
/// lock verification stopped it, an object is missing locally and on the
/// server, an object is corrupt, or an upload failed. Callers exit with
/// code 2 then, like upstream.
#[derive(Debug, Default)]
pub struct PushOutcome {
    pub report: Report,
    pub aborted: bool,
}

/// Arguments for [`push`].
pub struct PushOptions<'a> {
    pub args: &'a [String],
    /// The non-empty lines of stdin, read when `stdin` is set.
    pub stdin_lines: &'a [String],
    pub dry_run: bool,
    pub all: bool,
    pub stdin: bool,
    pub object_id: bool,
}

/// Run `git lfs push`.
pub fn push(
    cwd: &Path,
    remote: &str,
    opts: &PushOptions<'_>,
) -> Result<PushOutcome, PushCommandError> {
    if !is_remote_or_url(cwd, remote) {
        return Err(PushCommandError::Usage(format!(
            "Invalid remote name: {remote:?}"
        )));
    }

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

    if effective_args.is_empty() {
        return Err(PushCommandError::Usage(
            "At least one ref must be supplied without --all".into(),
        ));
    }

    // Check refs first so a typo gets `Invalid ref argument` instead of
    // an error from `git rev-list`.
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

/// Upload the objects named by `oids` from the local store.
///
/// Locks aren't verified, since there are no paths to check, like
/// upstream.
fn push_by_oid(
    cwd: &Path,
    remote: &str,
    oids: &[String],
    opts: &PushOptions<'_>,
) -> Result<PushOutcome, PushCommandError> {
    // Empty stdin is allowed, like upstream, to make scripting easier.
    if oids.is_empty() {
        if opts.stdin {
            return Ok(PushOutcome::default());
        }
        return Err(PushCommandError::Usage(
            "At least one object ID must be supplied with --object-id".into(),
        ));
    }

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
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
        // There's no path, but t-push.sh checks for `push <oid> =>`.
        for spec in &to_upload {
            println!("push {} => ", spec.oid);
        }
        return Ok(PushOutcome::default());
    }

    let fetcher = LfsFetcher::from_repo_with_remote(
        cwd,
        &store,
        Some(remote),
        git_lfs_git::endpoint::Operation::Upload,
    )?;

    let total = to_upload.len();
    let total_bytes: u64 = to_upload.iter().map(|s| s.size).sum();
    let succeeded_bytes_lookup: HashMap<String, u64> =
        to_upload.iter().map(|s| (s.oid.clone(), s.size)).collect();

    // When the whole upload fails, as when the batch request does, the
    // progress line is still printed, like upstream's meter.
    let uploaded = fetcher.upload_many(to_upload);
    fetcher.persist_access_mode();
    let done = uploaded.as_ref().map_or(&[][..], |r| &r.succeeded[..]);

    let succeeded = done.len();
    let succeeded_bytes: u64 = done
        .iter()
        .filter_map(|oid| succeeded_bytes_lookup.get(oid).copied())
        .sum();
    let percent = if total_bytes == 0 {
        100
    } else {
        ((succeeded_bytes as u128 * 100) / total_bytes as u128) as u32
    };
    println!(
        "Uploading LFS objects: {percent:>3}% ({succeeded}/{total}), {}",
        human_bytes(succeeded_bytes),
    );
    let report = uploaded.map_err(PushCommandError::Fetch)?;
    for (oid, err) in &report.failed {
        eprintln!("[{oid}] {err}");
    }
    Ok(PushOutcome {
        report,
        aborted: false,
    })
}

/// Parse an OID argument, with the error messages t-push.sh checks.
fn parse_oid(raw: &str) -> Result<Oid, PushCommandError> {
    if raw.len() < 64 {
        return Err(PushCommandError::Usage(format!(
            "too short object ID: {raw:?}"
        )));
    }
    raw.parse::<Oid>()
        .map_err(|_| PushCommandError::Usage(format!("invalid object ID: {raw:?}")))
}

/// Whether `name` is a configured remote, looks like a URL, or has an LFS
/// endpoint.
///
/// The last case lets a repository without remotes push to `lfs.url`.
fn is_remote_or_url(cwd: &Path, name: &str) -> bool {
    if name.contains("://") || name.starts_with("git@") || std::path::Path::new(name).is_absolute()
    {
        return true;
    }
    let key = format!("remote.{name}.url");
    if matches!(git_lfs_git::config::get_effective(cwd, &key), Ok(Some(_))) {
        return true;
    }
    git_lfs_git::endpoint::endpoint_for_remote(cwd, Some(name)).is_ok()
}

/// Whether `r` resolves to a commit.
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

/// Every local branch and tag, as full ref names.
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

/// Upload the objects in commits reachable from `includes` but not from
/// `excludes`.
///
/// Locks are verified first. Objects missing locally are checked against
/// the server, and fail the push unless the server has them or
/// `lfs.allowincompletepush` is set. Corrupt local objects always fail
/// it, after the others are uploaded. `refspec` is sent with the batch
/// requests. With `dry_run`, only print what would be uploaded.
pub(crate) fn upload_in_range(
    cwd: &Path,
    remote: &str,
    includes: &[&str],
    excludes: &[&str],
    refspec: Option<String>,
    dry_run: bool,
) -> Result<PushOutcome, PushCommandError> {
    upload_in_range_with_args(cwd, remote, includes, excludes, &[], refspec, dry_run)
}

/// Like [`upload_in_range`], with extra arguments for `git rev-list`.
///
/// `pre-push` passes `--not --remotes=<name>` here. Those arguments only
/// narrow the scan for lock verification: objects they exclude are still
/// checked against the server and uploaded if it's missing them, in case
/// a remote-tracking ref is stale.
pub(crate) fn upload_in_range_with_args(
    cwd: &Path,
    remote: &str,
    includes: &[&str],
    excludes: &[&str],
    extra_rev_list_args: &[&str],
    refspec: Option<String>,
    dry_run: bool,
) -> Result<PushOutcome, PushCommandError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    let pointers = git_lfs_git::scanner::scan_pointers_with_args(
        cwd,
        includes,
        excludes,
        extra_rev_list_args,
    )?;

    // An object with the wrong size locally is corrupt, and isn't
    // checked against the server.
    let mut to_upload: Vec<ObjectSpec> = Vec::new();
    let mut paths: HashMap<String, PathBuf> = HashMap::new();
    let mut missing: Vec<(ObjectSpec, Option<PathBuf>)> = Vec::new();
    let mut corrupt: Vec<(ObjectSpec, Option<PathBuf>)> = Vec::new();
    for entry in pointers {
        // An empty file is its own pointer and has no object to upload.
        if entry.size == 0 {
            continue;
        }
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
        } else if store.contains(entry.oid) {
            corrupt.push((spec, entry.path));
        } else {
            missing.push((spec, entry.path));
        }
    }

    // A remote-tracking ref may claim objects the server no longer has,
    // so scan again without the extra arguments.
    if !extra_rev_list_args.is_empty() {
        use std::collections::HashSet;
        let known: HashSet<git_lfs_pointer::Oid> = to_upload
            .iter()
            .chain(missing.iter().map(|(s, _)| s))
            .chain(corrupt.iter().map(|(s, _)| s))
            .filter_map(|s| s.oid.parse().ok())
            .collect();
        let full = git_lfs_git::scanner::scan_pointers_with_args(cwd, includes, excludes, &[])?;
        for entry in full {
            if known.contains(&entry.oid) {
                continue;
            }
            if entry.size == 0 {
                continue;
            }
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
            } else if store.contains(entry.oid) {
                corrupt.push((spec, entry.path));
            } else {
                missing.push((spec, entry.path));
            }
        }
    }

    if dry_run {
        // List missing and corrupt objects too, like upstream.
        for spec in &to_upload {
            if let Some(p) = paths.get(&spec.oid) {
                println!("push {} => {}", spec.oid, p.display());
            }
        }
        for (spec, _) in missing.iter().chain(corrupt.iter()) {
            if let Some(p) = paths.get(&spec.oid) {
                println!("push {} => {}", spec.oid, p.display());
            }
        }
        return Ok(PushOutcome::default());
    }

    let mut fetcher = LfsFetcher::from_repo_with_remote(
        cwd,
        &store,
        Some(remote),
        git_lfs_git::endpoint::Operation::Upload,
    )?;
    if refspec.is_some() {
        fetcher = fetcher.with_refspec(refspec);
    }

    // Verify locks before any batch request. Their locks block the push
    // for every path changed in the range, not only LFS files, since a
    // file can be lockable without being in LFS.
    let endpoint = git_lfs_git::endpoint::resolve_endpoint_for(
        cwd,
        Some(remote),
        git_lfs_git::endpoint::Operation::Upload,
    )
    .map_err(|e| std::io::Error::other(e.to_string()))?
    .url;
    let changed_paths = changed_paths_in_range(cwd, includes, excludes, extra_rev_list_args)?;
    let theirs_blockers: Vec<git_lfs_api::Lock> =
        match fetcher.preflight_verify_locks(cwd, Some(remote), remote, &endpoint)? {
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
                    let any_ours_pushed = ours_paths
                        .iter()
                        .any(|op| changed_paths.iter().any(|p| p == op));
                    if any_ours_pushed {
                        eprintln!(
                            "Consider unlocking your own locked files: \
                             (`git lfs unlock <path>`)"
                        );
                        for op in &ours_paths {
                            if changed_paths.iter().any(|p| p == op) {
                                eprintln!("* {op}");
                            }
                        }
                    }
                }
                theirs
                    .into_iter()
                    .filter(|l| changed_paths.iter().any(|p| p == &l.path))
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

    // Objects missing locally that the server already has count as
    // uploaded in the progress line.
    let (truly_missing, already_on_server): (Vec<(ObjectSpec, Option<PathBuf>)>, Vec<ObjectSpec>) =
        if missing.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let server_has = fetcher
                .check_server_has(missing.iter().map(|(s, _)| s.clone()).collect())
                .map_err(PushCommandError::Fetch)?;
            let mut missing_off = Vec::new();
            let mut on_server = Vec::new();
            for (spec, path) in missing {
                if server_has.contains(&spec.oid) {
                    on_server.push(spec);
                } else {
                    missing_off.push((spec, path));
                }
            }
            (missing_off, on_server)
        };

    if !truly_missing.is_empty() {
        let allow = allow_incomplete_push(cwd);
        if !allow {
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

    if to_upload.is_empty() && already_on_server.is_empty() {
        if !corrupt.is_empty() {
            eprintln!("LFS upload failed:");
            for (spec, path) in &corrupt {
                let path_str = path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                eprintln!("  (corrupt) {path_str} ({})", spec.oid);
            }
            return Ok(PushOutcome {
                report: Report::default(),
                aborted: true,
            });
        }
        return Ok(PushOutcome::default());
    }

    let total = to_upload.len() + already_on_server.len();
    let already_on_server_bytes: u64 = already_on_server.iter().map(|s| s.size).sum();
    let total_bytes: u64 = to_upload.iter().map(|s| s.size).sum::<u64>() + already_on_server_bytes;
    let succeeded_bytes_lookup: HashMap<String, u64> =
        to_upload.iter().map(|s| (s.oid.clone(), s.size)).collect();

    // When the whole upload fails, as when the batch request does, the
    // progress line is still printed, like upstream's meter.
    let uploaded = if to_upload.is_empty() {
        Ok(Report::default())
    } else {
        fetcher.set_progress_names(&paths);
        let uploaded = fetcher.upload_many(to_upload);
        fetcher.persist_access_mode();
        uploaded
    };
    let done = uploaded.as_ref().map_or(&[][..], |r| &r.succeeded[..]);

    let succeeded = done.len() + already_on_server.len();
    let succeeded_bytes: u64 = done
        .iter()
        .filter_map(|oid| succeeded_bytes_lookup.get(oid).copied())
        .sum::<u64>()
        + already_on_server_bytes;
    let percent = if total_bytes == 0 {
        100
    } else {
        ((succeeded_bytes as u128 * 100) / total_bytes as u128) as u32
    };
    println!(
        "Uploading LFS objects: {percent:>3}% ({succeeded}/{total}), {}",
        human_bytes(succeeded_bytes),
    );
    let report = uploaded.map_err(PushCommandError::Fetch)?;

    for (oid, err) in &report.failed {
        eprintln!("[{oid}] {err}");
    }

    // Corrupt objects fail the push even with `lfs.allowincompletepush`.
    // They're reported after the upload because t-push.sh expects the
    // healthy objects on the server, like upstream.
    let aborted = !corrupt.is_empty() || !report.failed.is_empty();
    if !corrupt.is_empty() {
        eprintln!("LFS upload failed:");
        for (spec, path) in &corrupt {
            let path_str = path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            eprintln!("  (corrupt) {path_str} ({})", spec.oid);
        }
    }

    Ok(PushOutcome { report, aborted })
}

/// Every path changed by a commit reachable from `includes` but not from
/// `excludes` or `extra_rev_list_args`.
fn changed_paths_in_range(
    cwd: &Path,
    includes: &[&str],
    excludes: &[&str],
    extra_rev_list_args: &[&str],
) -> Result<Vec<String>, PushCommandError> {
    if includes.is_empty() {
        return Ok(Vec::new());
    }
    let mut args: Vec<&str> = vec!["log", "-z", "--pretty=format:", "--name-only"];
    args.extend(includes.iter().copied());
    if !excludes.is_empty() {
        args.push("--not");
        args.extend(excludes.iter().copied());
    }
    args.extend(extra_rev_list_args.iter().copied());
    // So a file named like a ref isn't taken as a path.
    args.push("--");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(&args)
        .output()
        .map_err(PushCommandError::Io)?;
    if !out.status.success() {
        return Err(PushCommandError::Usage(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for raw in out.stdout.split(|&b| b == 0) {
        if raw.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(raw);
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            seen.insert(trimmed.to_owned());
        }
    }
    Ok(seen.into_iter().collect())
}

/// Whether `lfs.allowincompletepush` is set to `true` or `1`.
///
/// Upstream also accepts the other boolean spellings Git does.
fn allow_incomplete_push(cwd: &Path) -> bool {
    git_lfs_git::config::get_effective(cwd, "lfs.allowincompletepush")
        .ok()
        .flatten()
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

/// Format a byte count in decimal units, like go-humanize's `Bytes`, which
/// upstream's progress meter uses.
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

/// Every ref under `refs/remotes/<remote>/`, as full ref names.
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
