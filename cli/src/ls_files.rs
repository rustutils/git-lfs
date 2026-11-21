//! `git lfs ls-files`: list the LFS pointers in a tree.
//!
//! Without a ref, lists the index and HEAD together. With a ref, lists its
//! tree, with `--deleted` its whole history, and with `--all` the history
//! of every ref. `--include`, `--exclude`, and the form that takes two refs
//! aren't supported.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use git_lfs_git::scanner::{PointerEntry, scan_index_pointers, scan_pointers, scan_tree};
use git_lfs_pointer::VERSION_LATEST;
use git_lfs_store::Store;
use serde::Serialize;

/// Git's empty tree, which stands in for HEAD before the first commit.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, thiserror::Error)]
pub enum LsFilesError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("could not enumerate refs: {0}")]
    EnumerateRefs(String),
    #[error("could not serialize JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One line per file.
    Default,
    /// `--debug`: a block of fields per file.
    Debug,
    /// `--json`.
    Json,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// `--long`: the full OID instead of the first 10 digits.
    pub long: bool,
    /// `--size`: add the size.
    pub show_size: bool,
    /// `--name-only`: only the path.
    pub name_only: bool,
    /// `--all`: the history of every ref. Takes precedence over `deleted`.
    pub all: bool,
    /// `--deleted`: the whole history of the ref, so files that were
    /// deleted are listed too.
    pub deleted: bool,
    pub format: Format,
}

#[derive(Debug, Serialize)]
struct JsonOutput {
    files: Vec<JsonFile>,
}

#[derive(Debug, Serialize)]
struct JsonFile {
    name: String,
    size: u64,
    checkout: bool,
    downloaded: bool,
    oid_type: &'static str,
    oid: String,
    version: &'static str,
}

pub fn run(cwd: &Path, refspec: Option<&str>, opts: &Options) -> Result<(), LsFilesError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());

    let pointers = if opts.all {
        // `scan_pointers` passes refs on stdin, where `--all` doesn't
        // work, so list them here.
        let refs = enumerate_refs(cwd)?;
        if refs.is_empty() {
            Vec::new()
        } else {
            let r: Vec<&str> = refs.iter().map(String::as_str).collect();
            scan_pointers(cwd, &r, &[])?
        }
    } else if opts.deleted {
        let r = refspec.unwrap_or("HEAD");
        scan_pointers(cwd, &[r], &[])?
    } else if let Some(r) = refspec {
        scan_tree(cwd, r)?
    } else {
        // The index, then HEAD. Before the first commit, only the index.
        let has_head = head_exists(cwd);
        let ref_or_empty = if has_head { "HEAD" } else { EMPTY_TREE_SHA };
        let tree = if has_head {
            scan_tree(cwd, "HEAD")?
        } else {
            Vec::new()
        };
        // `scan_index_pointers` gives one entry per OID with all its
        // paths. Split it into one entry per path.
        let index_raw = scan_index_pointers(cwd, ref_or_empty).unwrap_or_default();
        let mut index: Vec<PointerEntry> = Vec::new();
        for e in index_raw {
            if e.paths.is_empty() {
                index.push(e);
            } else {
                for p in &e.paths {
                    index.push(PointerEntry {
                        oid: e.oid,
                        size: e.size,
                        path: Some(p.clone()),
                        paths: vec![p.clone()],
                        canonical: e.canonical,
                        extensions: e.extensions.clone(),
                    });
                }
            }
        }
        merge_by_path(index, tree)
    };

    // Paths are repo-relative, so check for the files from the root.
    let working_dir = repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    emit(&pointers, &store, &working_dir, opts)
}

fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

/// Concatenate two lists, keeping only the first entry for each path.
///
/// Entries without a path are all kept.
fn merge_by_path(first: Vec<PointerEntry>, second: Vec<PointerEntry>) -> Vec<PointerEntry> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::with_capacity(first.len() + second.len());
    for e in first.into_iter().chain(second) {
        match e.path.as_deref() {
            Some(p) if !seen.insert(p.to_path_buf()) => {}
            _ => out.push(e),
        }
    }
    out
}

