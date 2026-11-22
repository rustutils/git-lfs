//! `git lfs checkout`: replace pointer text in the working tree with the
//! object content from the local store.
//!
//! Pointers are found through the index, like `pull`, so files outside a
//! sparse checkout are left alone. Path arguments are gitignore-style
//! patterns relative to the current directory (see [`resolve_user_pattern`]).
//! Only files that are missing or still hold the same pointer are written.
//! Other content in the working tree is left as it is. Nothing is
//! downloaded: a file whose object isn't local gets the pointer text.
//!
//! With `--to` and one of `--base`, `--ours`, or `--theirs`, one merge stage
//! of a conflicted file is written to another path instead, downloading the
//! object if needed (see [`run_to_conflict`]).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git_lfs_filter::{SmudgeError, smudge_object_to};
use git_lfs_git::scanner::scan_index_lfs;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use globset::{Glob, GlobSetBuilder};

use crate::collect_smudge_extensions;
use crate::fetcher::LfsFetcher;

#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
    /// Run inside a bare repository, which has no working tree to write
    /// to.
    #[error("This operation must be run in a work tree.")]
    NotInWorkTree,
    /// Run outside any repository. t-checkout.sh checks this message.
    #[error("Not in a Git repository.")]
    NotInRepo,
    /// Invalid combination of `--to`, `--base`, `--ours`, and `--theirs`.
    ///
    /// `main.rs` prints the message to stderr and exits 2, like upstream.
    #[error("{0}")]
    Usage(String),
    #[error("smudge: {0}")]
    Smudge(#[from] SmudgeError),
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Patterns to filter pointers by. Empty means every pointer in the
    /// index.
    ///
    /// With `--to`, this must hold exactly one path: the conflicted file.
    pub paths: Vec<String>,
    /// `--to`: where to write the conflicted file, relative to `cwd`.
    pub to: Option<String>,
    /// `--ours`: merge stage 2.
    pub ours: bool,
    /// `--theirs`: merge stage 3.
    pub theirs: bool,
    /// `--base`: merge stage 1, the common ancestor.
    pub base: bool,
}

