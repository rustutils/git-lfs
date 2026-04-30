//! `git lfs ls-files [<ref>]` — list LFS-tracked files visible at a ref
//! (default: HEAD), optionally across full history with `--all`.
//!
//! v0 supports the most-used flags: `-l/--long`, `-s/--size`, `-n/--name-only`,
//! `-d/--debug`, `-a/--all`, `-j/--json`. The upstream `--include`/`--exclude`
//! path filters, `--deleted`, and the two-ref range form are deferred — see
//! NOTES.md.

use std::path::Path;

use git_lfs_git::{PointerEntry, scan_pointers, scan_tree};
use git_lfs_pointer::VERSION_LATEST;
use git_lfs_store::Store;
use serde::Serialize;

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
    /// `<oid> <marker> <name> [(<size>)]`
    Default,
    /// Multi-line per-file block with size/checkout/download/oid/version.
    Debug,
    /// `{"files": [...]}`.
    Json,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// `-l/--long`: emit full 64-char OID instead of the 10-char prefix.
    pub long: bool,
    /// `-s/--size`: append humanized size in parens.
    pub show_size: bool,
    /// `-n/--name-only`: emit only the path.
    pub name_only: bool,
    /// `-a/--all`: scan all refs' history, not just one tree.
    pub all: bool,
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
        // `--all`: walk every reachable commit from every ref. We
        // enumerate refs ourselves rather than passing `--all` to
        // rev-list because our `rev_list` wrapper feeds refs via stdin.
        let refs = enumerate_refs(cwd)?;
        if refs.is_empty() {
            Vec::new()
        } else {
            let r: Vec<&str> = refs.iter().map(String::as_str).collect();
            scan_pointers(cwd, &r, &[])?
        }
    } else {
        let r = refspec.unwrap_or("HEAD");
        // Tolerate empty repos: if HEAD doesn't exist yet, there's
        // nothing to list. Match upstream by silently emitting nothing
        // rather than erroring.
        if refspec.is_none() && !head_exists(cwd) {
            return emit(&[], &store, cwd, opts);
        }
        scan_tree(cwd, r)?
    };

    emit(&pointers, &store, cwd, opts)
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
                emit_default_line(p, store, cwd, opts);
            }
            Ok(())
        }
    }
}

fn emit_default_line(p: &PointerEntry, store: &Store, cwd: &Path, opts: &Options) {
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
    let _ = store; // unused in default mode; kept symmetric with debug
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
    let s = serde_json::to_string_pretty(&JsonOutput { files })?;
    println!("{s}");
    Ok(())
}

fn file_present(cwd: &Path, p: &PointerEntry) -> bool {
    let Some(rel) = p.path.as_deref() else {
        return false;
    };
    std::fs::metadata(cwd.join(rel))
        .map(|m| m.is_file() && m.len() == p.size)
        .unwrap_or(false)
}

fn head_exists(cwd: &Path) -> bool {
    // Capture (don't inherit) stdout/stderr — rev-parse prints the OID
    // on success and we don't want that leaking into our own output.
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

/// Match upstream's `humanize.FormatBytes`: powers of 1024, two decimals
/// for non-byte units, units `B/KB/MB/GB/TB`. We only need to be
/// approximately right — this is for human display, never parsed back.
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
