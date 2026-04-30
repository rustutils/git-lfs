//! `git lfs checkout [<path>...]` — replace pointer text in the working
//! tree with the actual LFS object content. Useful after a clone or
//! pull when the smudge filter wasn't configured (so files landed on
//! disk as pointer text instead of real bytes).
//!
//! No args → re-smudge every LFS pointer in HEAD's tree. With path
//! arguments → filter to those paths. Each pattern is matched against
//! the repo-relative path:
//! - exact match (e.g. `data/foo.bin`)
//! - trailing-slash prefix match (e.g. `data/` matches everything under
//!   `data/`)
//!
//! `--to <path> --ours|--theirs|--base <file>` switches into
//! conflict-resolution mode: read the staged pointer for the
//! conflicted file from one of the merge stages (1=base, 2=ours,
//! 3=theirs), fetch the LFS object, and write it to `<path>`. The
//! target may be outside the work tree; intermediate directories
//! are created on the fly. See [`run_to_conflict`].
//!
//! Out of scope (NOTES.md): glob/wildcard patterns for the regular
//! materialize path (shells handle the common case).

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git_lfs_git::scan_tree;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use globset::{Glob, GlobSetBuilder};

use crate::fetcher::LfsFetcher;

#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
    /// Command was run inside a bare repo. Checkout has no working
    /// tree to write to — surface upstream's exact wording.
    #[error("This operation must be run in a work tree.")]
    NotInWorkTree,
    /// Caller isn't inside any git repo at all. Distinct from
    /// `NotInWorkTree` (bare repo) — upstream prints a different
    /// message and the t-checkout outside-repo test greps for it.
    #[error("Not in a Git repository.")]
    NotInRepo,
    /// Argument validation failure for `--to`/`--ours`/`--theirs`/
    /// `--base`. The contained message is upstream-formatted and
    /// printed verbatim by main.rs to stderr; exit code 2.
    #[error("{0}")]
    Usage(String),
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Patterns to filter pointers by; empty means "all of HEAD".
    /// In conflict mode (`--to` set), this must contain exactly one
    /// path — the conflicted file to read from the index.
    pub paths: Vec<String>,
    /// Conflict-mode destination path. Resolves relative to cwd.
    pub to: Option<String>,
    /// `--ours` flag: stage 2 (HEAD's version of a conflict).
    pub ours: bool,
    /// `--theirs` flag: stage 3 (the merging-in version).
    pub theirs: bool,
    /// `--base` flag: stage 1 (the common ancestor).
    pub base: bool,
}

