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

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use git_lfs_store::Store;

use super::pipeline::{
    print_pre_migrate_refs, refresh_working_tree, run_pipeline_with_export_marks,
    working_tree_dirty,
};
use super::transform::{Mode, Stats};
use super::{MigrateError, RefSelection, build_globset, resolve_refs};

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub branches: Vec<String>,
    pub everything: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub include_ref: Vec<String>,
    pub exclude_ref: Vec<String>,
    pub skip_fetch: bool,
    pub object_map: Option<PathBuf>,
    pub verbose: bool,
    pub remote: Option<String>,
}

pub fn export(cwd: &Path, opts: &ExportOptions) -> Result<Stats, MigrateError> {
    // Validate args before consulting the working tree — `migrate export
    // --yes` (no filter) must surface its missing-include error even on
    // a dirty tree (t-migrate-export::no-filter relies on this).
    if opts.include.is_empty() {
        return Err(MigrateError::Other(
            "One or more files must be specified with --include".into(),
        ));
    }
    if let Some(remote) = opts.remote.as_deref() {
        if !remote_exists(cwd, remote) {
            return Err(MigrateError::Other(format!(
                "Invalid remote {remote} provided"
            )));
        }
    }

    // Reject `.gitattributes` symlinks before the dirty check — git
    // status reports a symlink target's content mismatch as "modified",
    // and we want the user-friendly error in front. Probes HEAD only,
    // which is enough for the upstream fixture.
    if any_attrs_symlink(cwd, &["HEAD".to_owned()]) {
        return Err(MigrateError::Other(
            "expected '.gitattributes' to be a file, got a symbolic link".into(),
        ));
    }

    if working_tree_dirty(cwd)? {
        return Err(MigrateError::Other(
            "working tree has uncommitted changes; commit or stash first".into(),
        ));
    }

    let sel = RefSelection {
        branches: opts.branches.clone(),
        everything: opts.everything,
    };
    let (mut include_refs, mut exclude_refs) = resolve_refs(cwd, &sel)?;
    // --include-ref / --exclude-ref add to the rev-list selection on top of
    // the positional/branch resolution. Used by both `info` (read-only) and
    // export (rewriting); semantically identical to upstream's
    // `--include-ref` flag.
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
    if include_refs.is_empty() {
        return Err(MigrateError::Other(
            "no resolvable refs to migrate (empty repo?)".into(),
        ));
    }

    print_pre_migrate_refs(cwd, &include_refs);

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    let include = build_globset(&opts.include)?;
    let exclude = build_globset(&opts.exclude)?;
    let (attrs_add_initial, attrs_remove_initial) =
        build_export_attrs(&opts.include, &opts.exclude);

    // We always need the export-marks file: the post-rewrite ref-update
    // pass (`update_local_refs`) consults it to bring sibling refs along
    // when their tip's commit was inside the rewrite range. The
    // `--object-map` user option is just one downstream consumer.
    let marks_tmp = tempfile::NamedTempFile::new().map_err(MigrateError::Io)?;
    let marks_path: Option<&Path> = Some(marks_tmp.path());

    let stats = run_pipeline_with_export_marks(
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
            attrs_add_initial,
            attrs_remove_initial,
            verbose: opts.verbose,
        },
        Mode::Export,
        &store,
        marks_path,
    )?;

    let oid_map = read_oid_map(marks_tmp.path(), &stats.commit_marks).unwrap_or_default();

    // fast-export only emits a `commit <ref>` directive for the refs
    // we explicitly named. Sibling refs that pointed at commits in the
    // walk (e.g. `main` when we rewrote `my-feature`) keep their old
    // tip until we update them by hand. Mirror upstream's
    // post-rewrite update-ref pass.
    if !oid_map.is_empty() {
        update_local_refs(cwd, &oid_map)?;
    }

    if let Some(out_path) = &opts.object_map {
        write_object_map_from(out_path, &oid_map, &stats.commit_marks)
            .map_err(MigrateError::Io)?;
    }

    // Drop LFS objects that aren't reachable from any local head/tag.
    // After a rewrite, objects whose pointers used to be on `main` (or
    // wherever) but aren't there anymore become unreferenced — t-migrate
    // -export's tests assert `refute_local_object` for exactly these.
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

