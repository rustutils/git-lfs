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

use std::io::{self, Write};
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
}

pub fn import(cwd: &Path, opts: &ImportOptions) -> Result<Stats, MigrateError> {
    if working_tree_dirty(cwd)? {
        return Err(MigrateError::Other(
            "working tree has uncommitted changes; commit or stash first".into(),
        ));
    }

    if opts.no_rewrite {
        return import_no_rewrite(cwd, opts);
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

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let mut stats = Stats::default();

    // Convert each working-tree file in place, capturing its OID for
    // a follow-up `git add`. Patterns get appended to .gitattributes.
    let repo_root = repo_root(cwd)?;
    let mut new_patterns: Vec<String> = Vec::new();
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
        let bytes = std::fs::read(&abs)?;
        if Pointer::parse(&bytes).is_ok() {
            // Already a pointer; nothing to do.
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

        // Add a pattern based on the file's extension.
        let rel = abs
            .strip_prefix(&repo_root)
            .map_err(|_| MigrateError::Other(format!("path outside repo: {raw}")))?;
        let leaf = rel.file_name().and_then(|o| o.to_str()).unwrap_or_default();
        if let Some(idx) = leaf.rfind('.')
            && idx > 0
            && idx < leaf.len() - 1
        {
            new_patterns.push(format!(
                "*{} filter=lfs diff=lfs merge=lfs -text",
                &leaf[idx..]
            ));
        }
    }

    if stats.blobs_converted == 0 {
        println!("Nothing to convert.");
        return Ok(stats);
    }

    update_gitattributes(&repo_root, &new_patterns)?;

    let message = opts
        .message
        .clone()
        .unwrap_or_else(|| format!("{}: convert to Git LFS", opts.paths.join(",")));

    // Stage everything we touched + .gitattributes; commit.
    let mut add = Command::new("git");
    add.arg("-C").arg(cwd).arg("add");
    for p in &opts.paths {
        add.arg(p);
    }
    add.arg(".gitattributes");
    let status = add.status().map_err(MigrateError::Io)?;
    if !status.success() {
        return Err(MigrateError::Other("git add failed".into()));
    }

    let commit_status = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["commit", "-q", "-m", &message])
        .status()
        .map_err(MigrateError::Io)?;
    if !commit_status.success() {
        return Err(MigrateError::Other("git commit failed".into()));
    }

    println!(
        "Converted {} file(s) ({}).",
        stats.blobs_converted,
        super::humanize(stats.bytes_converted),
    );
    Ok(stats)
}

fn update_gitattributes(repo_root: &Path, new_patterns: &[String]) -> io::Result<()> {
    let path = repo_root.join(".gitattributes");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut have: std::collections::HashSet<String> =
        existing.lines().map(|l| l.trim().to_owned()).collect();
    let mut buf = existing.clone();
    if !buf.is_empty() && !buf.ends_with('\n') {
        buf.push('\n');
    }
    for p in new_patterns {
        if have.insert(p.clone()) {
            buf.push_str(p);
            buf.push('\n');
        }
    }
    let mut f = std::fs::File::create(&path)?;
    f.write_all(buf.as_bytes())?;
    Ok(())
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
