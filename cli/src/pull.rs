//! `git lfs pull`: fetch, then replace pointer files in the working tree
//! with their content.
//!
//! The files come from the index, so files deleted from the working tree
//! are restored, like upstream. A file is only written if it's missing or
//! holds the pointer for the same object. We write the files ourselves
//! because `git checkout` sees a pointer file that matches the index as
//! unchanged and wouldn't run the smudge filter.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use git_lfs_filter::{SmudgeError, smudge_object_to};
use git_lfs_git::scanner::scan_index_lfs;
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

/// Run `git lfs pull` with the given arguments and include and exclude
/// patterns.
pub fn pull_with_filter(
    cwd: &Path,
    args: &[String],
    include: &[String],
    exclude: &[String],
) -> Result<(), PullCommandError> {
    let opts = fetch::FetchOptions {
        args,
        stdin_lines: &[],
        dry_run: false,
        json: false,
        all: false,
        refetch: false,
        stdin: false,
        prune: false,
        recent: false,
        include,
        exclude,
    };
    let outcome = fetch::fetch(cwd, &opts)?;
    if !outcome.report.failed.is_empty() {
        return Err(PullCommandError::FetchFailures(outcome.report.failed.len()));
    }

    // Like upstream's `newSingleCheckout`, which checks
    // `filter.lfs.clean`. The objects are already fetched.
    if !smudge_filter_installed(cwd) {
        println!(
            "Skipping object checkout, Git LFS is not installed for this repository.\n\
             Consider installing it with 'git lfs install'."
        );
        return Ok(());
    }

    if is_bare_repo(cwd) {
        return Ok(());
    }

    // Filter like fetch did, so an excluded file isn't written even if
    // its object happens to be in the store.
    let include_set = fetch::build_pattern_set(cwd, include, "lfs.fetchinclude")?;
    let exclude_set = fetch::build_pattern_set(cwd, exclude, "lfs.fetchexclude")?;

    let mut store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    if let Some(v) = crate::shared_repo_config(cwd) {
        store = store.with_shared_repository(&v);
    }
    let repo_root = repo_root(cwd)?;
    let smudge_extensions = collect_smudge_extensions(cwd);
    // The index rather than HEAD's tree, so files outside a sparse
    // checkout are left alone.
    let pointers = scan_index_lfs(&repo_root)?;
    let mut rewritten_paths: Vec<String> = Vec::new();
    for p in &pointers {
        // An empty file is its own pointer. Rewriting it would change
        // its mtime, which t-pull.sh checks.
        if p.size == 0 {
            continue;
        }
        for rel in &p.paths {
            if !fetch::path_passes_filter(Some(rel), &include_set, &exclude_set) {
                continue;
            }
            let rel_str = rel.to_string_lossy();
            let dst = repo_root.join(rel);

            // Don't write through a symlink or a file in the path, like
            // upstream.
            if let Some(rel_parent) = rel.parent()
                && !rel_parent.as_os_str().is_empty()
                && let Err(msg) = check_safe_parent(&repo_root, rel_parent)
            {
                println!("{rel_str:?}: {msg}");
                continue;
            }

            // Like upstream's `singleCheckout.Run`, only replace a
            // pointer for the same object, never a symlink or other
            // content. Keep the file's permissions, so a read-only
            // pointer stays read-only, as t-pull.sh checks.
            let mut preserved_perms: Option<std::fs::Permissions> = None;
            match std::fs::symlink_metadata(&dst) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    println!("{rel_str:?}: not a regular file");
                    continue;
                }
                Ok(meta) if meta.is_file() => {
                    preserved_perms = Some(meta.permissions());
                    let bytes = std::fs::read(&dst)?;
                    match Pointer::parse(&bytes) {
                        Ok(existing) if existing.oid == p.oid => {}
                        Ok(_) | Err(_) => continue,
                    }
                }
                Ok(_) => {
                    println!("{rel_str:?}: not a regular file");
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }

            if !store.contains_with_size(p.oid, p.size) {
                continue;
            }

            if let Some(parent) = dst.parent()
                && let Err(_e) = std::fs::create_dir_all(parent)
            {
                println!("{rel_str:?}: not a directory");
                continue;
            }
            // Removing first means a read-only file can be replaced.
            match std::fs::remove_file(&dst) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let mut out = match std::fs::File::create(&dst) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    // A read-only directory. t-pull.sh checks for these
                    // messages and that the pull goes on.
                    println!("could not check out {rel_str:?}");
                    println!("could not create working directory file");
                    println!("permission denied");
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            // Extensions run from the repository root, even when pull
            // runs in a subdirectory.
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
                let _ = std::fs::set_permissions(&dst, perms);
            }
            rewritten_paths.push(rel.to_string_lossy().into_owned());
        }
    }
    if !rewritten_paths.is_empty() {
        // The index has stale stat data for the rewritten files, so Git
        // would show them as modified until it refreshes.
        refresh_index(&repo_root, &rewritten_paths)?;
    }
    Ok(())
}

/// Check that each existing component of `rel_parent` is a real
/// directory.
///
/// The error is the message to print after the path.
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
    // `update-index --refresh` fails when a path is still modified,
    // which isn't an error here.
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
