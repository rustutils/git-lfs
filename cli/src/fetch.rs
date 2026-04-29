//! `git lfs fetch [<remote>] [<ref>...]` — download LFS objects reachable
//! from the named refs that aren't already in the local store.
//!
//! Argument shape mirrors upstream: the first positional arg is treated
//! as a remote name if it resolves (and isn't a refspec); everything
//! after is a ref. With `--all`, all `refs/heads/*` + `refs/tags/*` get
//! walked. With `--stdin`, refs are read from stdin one per line.
//!
//! Per-ref scanning uses `git/scanner.rs` (`ScanRefs` semantics — every
//! blob in every commit's tree is examined). Pointers we already have
//! locally are filtered out before the batch (`--refetch` skips this
//! filter). Path-pattern filters (`--include` / `--exclude`, plus the
//! `lfs.fetchinclude` / `lfs.fetchexclude` config keys) are applied to
//! each pointer's working-tree path; pointers whose path is unknown
//! pass through unfiltered (matching upstream's "include the orphan"
//! behavior).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_api::ObjectSpec;
use git_lfs_git::{PointerEntry, scan_pointers};
use git_lfs_store::Store;
use git_lfs_transfer::Report;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;

use crate::LfsFetcher;

#[derive(Debug, thiserror::Error)]
pub enum FetchCommandError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("fetch failed: {0}")]
    Fetch(git_lfs_filter::FetchError),
    /// User-facing argument error. Carries the message verbatim.
    #[error("{0}")]
    Usage(String),
}

/// All flags + positional args for `git lfs fetch`. Bundled so callers
/// don't have to thread eight independent parameters through dispatch.
pub struct FetchOptions<'a> {
    pub args: &'a [String],
    pub stdin_lines: &'a [String],
    pub dry_run: bool,
    pub json: bool,
    pub all: bool,
    pub refetch: bool,
    pub stdin: bool,
    pub prune: bool,
    /// Comma-separated globs from `--include` / `-I`. Empty string =
    /// "no override" (config still applies).
    pub include: &'a [String],
    /// Comma-separated globs from `--exclude` / `-X`. Same semantics.
    pub exclude: &'a [String],
}

/// Outcome of a fetch attempt — carries the per-object [`Report`]. No
/// `aborted` flag (fetch has no equivalent of push's exit-2 case).
#[derive(Debug, Default)]
pub struct FetchOutcome {
    pub report: Report,
}

