//! `git lfs fetch`: download the LFS objects for some refs into the local
//! store.
//!
//! The first argument is a remote if it names one or looks like a URL, and
//! the rest are refs. Which objects are wanted depends on the arguments:
//!
//! - Refs: the objects in each ref's tree.
//! - `--all`: every object in the history of the given refs, or of all
//!   local branches, tags, and remote-tracking refs.
//! - Neither: the objects in the index.
//!
//! `--recent` or `lfs.fetchrecentalways` adds recent refs and recent
//! history (see [`recent_walk`]). Then the include and exclude patterns
//! drop objects by path, and objects already in the store are dropped
//! unless `--refetch` is given.
//!
//! Unlike upstream, `--all` doesn't ignore `lfs.fetchinclude` and
//! `lfs.fetchexclude`, and isn't rejected together with `--recent`,
//! `--include`, or `--exclude`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_api::ObjectSpec;
use git_lfs_git::scanner::{PointerEntry, scan_pointers};
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
    /// An error whose message is shown as is.
    #[error("{0}")]
    Usage(String),
}

/// Arguments for [`fetch`].
pub struct FetchOptions<'a> {
    pub args: &'a [String],
    pub stdin_lines: &'a [String],
    pub dry_run: bool,
    pub json: bool,
    pub all: bool,
    pub refetch: bool,
    pub stdin: bool,
    pub prune: bool,
    /// `--recent`, which `lfs.fetchrecentalways` also turns on.
    pub recent: bool,
    /// Values of `--include`, each a comma-separated pattern list.
    ///
    /// When empty, `lfs.fetchinclude` applies. A single empty value turns
    /// off the config value.
    pub include: &'a [String],
    /// Values of `--exclude`, used like `include` with `lfs.fetchexclude`.
    pub exclude: &'a [String],
}

/// The result of [`fetch`].
#[derive(Debug, Default)]
pub struct FetchOutcome {
    pub report: Report,
    /// The download endpoint's URL, for upstream's failure message.
    pub endpoint: String,
}

