//! `git lfs migrate info` — read-only walk + extension report.
//!
//! Pipeline: `git rev-list --objects` → `cat-file --batch-check` to
//! filter to blobs → `cat-file --batch` to read pointer-sized blobs for
//! pointer-detection.

use std::collections::HashMap;
use std::path::Path;

use git_lfs_git::{CatFileBatch, CatFileBatchCheck, CatFileHeader, rev_list};
use git_lfs_pointer::{MAX_POINTER_SIZE, Pointer};

use super::{
    MigrateError, RefSelection, build_globset, ext_group, humanize, path_matches, resolve_refs,
};

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
    pub branches: Vec<String>,
    pub everything: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub above: u64,
    pub top: usize,
    pub pointers: PointerMode,
}

#[derive(Debug, Default, Clone)]
struct Entry {
    total: u64,
    total_above: u64,
    bytes_above: u64,
}

const LFS_GROUP: &str = "LFS Objects";

pub fn info(cwd: &Path, opts: &InfoOptions) -> Result<(), MigrateError> {
    let sel = RefSelection {
        branches: opts.branches.clone(),
        everything: opts.everything,
    };
    let (include_refs, exclude_refs) = resolve_refs(cwd, &sel)?;
    if include_refs.is_empty() {
        // Empty repo or no resolvable refs — nothing to report.
        return Ok(());
    }

    let include = build_globset(&opts.include)?;
    let exclude = build_globset(&opts.exclude)?;

    let include_strs: Vec<&str> = include_refs.iter().map(String::as_str).collect();
    let exclude_strs: Vec<&str> = exclude_refs.iter().map(String::as_str).collect();
    let entries = rev_list(cwd, &include_strs, &exclude_strs)?;

    let mut bcheck = CatFileBatchCheck::spawn(cwd)?;
    let mut by_ext: HashMap<String, Entry> = HashMap::new();
    let mut lfs_entry = Entry::default();

    let mut pointer_candidates: Vec<(String, String, u64)> = Vec::new();

    for e in entries {
        let Some(name) = &e.name else { continue };
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

        let could_be_pointer =
            opts.pointers != PointerMode::NoFollow && (size as usize) < MAX_POINTER_SIZE;
        if could_be_pointer {
            pointer_candidates.push((e.oid.clone(), name.clone(), size));
            continue;
        }

        let group = ext_group(name);
        accumulate(by_ext.entry(group).or_default(), size, opts.above);
    }
    drop(bcheck);

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

#[cfg(test)]
mod tests {
    use super::*;

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
