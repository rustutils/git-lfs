//! `git lfs migrate`: report on or rewrite history for LFS.
//!
//! [`info()`] walks history read-only and reports file sizes by
//! extension. [`import()`] and [`export()`] rewrite history by piping
//! `git fast-export` through a [`transform`] into `git fast-import`,
//! converting matching blobs to or from LFS pointers. This module holds
//! the ref selection, path filtering, and size helpers they share.

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
pub(crate) use pipeline::working_tree_dirty;

use std::path::Path;
use std::process::Command;

use git_lfs_git::attr::PathPatterns;

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid size {0:?}: expected formats like '500k', '1mb', '2g', or plain bytes")]
    BadSize(String),
    #[error("{0}")]
    Other(String),
    /// A usage error, printed without the `git-lfs:` prefix.
    ///
    /// The migrate shell tests compare these messages exactly.
    #[error("{0}")]
    Usage(String),
}

/// Which refs a subcommand walks.
///
/// Explicit branches, the current branch by default, or every local ref
/// with `--everything`.
#[derive(Debug, Clone, Default)]
pub(super) struct RefSelection {
    pub branches: Vec<String>,
    pub everything: bool,
}

/// Resolve the refs to include and exclude in the walk.
///
/// A `^name` argument is an exclude. With no arguments this is the
/// current branch, or nothing on an unborn or detached HEAD.
pub(super) fn resolve_refs(
    cwd: &Path,
    sel: &RefSelection,
) -> Result<(Vec<String>, Vec<String>), MigrateError> {
    if sel.everything {
        if !sel.branches.is_empty() {
            return Err(MigrateError::Usage(
                "Cannot use --everything with explicit reference arguments".into(),
            ));
        }
        return Ok((all_local_refs(cwd)?, Vec::new()));
    }

    if sel.branches.is_empty() {
        // An unborn HEAD still has a branch name, which rev-list would
        // reject.
        if !head_exists(cwd) {
            return Ok((Vec::new(), Vec::new()));
        }
        let Some(head) = current_branch(cwd) else {
            return Ok((Vec::new(), Vec::new()));
        };
        return Ok((vec![head], Vec::new()));
    }

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

/// Fail if any ref doesn't resolve to a commit.
///
/// The shell tests grep the error for "can't resolve ref", which is the
/// end of upstream's message.
pub(super) fn validate_refs(
    cwd: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<(), MigrateError> {
    for r in include.iter().chain(exclude.iter()) {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{r}^{{commit}}"),
            ])
            .output();
        let ok = matches!(out, Ok(o) if o.status.success());
        if !ok {
            return Err(MigrateError::Other(format!("can't resolve ref: {r:?}")));
        }
    }
    Ok(())
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

/// The refs `--everything` rewrites.
///
/// Every ref except remote-tracking ones and the special refs upstream's
/// `isSpecialGitRef` skips: the stash, notes, bisect, and replace refs.
/// Upstream also rewrites remote-tracking branches.
pub(super) fn all_local_refs(cwd: &Path) -> Result<Vec<String>, MigrateError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["for-each-ref", "--format=%(refname)"])
        .output()?;
    if !out.status.success() {
        return Err(MigrateError::Other(format!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("refs/remotes/") && !is_special_ref(l))
        .map(String::from)
        .collect())
}

/// Whether `refname` isn't history, and so mustn't be rewritten.
fn is_special_ref(refname: &str) -> bool {
    refname == "refs/stash"
        || ["refs/notes/", "refs/bisect/", "refs/replace/"]
            .iter()
            .any(|prefix| refname.starts_with(prefix))
}

/// The `--include` or `--exclude` patterns, or `None` if there are none.
///
/// They match like `.gitattributes` patterns, as upstream: `a.txt`
/// matches `a.txt` in any directory, and `/a.txt` only at the root. They
/// ignore case if `core.ignorecase` is set.
pub(super) fn build_patterns(cwd: &Path, patterns: &[String]) -> Option<PathPatterns> {
    if patterns.is_empty() {
        return None;
    }
    let ignore_case = git_lfs_git::config::get_bool(cwd, "core.ignorecase", false);
    Some(PathPatterns::new(patterns, ignore_case))
}

pub(super) fn path_matches(
    path: &str,
    include: &Option<PathPatterns>,
    exclude: &Option<PathPatterns>,
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

/// The group `migrate info` reports a path under.
///
/// `assets/foo.png` becomes `*.png` and `.gitattributes` becomes
/// `*.gitattributes`. A file name with no dot, or ending in one, is its
/// own group.
pub(super) fn ext_group(path: &str) -> String {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if let Some(idx) = leaf.rfind('.')
        && idx < leaf.len() - 1
    {
        return format!("*{}", &leaf[idx..]);
    }
    leaf.to_owned()
}

/// Parse a size like `1mb`, `500k`, `2g`, or a plain byte count.
///
/// Case-insensitive, and the multipliers are powers of 1024. Upstream's
/// `humanize.ParseBytes` uses powers of 1000 for these suffixes and 1024
/// only for `kib`, `mib`, and so on, which this rejects. An empty string
/// is 0.
pub fn parse_size(s: &str) -> Result<u64, MigrateError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let lower = trimmed.to_ascii_lowercase();
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
    // A bare unit means one of it, for `--unit=kb`.
    let trimmed_num = num_str.trim();
    let num: f64 = if trimmed_num.is_empty() {
        1.0
    } else {
        trimmed_num
            .parse()
            .map_err(|_| MigrateError::BadSize(s.to_owned()))?
    };
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

/// Format `n` in multiples of `unit` bytes, for `migrate info --unit`.
///
/// There's no unit suffix: upstream prints these as bare numbers too.
pub(super) fn humanize_with_unit(n: u64, unit: u64) -> String {
    let value = n as f64 / unit.max(1) as f64;
    format!("{value:.1}")
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
    fn ext_group_dotfiles_use_leading_dot_as_extension() {
        // Matches upstream, which reports `.gitattributes` as
        // `*.gitattributes`.
        assert_eq!(ext_group(".gitignore"), "*.gitignore");
        assert_eq!(ext_group("path/.env"), "*.env");
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
        for input in ["nonsense", "-1", "12xx"] {
            let err = parse_size(input).unwrap_err();
            assert!(
                matches!(&err, MigrateError::BadSize(s) if s == input),
                "input {input:?} got {err:?}"
            );
        }
    }

    #[test]
    fn path_matches_include_and_exclude() {
        let inc = Some(PathPatterns::new(["*.bin"], false));
        let exc = Some(PathPatterns::new(["skip.bin"], false));
        assert!(path_matches("foo.bin", &inc, &exc));
        assert!(!path_matches("foo.txt", &inc, &exc));
        assert!(!path_matches("dir/skip.bin", &inc, &exc));
    }
}
