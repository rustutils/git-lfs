//! `git lfs migrate info`: report file sizes in history by extension.
//!
//! Lists objects with `git rev-list --objects`, keeps the blobs with
//! `git cat-file --batch-check`, and reads the ones small enough to be
//! pointers with `git cat-file --batch`. `--fixup` instead looks only at
//! the tree of the first ref.

use std::collections::HashMap;
use std::path::Path;

use git_lfs_git::AttrSet;
use git_lfs_git::cat_file::{CatFileBatch, CatFileBatchCheck, CatFileHeader};
use git_lfs_git::rev_list::rev_list;
use git_lfs_git::scanner::scan_tree_blobs;
use git_lfs_pointer::{MAX_POINTER_SIZE, Pointer};

use super::transform::{dir_of, is_attrs_path};
use super::{
    MigrateError, RefSelection, build_globset, ext_group, humanize, humanize_with_unit,
    path_matches, resolve_refs,
};

/// How `migrate info` treats blobs that look like LFS pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerMode {
    /// Count pointers in a separate "LFS Objects" group, by the size of
    /// the object they point to.
    Follow,
    /// Count pointers like any other file, without reading any blob.
    NoFollow,
    /// Leave pointers out.
    Ignore,
}

#[derive(Debug, Clone)]
pub struct InfoOptions {
    pub branches: Vec<String>,
    pub everything: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub include_ref: Vec<String>,
    pub exclude_ref: Vec<String>,
    pub above: u64,
    pub top: usize,
    pub pointers: PointerMode,
    /// `--unit` in bytes, to print every size in that unit.
    ///
    /// Without it, each size gets its own unit.
    pub unit: Option<u64>,
    pub fixup: bool,
}

#[derive(Debug, Default, Clone)]
struct Entry {
    total: u64,
    total_above: u64,
    bytes_above: u64,
}

const LFS_GROUP: &str = "LFS Objects";

