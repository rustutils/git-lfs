//! `git lfs migrate` — analyze or rewrite history for LFS conversion.
//!
//! Phase 1 (this file): `info` only — read-only walk that reports the
//! biggest file extensions in a ref range. Phase 2 will add `import`
//! (rewrite history so matching files become LFS pointers); phase 3
//! adds the inverse `export`. The shared scaffolding (ref resolution,
//! blob walk, path filters) is built here so Phase 2 doesn't have to
//! retrofit.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use git_lfs_git::{CatFileBatch, CatFileBatchCheck, CatFileHeader, rev_list};
use git_lfs_pointer::{MAX_POINTER_SIZE, Pointer};
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

/// How `migrate info` treats blobs that look like LFS pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerMode {
    /// Parse pointers; count them under a separate "LFS Objects" group
    /// using the pointer's recorded size.
    Follow,
    /// Don't parse pointers; count by extension regardless. Cheapest
    /// — skips the cat-file --batch read entirely.
    NoFollow,
    /// Parse pointers; exclude them from the output. Useful for users
    /// who want to see what's *still* not in LFS.
    Ignore,
}

#[derive(Debug, Clone)]
pub struct InfoOptions {
    /// Branch / ref names to include. Empty + `everything = false`
    /// means "current branch."
    pub branches: Vec<String>,
    /// Walk every local ref instead of a chosen subset.
    pub everything: bool,
    /// Glob patterns: only blobs at matching paths are counted.
    pub include: Vec<String>,
    /// Glob patterns: blobs at matching paths are excluded.
    pub exclude: Vec<String>,
    /// Bytes threshold; only files at least this large count toward
    /// `BytesAbove` / `TotalAbove`. The total file count is unaffected.
    pub above: u64,
    /// Maximum number of extension rows to print (default 5).
    pub top: usize,
    pub pointers: PointerMode,
}

#[derive(Debug, Default, Clone)]
struct Entry {
    /// Total file count (above + below threshold).
    total: u64,
    /// Files at or above the threshold.
    total_above: u64,
    /// Sum of file sizes for the above-threshold files.
    bytes_above: u64,
}

const LFS_GROUP: &str = "LFS Objects";

pub fn info(cwd: &Path, opts: &InfoOptions) -> Result<(), MigrateError> {
    let (include_refs, exclude_refs) = resolve_refs(cwd, opts)?;
    if include_refs.is_empty() {
        // Empty repo or no resolvable refs — nothing to report.
        return Ok(());
    }

    let include = build_globset(&opts.include)?;
    let exclude = build_globset(&opts.exclude)?;

    // Pipeline:
    //   rev-list --objects → (oid, name) per blob/tree/commit
    //   cat-file --batch-check → (oid, kind, size) — keep blobs only
    //   if pointers != NoFollow and size <= 1024: cat-file --batch → read content, try Pointer::parse
    let include_strs: Vec<&str> = include_refs.iter().map(String::as_str).collect();
    let exclude_strs: Vec<&str> = exclude_refs.iter().map(String::as_str).collect();
    let entries = rev_list(cwd, &include_strs, &exclude_strs)?;

    let mut bcheck = CatFileBatchCheck::spawn(cwd)?;
    // Per-extension stats, plus a special "LFS Objects" entry.
    let mut by_ext: HashMap<String, Entry> = HashMap::new();
    let mut lfs_entry = Entry::default();

    // Defer batch-read until we've collected pointer-sized blob candidates.
    let mut pointer_candidates: Vec<(String, String, u64)> = Vec::new();

    for e in entries {
        let Some(name) = &e.name else { continue };
        // rev-list emits trees and blobs both with `<oid> <name>`. Use
        // batch-check to filter; trees and missing entries are dropped.
        let header = bcheck.check(&e.oid)?;
        let CatFileHeader::Found { kind, size, .. } = header else {
            continue;
        };
        if kind != "blob" {
            continue;
        }
        if !path_matches(name, &include, &exclude) {
            continue;
        }

        // Pointer detection: only blobs that fit in MAX_POINTER_SIZE
        // are candidates. We defer the batch-read to a single pass
        // after we've finished checking.
        let could_be_pointer =
            opts.pointers != PointerMode::NoFollow && (size as usize) < MAX_POINTER_SIZE;
        if could_be_pointer {
            pointer_candidates.push((e.oid.clone(), name.clone(), size));
            continue;
        }

        // Plain (non-pointer-candidate) blob: count by extension.
        let group = ext_group(name);
        accumulate(by_ext.entry(group).or_default(), size, opts.above);
    }
    drop(bcheck);

    // Read the pointer-candidate content; classify each as pointer-or-not.
    if !pointer_candidates.is_empty() {
        let mut batch = CatFileBatch::spawn(cwd)?;
        for (oid, name, blob_size) in pointer_candidates {
            let Some(blob) = batch.read(&oid)? else { continue };
            let pointer = Pointer::parse(&blob.content);
            match (pointer, opts.pointers) {
                (Ok(p), PointerMode::Follow) => {
                    accumulate(&mut lfs_entry, p.size, opts.above);
                }
                (Ok(_), PointerMode::Ignore) => {
                    // Skip — user wants to see only non-pointer blobs.
                }
                _ => {
                    // Not a parseable pointer (or NoFollow mode) — count
                    // as a regular blob by extension, using the actual
                    // blob size on disk.
                    let group = ext_group(&name);
                    accumulate(by_ext.entry(group).or_default(), blob_size, opts.above);
                }
            }
        }
    }

    print_table(&by_ext, &lfs_entry, opts);
    Ok(())
}

