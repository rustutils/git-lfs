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
    /// User passed `--recent`. Combined with `lfs.fetchrecentalways` to
    /// decide whether to walk recent refs + recent commits in addition
    /// to the named refs' HEAD-state.
    pub recent: bool,
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

    // Resolve the include set. Upstream's `--all` semantics:
    //   `--all` + ref args → walk only those refs (test 21
    //                       `fetch --all origin main` excludes
    //                       branch1's objects).
    //   `--all` alone     → walk every local + remote-tracking ref
    //                       so an LFS object reachable only via
    //                       refs/remotes/<remote>/<deleted-locally>
    //                       still gets fetched.
    //   no `--all`, refs  → use the refs.
    //   no args + no all  → discover via `git ls-files :(attr:filter=lfs)`
    //                       (matches upstream's git 2.42+ behavior:
    //                       respects sparse-checkout, skips bare repos
    //                       without an index, sidesteps rev-list on
    //                       partial clones).
    let mut store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    if let Some(v) = crate::shared_repo_config(cwd) {
        store = store.with_shared_repository(&v);
    }

    let walk_refs: Vec<String> = if !ref_args.is_empty() {
        ref_args
    } else if opts.all {
        all_local_refs(cwd)?
    } else {
        Vec::new()
    };
    let ref_strs: Vec<&str> = walk_refs.iter().map(String::as_str).collect();

    let mut pointers = if walk_refs.is_empty() {
        git_lfs_git::scan_index_lfs(cwd)?
    } else if opts.all {
        // `--all` walks every reachable commit so historical /
        // deleted-from-HEAD pointers still get fetched. Augmented
        // below with per-ref tree paths so include/exclude filters
        // see every working-tree path an OID lives at.
        scan_pointers(cwd, &ref_strs, &[])?
    } else {
        // Plain `git lfs fetch <ref>` walks the HEAD-state tree of
        // each ref only — matches upstream's `fetchRef` behavior
        // (recent-history walks happen via `--recent` or `--all`).
        // Dedup by OID across refs so a pointer reachable from two
        // refs at the same path doesn't double-enqueue.
        use std::collections::HashMap;
        let mut by_oid: HashMap<git_lfs_pointer::Oid, PointerEntry> = HashMap::new();
        for r in &walk_refs {
            for e in git_lfs_git::scan_tree(cwd, r)? {
                by_oid
                    .entry(e.oid)
                    .and_modify(|existing| {
                        for p in &e.paths {
                            if !existing.paths.contains(p) {
                                existing.paths.push(p.clone());
                            }
                        }
                    })
                    .or_insert(e);
            }
        }
        by_oid.into_values().collect()
    };

    // For the `--all` path, augment each pointer's `paths` with every
    // working-tree path the same LFS object lives at across the named
    // refs. `git rev-list --objects` only emits each blob OID once
    // (with one of its paths), so two `--include` patterns like
    // `big/a` vs `big/b` — pointing at files that share a blob —
    // would otherwise see one arbitrary side and reject the OID.
    // `scan_tree` does walk every path in one ref, so unioning that
    // gives us the full set. The non-all path already aggregates via
    // scan_tree above, and the index path emits one entry per index
    // path already.
    if !walk_refs.is_empty() && opts.all {
        use std::collections::HashMap;
        let mut extra_paths: HashMap<git_lfs_pointer::Oid, Vec<PathBuf>> = HashMap::new();
        for r in &walk_refs {
            let Ok(entries) = git_lfs_git::scan_tree(cwd, r) else {
                continue;
            };
            for e in entries {
                let Some(p) = e.path else { continue };
                let bucket = extra_paths.entry(e.oid).or_default();
                if !bucket.contains(&p) {
                    bucket.push(p);
                }
            }
        }
        for ptr in &mut pointers {
            if let Some(extras) = extra_paths.get(&ptr.oid) {
                for p in extras {
                    if !ptr.paths.contains(p) {
                        ptr.paths.push(p.clone());
                    }
                }
            }
        }
    }

    // Recent-history walk. `--recent` (or `lfs.fetchrecentalways`)
    // expands the fetch set with two extras:
    //   1. HEAD-state of every ref whose tip commit is within
    //      `lfs.fetchrecentrefsdays` of now.
    //   2. Pre-images of every LFS pointer modified within
    //      `lfs.fetchrecentcommitsdays` on each named ref AND on each
    //      recent ref.
    // Mirrors upstream's `fetchRecent` (command_fetch.go::fetchRecent).
    // Skipped on `--all` (already walks full history) and on the
    // index-only / no-arg path (no refs to anchor the walk).
    let fp_cfg = git_lfs_git::FetchPruneConfig::from_repo(cwd);
    let want_recent = (opts.recent || fp_cfg.fetch_recent_always) && !opts.all;
    if want_recent {
        // Anchor the scan: when the user passed no positional refs the
        // implicit anchor is HEAD's current branch (matches upstream's
        // `refs = [CurrentRef()]` default). The named-refs path uses
        // walk_refs verbatim.
        let anchors: Vec<String> = if walk_refs.is_empty() {
            vec!["HEAD".to_owned()]
        } else {
            walk_refs.clone()
        };
        recent_walk(cwd, &fp_cfg, remote.as_deref(), &anchors, &mut pointers)?;
    }

    // Apply include/exclude path filters. CLI flags take precedence
    // over `lfs.fetchinclude` / `lfs.fetchexclude`; an empty CLI flag
    // (e.g. `-X ""`) explicitly clears the config. Pointers without a
    // path go through unfiltered.
    let include_set = build_pattern_set(cwd, opts.include, "lfs.fetchinclude")?;
    let exclude_set = build_pattern_set(cwd, opts.exclude, "lfs.fetchexclude")?;
    let filtered: Vec<PointerEntry> = pointers
        .into_iter()
        .filter(|p| paths_pass_filter(&p.paths, &include_set, &exclude_set))
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

    // Validate `http.sslKey` upfront. We don't yet wire client certs
    // through to reqwest, but t-fetch 28 (`fetch does not crash on
    // empty key files`) configures `/dev/null` as the key and expects
    // an `Error decoding PEM block` message instead of a panic or
    // a generic network error. Mirrors upstream's PEM-decode check
    // in `lfshttp/certs.go::getClientCertForHost`.
    if let Ok(Some(path)) = git_lfs_git::config::get_effective(cwd, "http.sslkey")
        && !path.is_empty()
    {
        match std::fs::read(&path) {
            Ok(bytes) if bytes.windows(11).any(|w| w == b"-----BEGIN ") => {}
            _ => {
                return Err(FetchCommandError::Usage(format!(
                    "Error decoding PEM block from {path:?}"
                )));
            }
        }
    }

    // Probe the storage directory before resolving the LFS endpoint
    // so a chmod 400'd `.git/lfs/objects/` surfaces as
    // `error trying to create local storage directory` (t-fetch 28),
    // not as a downstream "can't resolve remote" error. Mirrors
    // upstream's `fs.ObjectPath` mkdir-on-first-use semantics — we
    // can't lazily defer it like the store's `commit` does because
    // the error path the test greps for fires before any object is
    // ever written.
    if let Some(spec) = to_fetch.first()
        && let Ok(oid) = spec.oid.parse::<git_lfs_pointer::Oid>()
    {
        let dir = store.object_path(oid);
        if let Some(parent) = dir.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Err(FetchCommandError::Usage(format!(
                "error trying to create local storage directory in {:?}: {e}",
                parent.display()
            )));
        }
    }

    // Real fetch: drive the transfer queue.
    let fetcher = LfsFetcher::from_repo_with_remote(cwd, &store, remote.as_deref())?;
    let total = to_fetch.len();
    let total_bytes: u64 = to_fetch.iter().map(|s| s.size).sum();

    // For `--json` we also want the batch response — the test diffs
    // against a literal JSON shape that includes `actions.download.href`.
    // The transfer queue runs its own batch internally, but doesn't
    // surface that response back. Easiest: do a one-off batch up front
    // when JSON is requested. The redundant second batch from the
    // transfer is harmless (server-side dedup, idempotent), and only
    // happens on the `--json` path which isn't a hot loop.
    let batch_resp =
        if opts.json {
            use git_lfs_api::{BatchRequest, Operation};
            let mut req = BatchRequest::new(Operation::Download, to_fetch.clone());
            if let Some(r) = git_lfs_git::refs::current_refspec(cwd).map(git_lfs_api::Ref::new) {
                req = req.with_ref(r);
            }
            let api = fetcher.api_client().map_err(FetchCommandError::Fetch)?;
            Some(fetcher.runtime_block_on(api.batch(&req)).map_err(
                |e: git_lfs_api::ApiError| FetchCommandError::Fetch(e.to_string().into()),
            )?)
        } else {
            None
        };

    let report = fetcher
        .download_many(to_fetch.clone())
        .map_err(FetchCommandError::Fetch)?;
    fetcher.persist_access_mode();

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
        print_json_transfers(&store, &to_fetch, &paths, batch_resp.as_ref())?;
    } else if total > 0 {
        // Suppress the progress line when there's literally nothing to
        // fetch — t-pull `with partial clone and sparse checkout` greps
        // for absence of "Downloading LFS objects" to confirm no
        // out-of-cone work happened.
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
            recent: false,
            force: false,
            verify_remote: false,
            no_verify_remote: false,
            verify_unreachable: false,
            no_verify_unreachable: false,
            continue_when_unverified: false,
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
    fetcher.persist_access_mode();
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
        // A trailing `/` means "directory contents", e.g. `dir/` should
        // match `dir/a.dat`. Drop the slash so the ancestor-dir branch
        // of `matches_with_prefix` handles it. Don't strip a lone `/`.
        let mut normalized: &str = pat
            .strip_suffix('/')
            .filter(|s| !s.is_empty())
            .unwrap_or(pat);
        // Leading `/` is upstream's root-anchor marker (e.g.
        // `lfs.fetchexclude=/foo` means "the foo directory at the
        // repo root"). Globset has no path-anchor concept, so strip
        // the slash before compiling — `matches_with_prefix` already
        // walks ancestors, which gives the correct subtree match.
        if let Some(rest) = normalized.strip_prefix('/')
            && !rest.is_empty()
        {
            normalized = rest;
        }
        let glob = Glob::new(normalized)
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
/// True if *any* of `paths` would pass [`path_passes_filter`]. A
/// single LFS OID can be checked in at multiple working-tree paths
/// across history, so a filter that picks one of those paths must
/// pull the OID even when the scanner's "primary" path doesn't
/// match. t-fetch-include 3 is the smoke test.
pub(crate) fn paths_pass_filter(
    paths: &[PathBuf],
    include: &Option<GlobSet>,
    exclude: &Option<GlobSet>,
) -> bool {
    if paths.is_empty() {
        return path_passes_filter(None, include, exclude);
    }
    paths
        .iter()
        .any(|p| path_passes_filter(Some(p), include, exclude))
}

pub(crate) fn path_passes_filter(
    path: Option<&Path>,
    include: &Option<GlobSet>,
    exclude: &Option<GlobSet>,
) -> bool {
    let Some(path) = path else { return true };
    if let Some(inc) = include
        && !matches_with_prefix(path, inc)
    {
        return false;
    }
    if let Some(exc) = exclude
        && matches_with_prefix(path, exc)
    {
        return false;
    }
    true
}

/// Match `path` against `set` with three escape hatches that mirror
/// `.gitignore` / upstream `filepathfilter` semantics:
///   1. Exact path match (`big/b/b1.big` against `big/b/*.big`).
///   2. Basename match (`b1.big` against `b*.big`) so users can
///      filter without knowing where in the tree the file lives.
///   3. Ancestor-directory match (`big/b/b1.big` accepted when the
///      pattern is `big/b`) — t-fetch-include's `--include=big/b`
///      relies on this.
fn matches_with_prefix(path: &Path, set: &GlobSet) -> bool {
    if set.is_match(path) {
        return true;
    }
    let basename = path.file_name().map(Path::new).unwrap_or(path);
    if set.is_match(basename) {
        return true;
    }
    // Walk up the path checking each ancestor directory. Skip the
    // path itself (already checked) and the empty root.
    let mut ancestor = path.parent();
    while let Some(a) = ancestor {
        if a.as_os_str().is_empty() {
            break;
        }
        if set.is_match(a) {
            return true;
        }
        ancestor = a.parent();
    }
    false
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

pub(crate) fn is_resolvable_ref(cwd: &Path, r: &str) -> bool {
    // Range syntax (`A..B`, `A...B`) — validate each side separately.
    // `git rev-parse --verify` doesn't accept ranges, but `git lfs fsck
    // HEAD^..HEAD` is a thing the test suite exercises.
    if let Some((a, b)) = r.split_once("...") {
        return is_resolvable_ref(cwd, a) && is_resolvable_ref(cwd, b);
    }
    if let Some((a, b)) = r.split_once("..") {
        return is_resolvable_ref(cwd, a) && is_resolvable_ref(cwd, b);
    }
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

/// Append "recent" pointers — HEAD-state of recent refs, plus
/// pre-images of LFS files modified within `commits_days` on each
/// named-and-recent ref — into `pointers`. Dedups by OID against
/// what's already present.
///
/// Mirrors upstream's `command_fetch.go::fetchRecent` (and the
/// `gitscanner.ScanPreviousVersions` calls it spawns).
fn recent_walk(
    cwd: &Path,
    cfg: &git_lfs_git::FetchPruneConfig,
    remote: Option<&str>,
    named_refs: &[String],
    pointers: &mut Vec<PointerEntry>,
) -> Result<(), FetchCommandError> {
    use std::collections::HashSet;
    use std::time::{Duration, SystemTime};

    let now = SystemTime::now();
    let day = Duration::from_secs(86_400);

    // Recent refs: tips with committer date within `refs_days` of now.
    // `refs_days = 0` disables this discovery (only the named refs +
    // their pre-images contribute).
    let mut recent_refs: Vec<String> = Vec::new();
    if cfg.fetch_recent_refs_days > 0 {
        let since = now - day * cfg.fetch_recent_refs_days as u32;
        // When include-remotes is on, restrict to *this* fetch's
        // remote — a recent ref under a different remote isn't going
        // to come from the same server.
        let only_remote = if cfg.fetch_recent_refs_include_remotes {
            remote
        } else {
            None
        };
        let refs = git_lfs_git::recent_branches(
            cwd,
            since,
            cfg.fetch_recent_refs_include_remotes,
            only_remote,
        )?;
        for r in refs {
            recent_refs.push(r.full);
        }
    }

    let mut have_oids: HashSet<git_lfs_pointer::Oid> = pointers.iter().map(|p| p.oid).collect();

    // 1. HEAD-state of every walk anchor (named or recent) → scan_tree.
    //    Anchors are added unconditionally — `scan_index_lfs` only
    //    finds pointers in repos with a committed `.gitattributes`,
    //    so on partial-clone / no-attrs setups (like the t-fetch-recent
    //    fixture) we'd otherwise miss the HEAD-state entirely. Dedup
    //    via `have_oids` keeps us from double-fetching when the index
    //    scan did surface them.
    let mut all_anchors: Vec<&str> = named_refs.iter().map(String::as_str).collect();
    for r in &recent_refs {
        if !all_anchors.contains(&r.as_str()) {
            all_anchors.push(r.as_str());
        }
    }
    for r in &all_anchors {
        for entry in git_lfs_git::scan_tree(cwd, r)? {
            if have_oids.insert(entry.oid) {
                pointers.push(entry);
            }
        }
    }

    // 2. Pre-images for the recent commits window. `commits_days = 0`
    //    means "at-ref only" (no pre-images). Window is measured from
    //    *each ref's tip commit date* (matching upstream's
    //    `summ.CommitDate.AddDate(0,0,-N)`), not from now — so a ref
    //    whose tip is itself old still surfaces its pre-images
    //    correctly relative to that tip.
    if cfg.fetch_recent_commits_days > 0 {
        for r in &all_anchors {
            let Some(tip_unix) = ref_tip_unix(cwd, r) else {
                continue;
            };
            let commits_since = SystemTime::UNIX_EPOCH + Duration::from_secs(tip_unix as u64)
                - day * cfg.fetch_recent_commits_days as u32;
            for entry in git_lfs_git::scan_previous_versions(cwd, r, commits_since)? {
                if have_oids.insert(entry.oid) {
                    pointers.push(entry);
                }
            }
        }
    }

    Ok(())
}

/// Tip commit's Unix timestamp for `reference`, or `None` if the ref
/// doesn't resolve. Used as the anchor for the per-ref
/// `commits_days` window in [`recent_walk`].
fn ref_tip_unix(cwd: &Path, reference: &str) -> Option<i64> {
    let out = Command::new("git")
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

fn all_local_refs(cwd: &Path) -> Result<Vec<String>, FetchCommandError> {
    // `--all` includes remote tracking refs so an LFS object that
    // lives only on `refs/remotes/origin/<deleted-locally>` still
    // gets walked (t-fetch-all `git branch -D remote_branch_only`
    // case). Mirrors upstream's scanRefsToChan with an empty range
    // — it ends up running `git rev-list --all`.
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/",
            "refs/tags/",
            "refs/remotes/",
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