/// Run `git lfs fetch`.
pub fn fetch(cwd: &Path, opts: &FetchOptions<'_>) -> Result<FetchOutcome, FetchCommandError> {
    // The dispatcher matches this message and exits 128, like upstream.
    // `--git-dir` also succeeds in a bare repository and when
    // `GIT_WORK_TREE` points away from the current directory.
    if !is_in_git_repo(cwd) {
        return Err(FetchCommandError::Usage("Not in a Git repository.".into()));
    }

    // With `--stdin`, the refs come from stdin.
    let (effective_args, stdin_overrode_args) = if opts.stdin {
        (opts.stdin_lines, !opts.args.is_empty())
    } else {
        (opts.args, false)
    };
    if stdin_overrode_args {
        eprintln!("Further command line arguments are ignored with --stdin.");
    }

    // A lone argument that is neither a remote nor a ref is reported as
    // an invalid remote, which t-fetch.sh checks.
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

    // Check refs first so a typo gets upstream's `Invalid ref argument`
    // message, which t-fetch.sh checks.
    for r in &ref_args {
        if !is_resolvable_ref(cwd, r) {
            return Err(FetchCommandError::Usage(format!(
                "Invalid ref argument: {r:?}"
            )));
        }
    }

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
        // Like upstream with Git 2.42 and later, which reads the index:
        // this respects sparse checkout and avoids walking history in a
        // partial clone.
        git_lfs_git::scanner::scan_index_lfs(cwd)?
    } else if opts.all {
        scan_pointers(cwd, &ref_strs, &[])?
    } else {
        // Like upstream's `fetchRef`, only each ref's tree. Merge the
        // paths of an object found in several refs, keeping the order objects
        // were first found in so the output is the same on every run.
        use std::collections::HashMap;
        let mut entries: Vec<PointerEntry> = Vec::new();
        let mut index: HashMap<git_lfs_pointer::Oid, usize> = HashMap::new();
        for r in &walk_refs {
            for e in git_lfs_git::scanner::scan_tree(cwd, r)? {
                match index.get(&e.oid) {
                    Some(&i) => {
                        let existing = &mut entries[i];
                        for p in &e.paths {
                            if !existing.paths.contains(p) {
                                existing.paths.push(p.clone());
                            }
                        }
                    }
                    None => {
                        index.insert(e.oid, entries.len());
                        entries.push(e);
                    }
                }
            }
        }
        entries
    };

    // `git rev-list --objects` gives each blob only one of its paths, so
    // an include pattern that matches another path of the same object
    // would miss it. Add every path the object has in the tip trees of
    // the walked refs.
    if !walk_refs.is_empty() && opts.all {
        use std::collections::HashMap;
        let mut extra_paths: HashMap<git_lfs_pointer::Oid, Vec<PathBuf>> = HashMap::new();
        for r in &walk_refs {
            let Ok(entries) = git_lfs_git::scanner::scan_tree(cwd, r) else {
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

    // `--all` already walks the whole history.
    let fp_cfg = git_lfs_git::fetch_prune::FetchPruneConfig::from_repo(cwd);
    let want_recent = (opts.recent || fp_cfg.fetch_recent_always) && !opts.all;
    if want_recent {
        // Without refs, start from HEAD like upstream.
        let anchors: Vec<String> = if walk_refs.is_empty() {
            vec!["HEAD".to_owned()]
        } else {
            walk_refs.clone()
        };
        recent_walk(cwd, &fp_cfg, remote.as_deref(), &anchors, &mut pointers)?;
    }

    let include_set = build_pattern_set(cwd, opts.include, "lfs.fetchinclude")?;
    let exclude_set = build_pattern_set(cwd, opts.exclude, "lfs.fetchexclude")?;
    let filtered: Vec<PointerEntry> = pointers
        .into_iter()
        .filter(|p| paths_pass_filter(&p.paths, &include_set, &exclude_set))
        .collect();

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
            return run_dry_run_with_json(cwd, remote.as_deref(), to_fetch, paths, &store);
        }
        for spec in &to_fetch {
            if let Some(p) = paths.get(&spec.oid) {
                println!("fetch {} => {}", spec.oid, p.display());
            }
        }
        return Ok(FetchOutcome::default());
    }

    // t-fetch.sh sets `http.sslKey` to `/dev/null` and expects this
    // message, like upstream's `getClientCertForHost`.
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

    // Create an object directory before contacting the server, so a
    // read-only `.git/lfs/objects` gives this message, which t-fetch.sh
    // checks, instead of a later transfer error.
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

    let fetcher = LfsFetcher::from_repo_with_remote(
        cwd,
        &store,
        remote.as_deref(),
        git_lfs_git::endpoint::Operation::Download,
    )?;
    let total = to_fetch.len();
    let total_bytes: u64 = to_fetch.iter().map(|s| s.size).sum();

    // The JSON output includes the batch actions, which the transfer
    // queue doesn't return, so `--json` sends one extra batch request.
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

    fetcher.set_progress_names(&paths);
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
    } else {
        eprintln!(
            "Downloading LFS objects: {percent:>3}% ({succeeded}/{total}), {}",
            human_bytes(succeeded_bytes),
        );
    }
    for (oid, err) in &report.failed {
        eprintln!("[{oid}] {err}");
    }

    if opts.prune {
        // Prune errors are ignored. Upstream exits with them. Prune is
        // also skipped when there was nothing to download or with
        // `--dry-run`, which upstream doesn't do.
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

    let endpoint = git_lfs_git::endpoint::resolve_endpoint_for(
        cwd,
        remote.as_deref(),
        git_lfs_git::endpoint::Operation::Download,
    )
    .map(|info| info.url)
    .unwrap_or_default();
    Ok(FetchOutcome { report, endpoint })
}

/// Print the `--dry-run --json` output, which needs a batch request for
/// the actions.
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
    let fetcher = LfsFetcher::from_repo_with_remote(
        cwd,
        store,
        remote,
        git_lfs_git::endpoint::Operation::Download,
    )?;
    let api = fetcher.api_client().map_err(FetchCommandError::Fetch)?;
    let resp = fetcher
        .runtime_block_on(api.batch(&req))
        .map_err(|e: git_lfs_api::ApiError| FetchCommandError::Fetch(e.to_string().into()))?;
    fetcher.persist_access_mode();
    print_json_transfers(store, &to_fetch, &paths, Some(&resp))?;
    Ok(FetchOutcome::default())
}

/// Print the `--json` output.
///
/// t-fetch.sh compares it to upstream's output byte for byte, which is
/// indented by one space.
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

/// The patterns in `config_key`, such as `lfs.fetchinclude`.
///
/// `None` if the key isn't set or has no patterns.
pub(crate) fn fetch_filter_set(
    cwd: &Path,
    config_key: &str,
) -> Result<Option<GlobSet>, FetchCommandError> {
    build_pattern_set(cwd, &[], config_key)
}

/// Build a pattern set from command-line values, or from `config_key` if
/// there are none.
///
/// Each value is a comma-separated list. Values that hold no patterns,
/// such as `--include ""`, give an empty set instead of the config value.
pub(crate) fn build_pattern_set(
    cwd: &Path,
    cli: &[String],
    config_key: &str,
) -> Result<Option<GlobSet>, FetchCommandError> {
    if !cli.is_empty() {
        return pattern_set(cli);
    }
    match git_lfs_git::config::get_effective(cwd, config_key)? {
        Some(value) => pattern_set(&[value]),
        None => Ok(None),
    }
}

