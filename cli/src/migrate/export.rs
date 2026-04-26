//! `git lfs migrate export` — rewrite history so LFS pointers become
//! the raw bytes they reference. Inverse of `import`.
//!
//! Pipeline is identical: `git fast-export --full-tree | transform |
//! git fast-import --force`. Only the [`super::transform::Mode`]
//! changes — `Mode::Export` parses each blob as a pointer, looks up
//! its content from the local LFS store, and replaces the blob bytes
//! with that content. `.gitattributes` is updated to *un*track the
//! affected patterns.
//!
//! For v0 we require `--include` (matching upstream) — there's no
//! useful "export everything" mode since every export needs an
//! explicit set of paths to convert back. Objects must already be in
//! the local LFS store; missing objects pass through unchanged so the
//! user can `git lfs fetch` and re-run.

use std::path::Path;

use git_lfs_store::Store;

use super::pipeline::{
    print_pre_migrate_refs, refresh_working_tree, run_pipeline, working_tree_dirty,
};
use super::transform::{Mode, Stats};
use super::{MigrateError, RefSelection, build_globset, resolve_refs};

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub branches: Vec<String>,
    pub everything: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

pub fn export(cwd: &Path, opts: &ExportOptions) -> Result<Stats, MigrateError> {
    if working_tree_dirty(cwd)? {
        return Err(MigrateError::Other(
            "working tree has uncommitted changes; commit or stash first".into(),
        ));
    }

    if opts.include.is_empty() {
        return Err(MigrateError::Other(
            "export requires --include to constrain which paths get unconverted".into(),
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
            // --above doesn't apply to export — pointer files are
            // tiny by definition; the size that matters is the
            // pointer's *recorded* size (the LFS object size), and
            // export converts based on path, not size.
            above: 0,
        },
        Mode::Export,
        &store,
    )?;

    refresh_working_tree(cwd)?;

    println!(
        "Expanded {} pointer(s) ({}). Untracked {} pattern(s).",
        stats.blobs_converted,
        super::humanize(stats.bytes_converted),
        stats.patterns.len(),
    );
    Ok(stats)
}