pub fn info(cwd: &Path, opts: &InfoOptions) -> Result<(), MigrateError> {
    if opts.everything && (!opts.include_ref.is_empty() || !opts.exclude_ref.is_empty()) {
        return Err(MigrateError::Usage(
            "Cannot use --everything with --include-ref or --exclude-ref".into(),
        ));
    }
    if opts.fixup {
        match opts.pointers {
            PointerMode::Follow => {
                return Err(MigrateError::Usage(
                    "Cannot use --fixup with --pointers=follow".into(),
                ));
            }
            PointerMode::NoFollow => {
                return Err(MigrateError::Usage(
                    "Cannot use --fixup with --pointers=no-follow".into(),
                ));
            }
            PointerMode::Ignore => {}
        }
        if !opts.include.is_empty() || !opts.exclude.is_empty() {
            return Err(MigrateError::Usage(
                "Cannot use --fixup with --include, --exclude".into(),
            ));
        }
    }
    let sel = RefSelection {
        branches: opts.branches.clone(),
        everything: opts.everything,
    };
    let (mut include_refs, mut exclude_refs) = resolve_refs(cwd, &sel)?;
    for r in &opts.include_ref {
        if !include_refs.iter().any(|x| x == r) {
            include_refs.push(r.clone());
        }
    }
    for r in &opts.exclude_ref {
        if !exclude_refs.iter().any(|x| x == r) {
            exclude_refs.push(r.clone());
        }
    }
    super::validate_refs(cwd, &include_refs, &exclude_refs)?;
    // Skip commits already on a remote, as in import.
    if !opts.everything && opts.include_ref.is_empty() && opts.exclude_ref.is_empty() {
        for r in super::export::list_remote_tracking_refs(cwd) {
            if !exclude_refs.iter().any(|x| x == &r) {
                exclude_refs.push(r);
            }
        }
    }
    if include_refs.is_empty() {
        return Ok(());
    }
    if super::export::any_attrs_symlink(cwd, &include_refs) {
        return Err(MigrateError::Other(
            "expected '.gitattributes' to be a file, got a symbolic link".into(),
        ));
    }
    if opts.fixup {
        return info_fixup(cwd, opts, &include_refs);
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
            let Some(blob) = batch.read(&oid)? else {
                continue;
            };
            let pointer = Pointer::parse(&blob.content);
            match (pointer, opts.pointers) {
                (Ok(p), PointerMode::Follow) => {
                    accumulate(&mut lfs_entry, p.size, opts.above);
                }
                (Ok(_), PointerMode::Ignore) => {}
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

/// `--fixup`: count the files that are tracked by the attributes but
/// aren't pointers.
///
/// Looks only at the tree of the first ref, with that tree's
/// `.gitattributes` files, where upstream checks every commit with its
/// own. `info/attributes` and the global attributes file aren't used.
fn info_fixup(cwd: &Path, opts: &InfoOptions, include_refs: &[String]) -> Result<(), MigrateError> {
    let Some(reference) = include_refs.first() else {
        return Ok(());
    };

    let blobs = scan_tree_blobs(cwd, reference).map_err(|e| MigrateError::Other(e.to_string()))?;

    // Add shallow to deep, since the last list added takes precedence.
    let mut attrs_dirs: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let mut batch = CatFileBatch::spawn(cwd)?;
        for blob in &blobs {
            let path = blob.path.to_string_lossy();
            if !is_attrs_path(&path) {
                continue;
            }
            if let Some(b) = batch.read(&blob.blob_oid)? {
                attrs_dirs.push((dir_of(&path), b.content));
            }
        }
    }
    attrs_dirs.sort_by_key(|(d, _)| d.matches('/').count());

    let mut attrs = AttrSet::empty();
    for (dir, content) in &attrs_dirs {
        attrs.add_buffer_at(content, dir);
    }

    let mut by_ext: HashMap<String, Entry> = HashMap::new();
    let mut batch = CatFileBatch::spawn(cwd)?;
    for blob in &blobs {
        let path = blob.path.to_string_lossy();
        if is_attrs_path(&path) {
            continue;
        }
        if blob.mode == "120000" {
            continue;
        }
        if !attrs.is_lfs_tracked(&path) {
            continue;
        }
        if (blob.size as usize) < MAX_POINTER_SIZE
            && let Some(b) = batch.read(&blob.blob_oid)?
            && Pointer::parse(&b.content).is_ok()
        {
            continue;
        }

        let group = ext_group(&path);
        accumulate(by_ext.entry(group).or_default(), blob.size, opts.above);
    }

    let lfs = Entry::default();
    print_table(&by_ext, &lfs, opts);
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
        formatted.push(format_row(qual, entry, false, opts.unit));
    }
    if lfs.total > 0 {
        formatted.push(format_row(LFS_GROUP, lfs, true, opts.unit));
    }

    let max_qual = formatted.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let max_size = formatted.iter().map(|r| r.1.len()).max().unwrap_or(0);
    let max_stat = formatted.iter().map(|r| r.2.len()).max().unwrap_or(0);
    let max_pct = formatted.iter().map(|r| r.3.len()).max().unwrap_or(0);

    for (i, (qual, size, stat, pct, separate)) in formatted.iter().enumerate() {
        if *separate && i > 0 {
            println!();
        }
        // As upstream, only the percentage is right-aligned.
        println!(
            "{:<qw$}\t{:<sw$}\t{:<tw$}\t{:>pw$}",
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
    unit: Option<u64>,
) -> (String, String, String, String, bool) {
    let pct = if entry.total > 0 {
        100.0 * (entry.total_above as f64) / (entry.total as f64)
    } else {
        0.0
    };
    let size = match unit {
        Some(u) => humanize_with_unit(entry.bytes_above, u),
        None => humanize(entry.bytes_above),
    };
    // Upstream pads "file" to the width of "files".
    let noun = if entry.total == 1 { "file " } else { "files" };
    (
        qual.to_owned(),
        size,
        format!("{}/{} {noun}", entry.total_above, entry.total),
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
