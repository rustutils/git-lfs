//! `git lfs migrate import`: convert files in history to LFS pointers.
//!
//! By default this rewrites the selected refs through the
//! [`pipeline`](super::pipeline), skipping commits reachable from
//! remote-tracking refs, then moves any other local branch or tag that
//! pointed at a rewritten commit and force-checks out HEAD. `--fixup`
//! rewrites the same way but converts what each commit's own attributes
//! say should be LFS files.
//!
//! `--no-rewrite` leaves history alone: it converts the given working
//! tree files to pointers and commits them on top of HEAD.
//!
//! Every mode refuses to run with uncommitted changes unless `--yes` is
//! given.

use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_pointer::{Oid, Pointer};
use git_lfs_store::Store;
use sha2::{Digest, Sha256};

use super::pipeline::{ensure_working_copy_clean, print_pre_migrate_refs, refresh_working_tree};
use super::transform::{Mode, Stats};
use super::{MigrateError, RefSelection, build_patterns, head_exists, resolve_refs};

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub branches: Vec<String>,
    pub everything: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub include_ref: Vec<String>,
    pub exclude_ref: Vec<String>,
    pub above: u64,
    pub no_rewrite: bool,
    pub message: Option<String>,
    pub paths: Vec<String>,
    pub fixup: bool,
    /// Ignored: remote-tracking refs are never fetched before the walk.
    #[allow(dead_code)]
    pub skip_fetch: bool,
    pub object_map: Option<std::path::PathBuf>,
    pub verbose: bool,
    pub remote: Option<String>,
    /// Rewrite even if the working tree is dirty.
    ///
    /// Upstream asks for confirmation instead of refusing, and `--yes`
    /// answers yes.
    pub yes: bool,
}

pub fn import(cwd: &Path, opts: &ImportOptions) -> Result<Stats, MigrateError> {
    if opts.fixup && opts.no_rewrite {
        return Err(MigrateError::Other(
            "--no-rewrite and --fixup cannot be combined".into(),
        ));
    }
    if opts.fixup && (!opts.include.is_empty() || !opts.exclude.is_empty()) {
        return Err(MigrateError::Other(
            "Cannot use --fixup with --include, --exclude".into(),
        ));
    }
    if opts.above > 0 && (!opts.include.is_empty() || !opts.exclude.is_empty() || opts.fixup) {
        return Err(MigrateError::Other(
            "Cannot use --above with --include, --exclude, --fixup".into(),
        ));
    }
    if opts.everything && (!opts.include_ref.is_empty() || !opts.exclude_ref.is_empty()) {
        return Err(MigrateError::Usage(
            "Cannot use --everything with --include-ref or --exclude-ref".into(),
        ));
    }

    // Every mode checks first, as upstream does.
    ensure_working_copy_clean(cwd, opts.yes)?;

    if opts.no_rewrite {
        return import_no_rewrite(cwd, opts);
    }

    if opts.fixup {
        return import_fixup(cwd, opts);
    }

    if let Some(remote) = opts.remote.as_deref()
        && !super::export::remote_exists(cwd, remote)
    {
        return Err(MigrateError::Other(format!(
            "Invalid remote {remote} provided"
        )));
    }

    if super::export::any_attrs_symlink(cwd, &["HEAD".to_owned()]) {
        return Err(MigrateError::Other(
            "expected '.gitattributes' to be a file, got a symbolic link".into(),
        ));
    }

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

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let include = build_patterns(cwd, &opts.include);
    let exclude = build_patterns(cwd, &opts.exclude);

    let marks_tmp = tempfile::NamedTempFile::new().map_err(MigrateError::Io)?;

    // `--include` patterns become `.gitattributes` lines as given, with
    // spaces escaped. Deriving them from each path's extension would
    // turn `--include "a file.txt"` into `*.txt`. Each `--exclude`
    // pattern gets a line that turns LFS off, as upstream.
    let mut attrs_add_initial: Vec<String> = opts
        .include
        .iter()
        .map(|p| {
            format!(
                "{} filter=lfs diff=lfs merge=lfs -text",
                escape_attr_pattern(p)
            )
        })
        .collect();
    attrs_add_initial.extend(
        opts.exclude
            .iter()
            .map(|p| format!("{} !text -filter -merge -diff", escape_attr_pattern(p))),
    );

    let skip_path_derived_attrs = !opts.include.is_empty();

    let stats = super::pipeline::run_pipeline_with_export_marks(
        cwd,
        &include_refs,
        &exclude_refs,
        super::transform::Options {
            include,
            exclude,
            above: opts.above,
            verbose: opts.verbose,
            attrs_add_initial,
            skip_path_derived_attrs,
            ..Default::default()
        },
        Mode::Import,
        &store,
        Some(marks_tmp.path()),
    )?;

    let oid_map =
        super::export::read_oid_map(marks_tmp.path(), &stats.commit_marks).unwrap_or_default();
    if !oid_map.is_empty() {
        super::export::update_local_refs(cwd, &oid_map)?;
    }
    if let Some(out_path) = &opts.object_map {
        super::export::write_object_map_from(out_path, &oid_map, &stats.commit_marks)
            .map_err(MigrateError::Io)?;
    }

    refresh_working_tree(cwd)?;

    println!(
        "Converted {} blob(s) ({}). Tracked {} pattern(s).",
        stats.blobs_converted,
        super::humanize(stats.bytes_converted),
        stats.patterns.len(),
    );
    Ok(stats)
}

