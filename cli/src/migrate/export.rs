//! `git lfs migrate export`: replace LFS pointers in history with their
//! objects.
//!
//! The inverse of import, through the same [`pipeline`](super::pipeline).
//! `--include` is required, as upstream. Each commit's `.gitattributes`
//! gets lines turning LFS off for the included patterns. Objects must be
//! in the local store: pointers whose object is missing stay as they
//! are. Afterwards, other local branches and tags that pointed at a
//! rewritten commit are moved, local objects no longer referenced are
//! deleted, and HEAD is force-checked out.
//!
//! Unlike import, this doesn't skip commits already on a remote.

use git_lfs_git::attr::PathPatterns;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use git_lfs_store::Store;

use super::pipeline::{
    ensure_working_copy_clean, print_pre_migrate_refs, refresh_working_tree,
    run_pipeline_with_export_marks,
};
use super::transform::{Mode, Stats};
use super::{MigrateError, RefSelection, build_patterns, escape_attr_pattern, resolve_refs};

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub branches: Vec<String>,
    pub everything: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub include_ref: Vec<String>,
    pub exclude_ref: Vec<String>,
    /// Ignored: remote-tracking refs are never fetched before the walk.
    #[allow(dead_code)]
    pub skip_fetch: bool,
    pub object_map: Option<PathBuf>,
    pub verbose: bool,
    pub remote: Option<String>,
    /// Rewrite a dirty working tree without asking.
    pub yes: bool,
}

pub fn export(cwd: &Path, opts: &ExportOptions) -> Result<Stats, MigrateError> {
    // Before the dirty check: t-migrate-export expects this error even
    // on a dirty tree.
    if opts.include.is_empty() {
        return Err(MigrateError::Other(
            "One or more files must be specified with --include".into(),
        ));
    }
    if let Some(remote) = opts.remote.as_deref()
        && !remote_exists(cwd, remote)
    {
        return Err(MigrateError::Other(format!(
            "Invalid remote {remote} provided"
        )));
    }

    // Before the dirty check, since Git reports a symlinked
    // `.gitattributes` as modified. Only HEAD is checked.
    if any_attrs_symlink(cwd, &["HEAD".to_owned()]) {
        return Err(MigrateError::Other(
            "expected '.gitattributes' to be a file, got a symbolic link".into(),
        ));
    }

    ensure_working_copy_clean(cwd, opts.yes)?;

    let sel = RefSelection {
        branches: opts.branches.clone(),
        everything: opts.everything,
    };
    let (mut include_refs, mut exclude_refs) = resolve_refs(cwd, &sel)?;
    for r in &opts.include_ref {
        if !include_refs.iter().any(|x| x == r) {
            include_refs.push(r.clone());
        }
    }
    for r in &opts.exclude_ref {
        if !exclude_refs.iter().any(|x| x == r) {
            exclude_refs.push(r.clone());
        }
    }
    super::validate_refs(cwd, &include_refs, &exclude_refs)?;
    super::exclude_remote_refs(
        cwd,
        opts.everything,
        (&opts.include_ref, &opts.exclude_ref),
        &mut exclude_refs,
    );
    if include_refs.is_empty() {
        return Err(MigrateError::Other(
            "no resolvable refs to migrate (empty repo?)".into(),
        ));
    }

    print_pre_migrate_refs(cwd, &include_refs);

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    let include = build_patterns(cwd, &opts.include);
    let exclude = build_patterns(cwd, &opts.exclude);
    download_missing(
        cwd,
        &store,
        (&include_refs, &exclude_refs),
        (&include, &exclude),
        opts.remote.as_deref(),
    )?;
    let (attrs_add_initial, attrs_remove_initial) =
        build_export_attrs(&opts.include, &opts.exclude);

    // The marks are needed to move other refs, not just for
    // `--object-map`.
    let marks_tmp = tempfile::NamedTempFile::new().map_err(MigrateError::Io)?;
    let marks_path: Option<&Path> = Some(marks_tmp.path());

    let stats = run_pipeline_with_export_marks(
        cwd,
        &include_refs,
        &exclude_refs,
        super::transform::Options {
            include,
            exclude,
            above: 0,
            attrs_add_initial,
            attrs_remove_initial,
            verbose: opts.verbose,
            skip_path_derived_attrs: false,
            ..Default::default()
        },
        Mode::Export,
        &store,
        marks_path,
    )?;

    let oid_map = read_oid_map(marks_tmp.path(), &stats.commit_marks).unwrap_or_default();

    // fast-import only updates the refs that were walked. Move the
    // others that pointed at a rewritten commit, as upstream does.
    if !oid_map.is_empty() {
        update_local_refs(cwd, &oid_map)?;
    }

    if let Some(out_path) = &opts.object_map {
        write_object_map_from(out_path, &oid_map, &stats.commit_marks).map_err(MigrateError::Io)?;
    }

    // Upstream prunes here too, and t-migrate-export checks that the
    // exported objects are gone.
    prune_unreferenced(cwd, &store)?;

    refresh_working_tree(cwd)?;

    println!(
        "Expanded {} pointer(s) ({}). Untracked {} pattern(s).",
        stats.blobs_converted,
        super::humanize(stats.bytes_converted),
        stats.patterns.len(),
    );
    Ok(stats)
}

