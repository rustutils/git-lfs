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

use std::path::{Path, PathBuf};

use git_lfs_api::ObjectSpec;
use git_lfs_git::scan_tree;
use git_lfs_store::Store;

use crate::fetcher::LfsFetcher;

#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Fetch(git_lfs_filter::FetchError),
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

    let pointers = scan_tree(cwd, "HEAD")?;

    // Filter by path patterns if any were given. Each user pattern is
    // resolved to a repo-relative form once, up front.
    let pointers = if opts.paths.is_empty() {
        pointers
    } else {
        let repo_root = repo_root(cwd)?;
        let patterns = opts
            .paths
            .iter()
            .map(|p| to_repo_relative(cwd, &repo_root, p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(CheckoutError::Other)?;
        pointers
            .into_iter()
            .filter(|p| {
                p.path
                    .as_deref()
                    .map(|path| matches_any(&path.to_string_lossy(), &patterns))
                    .unwrap_or(false)
            })
            .collect()
    };

    if pointers.is_empty() {
        println!("Nothing to checkout.");
        return Ok(());
    }

    // Fetch any objects we don't already have locally. Same code path
    // smudge uses on demand.
    let missing: Vec<ObjectSpec> = pointers
        .iter()
        .filter(|p| !store.contains_with_size(p.oid, p.size))
        .map(|p| ObjectSpec { oid: p.oid.to_string(), size: p.size })
        .collect();
    if !missing.is_empty() {
        println!("Fetching {} missing object(s)...", missing.len());
        let fetcher = LfsFetcher::from_repo(cwd, &store)?;
        let report = fetcher
            .download_many(missing)
            .map_err(CheckoutError::Fetch)?;
        for (oid, err) in &report.failed {
            eprintln!("git-lfs: download failed for {oid}: {err}");
        }
    }

    // Now materialize. Skip pointers whose objects still aren't local
    // (download failed); they stay as pointer text.
    let mut materialized = 0usize;
    let mut skipped = 0usize;
    for p in &pointers {
        let Some(rel) = &p.path else {
            skipped += 1;
            continue;
        };
        if !store.contains_with_size(p.oid, p.size) {
            eprintln!("git-lfs: skipping {} (object not available)", rel.display());
            skipped += 1;
            continue;
        }
        let dst = repo_relative_to_abs(cwd, rel)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut src = store.open(p.oid)?;
        let mut out = std::fs::File::create(&dst)?;
        std::io::copy(&mut src, &mut out)?;
        materialized += 1;
    }

    if skipped > 0 {
        println!("Checked out {materialized} file(s); {skipped} skipped.");
    } else {
        println!("Checked out {materialized} file(s).");
    }
    Ok(())
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

/// Convert a user-supplied path to repo-relative form. Tolerates
/// non-existent paths (a wildcard the shell didn't expand against the
/// cwd should still go through, even if it doesn't match anything).
fn to_repo_relative(cwd: &Path, repo_root: &Path, file: &str) -> Result<String, String> {
    let path = Path::new(file);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let abs = abs.canonicalize().unwrap_or(abs);
    let root = repo_root
        .canonicalize()
        .map_err(|e| format!("canonicalizing repo root: {e}"))?;
    let rel = abs
        .strip_prefix(&root)
        .map_err(|_| format!("path is outside the repository: {file}"))?;
    let mut s = rel.to_string_lossy().replace('\\', "/");
    // Re-attach a trailing slash if the user supplied one — that's how
    // we tell directory patterns from file patterns.
    if file.ends_with('/') && !s.ends_with('/') {
        s.push('/');
    }
    Ok(s)
}

fn repo_relative_to_abs(cwd: &Path, rel: &Path) -> Result<PathBuf, CheckoutError> {
    let root = repo_root(cwd)?;
    Ok(root.join(rel))
}

fn matches_any(repo_rel: &str, patterns: &[String]) -> bool {
    for p in patterns {
        if let Some(prefix) = p.strip_suffix('/') {
            // Directory pattern: match `<prefix>` itself or anything beneath.
            if repo_rel.starts_with(prefix)
                && (repo_rel.len() == prefix.len()
                    || repo_rel.as_bytes().get(prefix.len()) == Some(&b'/'))
            {
                return true;
            }
        } else if repo_rel == p {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_any_exact_match() {
        assert!(matches_any("foo.bin", &["foo.bin".into()]));
        assert!(!matches_any("foo.bin", &["bar.bin".into()]));
    }

    #[test]
    fn matches_any_directory_prefix() {
        let p = vec!["data/".into()];
        assert!(matches_any("data", &p));
        assert!(matches_any("data/x.bin", &p));
        assert!(matches_any("data/sub/y.bin", &p));
        assert!(!matches_any("databases/x", &p));
        assert!(!matches_any("other.bin", &p));
    }

    #[test]
    fn matches_any_multiple_patterns() {
        let p = vec!["a.bin".into(), "b.bin".into(), "data/".into()];
        assert!(matches_any("a.bin", &p));
        assert!(matches_any("b.bin", &p));
        assert!(matches_any("data/c.bin", &p));
        assert!(!matches_any("c.bin", &p));
    }
}