fn accumulate(entry: &mut Entry, size: u64, above: u64) {
    entry.total += 1;
    if size >= above {
        entry.total_above += 1;
        entry.bytes_above += size;
    }
}

fn ext_group(path: &str) -> String {
    // Use the leaf component (after the last `/`) for extension lookup,
    // so a path like `assets/images/foo.png` becomes `*.png`.
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if let Some(idx) = leaf.rfind('.')
        && idx > 0
        && idx < leaf.len() - 1
    {
        return format!("*{}", &leaf[idx..]);
    }
    leaf.to_owned()
}

fn path_matches(path: &str, include: &Option<GlobSet>, exclude: &Option<GlobSet>) -> bool {
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

fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>, MigrateError> {
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

/// Resolve the include/exclude ref sets to pass to `git rev-list`.
fn resolve_refs(
    cwd: &Path,
    opts: &InfoOptions,
) -> Result<(Vec<String>, Vec<String>), MigrateError> {
    if opts.everything {
        if !opts.branches.is_empty() {
            return Err(MigrateError::Other(
                "cannot use --everything with explicit refs".into(),
            ));
        }
        return Ok((all_local_refs(cwd)?, Vec::new()));
    }

    if opts.branches.is_empty() {
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
    for arg in &opts.branches {
        if let Some(rest) = arg.strip_prefix('^') {
            exclude.push(rest.to_owned());
        } else {
            include.push(arg.clone());
        }
    }
    Ok((include, exclude))
}

fn head_exists(cwd: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn current_branch(cwd: &Path) -> Option<String> {
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

fn all_local_refs(cwd: &Path) -> Result<Vec<String>, MigrateError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["for-each-ref", "--format=%(refname)", "refs/heads/", "refs/tags/"])
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

/// Parse strings like "1mb", "500k", "2g", or plain byte counts.
/// Decimal multipliers (1k = 1000) — matches upstream's `humanize.ParseBytes`
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

fn print_table(by_ext: &HashMap<String, Entry>, lfs: &Entry, opts: &InfoOptions) {
    let mut rows: Vec<(String, &Entry)> = by_ext
        .iter()
        .filter(|(_, e)| e.total_above > 0)
        .map(|(k, v)| (k.clone(), v))
        .collect();
    rows.sort_by(|a, b| {
        b.1.bytes_above
            .cmp(&a.1.bytes_above)
            .then_with(|| a.0.cmp(&b.0))
    });
    if rows.len() > opts.top {
        rows.truncate(opts.top);
    }

    if rows.is_empty() && lfs.total == 0 {
        return;
    }

    // Pre-format every column so we can left/right-justify uniformly.
    let mut formatted: Vec<(String, String, String, String, bool)> = Vec::new();
    for (qual, entry) in &rows {
        formatted.push(format_row(qual, entry, false));
    }
    if lfs.total > 0 {
        formatted.push(format_row(LFS_GROUP, lfs, true));
    }

    let max_qual = formatted.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let max_size = formatted.iter().map(|r| r.1.len()).max().unwrap_or(0);
    let max_stat = formatted.iter().map(|r| r.2.len()).max().unwrap_or(0);
    let max_pct = formatted.iter().map(|r| r.3.len()).max().unwrap_or(0);

    for (i, (qual, size, stat, pct, separate)) in formatted.iter().enumerate() {
        if *separate && i > 0 {
            println!();
        }
        println!(
            "{:<qw$}\t{:<sw$}\t{:>tw$}\t{:>pw$}",
            qual,
            size,
            stat,
            pct,
            qw = max_qual,
            sw = max_size,
            tw = max_stat,
            pw = max_pct,
        );
    }
}

fn format_row(
    qual: &str,
    entry: &Entry,
    separate: bool,
) -> (String, String, String, String, bool) {
    let pct = if entry.total > 0 {
        100.0 * (entry.total_above as f64) / (entry.total as f64)
    } else {
        0.0
    };
    (
        qual.to_owned(),
        humanize(entry.bytes_above),
        format!("{}/{} files", entry.total_above, entry.total),
        format!("{pct:.0}%"),
        separate,
    )
}

fn humanize(n: u64) -> String {
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
        // ".gitignore" is a dotfile, not "gitignore" with extension ".gitignore".
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
        assert_eq!(parse_size("1.5g").unwrap(), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
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

    #[test]
    fn accumulate_above_threshold() {
        let mut e = Entry::default();
        accumulate(&mut e, 10, 100);
        accumulate(&mut e, 100, 100);
        accumulate(&mut e, 200, 100);
        assert_eq!(e.total, 3);
        assert_eq!(e.total_above, 2);
        assert_eq!(e.bytes_above, 300);
    }
}