fn emit(
    pointers: &[PointerEntry],
    store: &Store,
    cwd: &Path,
    opts: &Options,
) -> Result<(), LsFilesError> {
    match opts.format {
        Format::Json => emit_json(pointers, store, cwd),
        Format::Debug => {
            for p in pointers {
                emit_debug_block(p, store, cwd);
            }
            Ok(())
        }
        Format::Default => {
            for p in pointers {
                emit_default_line(p, cwd, opts);
            }
            Ok(())
        }
    }
}

fn emit_default_line(p: &PointerEntry, cwd: &Path, opts: &Options) {
    let name = p
        .path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    if opts.name_only {
        if opts.show_size {
            println!("{} ({})", name, humanize(p.size));
        } else {
            println!("{name}");
        }
        return;
    }

    let oid = p.oid.to_string();
    let oid_short: &str = if opts.long { &oid } else { &oid[..10] };
    let marker = if file_present(cwd, p) { '*' } else { '-' };

    if opts.show_size {
        println!("{oid_short} {marker} {name} ({})", humanize(p.size));
    } else {
        println!("{oid_short} {marker} {name}");
    }
}

fn emit_debug_block(p: &PointerEntry, store: &Store, cwd: &Path) {
    let name = p
        .path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    println!("filepath: {name}");
    println!("    size: {}", p.size);
    println!("checkout: {}", file_present(cwd, p));
    println!("download: {}", store.contains_with_size(p.oid, p.size));
    println!("     oid: sha256 {}", p.oid);
    println!(" version: {VERSION_LATEST}");
    println!();
}

fn emit_json(pointers: &[PointerEntry], store: &Store, cwd: &Path) -> Result<(), LsFilesError> {
    let files: Vec<JsonFile> = pointers
        .iter()
        .map(|p| JsonFile {
            name: p
                .path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            size: p.size,
            checkout: file_present(cwd, p),
            downloaded: store.contains_with_size(p.oid, p.size),
            oid_type: "sha256",
            oid: p.oid.to_string(),
            version: VERSION_LATEST,
        })
        .collect();
    // Like upstream's encoder: one-space indent and a final newline.
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    use serde::Serialize as _;
    JsonOutput { files }.serialize(&mut ser)?;
    buf.push(b'\n');
    use std::io::Write;
    std::io::stdout().write_all(&buf)?;
    Ok(())
}

/// Whether the working-tree file exists with the object's size.
fn file_present(cwd: &Path, p: &PointerEntry) -> bool {
    let Some(rel) = p.path.as_deref() else {
        return false;
    };
    std::fs::metadata(cwd.join(rel))
        .map(|m| m.is_file() && m.len() == p.size)
        .unwrap_or(false)
}

fn head_exists(cwd: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn enumerate_refs(cwd: &Path) -> Result<Vec<String>, LsFilesError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["for-each-ref", "--format=%(refname)"])
        .output()?;
    if !out.status.success() {
        return Err(LsFilesError::EnumerateRefs(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Format a size in powers of 1024 with two decimals, such as `1.50 KB`.
///
/// Upstream's `humanize.FormatBytes` uses powers of 1000 and one decimal
/// below 10, such as `1.5 KB`.
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
    fn humanize_below_1k_is_bytes() {
        assert_eq!(humanize(0), "0 B");
        assert_eq!(humanize(1023), "1023 B");
    }

    #[test]
    fn humanize_kib_and_mib() {
        assert_eq!(humanize(1024), "1.00 KB");
        assert_eq!(humanize(1024 * 1024), "1.00 MB");
        assert_eq!(humanize(1024 * 1024 * 5 + 512 * 1024), "5.50 MB");
    }
}