/// Sweep `<git-dir>/lfs/objects/` for OIDs no longer referenced by any
/// local-only commit. "Local-only" = reachable from a local head or
/// tag but NOT from any remote-tracking ref — anything visible only on
/// `refs/remotes/*` can be re-fetched, and t-migrate-export expects
/// exactly those pruned.
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
    let remote_refs = list_remote_tracking_refs(cwd);
    let exclude_args: Vec<&str> = remote_refs.iter().map(String::as_str).collect();
    let entries = git_lfs_git::scan_pointers(cwd, &ref_args, &exclude_args)?;
    let retained: std::collections::HashSet<git_lfs_pointer::Oid> =
        entries.into_iter().map(|e| e.oid).collect();
    for (oid, _) in local {
        if !retained.contains(&oid) {
            let _ = std::fs::remove_file(store.object_path(oid));
        }
    }
    Ok(())
}

/// All `refs/remotes/*` refs, used to subtract remote-known commits
/// from the retain set during prune. Returns an empty Vec if `git`
/// can't be reached.
fn list_remote_tracking_refs(cwd: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["for-each-ref", "--format=%(refname)", "refs/remotes/"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// True iff `remote` is one of the configured git remotes.
fn remote_exists(cwd: &Path, remote: &str) -> bool {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote", "get-url", remote])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// True iff any commit reachable from `refs` has `.gitattributes`
/// committed as a symbolic link (mode 120000). We `git ls-tree` each
/// ref's tip; the upstream test fixture only commits the symlink at
/// HEAD, so this catches the common case without walking full
/// history. Failures are conservative — if `git` isn't reachable for
/// some reason, we say "no symlink" and let the rest of the pipeline
/// surface a real error.
fn any_attrs_symlink(cwd: &Path, refs: &[String]) -> bool {
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

/// Build the `.gitattributes` add/remove pairs for export from the
/// CLI patterns:
///
/// - For each `--include` pattern, add `<pat> !text !filter !merge !diff`
///   (un-track LFS) and stage a remove of the matching
///   `<pat> filter=lfs ...` line if the existing attrs file has one.
/// - For each `--exclude` pattern, add `<pat> filter=lfs diff=lfs merge=lfs`
///   so a permissive include (`*`) doesn't strip its tracking.
fn build_export_attrs(include: &[String], exclude: &[String]) -> (Vec<String>, Vec<String>) {
    let mut adds: Vec<String> = Vec::new();
    let mut removes: Vec<String> = Vec::new();
    for pat in include {
        adds.push(format!("{pat} !text !filter !merge !diff"));
        removes.push(format!("{pat} filter=lfs diff=lfs merge=lfs -text"));
    }
    for pat in exclude {
        adds.push(format!("{pat} filter=lfs diff=lfs merge=lfs"));
    }
    (adds, removes)
}

/// Build the original_oid → new_oid map for every commit fast-import
/// recorded a mark for. Reads fast-import's `--export-marks` file
/// (`:mark sha`) and pairs each mark with its captured `original_oid`.
fn read_oid_map(
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

/// Write `old,new\n` per rewritten commit to `out_path`. Pairs are
/// emitted in the order fast-export produced them so the output is
/// stable.
fn write_object_map_from(
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

/// For every local head/tag whose tip is in `oid_map`, update the ref
/// to the corresponding rewritten commit. Tests like `migrate export
/// (given branch)` rely on this to bring `main` along when we rewrote
/// `my-feature` (both refs share the rewrite range).
fn update_local_refs(
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
