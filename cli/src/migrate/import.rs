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
    pub above: u64,
    pub no_rewrite: bool,
    pub message: Option<String>,
    pub paths: Vec<String>,
    pub fixup: bool,
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

    // `--no-rewrite` mutates the working tree by design (it converts
    // tracked paths to pointer files, then commits). The dirty-tree
    // guard is for the history-rewriting modes only.
    if opts.no_rewrite {
        return import_no_rewrite(cwd, opts);
    }

    if working_tree_dirty(cwd)? {
        return Err(MigrateError::Other(
            "working tree has uncommitted changes; commit or stash first".into(),
        ));
    }

    if opts.fixup {
        return Err(MigrateError::Other(
            "--fixup is not yet implemented in this Rust port; see NOTES.md".into(),
        ));
    }

    if opts.include.is_empty() && opts.above == 0 {
        return Err(MigrateError::Other(
            "rewrite mode requires --include or --above to constrain the set of files to convert"
                .into(),
        ));
    }

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

    // Tell the user the pre-migrate ref values so they can roll back
    // by hand if it goes wrong. We don't auto-backup — see NOTES.md.
    print_pre_migrate_refs(cwd, &include_refs);

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let include = build_globset(&opts.include)?;
    let exclude = build_globset(&opts.exclude)?;

    let stats = run_pipeline(
        cwd,
        &include_refs,
        &exclude_refs,
        super::transform::Options {
            include,
            exclude,
            above: opts.above,
            ..Default::default()
        },
        Mode::Import,
        &store,
    )?;

    // Refresh the working tree so the user sees the rewritten content.
    refresh_working_tree(cwd)?;

    println!(
        "Converted {} blob(s) ({}). Tracked {} pattern(s).",
        stats.blobs_converted,
        super::humanize(stats.bytes_converted),
        stats.patterns.len(),
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
