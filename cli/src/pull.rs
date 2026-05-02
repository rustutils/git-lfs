//! `git lfs pull [<ref>...]` — `fetch` + materialize LFS files in the
//! working tree.
//!
//! After `fetch` populates the store, walk HEAD's tree to find every
//! tracked LFS pointer and rewrite the working-tree file with its
//! content from the store. Walking the tree (rather than `git ls-files`)
//! handles the "user `rm`'d the file" case — `git lfs pull` should
//! restore deleted tracked files from the store, matching upstream.
//!
//! Doing the rewrite ourselves (rather than `git checkout HEAD -- .`)
//! is deliberate: `git checkout` skips files it considers "unchanged"
//! relative to the index — and a pointer text that's also what's in
//! the index counts as unchanged. We'd never re-trigger the smudge
//! filter that way.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use git_lfs_filter::{SmudgeError, smudge_object_to};
use git_lfs_git::scan_index_lfs;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;

use crate::collect_smudge_extensions;
use crate::fetch::{self, FetchCommandError};

#[derive(Debug, thiserror::Error)]
pub enum PullCommandError {
    #[error(transparent)]
    Fetch(#[from] FetchCommandError),
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("git ls-files failed: {0}")]
    LsFiles(String),
    #[error("partial pull: {0} object(s) failed to fetch — working tree not updated")]
    FetchFailures(usize),
    #[error("smudge: {0}")]
    Smudge(#[from] SmudgeError),
}

pub fn pull_with_filter(
    cwd: &Path,
    refs: &[String],
    include: &[String],
    exclude: &[String],
) -> Result<(), PullCommandError> {
    let opts = fetch::FetchOptions {
        args: refs,
        stdin_lines: &[],
        dry_run: false,
        json: false,
        all: false,
        refetch: false,
        stdin: false,
        prune: false,
        include,
        exclude,
    };
    let outcome = fetch::fetch(cwd, &opts)?;
    if !outcome.report.failed.is_empty() {
        return Err(PullCommandError::FetchFailures(outcome.report.failed.len()));
    }

    // Match upstream `newSingleCheckout`: if the smudge filter isn't
    // installed (no `filter.lfs.clean` config), skip the working-
    // tree materialize step and tell the user how to fix it. The
    // fetch above still ran, so objects land in `.git/lfs/objects/`
    // and `git lfs install` later will smudge them in.
    if !smudge_filter_installed(cwd) {
        println!(
            "Skipping object checkout, Git LFS is not installed for this repository.\n\
             Consider installing it with 'git lfs install'."
        );
        return Ok(());
    }

    // Bare repos have no working tree, so the materialize phase is a
    // no-op. Fetch already ran above; we're done.
    if is_bare_repo(cwd) {
        return Ok(());
    }

    // Build the same include/exclude filter `fetch` used so the
    // working-tree rewrite respects -I / -X (or `lfs.fetchinclude` /
    // `lfs.fetchexclude`). Without this an LFS object that fetch
    // skipped would still be rewritten in-place if it happened to be
    // present locally already.
    let include_set = fetch::build_pattern_set(cwd, include, "lfs.fetchinclude")?;
    let exclude_set = fetch::build_pattern_set(cwd, exclude, "lfs.fetchexclude")?;

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    let repo_root = repo_root(cwd)?;
    // Pointer extensions (case-inverter, encryption shims, etc.) need
    // to run on the way out of the store. Resolve once before the
    // walk; non-extension files take the direct-copy fast path inside
    // the loop.
    let smudge_extensions = collect_smudge_extensions(cwd);
    // Walk via the index (`git ls-files :(attr:filter=lfs)`) instead
    // of HEAD's tree: respects sparse-checkout (don't materialize
    // out-of-cone files) and reads what's *staged*, matching what a
    // checkout would actually smudge.
    let pointers = scan_index_lfs(&repo_root)?;
    let mut rewritten_paths: Vec<String> = Vec::new();
    for p in &pointers {
        // Empty pointers (size 0) come from genuinely empty files in
        // the index — git stores those under the empty-blob hash, and
        // `Pointer::parse` of empty bytes is Ok(empty()). There's
        // nothing to materialize; touching the working-tree file
        // would needlessly bump mtime (test 17).
        if p.size == 0 {
            continue;
        }
        // Iterate every working-tree path the same LFS object lives
        // at — `scan_index_lfs` dedups by LFS OID, but a deduped
        // entry can have multiple paths (`dir1/dir.dat` and
        // `dir2/dir.dat` sharing the same LFS pointer text).
        for rel in &p.paths {
            if !fetch::path_passes_filter(Some(rel), &include_set, &exclude_set) {
                continue;
            }
            let rel_str = rel.to_string_lossy();
            let dst = repo_root.join(rel);

            // Walk the parent path components. If any is a regular
            // file or symlink, refuse to write through it — matches
            // upstream's "skip and warn" behavior on dir/file/symlink
            // conflicts.
            if let Some(rel_parent) = rel.parent()
                && !rel_parent.as_os_str().is_empty()
                && let Err(msg) = check_safe_parent(&repo_root, rel_parent)
            {
                println!("{rel_str:?}: {msg}");
                continue;
            }

            // Destination policy. Symlink at the destination is a
            // "not a regular file" warning (we won't overwrite a
            // symlink). For regular files, mirror checkout / upstream's
            // `singleCheckout.Run`: leave alone raw content or
            // different-OID pointers; materialize over our own
            // pointer. Capture permissions of the existing file so a
            // read-only pointer text → read-only smudged content
            // (test 16).
            let mut preserved_perms: Option<std::fs::Permissions> = None;
            match std::fs::symlink_metadata(&dst) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    println!("{rel_str:?}: not a regular file");
                    continue;
                }
                Ok(meta) if meta.is_file() => {
                    preserved_perms = Some(meta.permissions());
                    match std::fs::read(&dst) {
                        Ok(bytes) => match Pointer::parse(&bytes) {
                            Ok(existing) if existing.oid == p.oid => {}
                            Ok(_) => continue,
                            Err(_) => continue,
                        },
                        Err(e) => return Err(e.into()),
                    }
                }
                Ok(_) => {
                    // Some other file type (dir, fifo, …). Skip.
                    println!("{rel_str:?}: not a regular file");
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }

            if !store.contains_with_size(p.oid, p.size) {
                // Object missing locally (fetch failed or skipped).
                // Leave whatever is on disk alone.
                continue;
            }

            if let Some(parent) = dst.parent()
                && let Err(_e) = std::fs::create_dir_all(parent)
            {
                println!("{rel_str:?}: not a directory");
                continue;
            }
            // Unlink the existing file (if any) before recreating:
            // this works around a read-only existing file (we only
            // need write permission on the parent directory). Ignore
            // NotFound.
            match std::fs::remove_file(&dst) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let mut out = match std::fs::File::create(&dst) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    // Read-only parent directory (test 15): warn in
                    // the upstream-compatible format and keep going.
                    // Object is in the local store; user can chmod
                    // and rerun.
                    println!("could not check out {rel_str:?}");
                    println!("could not create working directory file");
                    println!("permission denied");
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            // Reconstruct the pointer from the index entry and route
            // through the smudge filter — for plain pointers this is
            // a direct store→file copy; pointers with extensions get
            // their chain run from the work-tree root (so case-
            // inverter / encryption shims find `.git/`, even when
            // pull was invoked from a subdirectory).
            let pointer = Pointer {
                oid: p.oid,
                size: p.size,
                extensions: p.extensions.clone(),
                canonical: true,
            };
            smudge_object_to(
                &store,
                &pointer,
                &mut out,
                &rel_str,
                &smudge_extensions,
                Some(&repo_root),
            )?;
            if let Some(perms) = preserved_perms {
                // Restore the original mode so a chmod-a-w pointer
                // remains read-only after we materialize.
                let _ = std::fs::set_permissions(&dst, perms);
            }
            rewritten_paths.push(rel.to_string_lossy().into_owned());
        }
    }
    if !rewritten_paths.is_empty() {
        // After overwriting working-tree files, the stat info in the
        // index is stale; `git diff-index HEAD` would report each as
        // modified even though `clean(content)` hashes back to the
        // original blob. `git update-index -q --refresh --stdin`
        // re-stats each path and runs the clean filter to confirm
        // the content blob matches; matching paths get fresh stat
        // info and drop out of subsequent diff-index walks.
        refresh_index(&repo_root, &rewritten_paths)?;
    }
    Ok(())
}

