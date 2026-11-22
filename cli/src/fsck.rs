//! `git lfs fsck`: check LFS objects and pointers.
//!
//! - `--objects` hashes the local object for every pointer in the history of
//!   the ref (or range). Paths excluded by `lfs.fetchinclude` and
//!   `lfs.fetchexclude` are skipped. Unless `--dry-run` is given, corrupt
//!   objects are moved to `<lfs>/bad/<oid>`.
//! - `--pointers` checks every blob that `.gitattributes` tracks in the
//!   ref's tree (or every tree in the range). A blob that isn't a pointer is
//!   an `unexpectedGitObject`, and a pointer that isn't canonical is a
//!   `nonCanonicalPointer`.
//!
//! Both run when neither is given. Without a ref, HEAD is checked, but not
//! the index as upstream does.

use std::io::Read;
use std::path::Path;

use git_lfs_git::AttrSet;
use git_lfs_git::cat_file::CatFileBatch;
use git_lfs_git::scanner::{scan_pointers_with_args, scan_tree_blobs};
use git_lfs_pointer::{MAX_POINTER_SIZE, Oid, Pointer};
use git_lfs_store::Store;
use sha2::{Digest, Sha256};

use crate::fetch::fetch_filter_set;

/// Whether Git can find a repository from `cwd`.
///
/// Only the Git directory is required, not being inside the work tree:
/// t-fsck.sh runs fsck from outside the work tree with `GIT_DIR` and
/// `GIT_WORK_TREE` set.
fn is_in_git_repo(cwd: &Path) -> bool {
    git_lfs_git::git_dir(cwd).is_ok()
}

#[derive(Debug, thiserror::Error)]
pub enum FsckError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Fetch(#[from] crate::fetch::FetchCommandError),
    /// Error printed as is, such as an unresolvable ref.
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `--objects` only.
    Objects,
    /// `--pointers` only.
    Pointers,
    /// Both, the default.
    Both,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    pub dry_run: bool,
}

/// Run fsck and return the exit code.
///
/// The code is 0 if everything is fine, 1 if a problem was found, and 128
/// outside a repository.
pub fn run(cwd: &Path, refspec: Option<&str>, opts: &Options) -> Result<i32, FsckError> {
    // t-fsck.sh expects this on stdout.
    if !is_in_git_repo(cwd) {
        println!("Not in a Git repository.");
        return Ok(128);
    }
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let r = refspec.unwrap_or("HEAD");

    // Check the ref first so a typo gets upstream's message rather than a
    // failed rev-list.
    if !crate::fetch::is_resolvable_ref(cwd, r) {
        return Err(FsckError::Other(format!("Git can't resolve ref: {r:?}")));
    }

    let mut corrupt_oids: Vec<Oid> = Vec::new();
    let mut non_canonical: usize = 0;

    if matches!(opts.mode, Mode::Objects | Mode::Both) {
        // Objects that fetch wouldn't download aren't expected locally.
        // Upstream only applies `lfs.fetchexclude` here.
        let include_set = fetch_filter_set(cwd, "lfs.fetchinclude")?;
        let exclude_set = fetch_filter_set(cwd, "lfs.fetchexclude")?;
        // Like upstream's `ScanRef`, a single ref is checked without its
        // history. Only a range walks the commits in it.
        let walk: &[&str] = if r.contains("..") {
            &[]
        } else {
            &["--no-walk"]
        };
        let pointers = scan_pointers_with_args(cwd, &[r], &[], walk)?;
        for entry in &pointers {
            if !crate::fetch::path_passes_filter(entry.path.as_deref(), &include_set, &exclude_set)
            {
                continue;
            }
            match verify_object(&store, entry.oid, entry.size)? {
                ObjectVerify::Ok => {}
                ObjectVerify::Missing => {
                    let name = entry
                        .path
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| entry.oid.to_string());
                    println!(
                        "objects: openError: {name} ({}) could not be checked: no such file",
                        entry.oid
                    );
                    corrupt_oids.push(entry.oid);
                }
                ObjectVerify::Corrupt => {
                    let name = entry
                        .path
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| entry.oid.to_string());
                    println!("objects: corruptObject: {name} ({}) is corrupt", entry.oid);
                    corrupt_oids.push(entry.oid);
                }
            }
        }
    }

    let mut unexpected: usize = 0;
    if matches!(opts.mode, Mode::Pointers | Mode::Both) {
        let blobs = scan_tree_blobs(cwd, r)?;
        let mut batch = CatFileBatch::spawn(cwd)?;
        // Read attributes from the tree, not the working tree, which may
        // be empty or out of date.
        let attrs = build_tree_attrs(&blobs, &mut batch)?;
        for blob in &blobs {
            // A symlink is never a pointer, whatever `.gitattributes` says.
            if blob.mode == "120000" {
                continue;
            }
            let path_str = blob.path.to_string_lossy().replace('\\', "/");
            if !attrs.is_lfs_tracked(&path_str) {
                continue;
            }
            // Too big to be a pointer, so don't read it.
            if (blob.size as usize) >= MAX_POINTER_SIZE {
                println!(
                    "pointer: unexpectedGitObject: \"{path_str}\" (treeish {}) should have been a pointer but was not",
                    blob.blob_oid,
                );
                unexpected += 1;
                continue;
            }
            let Some(content) = batch.read(&blob.blob_oid)? else {
                continue;
            };
            match Pointer::parse(&content.content) {
                Err(_) => {
                    println!(
                        "pointer: unexpectedGitObject: \"{path_str}\" (treeish {}) should have been a pointer but was not",
                        blob.blob_oid,
                    );
                    unexpected += 1;
                }
                Ok(p) if !p.canonical => {
                    println!(
                        "pointer: nonCanonicalPointer: Pointer for {} (blob {}) was not canonical",
                        p.oid, blob.blob_oid,
                    );
                    non_canonical += 1;
                }
                Ok(_) => {}
            }
        }
    }

    let ok = corrupt_oids.is_empty() && non_canonical == 0 && unexpected == 0;
    if ok {
        println!("Git LFS fsck OK");
        return Ok(0);
    }

    if opts.dry_run || corrupt_oids.is_empty() {
        return Ok(1);
    }

    // Move corrupt objects aside so the next fetch downloads them again.
    let bad_dir = store.root().join("bad");
    println!(
        "objects: repair: moving corrupt objects to {}",
        bad_dir.display()
    );
    std::fs::create_dir_all(&bad_dir)?;
    for oid in &corrupt_oids {
        let src = store.object_path(*oid);
        let dst = bad_dir.join(oid.to_string());
        match std::fs::rename(&src, &dst) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A missing object.
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(1)
}