pub fn run(cwd: &Path, opts: &Options) -> Result<(), CheckoutError> {
    // Conflict-mode (`--to <path> --base|--ours|--theirs <file>`)
    // takes a different code path: read the requested stage from
    // the index, fetch the LFS object, write it to `--to`. Validate
    // the flag combination first — upstream's wording is part of
    // the t-checkout/conflicts test contract.
    let stage = which_stage(opts)?;
    if opts.to.is_some() || stage.is_some() {
        return run_to_conflict(cwd, opts, stage);
    }

    // Outside any git repo: upstream prints "Not in a Git repository."
    // and exits 128. Check before bare/smudge-installed because those
    // also need a repo to be meaningful.
    if !is_in_git_repo(cwd) {
        return Err(CheckoutError::NotInRepo);
    }

    // Bare repos have no working tree to materialize into. Surface
    // the upstream-compatible message and let the dispatcher emit
    // it on stdout (test 15).
    if is_bare_repo(cwd) {
        return Err(CheckoutError::NotInWorkTree);
    }

    // Safety net: if the smudge filter isn't installed, skip with a
    // friendly message. Otherwise we'd materialize content that the
    // next `git checkout` would clobber back to pointer text — surprising.
    if !smudge_installed(cwd) {
        println!("Cannot checkout LFS objects, Git LFS is not installed.");
        return Ok(());
    }

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let repo_root = repo_root(cwd)?;

    // Walk HEAD's tree for the set of LFS pointers, then drop any
    // whose path was removed from the index since (e.g. `git rm`).
    // Both walks run from the repo root so they return repo-relative
    // paths regardless of where the user invoked checkout. Upstream
    // consults `git diff-index HEAD <path>` per file and skips
    // entries reported as deleted; intersecting with the
    // `git ls-files` set is the equivalent in one shell-out.
    let pointers = scan_tree(&repo_root, "HEAD")?;
    let indexed = indexed_paths(&repo_root)?;
    let pointers: Vec<_> = pointers
        .into_iter()
        .filter(|p| match p.path.as_deref() {
            Some(path) => indexed.contains(path.to_string_lossy().as_ref()),
            None => true,
        })
        .collect();

    // Filter by path patterns if any were given. Each user pattern is
    // resolved to a repo-relative glob, then fed to a single globset
    // matcher. Supports `*` wildcards, trailing-slash directory
    // patterns, `.` (cwd subtree), `..` (cwd's parent subtree), and
    // exact relative paths. Emit `filepathfilter: accepting`/
    // `rejecting` trace lines under GIT_TRACE so the upstream test
    // suite's grep assertions still line up.
    let trace = trace_enabled();
    let pointers = if opts.paths.is_empty() {
        if trace {
            for p in &pointers {
                if let Some(path) = p.path.as_deref() {
                    eprintln!("filepathfilter: accepting {:?}", path.to_string_lossy());
                }
            }
        }
        pointers
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
        let set = builder
            .build()
            .map_err(|e| CheckoutError::Other(format!("pattern set build failed: {e}")))?;
        pointers
            .into_iter()
            .filter(|p| {
                let Some(path) = p.path.as_deref() else {
                    return false;
                };
                let s = path.to_string_lossy();
                let accepted = set.is_match(s.as_ref());
                if trace {
                    let verb = if accepted { "accepting" } else { "rejecting" };
                    eprintln!("filepathfilter: {verb} {s:?}");
                }
                accepted
            })
            .collect()
    };

    if pointers.is_empty() {
        println!("Nothing to checkout.");
        return Ok(());
    }

    // Materialize, but don't fetch. Upstream's checkout never
    // downloads — that's `git lfs fetch`'s job. If an object is
    // missing locally, we fall back to writing the pointer text
    // (handled below per-file). Skip pointers whose objects still aren't local
    // (download failed); they stay as pointer text.
    let total = pointers.len();
    let mut materialized = 0usize;
    let mut materialized_bytes: u64 = 0;
    let mut refreshed_paths: Vec<String> = Vec::new();
    for p in &pointers {
        let Some(rel) = &p.path else {
            continue;
        };
        // Empty pointers (size 0) come from genuinely empty files.
        // git stores those under the empty-blob hash, parses as
        // Pointer::empty(), and there's nothing to materialize —
        // touching the working-tree file just bumps mtime.
        if p.size == 0 {
            continue;
        }
        let rel_str = rel.to_string_lossy();
        let dst = repo_root.join(rel);

        // Walk the parent path components. A regular file or
        // symlink on the way to `dst` means we can't safely write
        // through it — emit the upstream-formatted warning and
        // skip. Same shape as pull's conflict handling.
        if let Some(rel_parent) = rel.parent()
            && !rel_parent.as_os_str().is_empty()
            && let Err(msg) = check_safe_parent(&repo_root, rel_parent)
        {
            println!("{rel_str:?}: {msg}");
            continue;
        }

        // Destination policy. Symlink at the destination is a "not a
        // regular file" warning; non-file (directory etc.) likewise.
        // For regular files, mirror upstream's `singleCheckout.Run`:
        // leave alone raw content or different-OID pointers, smudge
        // over our own pointer. Capture permissions so a read-only
        // pointer text → read-only smudged content.
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
                // Directory or some other non-regular file at dst
                // (e.g. test 6: `rm a.dat && mkdir a.dat`). Skip.
                println!("{rel_str:?}: not a regular file");
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        if !store.contains_with_size(p.oid, p.size) {
            // Object isn't in the local store. Upstream handles this
            // by re-emitting the pointer text — the file exists, but
            // its content is the pointer rather than the smudged
            // bytes. Matches `t-checkout.sh::checkout` "test checkout
            // with missing data doesn't fail".
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
        // Unlink before recreating to handle a read-only existing
        // file (we only need write permission on the parent dir).
        match std::fs::remove_file(&dst) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut src = store.open(p.oid)?;
        let mut out = match std::fs::File::create(&dst) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // Read-only parent directory (test 10): warn and
                // keep going, matching pull's behavior. Object is
                // already in the store; user can chmod and rerun.
                println!("could not check out {rel_str:?}");
                println!("could not create working directory file");
                println!("permission denied");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        std::io::copy(&mut src, &mut out)?;
        drop(out);
        if let Some(perms) = preserved_perms {
            // Restore the original mode so a read-only pointer
            // remains read-only after smudging (test 11).
            let _ = std::fs::set_permissions(&dst, perms);
        }
        materialized += 1;
        materialized_bytes += p.size;
        refreshed_paths.push(rel.to_string_lossy().into_owned());
    }

    // Refresh the index so `git diff-index HEAD` doesn't flag every
    // freshly-overwritten file as modified. Same fix as `pull` —
    // without this, `assert_clean_status` would fail in tests that
    // re-create deleted working-tree files via checkout. Run from
    // repo root so the repo-relative paths we hand it resolve
    // correctly, even when checkout was invoked from a subdir.
    if !refreshed_paths.is_empty() {
        refresh_index(&repo_root, &refreshed_paths)?;
    }

    // Final progress line — format mirrors upstream's `tq.Meter` output
    // for the checkout queue. Goes to stderr so it doesn't mix with
    // stdout that callers may pipe.
    let percent = if total == 0 {
        100
    } else {
        materialized * 100 / total
    };
    eprintln!(
        "Checking out LFS objects: {percent}% ({materialized}/{total}), {}",
        crate::push::human_bytes(materialized_bytes)
    );
    Ok(())
}

/// Walk the parent components of a repo-relative path. If any
/// component is a regular file, symlink, or some other non-directory,
/// return the upstream-formatted "not a directory" string so the
/// caller can emit `"path": not a directory` and skip. Stops at the
/// first non-existent component (we'd `create_dir_all` from there).
/// Same shape as pull's check.
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

/// `git ls-files -z` listing of paths currently in the index. Used
/// to drop pointers that have been removed (e.g. `git rm`) from the
/// re-materialize set. `--full-name` is required so the listing is
/// repo-relative even when invoked from a subdirectory; otherwise
/// the intersection with scan_tree's `--full-tree` output misses.
fn indexed_paths(cwd: &Path) -> std::io::Result<HashSet<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-files", "-z", "--full-name"])
        .output()?;
    if !out.status.success() {
        return Ok(HashSet::new());
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect())
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
    // Ignore exit status: refresh exits non-zero if any path is still
    // considered dirty (e.g. user edits we didn't touch), and that's
    // not a checkout failure.
    let _ = child.wait()?;
    Ok(())
}