pub fn run(cwd: &Path, opts: &Options) -> Result<(), CheckoutError> {
    let stage = which_stage(opts)?;
    if opts.to.is_some() || stage.is_some() {
        return run_to_conflict(cwd, opts, stage);
    }

    if !is_in_git_repo(cwd) {
        return Err(CheckoutError::NotInRepo);
    }

    if is_bare_repo(cwd) {
        return Err(CheckoutError::NotInWorkTree);
    }

    // Upstream prints the same message when the filter isn't installed.
    if !smudge_installed(cwd) {
        println!("Cannot checkout LFS objects, Git LFS is not installed.");
        return Ok(());
    }

    let mut store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    if let Some(v) = crate::shared_repo_config(cwd) {
        store = store.with_shared_repository(&v);
    }
    let repo_root = repo_root(cwd)?;
    let smudge_extensions = collect_smudge_extensions(cwd);

    // One entry per OID, possibly with several paths.
    let pointers = scan_index_lfs(&repo_root)?;

    let glob_set = if opts.paths.is_empty() {
        None
    } else {
        let mut builder = GlobSetBuilder::new();
        for pat in &opts.paths {
            let glob_pats = resolve_user_pattern(cwd, &repo_root, pat).ok_or_else(|| {
                CheckoutError::Other(format!("path is outside the repository: {pat}"))
            })?;
            for gp in glob_pats {
                let glob = Glob::new(&gp)
                    .map_err(|e| CheckoutError::Other(format!("invalid pattern {pat:?}: {e}")))?;
                builder.add(glob);
            }
        }
        Some(
            builder
                .build()
                .map_err(|e| CheckoutError::Other(format!("pattern set build failed: {e}")))?,
        )
    };

    // The shell tests grep the `filepathfilter:` trace lines.
    let trace = trace_enabled();
    let mut work: Vec<(&git_lfs_git::scanner::PointerEntry, &PathBuf)> = Vec::new();
    for p in &pointers {
        for rel in &p.paths {
            let s = rel.to_string_lossy();
            let accepted = match &glob_set {
                None => true,
                Some(set) => set.is_match(s.as_ref()),
            };
            if trace {
                let verb = if accepted { "accepting" } else { "rejecting" };
                eprintln!("filepathfilter: {verb} {s:?}");
            }
            if accepted {
                work.push((p, rel));
            }
        }
    }

    if work.is_empty() {
        println!("Nothing to checkout.");
        return Ok(());
    }

    let total = work.len();
    let mut materialized = 0usize;
    let mut materialized_bytes: u64 = 0;
    let mut refreshed_paths: Vec<String> = Vec::new();
    for (p, rel) in &work {
        // An empty file is its own pointer, so there's nothing to write.
        if p.size == 0 {
            continue;
        }
        let rel_str = rel.to_string_lossy();
        let dst = repo_root.join(rel);

        // Never write through a symlink or file in the parent path.
        if let Some(rel_parent) = rel.parent()
            && !rel_parent.as_os_str().is_empty()
            && let Err(msg) = check_safe_parent(&repo_root, rel_parent)
        {
            println!("{rel_str:?}: {msg}");
            continue;
        }

        // Like upstream's `singleCheckout.Run`, only overwrite a regular
        // file that holds this same pointer. Its permissions are kept, so
        // a read-only pointer stays read-only.
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
            // Like upstream, write the pointer text instead.
            let pointer = Pointer::new(p.oid, p.size).encode();
            std::fs::write(&dst, pointer)?;
            eprintln!("git-lfs: {} (content not local)", rel.display());
            continue;
        }

        if let Some(parent) = dst.parent()
            && let Err(_e) = std::fs::create_dir_all(parent)
        {
            println!("{rel_str:?}: not a directory");
            continue;
        }
        // Removing first lets us replace a read-only file.
        match std::fs::remove_file(&dst) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut out = match std::fs::File::create(&dst) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // Read-only parent directory: warn and go on, like pull.
                println!("could not check out {rel_str:?}");
                println!("could not create working directory file");
                println!("permission denied");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
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
        drop(out);
        if let Some(perms) = preserved_perms {
            let _ = std::fs::set_permissions(&dst, perms);
        }
        materialized += 1;
        materialized_bytes += p.size;
        refreshed_paths.push(rel.to_string_lossy().into_owned());
    }

    // Refresh the index so the rewritten files don't show as modified.
    if !refreshed_paths.is_empty() {
        refresh_index(&repo_root, &refreshed_paths)?;
    }

    // The final line of upstream's progress meter.
    let percent = (materialized * 100).checked_div(total).unwrap_or(100);
    eprintln!(
        "Checking out LFS objects: {percent:>3}% ({materialized}/{total}), {}",
        crate::push::human_bytes(materialized_bytes)
    );
    Ok(())
}

/// Check that no component of `rel_parent` is a symlink or non-directory.
///
/// Returns the message to print otherwise. Components after the first
/// missing one aren't checked, since they'll be created.
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

fn refresh_index(cwd: &Path, paths: &[String]) -> std::io::Result<()> {
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
    // Refresh exits non-zero if any path is still dirty, which isn't a
    // checkout failure.
    let _ = child.wait()?;
    Ok(())
}