/// Build a pattern set from command-line values, without a config
/// fallback.
///
/// Each value is a comma-separated list. Returns `None` if they hold no
/// patterns.
pub(crate) fn pattern_set(values: &[String]) -> Result<Option<GlobSet>, FetchCommandError> {
    let raw: Vec<&str> = values
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if raw.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for &pat in &raw {
        // A trailing slash means the directory's contents. Drop it so the
        // ancestor check in `matches_with_prefix` matches. Keep a lone
        // slash.
        let mut normalized: &str = pat
            .strip_suffix('/')
            .filter(|s| !s.is_empty())
            .unwrap_or(pat);
        // A leading slash anchors the pattern at the repository root.
        // Globset paths have no leading slash, so drop it.
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

/// Whether any of `paths` passes [`path_passes_filter`].
///
/// An object can be at several paths, and is wanted if any of them is.
/// An object with no known path passes.
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

/// Whether `path` matches `include` (if set) and doesn't match `exclude`.
///
/// `None` always passes.
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

/// Whether `set` matches `path`, its file name, or one of its parent
/// directories.
///
/// So `big/b/b1.big` matches `big/b/*.big`, `b*.big`, and `big/b`, much
/// like upstream's `filepathfilter`.
fn matches_with_prefix(path: &Path, set: &GlobSet) -> bool {
    if set.is_match(path) {
        return true;
    }
    let basename = path.file_name().map(Path::new).unwrap_or(path);
    if set.is_match(basename) {
        return true;
    }
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

/// Format a byte count in decimal units, like go-humanize's `Bytes`.
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
    if name.contains("://") || name.starts_with("git@") || std::path::Path::new(name).is_absolute()
    {
        return true;
    }
    let key = format!("remote.{name}.url");
    matches!(git_lfs_git::config::get_effective(cwd, &key), Ok(Some(_)))
}

pub(crate) fn is_resolvable_ref(cwd: &Path, r: &str) -> bool {
    // `git rev-parse --verify` rejects ranges, which `git lfs fsck`
    // accepts, so check each side.
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

/// Add the objects for `--recent` to `pointers`, like upstream's
/// `fetchRecent`.
///
/// These are the trees of `named_refs` and of refs whose tip is within
/// `lfs.fetchrecentrefsdays` of now, and the earlier versions of files
/// changed on any of them within `lfs.fetchrecentcommitsdays` of that
/// ref's tip. Objects already in `pointers` aren't added again.
fn recent_walk(
    cwd: &Path,
    cfg: &git_lfs_git::fetch_prune::FetchPruneConfig,
    remote: Option<&str>,
    named_refs: &[String],
    pointers: &mut Vec<PointerEntry>,
) -> Result<(), FetchCommandError> {
    use std::collections::HashSet;
    use std::time::{Duration, SystemTime};

    let now = SystemTime::now();
    let day = Duration::from_secs(86_400);

    let mut recent_refs: Vec<String> = Vec::new();
    if cfg.fetch_recent_refs_days > 0 {
        let since = now - day * cfg.fetch_recent_refs_days as u32;
        // Only remote refs of the remote named on the command line, if
        // any. Upstream uses the default remote when none is named.
        let only_remote = if cfg.fetch_recent_refs_include_remotes {
            remote
        } else {
            None
        };
        let refs = git_lfs_git::refs::recent_branches(
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

    // Scan the named refs' trees too: when the caller read the index
    // instead, it finds nothing without a `.gitattributes`, as in the
    // t-fetch-recent.sh fixtures.
    let mut all_anchors: Vec<&str> = named_refs.iter().map(String::as_str).collect();
    for r in &recent_refs {
        if !all_anchors.contains(&r.as_str()) {
            all_anchors.push(r.as_str());
        }
    }
    for r in &all_anchors {
        for entry in git_lfs_git::scanner::scan_tree(cwd, r)? {
            if have_oids.insert(entry.oid) {
                pointers.push(entry);
            }
        }
    }

    // The window ends at each ref's tip commit, not at now.
    if cfg.fetch_recent_commits_days > 0 {
        for r in &all_anchors {
            let Some(tip_unix) = ref_tip_unix(cwd, r) else {
                continue;
            };
            let commits_since = SystemTime::UNIX_EPOCH + Duration::from_secs(tip_unix as u64)
                - day * cfg.fetch_recent_commits_days as u32;
            for entry in git_lfs_git::scanner::scan_previous_versions(cwd, r, commits_since)? {
                if have_oids.insert(entry.oid) {
                    pointers.push(entry);
                }
            }
        }
    }

    Ok(())
}

/// The committer time of `reference`'s tip as a Unix timestamp, or `None`
/// if it doesn't resolve.
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
    // Remote-tracking refs too, so objects only on a branch deleted
    // locally are fetched, as t-fetch-all.sh checks. Upstream runs
    // `git rev-list --all`.
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