#[derive(Debug, PartialEq, Eq)]
enum ObjectVerify {
    Ok,
    Missing,
    Corrupt,
}

/// Build an [`AttrSet`] from the `.gitattributes` files among `blobs`.
///
/// A `.gitattributes` blob missing from the object database is skipped.
fn build_tree_attrs(
    blobs: &[git_lfs_git::scanner::TreeBlob],
    batch: &mut CatFileBatch,
) -> std::io::Result<AttrSet> {
    let mut attrs = AttrSet::empty();
    // Shallowest first, since the last file added wins.
    let mut by_depth: Vec<&git_lfs_git::scanner::TreeBlob> = blobs
        .iter()
        .filter(|b| b.path.file_name().is_some_and(|n| n == ".gitattributes"))
        .collect();
    by_depth.sort_by_key(|b| b.path.components().count());
    for blob in by_depth {
        let Some(content) = batch.read(&blob.blob_oid).map_err(std::io::Error::other)? else {
            continue;
        };
        let dir = blob
            .path
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        attrs.add_buffer_at(&content.content, &dir);
    }
    Ok(attrs)
}

fn verify_object(store: &Store, oid: Oid, size: u64) -> std::io::Result<ObjectVerify> {
    if oid == Oid::EMPTY {
        // The empty object is never stored.
        return Ok(ObjectVerify::Ok);
    }
    let mut file = match store.open(oid) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Like upstream, a missing zero-size object is fine.
            if size == 0 {
                return Ok(ObjectVerify::Ok);
            }
            return Ok(ObjectVerify::Missing);
        }
        Err(e) => return Err(e),
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let computed: [u8; 32] = hasher.finalize().into();
    if total != size || Oid::from_bytes(computed) != oid {
        Ok(ObjectVerify::Corrupt)
    } else {
        Ok(ObjectVerify::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("lfs"));
        (tmp, store)
    }

    #[test]
    fn verify_object_ok_for_well_formed_object() {
        let (_tmp, store) = fixture();
        let (oid, size) = store.insert(&mut b"hello".as_slice()).unwrap();
        assert_eq!(verify_object(&store, oid, size).unwrap(), ObjectVerify::Ok);
    }

    #[test]
    fn verify_object_missing_for_unknown_oid() {
        let (_tmp, store) = fixture();
        let oid: Oid = "1111111111111111111111111111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        assert_eq!(
            verify_object(&store, oid, 1).unwrap(),
            ObjectVerify::Missing
        );
    }

    #[test]
    fn verify_object_corrupt_when_size_lies() {
        let (_tmp, store) = fixture();
        let (oid, _) = store.insert(&mut b"hello".as_slice()).unwrap();
        // The content matches its OID but not this size.
        assert_eq!(
            verify_object(&store, oid, 99).unwrap(),
            ObjectVerify::Corrupt,
        );
    }

    #[test]
    fn verify_object_corrupt_when_content_was_tampered() {
        let (_tmp, store) = fixture();
        let (oid, size) = store.insert(&mut b"hello".as_slice()).unwrap();
        // Overwrite the on-disk file with bytes that don't hash to its OID.
        let path = store.object_path(oid);
        std::fs::write(&path, b"world").unwrap();
        assert_eq!(
            verify_object(&store, oid, size).unwrap(),
            ObjectVerify::Corrupt,
        );
    }

    #[test]
    fn verify_object_handles_empty_oid() {
        let (_tmp, store) = fixture();
        // The empty object never has a file.
        assert_eq!(
            verify_object(&store, Oid::EMPTY, 0).unwrap(),
            ObjectVerify::Ok
        );
    }
}