/// Delete local objects that no unpushed commit references.
///
/// Keeps the objects of pointers reachable from a local ref but not
/// from a remote-tracking one. Errors deleting a file are ignored.
fn prune_unreferenced(cwd: &Path, store: &Store) -> Result<(), MigrateError> {
    let local = store.each_object().map_err(MigrateError::Io)?;
    if local.is_empty() {
        return Ok(());
    }
    let refs = super::all_local_refs(cwd)?;
    if refs.is_empty() {
        return Ok(());
    }
    let ref_args: Vec<&str> = refs.iter().map(String::as_str).collect();
    let remote_refs = super::list_remote_tracking_refs(cwd);
    let exclude_args: Vec<&str> = remote_refs.iter().map(String::as_str).collect();
    let entries = git_lfs_git::scanner::scan_pointers(cwd, &ref_args, &exclude_args)?;
    let retained: std::collections::HashSet<git_lfs_pointer::Oid> =
        entries.into_iter().map(|e| e.oid).collect();
    for (oid, _) in local {
        if !retained.contains(&oid) {
            let _ = std::fs::remove_file(store.object_path(oid));
        }
    }
    Ok(())
}

/// Whether `remote` is a configured remote.
pub(super) fn remote_exists(cwd: &Path, remote: &str) -> bool {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote", "get-url", remote])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// Whether the top-level `.gitattributes` is a symlink at the tip of
/// any of `refs`.
///
/// Earlier commits aren't checked. A ref that `git ls-tree` fails on
/// counts as no symlink.
pub(super) fn any_attrs_symlink(cwd: &Path, refs: &[String]) -> bool {
    for r in refs {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["ls-tree", r, "--", ".gitattributes"])
            .output();
        let Ok(out) = out else { continue };
        if !out.status.success() {
            continue;
        }
        let line = String::from_utf8_lossy(&out.stdout);
        if line.starts_with("120000") {
            return true;
        }
    }
    false
}

/// The `.gitattributes` lines to add and to remove for export.
///
/// Each `--include` pattern adds `<pat> !text !filter !merge !diff` and
/// removes `<pat> filter=lfs diff=lfs merge=lfs -text`. Each `--exclude`
/// pattern adds `<pat> filter=lfs diff=lfs merge=lfs -text`, so a broad
/// include doesn't untrack it. The patterns are escaped with
/// [`escape_attr_pattern`].
fn build_export_attrs(include: &[String], exclude: &[String]) -> (Vec<String>, Vec<String>) {
    let mut adds: Vec<String> = Vec::new();
    let mut removes: Vec<String> = Vec::new();
    for pat in include {
        let pat = escape_attr_pattern(pat);
        adds.push(format!("{pat} !text !filter !merge !diff"));
        removes.push(format!("{pat} filter=lfs diff=lfs merge=lfs -text"));
    }
    for pat in exclude {
        let pat = escape_attr_pattern(pat);
        adds.push(format!("{pat} filter=lfs diff=lfs merge=lfs -text"));
    }
    (adds, removes)
}