/// Run the fetch command. Routes to the right scan / filter / download
/// path based on `opts`.
pub fn fetch(cwd: &Path, opts: &FetchOptions<'_>) -> Result<FetchOutcome, FetchCommandError> {
    // Outside-a-repo guard. Upstream exits 128 here; we surface the
    // condition via Usage and let the dispatcher map it. Use
    // `--git-dir` rather than `--is-inside-work-tree`: the former
    // succeeds for any valid repo configuration (work-tree, bare,
    // or `GIT_DIR` / `GIT_WORK_TREE` env-var redirection where cwd
    // sits outside the work tree, t-checkout test 14).
    if !is_in_git_repo(cwd) {
        return Err(FetchCommandError::Usage("Not in a Git repository.".into()));
    }

    // Resolve the effective positional args: --stdin overrides argv.
    let (effective_args, stdin_overrode_args) = if opts.stdin {
        (opts.stdin_lines, !opts.args.is_empty())
    } else {
        (opts.args, false)
    };
    if stdin_overrode_args {
        eprintln!("Further command line arguments are ignored with --stdin.");
    }

    // Split first positional into "remote" if it resolves; rest are
    // refs. With `--stdin`, all stdin lines are refs (no remote in
    // stdin); the remote is taken from argv[0] if argv had one.
    //
    // Disambiguation rule (matching upstream): when argv[0] resolves
    // as neither a remote nor a ref, prefer the "remote name" error
    // — the upstream test asks for `Invalid remote name` when the
    // first arg is genuinely bogus (`t-fetch.sh::fetch with invalid
    // remote`).
    let (remote, ref_args): (Option<String>, Vec<String>) = if opts.stdin {
        let remote = opts
            .args
            .first()
            .filter(|s| is_remote_or_url(cwd, s))
            .cloned();
        (remote, effective_args.to_vec())
    } else {
        match effective_args.split_first() {
            Some((first, rest)) if is_remote_or_url(cwd, first) => {
                (Some(first.clone()), rest.to_vec())
            }
            Some((first, rest)) if rest.is_empty() && !is_resolvable_ref(cwd, first) => {
                return Err(FetchCommandError::Usage(format!(
                    "Invalid remote name: {first:?}"
                )));
            }
            _ => (None, effective_args.to_vec()),
        }
    };

    // Validate ref args before handing to rev-list. Lets us emit the
    // upstream-format `Invalid ref argument:` for typos, matching
    // `t-fetch.sh::fetch with invalid ref`.
    for r in &ref_args {
        if !is_resolvable_ref(cwd, r) {
            return Err(FetchCommandError::Usage(format!(
                "Invalid ref argument: {r:?}"
            )));
        }
    }

    // Resolve the include set: --all > explicit refs > current HEAD.
    let walk_refs: Vec<String> = if opts.all {
        all_local_refs(cwd)?
    } else if !ref_args.is_empty() {
        ref_args
    } else {
        // No refs / no --all → current HEAD. Matches upstream when the
        // user runs bare `git lfs fetch`.
        vec!["HEAD".to_string()]
    };
    let ref_strs: Vec<&str> = walk_refs.iter().map(String::as_str).collect();

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let pointers = scan_pointers(cwd, &ref_strs, &[])?;

    // Apply include/exclude path filters. CLI flags take precedence
    // over `lfs.fetchinclude` / `lfs.fetchexclude`; an empty CLI flag
    // (e.g. `-X ""`) explicitly clears the config. Pointers without a
    // path go through unfiltered.
    let include_set = build_pattern_set(cwd, opts.include, "lfs.fetchinclude")?;
    let exclude_set = build_pattern_set(cwd, opts.exclude, "lfs.fetchexclude")?;
    let filtered: Vec<PointerEntry> = pointers
        .into_iter()
        .filter(|p| path_passes_filter(p.path.as_deref(), &include_set, &exclude_set))
        .collect();

    // Decide what to actually fetch. With `--refetch`, download
    // everything regardless of local state (corrupt-recovery
    // scenario); otherwise filter to what we don't already have at
    // the right size.
    let mut to_fetch: Vec<ObjectSpec> = Vec::new();
    let mut paths: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    for p in &filtered {
        let oid_str = p.oid.to_string();
        if let Some(path) = p.path.clone() {
            paths.entry(oid_str.clone()).or_insert(path);
        }
        if !opts.refetch && store.contains_with_size(p.oid, p.size) {
            continue;
        }
        to_fetch.push(ObjectSpec {
            oid: oid_str,
            size: p.size,
        });
    }

    if to_fetch.is_empty() {
        if opts.json {
            print_json_transfers(&store, &[], &paths, None)?;
        }
        return Ok(FetchOutcome::default());
    }

    if opts.dry_run {
        if opts.json {
            // For --json --dry-run, we still need batch URLs to fill
            // in the `actions` field — call batch but skip transfer.
            return run_dry_run_with_json(cwd, remote.as_deref(), to_fetch, paths, &store);
        }
        for spec in &to_fetch {
            if let Some(p) = paths.get(&spec.oid) {
                println!("fetch {} => {}", spec.oid, p.display());
            }
        }
        return Ok(FetchOutcome::default());
    }

    // Real fetch: drive the transfer queue.
    let fetcher = LfsFetcher::from_repo_with_remote(cwd, &store, remote.as_deref())?;
    let total = to_fetch.len();
    let total_bytes: u64 = to_fetch.iter().map(|s| s.size).sum();

    // For --json, we also need the batch response (so we can emit the
    // `actions` field). Drive download_many then capture the
    // transfers list. For now, --json without dry-run will still
    // download but emit a minimal transfer list; full action capture
    // is deferred (see NOTES.md).
    let report = fetcher
        .download_many(to_fetch.clone())
        .map_err(FetchCommandError::Fetch)?;

    let succeeded = report.succeeded.len();
    let succeeded_bytes: u64 = to_fetch
        .iter()
        .filter(|s| report.succeeded.contains(&s.oid))
        .map(|s| s.size)
        .sum();
    let percent = if total_bytes == 0 {
        100
    } else {
        ((succeeded_bytes as u128 * 100) / total_bytes as u128) as u32
    };

    if opts.json {
        // Emit a minimal transfer list for completed objects.
        print_json_transfers(&store, &to_fetch, &paths, None)?;
    } else {
        eprintln!(
            "Downloading LFS objects: {percent}% ({succeeded}/{total}), {}",
            human_bytes(succeeded_bytes),
        );
    }
    for (oid, err) in &report.failed {
        eprintln!("  {oid}: {err}");
    }

    if opts.prune {
        // Best-effort prune after a successful fetch — matches
        // upstream's `--prune` shorthand.
        let prune_opts = crate::prune::Options {
            dry_run: false,
            verbose: false,
        };
        let _ = crate::prune::run(cwd, &prune_opts);
    }

    Ok(FetchOutcome { report })
}