/// Walk the parent components of a repo-relative path. If any
/// component is a regular file, symlink, or some other non-directory,
/// return the upstream-formatted "not a directory" string so the
/// caller can emit `"path": not a directory` and skip. Stops at the
/// first non-existent component (we'd `create_dir_all` from there).
fn check_safe_parent(repo_root: &Path, rel_parent: &Path) -> Result<(), &'static str> {
    let mut current = repo_root.to_path_buf();
    for comp in rel_parent.components() {
        current.push(comp);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                let ft = meta.file_type();
                if ft.is_symlink() || !ft.is_dir() {
                    return Err("not a directory");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err("not a directory"),
        }
    }
    Ok(())
}

fn repo_root(cwd: &Path) -> Result<std::path::PathBuf, PullCommandError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !out.status.success() {
        return Err(PullCommandError::LsFiles(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok(std::path::PathBuf::from(s))
}

fn refresh_index(cwd: &Path, paths: &[String]) -> Result<(), PullCommandError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["update-index", "-q", "--refresh", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        for p in paths {
            stdin.write_all(p.as_bytes())?;
            stdin.write_all(b"\n")?;
        }
    }
    // Don't surface failures: `update-index --refresh` exits non-zero
    // when *some* path is still considered dirty (e.g. genuine local
    // edits we didn't rewrite), and treating that as a hard error
    // would break the legitimate "clean partial pull" case.
    let _ = child.wait()?;
    Ok(())
}

fn is_bare_repo(cwd: &Path) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--is-bare-repository"])
        .output();
    matches!(out, Ok(o) if o.status.success() && o.stdout.trim_ascii() == b"true")
}

fn smudge_filter_installed(cwd: &Path) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "filter.lfs.clean"])
        .output();
    matches!(out, Ok(o) if o.status.success() && !o.stdout.trim_ascii().is_empty())
}
