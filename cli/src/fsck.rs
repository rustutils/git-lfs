//! `git lfs fsck` — integrity check for LFS objects + pointers.
//!
//! Two independent checks driven by `--objects` and `--pointers`:
//!
//! - **`--objects`**: for every LFS pointer reachable from the named ref's
//!   history, verify the local store has its bytes and that re-hashing
//!   them matches the pointer's OID. Missing or corrupt files are
//!   reported and (unless `--dry-run`) moved to `<lfs>/bad/<oid>`.
//! - **`--pointers`**: for every blob in the named ref's tree that parses
//!   as a pointer, flag any that aren't byte-canonical.
//!
//! Defaults to running both. Exit code: 0 if everything is fine,
//! 1 otherwise.
//!
//! Out of scope (see NOTES.md): the `<a>..<b>` range form, scanning the
//! index alongside HEAD when no ref is given, `lfs.fetchexclude` honor,
//! and the "unexpectedGitObject" detection that requires gitattribute
//! awareness.

use std::io::Read;
use std::path::Path;

use git_lfs_git::{scan_pointers, scan_tree};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum FsckError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Run `--objects` only.
    Objects,
    /// Run `--pointers` only.
    Pointers,
    /// Run both (the default when no flag is given).
    Both,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    pub dry_run: bool,
}

/// Returns the intended process exit code (0 = OK, 1 = at least one
/// problem found).
pub fn run(cwd: &Path, refspec: Option<&str>, opts: &Options) -> Result<i32, FsckError> {
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let r = refspec.unwrap_or("HEAD");

    let mut corrupt_oids: Vec<Oid> = Vec::new();
    let mut non_canonical: usize = 0;

    if matches!(opts.mode, Mode::Objects | Mode::Both) {
        // Scan the full reachable history; this is what upstream's
        // ScanRef does, and it's what most users mean by "fsck this
        // ref's LFS objects."
        let pointers = scan_pointers(cwd, &[r], &[])?;
        for entry in &pointers {
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

    if matches!(opts.mode, Mode::Pointers | Mode::Both) {
        let pointers = scan_tree(cwd, r)?;
        for entry in &pointers {
            if !entry.canonical {
                let name = entry
                    .path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                println!(
                    "pointer: nonCanonicalPointer: Pointer for {name} ({}) was not canonical",
                    entry.oid,
                );
                non_canonical += 1;
            }
        }
    }

    let ok = corrupt_oids.is_empty() && non_canonical == 0;
    if ok {
        println!("Git LFS fsck OK");
        return Ok(0);
    }

    if opts.dry_run || corrupt_oids.is_empty() {
        return Ok(1);
    }

    // Recovery: move corrupt object files to `<lfs>/bad/<oid>` so the
    // next fetch will re-download them. Atomicity isn't critical — if
    // the rename fails we report and exit non-zero.
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
                // Already absent (the missing case above); nothing to move.
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

fn verify_object(store: &Store, oid: Oid, size: u64) -> std::io::Result<ObjectVerify> {
    if oid == Oid::EMPTY {
        // The empty pointer doesn't reference any file; treat as OK.
        return Ok(ObjectVerify::Ok);
    }
    let mut file = match store.open(oid) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Special-case zero-size pointers — upstream reports them as
            // OK even when the on-disk file is missing.
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
        assert_eq!(verify_object(&store, oid, 1).unwrap(), ObjectVerify::Missing);
    }

    #[test]
    fn verify_object_corrupt_when_size_lies() {
        let (_tmp, store) = fixture();
        let (oid, _) = store.insert(&mut b"hello".as_slice()).unwrap();
        // Lie about the size — the on-disk content (5 bytes "hello")
        // is fine for its own OID but doesn't match this 99 size.
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
        // Empty OID is a sentinel; never has a backing file.
        assert_eq!(verify_object(&store, Oid::EMPTY, 0).unwrap(), ObjectVerify::Ok);
    }
}