/// `--dry-run --json`: call batch to learn the action URLs without
/// downloading bytes. Used by `t-fetch.sh::fetch --json`.
fn run_dry_run_with_json(
    cwd: &Path,
    remote: Option<&str>,
    to_fetch: Vec<ObjectSpec>,
    paths: std::collections::HashMap<String, PathBuf>,
    store: &Store,
) -> Result<FetchOutcome, FetchCommandError> {
    use git_lfs_api::{BatchRequest, Operation};
    let mut req = BatchRequest::new(Operation::Download, to_fetch.clone());
    if let Some(r) = git_lfs_git::refs::current_refspec(cwd).map(git_lfs_api::Ref::new) {
        req = req.with_ref(r);
    }
    let fetcher = LfsFetcher::from_repo_with_remote(cwd, store, remote)?;
    let api = fetcher.api_client().map_err(FetchCommandError::Fetch)?;
    let resp = fetcher
        .runtime_block_on(api.batch(&req))
        .map_err(|e: git_lfs_api::ApiError| FetchCommandError::Fetch(e.to_string().into()))?;
    print_json_transfers(store, &to_fetch, &paths, Some(&resp))?;
    Ok(FetchOutcome::default())
}

/// JSON output: `{ "transfers": [ { name, oid, size, actions, path } ] }`.
/// Single-space indent matches upstream + `t-fetch.sh::fetch --json`'s
/// literal-string diff.
fn print_json_transfers(
    store: &Store,
    specs: &[ObjectSpec],
    paths: &std::collections::HashMap<String, PathBuf>,
    batch_resp: Option<&git_lfs_api::BatchResponse>,
) -> Result<(), FetchCommandError> {
    #[derive(Serialize)]
    struct Transfer<'a> {
        name: String,
        oid: &'a str,
        size: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        actions: Option<&'a git_lfs_api::Actions>,
        path: String,
    }
    #[derive(Serialize)]
    struct Doc<'a> {
        transfers: Vec<Transfer<'a>>,
    }

    let actions_by_oid: std::collections::HashMap<&str, &git_lfs_api::Actions> = batch_resp
        .map(|r| {
            r.objects
                .iter()
                .filter_map(|o| o.actions.as_ref().map(|a| (o.oid.as_str(), a)))
                .collect()
        })
        .unwrap_or_default();

    let transfers: Vec<Transfer> = specs
        .iter()
        .map(|s| {
            let oid = s
                .oid
                .parse::<git_lfs_pointer::Oid>()
                .expect("oid valid post-batch");
            Transfer {
                name: paths
                    .get(&s.oid)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                oid: &s.oid,
                size: s.size,
                actions: actions_by_oid.get(s.oid.as_str()).copied(),
                path: store.object_path(oid).display().to_string(),
            }
        })
        .collect();
    let doc = Doc { transfers };

    // Single-space indent matches upstream's JSON formatter.
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(&doc, &mut ser)
        .map_err(|e| FetchCommandError::Io(std::io::Error::other(e.to_string())))?;
    let mut out = std::io::stdout().lock();
    out.write_all(&buf)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Read `<config_key>` (e.g. `lfs.fetchinclude`) and turn its
