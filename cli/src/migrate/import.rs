//! `git lfs migrate import` — rewrite history so matching files become
//! LFS pointers.
//!
//! Two modes:
//!
//! - **Default (rewrite mode):** spawn `git fast-export --full-tree` →
//!   stream through [`super::transform::Transform`] → pipe to
//!   `git fast-import --force`. After import, force-checkout HEAD so
//!   the working tree reflects the rewritten history.
//!
//! - **`--no-rewrite`:** keep history intact, just clean specified
//!   working-tree files into pointers and add one new commit on top of
//!   HEAD.

use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_pointer::{Oid, Pointer};
use git_lfs_store::Store;
use sha2::{Digest, Sha256};

use super::pipeline::{
    print_pre_migrate_refs, refresh_working_tree, run_pipeline, working_tree_dirty,
};
use super::transform::{Mode, Stats};
use super::{MigrateError, RefSelection, build_globset, head_exists, resolve_refs};

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
    pub skip_fetch: bool,
    pub object_map: Option<std::path::PathBuf>,
    pub verbose: bool,
    pub remote: Option<String>,
    /// `--yes`: bypass the dirty-working-tree refusal. Upstream uses
    /// it to skip an interactive "rewrite anyway?" prompt; we don't
    /// prompt, so the flag just disables the guard.
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
    // --above is a size-only filter; mixing it with the path-based
    // include/exclude patterns or the per-commit fixup walk doesn't
    // cleanly compose, so upstream rejects the combo outright.
    if opts.above > 0
        && (!opts.include.is_empty() || !opts.exclude.is_empty() || opts.fixup)
    {
        return Err(MigrateError::Other(
            "Cannot use --above with --include, --exclude, --fixup".into(),
        ));
    }
    // --everything walks every local ref; combining it with explicit
    // ref selectors is contradictory.
    if opts.everything && (!opts.include_ref.is_empty() || !opts.exclude_ref.is_empty()) {
        return Err(MigrateError::Usage(
            "Cannot use --everything with --include-ref or --exclude-ref".into(),
        ));
    }

    // `--no-rewrite` mutates the working tree by design (it converts
    // tracked paths to pointer files, then commits). The dirty-tree
    // guard is for the history-rewriting modes only.
    if opts.no_rewrite {
        return import_no_rewrite(cwd, opts);
    }

    // `--fixup` runs before the dirty check too — its symlink case
    // (`.gitattributes` committed as 120000) confuses git status into
    // reporting modifications, and we want the friendlier
    // symbolic-link error in front.
    if opts.fixup {
        return import_fixup(cwd, opts);
    }

    if !opts.yes && working_tree_dirty(cwd)? {
        return Err(MigrateError::Other(
            "working tree has uncommitted changes; commit or stash first".into(),
        ));
    }

    if let Some(remote) = opts.remote.as_deref() {
        if !super::export::remote_exists(cwd, remote) {
            return Err(MigrateError::Other(format!(
                "Invalid remote {remote} provided"
            )));
        }
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
    // Default behavior matches upstream: unless the user asked for
    // `--everything` or named refs explicitly via `--include-ref` /
    // `--exclude-ref`, exclude remote-tracking refs from the walk so
    // commits already pushed to `origin` aren't rewritten locally.
    // Applies even when a positional branch is given — t-migrate-
    // import's "given branch, exclude remote refs" tests this.
    if !opts.everything && opts.include_ref.is_empty() && opts.exclude_ref.is_empty() {
        for r in super::export::list_remote_tracking_refs(cwd) {
            if !exclude_refs.iter().any(|x| x == &r) {
                exclude_refs.push(r);
            }
        }
    }
    if include_refs.is_empty() {
        return Err(MigrateError::Other(
            "no resolvable refs to migrate (empty repo?)".into(),
        ));
    }

    print_pre_migrate_refs(cwd, &include_refs);

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let include = build_globset(&opts.include)?;
    let exclude = build_globset(&opts.exclude)?;

    let marks_tmp = tempfile::NamedTempFile::new().map_err(MigrateError::Io)?;

    // When the user supplied `--include`, use those patterns
    // verbatim (with space escaping) as the `.gitattributes` lines —
    // otherwise we'd lose information (e.g. `--include "a file.txt"`
    // collapses to `*.txt` when derived from extension, breaking
    // t-migrate-import's --include-with-space test).
    //
    // Each `--exclude` pattern earns a non-LFS marker line so a more
    // permissive include can't drag the excluded path back into LFS
    // later. Upstream uses the `-filter -merge -diff` form.
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

    // If the user supplied `--include`, those patterns are already in
    // `attrs_add_initial`; the per-path derivation would duplicate or
    // re-derive them with extension semantics that lose the user's
    // wording.
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

    let oid_map = super::export::read_oid_map(marks_tmp.path(), &stats.commit_marks)
        .unwrap_or_default();
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

/// Escape spaces in a `.gitattributes` pattern. Git attribute files
/// treat the first space as the boundary between pattern and
/// attributes, so a literal space in the path has to be expressed as
/// `[[:space:]]`. Glob metacharacters (`*`, `?`, `[`) are *not*
/// escaped — user-supplied patterns like `*.md` are meant as globs
/// and we honor that intent.
pub(crate) fn escape_attr_pattern(p: &str) -> String {
    p.replace(' ', "[[:space:]]")
}

/// Escape every character in `p` that's special to git's attribute
/// matcher, so the resulting `.gitattributes` line matches exactly
/// the path passed in. Used by the `--above` per-path derivation
/// where the user committed a literal name like `test * special.bin`
/// and we don't want our written attrs to glob-expand it.
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

/// Walk history and convert any plain blob whose path the commit's
/// `.gitattributes` declared LFS-tracked. Used to repair repos where
/// someone committed real bytes for a path that was supposed to be a
/// pointer (e.g. `git lfs uninstall` was active during the commit).
///
/// Output is intentionally quiet on the no-op path: t-migrate-fixup's
/// "no potential fixup" tests assert `0 == wc -l` on stdout. We only
/// print after a conversion succeeds.
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

    // Reject `.gitattributes` symlinks (mode 120000) up-front. We check
    // HEAD only; nested commits with symlink attrs are rare in
    // practice and the upstream fixture targets HEAD.
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
        // No-op fixup — refs unchanged because fast-import re-emits
        // the same commits with identical trees and gets back the same
        // SHAs. Don't print anything; the `no potential fixup` tests
        // assert empty output.
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
    // `--no-rewrite` doesn't add new tracking lines — it requires the
    // working tree to already declare each path as LFS in some
    // `.gitattributes`. Mismatches surface upstream-shaped errors that
    // the test suite greps verbatim.
    let attrs = git_lfs_git::AttrSet::from_workdir(&repo_root)
        .map_err(MigrateError::Io)?;
    let listing = git_lfs_git::attr::list_lfs_patterns(&repo_root)
        .map_err(MigrateError::Io)?;
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

        // The error message + the gix-attributes lookup both want a
        // workdir-relative, forward-slash path.
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
        // Either nothing matched or every file was already a pointer.
        // Either way the user already has the state they wanted.
        return Ok(stats);
    }

    // `-m ""` is meaningful: an empty message preserved verbatim. Only
    // fall back to the autogenerated default when the user didn't pass
    // `-m` at all.
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

    // If `git add` left the index unchanged (e.g. the files were
    // already cleaned into pointers earlier — happens when
    // `.gitattributes` was added after the original commit and the
    // staging-time clean filter normalized the blobs), there's
    // nothing to commit. The desired state is already in HEAD;
    // returning success is correct.
    let diff_status = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .map_err(MigrateError::Io)?;
    if diff_status.success() {
        return Ok(stats);
    }

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