/// Map each rewritten commit's old SHA to its new one.
///
/// Pairs the `:<mark> <sha>` lines of fast-import's marks file with
/// `commit_marks`.
pub(super) fn read_oid_map(
    marks_path: &Path,
    commit_marks: &[(u32, String)],
) -> std::io::Result<HashMap<String, String>> {
    let raw = std::fs::read_to_string(marks_path)?;
    let mut mark_to_new: HashMap<u32, String> = HashMap::new();
    for line in raw.lines() {
        let Some(rest) = line.strip_prefix(':') else {
            continue;
        };
        let Some((mark, sha)) = rest.split_once(' ') else {
            continue;
        };
        let Ok(m) = mark.parse::<u32>() else {
            continue;
        };
        mark_to_new.insert(m, sha.trim().to_owned());
    }
    let mut map = HashMap::new();
    for (mark, old_oid) in commit_marks {
        if let Some(new_oid) = mark_to_new.get(mark) {
            map.insert(old_oid.clone(), new_oid.clone());
        }
    }
    Ok(map)
}

/// Write the `--object-map` file: an `old,new` line per rewritten
/// commit, in stream order.
pub(super) fn write_object_map_from(
    out_path: &Path,
    oid_map: &HashMap<String, String>,
    commit_marks: &[(u32, String)],
) -> std::io::Result<()> {
    let mut out = std::fs::File::create(out_path)?;
    for (_, old_oid) in commit_marks {
        if let Some(new_oid) = oid_map.get(old_oid) {
            writeln!(out, "{old_oid},{new_oid}")?;
        }
    }
    Ok(())
}

/// Move each branch and lightweight tag whose tip was rewritten to the
/// new commit.
///
/// Errors from `git update-ref` are ignored.
pub(super) fn update_local_refs(
    cwd: &Path,
    oid_map: &HashMap<String, String>,
) -> Result<(), MigrateError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads/",
            "refs/tags/",
        ])
        .output()
        .map_err(MigrateError::Io)?;
    if !out.status.success() {
        return Ok(());
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    for line in raw.lines() {
        let Some((sha, refname)) = line.split_once(' ') else {
            continue;
        };
        let Some(new_sha) = oid_map.get(sha) else {
            continue;
        };
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["update-ref", refname, new_sha, sha])
            .output();
    }
    Ok(())
}

/// Download the objects the export will expand that aren't local, as
/// upstream does before rewriting.
///
/// Those are the objects of every pointer in the selected history at a
/// path the patterns match. A repository whose remote has no LFS endpoint
/// skips this, and its objects must already be local.
fn download_missing(
    cwd: &Path,
    store: &Store,
    (include_refs, exclude_refs): (&[String], &[String]),
    (include, exclude): (&Option<PathPatterns>, &Option<PathPatterns>),
    remote: Option<&str>,
) -> Result<(), MigrateError> {
    use git_lfs_git::endpoint::Operation;

    if git_lfs_git::endpoint::resolve_endpoint_for(cwd, remote, Operation::Download).is_err() {
        return Ok(());
    }
    let inc: Vec<&str> = include_refs.iter().map(String::as_str).collect();
    let exc: Vec<&str> = exclude_refs.iter().map(String::as_str).collect();
    let pointers = git_lfs_git::scanner::scan_pointers(cwd, &inc, &exc)
        .map_err(|e| MigrateError::Other(e.to_string()))?;
    let specs: Vec<git_lfs_api::ObjectSpec> = pointers
        .into_iter()
        .filter(|p| {
            p.paths
                .iter()
                .any(|path| super::path_matches(&path.to_string_lossy(), include, exclude))
        })
        .filter(|p| !store.contains_with_size(p.oid, p.size))
        .map(|p| git_lfs_api::ObjectSpec {
            oid: p.oid.to_string(),
            size: p.size,
        })
        .collect();
    if specs.is_empty() {
        return Ok(());
    }

    let fetcher =
        crate::fetcher::LfsFetcher::from_repo_with_remote(cwd, store, remote, Operation::Download)
            .map_err(MigrateError::Io)?;
    let report = fetcher
        .download_many(specs)
        .map_err(|e| MigrateError::Other(e.to_string()))?;
    if let Some((oid, err)) = report.failed.first() {
        return Err(MigrateError::Other(format!("[{oid}] {err}")));
    }
    Ok(())
}
