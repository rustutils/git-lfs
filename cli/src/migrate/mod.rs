//! `git lfs migrate` — analyze or rewrite history for LFS conversion.
//!
//! Three subcommands sharing ref-resolution + glob-path-filter
//! scaffolding:
//!
//! - **`info`** (Phase 1, [`info()`]): read-only walk + extension report.
//! - **`import`** (Phase 2, planned): rewrite history so matching files
//!   become LFS pointers. Implementation will pipe `git fast-export` →
//!   blob-transform → `git fast-import`.
//! - **`export`** (Phase 3, planned): inverse of import.
//!
//! This module hosts the shared types and helpers; subcommands live in
//! sibling files.

mod export;
mod fast_export;
mod fast_import;
mod import;
mod info;
mod pipeline;
mod transform;

pub use export::{ExportOptions, export};
pub use import::{ImportOptions, import};
pub use info::{InfoOptions, PointerMode, info};

use std::path::Path;
use std::process::Command;

use globset::{Glob, GlobSet, GlobSetBuilder};

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid glob pattern {pattern:?}: {source}")]
    BadGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("invalid size {0:?}: expected formats like '500k', '1mb', '2g', or plain bytes")]
    BadSize(String),
    #[error("{0}")]
    Other(String),
}

/// Common ref selection for any subcommand: explicit branches, or the
/// current branch by default, or all local refs with `--everything`.
#[derive(Debug, Clone, Default)]
pub(super) struct RefSelection {
    pub branches: Vec<String>,
    pub everything: bool,
}

/// Resolve the include/exclude ref sets to pass to `git rev-list`.
pub(super) fn resolve_refs(
    cwd: &Path,
    sel: &RefSelection,
) -> Result<(Vec<String>, Vec<String>), MigrateError> {
    if sel.everything {
        if !sel.branches.is_empty() {
            return Err(MigrateError::Other(
                "cannot use --everything with explicit refs".into(),
            ));
        }
        return Ok((all_local_refs(cwd)?, Vec::new()));
    }

    if sel.branches.is_empty() {
        // No explicit args → current branch (matching upstream's
        // default). Empty repos have no resolvable HEAD; bail before
        // we hand a phantom branch name to rev-list.
        if !head_exists(cwd) {
            return Ok((Vec::new(), Vec::new()));
        }
        let Some(head) = current_branch(cwd) else {
            return Ok((Vec::new(), Vec::new()));
        };
        return Ok((vec![head], Vec::new()));
    }

    // Explicit args. `^name` becomes an exclude.
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for arg in &sel.branches {
        if let Some(rest) = arg.strip_prefix('^') {
            exclude.push(rest.to_owned());
        } else {
            include.push(arg.clone());
        }
    }
    Ok((include, exclude))
}

pub(super) fn head_exists(cwd: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(super) fn current_branch(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

pub(super) fn all_local_refs(cwd: &Path) -> Result<Vec<String>, MigrateError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/",
            "refs/tags/",
        ])
        .output()?;
    if !out.status.success() {
        return Err(MigrateError::Other(format!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

pub(super) fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>, MigrateError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p).map_err(|e| MigrateError::BadGlob {
            pattern: p.clone(),
            source: e,
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| MigrateError::BadGlob {
            pattern: patterns.join(", "),
            source: e,
        })
}

pub(super) fn path_matches(
    path: &str,
    include: &Option<GlobSet>,
    exclude: &Option<GlobSet>,
) -> bool {
    if let Some(ex) = exclude
        && ex.is_match(path)
    {
        return false;
    }
    match include {
        Some(inc) => inc.is_match(path),
        None => true,
    }
}

/// Group a path by its file extension for size-bucketing display.
/// `assets/foo.png` becomes `*.png`; extensionless files use their
/// basename; dotfiles like `.gitignore` are treated as basenames.
pub(super) fn ext_group(path: &str) -> String {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if let Some(idx) = leaf.rfind('.')
        && idx > 0
        && idx < leaf.len() - 1
    {
        return format!("*{}", &leaf[idx..]);
    }
    leaf.to_owned()
}

/// Parse strings like "1mb", "500k", "2g", or plain byte counts.
/// Decimal multipliers (1k = 1024) — matches upstream's `humanize.ParseBytes`
/// when no `i` suffix is present. We accept either case.
pub fn parse_size(s: &str) -> Result<u64, MigrateError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let lower = trimmed.to_ascii_lowercase();
    // Strip an optional trailing `b` (so "1kb" works the same as "1k").
    let lower = lower.strip_suffix('b').unwrap_or(&lower);
    let (num_str, mul) = if let Some(rest) = lower.strip_suffix('k') {
        (rest, 1024u64)
    } else if let Some(rest) = lower.strip_suffix('m') {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = lower.strip_suffix('g') {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = lower.strip_suffix('t') {
        (rest, 1024u64 * 1024 * 1024 * 1024)
    } else {
        (lower, 1u64)
    };
    let num: f64 = num_str
        .trim()
        .parse()
        .map_err(|_| MigrateError::BadSize(s.to_owned()))?;
    if num < 0.0 {
        return Err(MigrateError::BadSize(s.to_owned()));
    }
    Ok((num * mul as f64) as u64)
}

pub(super) fn humanize(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut i = 0;
    while value >= 1024.0 && i + 1 < UNITS.len() {
        value /= 1024.0;
        i += 1;
    }
    format!("{value:.2} {}", UNITS[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_group_uses_extension() {
        assert_eq!(ext_group("foo.png"), "*.png");
        assert_eq!(ext_group("a/b/c.jpg"), "*.jpg");
        assert_eq!(ext_group("path/with.many.dots/file.tar.gz"), "*.gz");
    }

    #[test]
    fn ext_group_no_extension_uses_basename() {
        assert_eq!(ext_group("README"), "README");
        assert_eq!(ext_group("path/Makefile"), "Makefile");
    }

    #[test]
    fn ext_group_dotfiles_have_no_extension() {
        assert_eq!(ext_group(".gitignore"), ".gitignore");
        assert_eq!(ext_group("path/.env"), ".env");
    }

    #[test]
    fn parse_size_plain_bytes() {
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("123").unwrap(), 123);
    }

    #[test]
    fn parse_size_with_suffixes() {
        assert_eq!(parse_size("1k").unwrap(), 1024);
        assert_eq!(parse_size("1kb").unwrap(), 1024);
        assert_eq!(parse_size("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(
            parse_size("1.5g").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
    }

    #[test]
    fn parse_size_rejects_garbage() {
        assert!(parse_size("nonsense").is_err());
        assert!(parse_size("-1").is_err());
        assert!(parse_size("12xx").is_err());
    }

    #[test]
    fn path_matches_include_and_exclude() {
        let inc = build_globset(&["*.bin".into()]).unwrap();
        let exc = build_globset(&["**/skip.bin".into()]).unwrap();
        assert!(path_matches("foo.bin", &inc, &exc));
        assert!(!path_matches("foo.txt", &inc, &exc));
        assert!(!path_matches("dir/skip.bin", &inc, &exc));
    }
}