/// Mirrors git's own `GIT_TRACE` semantics: any value other than
/// "", "0", "false", "no", "off" enables tracing. Used to gate
/// `filepathfilter:`-style trace lines that the upstream shell
/// tests grep for.
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

/// Resolve a user-supplied path argument to a repo-relative glob
/// pattern. Handles cwd-relative resolution (so `nested.dat` from a
/// subdir matches `subdir/nested.dat` in the repo), `./` and `../`
/// path-navigation prefixes, the bare `.` and `..` shortcuts, and
/// trailing-slash directory patterns. Returns `None` if the resolved
/// path falls outside the repo.
fn resolve_user_pattern(cwd: &Path, repo_root: &Path, pat: &str) -> Option<Vec<String>> {
    let cwd_canon = cwd.canonicalize().ok()?;
    let root_canon = repo_root.canonicalize().ok()?;
    let cwd_rel = cwd_canon
        .strip_prefix(&root_canon)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");

    // Strip leading "./" and count "../" pops, so "../foo/**" from
    // a `subdir` cwd resolves to "foo/**" repo-relative.
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
    // Already a recursive pattern (from `.` / `..` / a trailing
    // `/**`)? Return as-is.
    if combined == "**" || combined.ends_with("/**") {
        return Some(vec![combined]);
    }
    // Trailing-slash pattern (`foo/`) → recursive subtree only.
    if dir_only {
        return Some(vec![format!("{combined}/**")]);
    }
    // Bare names (`foo`, `file*.dat`) match both a file and the
    // directory's contents — gitignore-style semantics.
    let subtree = format!("{combined}/**");
    Some(vec![combined, subtree])
}