/// comma-separated globs into a [`GlobSet`]. `None` if the key isn't
/// set or has no patterns. Used by `fsck` to honor the same
/// include/exclude policy as `fetch` without going through CLI flags.
pub(crate) fn fetch_filter_set(
    cwd: &Path,
    config_key: &str,
) -> Result<Option<GlobSet>, FetchCommandError> {
    build_pattern_set(cwd, &[], config_key)
}

/// Build a [`GlobSet`] from CLI patterns + a config-key fallback. Empty
/// pattern strings (e.g. `--include ""`) clear the set entirely. Empty
/// list of patterns falls back to the config value (which is itself
/// comma-separated).
pub(crate) fn build_pattern_set(
    cwd: &Path,
    cli: &[String],
    config_key: &str,
) -> Result<Option<GlobSet>, FetchCommandError> {
    let raw: Vec<String> = if !cli.is_empty() {
        cli.iter()
            .flat_map(|s| s.split(',').map(str::trim).map(String::from))
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Some(cfg) = git_lfs_git::config::get_effective(cwd, config_key)? {
        cfg.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    } else {
        Vec::new()
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pat in &raw {
        let glob = Glob::new(pat)
            .map_err(|e| FetchCommandError::Usage(format!("invalid pattern {pat:?}: {e}")))?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|e| FetchCommandError::Usage(format!("pattern set build failed: {e}")))?;
    Ok(Some(set))
}

/// Apply include/exclude logic to a single pointer's working-tree
/// path. Pointers without a path always pass (orphan blobs from
/// rev-list — keep them, since the user can't filter what they can't
/// see). Exposed pub(crate) so `fsck` can apply the same
/// `lfs.fetchinclude` / `lfs.fetchexclude` semantics.
pub(crate) fn path_passes_filter(
    path: Option<&Path>,
    include: &Option<GlobSet>,
    exclude: &Option<GlobSet>,
) -> bool {
    let Some(path) = path else { return true };
    if let Some(inc) = include {
        if !inc.is_match(path) {
            // Try matching the basename too — upstream's tests use
            // `a*` to match `a.dat` regardless of directory depth.
            let basename = path.file_name().map(Path::new).unwrap_or(path);
            if !inc.is_match(basename) {
                return false;
            }
        }
    }
    if let Some(exc) = exclude {
        if exc.is_match(path) {
            return false;
        }
        let basename = path.file_name().map(Path::new).unwrap_or(path);
        if exc.is_match(basename) {
            return false;
        }
    }
    true
}

/// Decimal SI byte humanizer matching `dustin/go-humanize`'s `Bytes()`.
fn human_bytes(n: u64) -> String {
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

fn is_in_git_repo(cwd: &Path) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

fn is_remote_or_url(cwd: &Path, name: &str) -> bool {
    if name.contains("://")
        || name.starts_with("git@")
        || name.starts_with("file://")
        || std::path::Path::new(name).is_absolute()
    {
        return true;
    }
    let key = format!("remote.{name}.url");
    matches!(git_lfs_git::config::get_effective(cwd, &key), Ok(Some(_)))
}

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

fn all_local_refs(cwd: &Path) -> Result<Vec<String>, FetchCommandError> {
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
        return Err(FetchCommandError::Usage(format!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}