/// Whether `GIT_TRACE` is set to anything but empty, 0, false, no, or off.
fn trace_enabled() -> bool {
    match std::env::var_os("GIT_TRACE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy().trim().to_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
}

fn is_bare_repo(cwd: &Path) -> bool {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--is-bare-repository"])
        .output();
    matches!(out, Ok(o) if o.status.success() && o.stdout.starts_with(b"true"))
}

fn is_in_git_repo(cwd: &Path) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

fn smudge_installed(cwd: &Path) -> bool {
    matches!(
        git_lfs_git::config::get_effective(cwd, "filter.lfs.smudge"),
        Ok(Some(_))
    )
}

fn repo_root(cwd: &Path) -> Result<PathBuf, CheckoutError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !out.status.success() {
        return Err(CheckoutError::Other(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        return Err(CheckoutError::Other("not in a git repository".into()));
    }
    Ok(PathBuf::from(s))
}

/// Turn a path argument into repo-relative glob patterns.
///
/// The argument is relative to `cwd`, and may start with `./` or `../`.
/// `.` and `..` mean everything under that directory, a trailing `/` means
/// everything under it, and anything else matches a file or everything
/// under a directory of that name. Returns `None` if the path leaves the
/// repository.
fn resolve_user_pattern(cwd: &Path, repo_root: &Path, pat: &str) -> Option<Vec<String>> {
    let cwd_canon = cwd.canonicalize().ok()?;
    let root_canon = repo_root.canonicalize().ok()?;
    let cwd_rel = cwd_canon
        .strip_prefix(&root_canon)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");

    // Strip leading `./` and count `../`, so `../foo/**` from `subdir`
    // becomes `foo/**`.
    let mut remaining = pat;
    let mut pops = 0usize;
    loop {
        if let Some(rest) = remaining.strip_prefix("../") {
            pops += 1;
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix("./") {
            remaining = rest;
        } else if remaining == ".." {
            pops += 1;
            remaining = "";
            break;
        } else if remaining == "." {
            remaining = "";
            break;
        } else {
            break;
        }
    }

    let mut prefix_parts: Vec<&str> = if cwd_rel.is_empty() {
        Vec::new()
    } else {
        cwd_rel.split('/').collect()
    };
    if pops > prefix_parts.len() {
        return None;
    }
    for _ in 0..pops {
        prefix_parts.pop();
    }
    let prefix = prefix_parts.join("/");

    let dir_only = remaining.ends_with('/');
    let remaining = remaining.trim_end_matches('/');

    let combined = match (prefix.is_empty(), remaining.is_empty()) {
        (true, true) => "**".to_string(),
        (true, false) => remaining.to_string(),
        (false, true) => format!("{prefix}/**"),
        (false, false) => format!("{prefix}/{remaining}"),
    };
    if combined == "**" || combined.ends_with("/**") {
        return Some(vec![combined]);
    }
    if dir_only {
        return Some(vec![format!("{combined}/**")]);
    }
    let subtree = format!("{combined}/**");
    Some(vec![combined, subtree])
}

/// The merge stage selected by `--base`, `--ours`, or `--theirs`.
///
/// `None` if none is set. More than one is a usage error.
fn which_stage(opts: &Options) -> Result<Option<u8>, CheckoutError> {
    let mut stage = None;
    let mut count = 0u8;
    if opts.base {
        stage = Some(1);
        count += 1;
    }
    if opts.ours {
        stage = Some(2);
        count += 1;
    }
    if opts.theirs {
        stage = Some(3);
        count += 1;
    }
    if count > 1 {
        return Err(CheckoutError::Usage(
            "at most one of --base, --theirs, and --ours is allowed".into(),
        ));
    }
    Ok(stage)
}

/// Write one merge stage of a conflicted file to the `--to` path.
///
/// Reads the pointer at `:<stage>:<file>` in the index, downloads the
/// object if it isn't local, and writes the content to `--to`. Whatever is
/// at that path is removed first, so a symlink or hard link there is
/// replaced rather than written through.
fn run_to_conflict(cwd: &Path, opts: &Options, stage: Option<u8>) -> Result<(), CheckoutError> {
    let (Some(to), Some(stage)) = (opts.to.as_deref(), stage) else {
        return Err(CheckoutError::Usage(
            "--to and exactly one of --theirs, --ours, and --base must be used together".into(),
        ));
    };
    if opts.paths.len() != 1 {
        return Err(CheckoutError::Usage(
            "--to requires exactly one Git LFS object file path".into(),
        ));
    }
    let file_arg = &opts.paths[0];

    if !is_in_git_repo(cwd) {
        return Err(CheckoutError::NotInRepo);
    }
    if is_bare_repo(cwd) {
        return Err(CheckoutError::NotInWorkTree);
    }

    let to_path = if Path::new(to).is_absolute() {
        PathBuf::from(to)
    } else {
        cwd.join(to)
    };
    if let Some(parent) = to_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    // `:<stage>:<path>` takes a path from the repository root.
    let repo_root_for_path = repo_root(cwd)?;
    let lookup_path = repo_relative_path(cwd, &repo_root_for_path, file_arg);
    let ref_str = format!(":{stage}:{lookup_path}");
    let blob_oid = match resolve_index_blob(cwd, &ref_str) {
        Some(oid) => oid,
        None => {
            return Err(CheckoutError::Other(format!(
                "Could not checkout (are you not in the middle of a merge?): \
                 Git can't resolve ref: {ref_str:?}"
            )));
        }
    };

    let blob = read_blob(cwd, &blob_oid)?;
    let pointer = match Pointer::parse(&blob) {
        Ok(p) => p,
        Err(e) => {
            return Err(CheckoutError::Other(format!(
                "Could not find decoder pointer for object {blob_oid:?}: {e}"
            )));
        }
    };

    let mut store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    if let Some(v) = crate::shared_repo_config(cwd) {
        store = store.with_shared_repository(&v);
    }
    if !store.contains_with_size(pointer.oid, pointer.size) {
        let fetcher = LfsFetcher::from_repo(cwd, &store)?;
        fetcher.fetch(&pointer).map_err(|e| {
            CheckoutError::Other(format!(
                "Error checking out {} to {:?}: {e}",
                pointer.oid,
                to_path.display(),
            ))
        })?;
    }

    // Remove first so a read-only file is replaced, and a symlink or
    // hard link is replaced instead of written through.
    match std::fs::remove_file(&to_path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut out = std::fs::File::create(&to_path)?;
    // Extensions run from the repository root and get the repo-relative
    // path as `%f`, wherever checkout was run from.
    let smudge_extensions = collect_smudge_extensions(cwd);
    smudge_object_to(
        &store,
        &pointer,
        &mut out,
        &lookup_path,
        &smudge_extensions,
        Some(&repo_root_for_path),
    )?;
    Ok(())
}

/// Make a path argument relative to the repository root.
///
/// Relative paths are joined to `cwd` and `.` and `..` are resolved
/// lexically, like upstream's `NewCurrentToRepoPathConverter`. The root
/// itself becomes `.`, which t-checkout.sh expects in an error message. A
/// path outside the repository is returned unchanged.
fn repo_relative_path(cwd: &Path, repo_root: &Path, raw: &str) -> String {
    let absolute = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };
    let normalized = normalize_lexical(&absolute);
    match normalized.strip_prefix(repo_root) {
        Ok(rel) => {
            let s = rel.to_string_lossy();
            if s.is_empty() {
                ".".into()
            } else {
                s.into_owned()
            }
        }
        Err(_) => raw.to_owned(),
    }
}

/// Resolve `.` and `..` without touching the filesystem.
///
/// A `..` that would climb above the start of the path is kept.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Resolve `ref_str` to an object ID, or `None` if it doesn't resolve.
fn resolve_index_blob(cwd: &Path, ref_str: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", ref_str])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

/// Read a blob's content with `git cat-file -p`.
fn read_blob(cwd: &Path, oid: &str) -> Result<Vec<u8>, CheckoutError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["cat-file", "-p", oid])
        .output()?;
    if !out.status.success() {
        return Err(CheckoutError::Other(format!(
            "Could not read blob {oid}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}
