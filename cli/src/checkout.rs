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
//! Out of scope (NOTES.md): glob/wildcard patterns (shells handle the
//! common case), and the `--to <path> [--ours|--theirs|--base]` form
//! for resolving conflicted LFS files during a merge.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git_lfs_git::scan_tree;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use globset::{Glob, GlobSetBuilder};

#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Patterns to filter pointers by; empty means "all of HEAD".
    pub paths: Vec<String>,
}

pub fn run(cwd: &Path, opts: &Options) -> Result<(), CheckoutError> {
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
            let glob_pats = resolve_user_pattern(cwd, &repo_root, pat)
                .ok_or_else(|| CheckoutError::Other(format!("path is outside the repository: {pat}")))?;
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
                let Some(path) = p.path.as_deref() else { return false };
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
        let dst = repo_root.join(rel);

        // Existing-file policy mirrors upstream's
        // `singleCheckout.Run`: if the file is already on disk with
        // raw (non-pointer) content, leave it alone. Likewise if it's
        // a pointer for a different OID. Only smudge into files that
        // are missing or already point at our expected OID.
        match std::fs::read(&dst) {
            Ok(bytes) => match Pointer::parse(&bytes) {
                Ok(existing) if existing.oid == p.oid => {
                    // Pointer at the same OID — proceed to materialize.
                }
                Ok(_) => continue,  // Different-OID pointer: user-modified.
                Err(_) => continue, // Raw content: assume user has it intact.
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Missing — proceed.
            }
            Err(e) => return Err(e.into()),
        }

        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
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
        let mut src = store.open(p.oid)?;
        let mut out = std::fs::File::create(&dst)?;
        std::io::copy(&mut src, &mut out)?;
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