/// Map the `--base` / `--ours` / `--theirs` flags onto a merge stage
/// number (1/2/3), or `None` when no stage flag is set. Returns a
/// usage error when more than one stage flag is set, mirroring
/// upstream's "at most one of --base, --theirs, and --ours is
/// allowed."
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

/// Conflict-resolution mode: write the LFS content from one merge
/// stage to a user-specified path.
///
/// The stage flag and the path argument come paired (`--to PATH
/// --ours FILE`). We read `:STAGE:FILE` from the index, decode the
/// pointer, ensure the object is local (downloading if needed), and
/// write its bytes to PATH — replacing whatever's there, including
/// symlinks (which we want to overwrite as plain files) and
/// hardlinks (which we want to break so the original file's content
/// is preserved).
fn run_to_conflict(
    cwd: &Path,
    opts: &Options,
    stage: Option<u8>,
) -> Result<(), CheckoutError> {
    // `--to` and a stage flag must come together. Either alone is a
    // usage error with this exact wording (test 13's first two
    // greps).
    let (Some(to), Some(stage)) = (opts.to.as_deref(), stage) else {
        return Err(CheckoutError::Usage(
            "--to and exactly one of --theirs, --ours, and --base must be used together".into(),
        ));
    };
    // Conflict mode requires exactly one path argument — the
    // conflicted file to read from the index. Zero or many is a
    // usage error.
    if opts.paths.len() != 1 {
        return Err(CheckoutError::Usage(
            "--to requires exactly one Git LFS object file path".into(),
        ));
    }
    let file_arg = &opts.paths[0];

    // Conflict mode still needs a real (non-bare) repo to read the
    // index from.
    if !is_in_git_repo(cwd) {
        return Err(CheckoutError::NotInRepo);
    }
    if is_bare_repo(cwd) {
        return Err(CheckoutError::NotInWorkTree);
    }

    // Resolve `--to` against the caller's cwd; create intermediate
    // dirs so e.g. `--to dir1/dir2/foo.txt` from a fresh repo just
    // works. Absolute `--to` paths pass through unchanged.
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

    // Look up the staged blob. Pass the user's path through verbatim
    // — `git rev-parse :<stage>:<arg>` resolves relative paths against
    // the caller's cwd already, so subdirectory invocations work
    // without us doing manual repo-relative conversion. On any
    // failure (missing index entry, no merge in progress, the path
    // pointing at a directory like ".") we surface the upstream
    // error wording the test greps for.
    let ref_str = format!(":{stage}:{file_arg}");
    let blob_oid = match resolve_index_blob(cwd, &ref_str) {
        Some(oid) => oid,
        None => {
            return Err(CheckoutError::Other(format!(
                "Could not checkout (are you not in the middle of a merge?): \
                 Git can't resolve ref: {ref_str:?}"
            )));
        }
    };

    // Read the blob and parse it as an LFS pointer. Non-LFS files
    // (e.g. `other.txt` in test 13) come back as plain text and fail
    // pointer parsing — surface the upstream wording.
    let blob = read_blob(cwd, &blob_oid)?;
    let pointer = match Pointer::parse(&blob) {
        Ok(p) => p,
        Err(e) => {
            return Err(CheckoutError::Other(format!(
                "Could not find decoder pointer for object {blob_oid:?}: {e}"
            )));
        }
    };

    // Make sure the object is local. If not, fetch through the same
    // pipeline `git lfs fetch` uses; failures bubble up as a generic
    // "Error checking out" line.
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
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

    // Write the smudged content. Unlinking first handles three cases
    // we have to support per upstream parity:
    //   - read-only existing file (write fails otherwise)
    //   - symlink at PATH (we want to overwrite the symlink itself)
    //   - hardlink at PATH (we want to break the link so the original
    //     file the user has elsewhere isn't clobbered)
    match std::fs::remove_file(&to_path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut src = store.open(pointer.oid)?;
    let mut out = std::fs::File::create(&to_path)?;
    std::io::copy(&mut src, &mut out)?;
    Ok(())
}

/// `git rev-parse <ref>` — return the object SHA, or `None` if the
/// ref doesn't resolve. Used by conflict-mode lookup of `:<stage>:
/// <file>`.
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

/// `git cat-file -p <oid>` — return the blob bytes. Used to read
/// the conflicted pointer text out of the index.
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