/// Escape the spaces in a `.gitattributes` pattern as `[[:space:]]`.
///
/// A space would otherwise end the pattern. Glob characters stay, since
/// the user's pattern is a glob.
pub(crate) fn escape_attr_pattern(p: &str) -> String {
    p.replace(' ', "[[:space:]]")
}

/// Escape a path for a `.gitattributes` pattern that matches only it.
///
/// Escapes spaces and the glob characters `*`, `?`, and `[`.
pub(crate) fn escape_attr_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for c in p.chars() {
        match c {
            ' ' => out.push_str("[[:space:]]"),
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            '[' => out.push_str("[[]"),
            other => out.push(other),
        }
    }
    out
}

// --------------------------------------------------------------------
// --fixup mode
// --------------------------------------------------------------------

/// `--fixup`: convert files that were committed without the LFS filter
/// although their commit's attributes track them.
///
/// Prints nothing when there's nothing to fix: t-migrate-fixup checks
/// that stdout is empty. Unlike the default mode, this doesn't skip
/// commits on a remote, ignores `--include-ref` and `--exclude-ref`,
/// and doesn't move other local refs.
fn import_fixup(cwd: &Path, opts: &ImportOptions) -> Result<Stats, MigrateError> {
    let sel = RefSelection {
        branches: opts.branches.clone(),
        everything: opts.everything,
    };
    let (include_refs, exclude_refs) = resolve_refs(cwd, &sel)?;
    if include_refs.is_empty() {
        return Err(MigrateError::Other(
            "no resolvable refs to migrate (empty repo?)".into(),
        ));
    }

    // Only the tips are checked for a symlinked `.gitattributes`.
    if super::export::any_attrs_symlink(cwd, &include_refs) {
        return Err(MigrateError::Other(
            "expected '.gitattributes' to be a file, got a symbolic link".into(),
        ));
    }

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let stats = super::pipeline::run_pipeline(
        cwd,
        &include_refs,
        &exclude_refs,
        super::transform::Options::default(),
        Mode::Fixup,
        &store,
    )?;

    if stats.blobs_converted == 0 {
        // fast-import recreated the same commits, so no ref moved.
        return Ok(stats);
    }

    refresh_working_tree(cwd)?;

    println!(
        "Fixed {} blob(s) ({}).",
        stats.blobs_converted,
        super::humanize(stats.bytes_converted),
    );
    Ok(stats)
}

// --------------------------------------------------------------------
// --no-rewrite mode
// --------------------------------------------------------------------

fn import_no_rewrite(cwd: &Path, opts: &ImportOptions) -> Result<Stats, MigrateError> {
    if opts.paths.is_empty() {
        return Err(MigrateError::Other(
            "--no-rewrite requires one or more paths".into(),
        ));
    }
    if !head_exists(cwd) {
        return Err(MigrateError::Other(
            "--no-rewrite requires an existing HEAD commit".into(),
        ));
    }

    let repo_root = repo_root(cwd)?;
    // Each path must already be tracked by the working tree's
    // attributes. The shell tests grep these errors.
    let attrs = git_lfs_git::AttrSet::from_workdir(&repo_root).map_err(MigrateError::Io)?;
    let listing = git_lfs_git::attr::list_lfs_patterns(&repo_root).map_err(MigrateError::Io)?;
    if listing.tracked().next().is_none() {
        return Err(MigrateError::Other(
            "No Git LFS filters found in '.gitattributes'".into(),
        ));
    }

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let mut stats = Stats::default();

    for raw in &opts.paths {
        let abs = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            cwd.join(raw)
        };
        if !abs.is_file() {
            return Err(MigrateError::Other(format!(
                "path is not a regular file: {raw}"
            )));
        }

        let rel = abs
            .strip_prefix(&repo_root)
            .map_err(|_| MigrateError::Other(format!("path outside repo: {raw}")))?;
        let rel_str = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");

        if !attrs.is_lfs_tracked(&rel_str) {
            return Err(MigrateError::Other(format!(
                "{raw} did not match any Git LFS filters in '.gitattributes'"
            )));
        }

        let bytes = std::fs::read(&abs)?;
        if Pointer::parse(&bytes).is_ok() {
            continue;
        }
        let size = bytes.len() as u64;
        let oid_bytes: [u8; 32] = Sha256::digest(&bytes).into();
        let oid = Oid::from_bytes(oid_bytes);
        store
            .insert_verified(oid, &mut bytes.as_slice())
            .map_err(|e| MigrateError::Other(format!("storing object: {e}")))?;
        let pointer_text = Pointer::new(oid, size).encode();
        std::fs::write(&abs, pointer_text.as_bytes())?;
        stats.blobs_converted += 1;
        stats.bytes_converted += size;
    }

    if stats.blobs_converted == 0 {
        // Every file was already a pointer.
        return Ok(stats);
    }

    // `-m ""` gives an empty message, not the default.
    let message = opts
        .message
        .clone()
        .unwrap_or_else(|| format!("{}: convert to Git LFS", opts.paths.join(",")));

    let mut add = Command::new("git");
    add.arg("-C").arg(cwd).arg("add");
    for p in &opts.paths {
        add.arg(p);
    }
    let status = add.status().map_err(MigrateError::Io)?;
    if !status.success() {
        return Err(MigrateError::Other("git add failed".into()));
    }

    // HEAD may already have the pointers, if the working tree files
    // were committed through the clean filter.
    let diff_status = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .map_err(MigrateError::Io)?;
    if diff_status.success() {
        return Ok(stats);
    }

    // This commits the whole index, including anything staged before.
    let commit_out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["commit", "-q", "--allow-empty-message", "-m", &message])
        .output()
        .map_err(MigrateError::Io)?;
    if !commit_out.status.success() {
        return Err(MigrateError::Other(format!(
            "git commit failed: {}",
            String::from_utf8_lossy(&commit_out.stderr).trim()
        )));
    }

    Ok(stats)
}

fn repo_root(cwd: &Path) -> Result<PathBuf, MigrateError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(MigrateError::Io)?;
    if !out.status.success() {
        return Err(MigrateError::Other(format!(
            "git rev-parse --show-toplevel failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    ))
}
