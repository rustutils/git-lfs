//! Local content-addressable object store for git-lfs.
//!
//! Objects live under `<lfs_dir>/objects/aa/bb/aabbcc…` where `aabbcc…` is
//! the SHA-256 hex of the content (sharded by the first two hex bytes — see
//! `docs/spec.md`). Writes go through a tmp file in `<lfs_dir>/tmp/` and are
//! atomically renamed into place once their hash is known.
//!
//! ```no_run
//! use git_lfs_store::Store;
//! let store = Store::new(".git/lfs");
//! let mut input: &[u8] = b"hello world";
//! let (oid, size) = store.insert(&mut input).unwrap();
//! assert!(store.contains(oid));
//! # let _ = size;
//! ```

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use git_lfs_pointer::Oid;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

/// Platform null device — what `object_path` returns for [`Oid::EMPTY`].
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

const COPY_BUFFER: usize = 64 * 1024;

/// A local LFS object store rooted at `<lfs_dir>` (typically `.git/lfs`).
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: Oid, actual: Oid },
}

impl Store {
    /// Create a store rooted at the given LFS directory. The directory is not
    /// created eagerly; subdirectories are created on demand as objects land.
    pub fn new(lfs_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: lfs_dir.into(),
        }
    }

    /// Root LFS directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding temp files for in-flight inserts.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Where the object with this OID lives on disk.
    ///
    /// For [`Oid::EMPTY`] this returns the platform null device, mirroring
    /// upstream's behavior so callers can `open` an empty object without
    /// special-casing.
    pub fn object_path(&self, oid: Oid) -> PathBuf {
        if oid == Oid::EMPTY {
            return PathBuf::from(NULL_DEVICE);
        }
        let hex = oid.to_string();
        self.root
            .join("objects")
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(&hex)
    }

    /// `true` if this object is present locally as a regular file. The empty
    /// OID is always considered present.
    pub fn contains(&self, oid: Oid) -> bool {
        if oid == Oid::EMPTY {
            return true;
        }
        self.object_path(oid).is_file()
    }

    /// `true` if the object is present and its on-disk size matches `size`.
    /// Used to detect partial/corrupted local copies.
    pub fn contains_with_size(&self, oid: Oid, size: u64) -> bool {
        if oid == Oid::EMPTY {
            return size == 0;
        }
        std::fs::metadata(self.object_path(oid))
            .map(|m| m.is_file() && m.len() == size)
            .unwrap_or(false)
    }

    /// Walk every object file in the store, yielding (oid, size_on_disk).
    ///
    /// Traverses the sharded `objects/<aa>/<bb>/<oid>` layout. Filenames
    /// that don't parse as 64-char SHA-256 hex are silently skipped, as
    /// are unexpected directories. The store directory not existing is
    /// not an error — the result is just empty.
    ///
    /// Used by `git lfs prune` and (eventually) `fsck --orphaned`.
    pub fn each_object(&self) -> io::Result<Vec<(Oid, u64)>> {
        let objects_dir = self.root.join("objects");
        if !objects_dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for aa in std::fs::read_dir(&objects_dir)? {
            let aa = aa?;
            if !aa.file_type()?.is_dir() {
                continue;
            }
            for bb in std::fs::read_dir(aa.path())? {
                let bb = bb?;
                if !bb.file_type()?.is_dir() {
                    continue;
                }
                for entry in std::fs::read_dir(bb.path())? {
                    let entry = entry?;
                    let name = entry.file_name();
                    let Some(name_str) = name.to_str() else {
                        continue;
                    };
                    let Ok(oid) = name_str.parse::<Oid>() else {
                        continue;
                    };
                    let meta = entry.metadata()?;
                    if !meta.is_file() {
                        continue;
                    }
                    out.push((oid, meta.len()));
                }
            }
        }
        Ok(out)
    }

    /// Open an object for reading. Errors with [`io::ErrorKind::NotFound`]
    /// if the object isn't in the store.
    pub fn open(&self, oid: Oid) -> io::Result<File> {
        File::open(self.object_path(oid))
    }

    /// Stream `src` into the store, computing SHA-256 as we go.
    /// Returns the resulting OID and byte count.
    ///
    /// This is the clean-filter path: we don't know the OID until after the
    /// content is hashed.
    pub fn insert(&self, src: &mut impl Read) -> Result<(Oid, u64), StoreError> {
        let (oid, size, tmp) = self.stream_to_tmp(src)?;
        self.commit(oid, tmp)?;
        Ok((oid, size))
    }

    /// Stream `src` into the store, requiring the resulting hash to equal
    /// `expected`. On mismatch, returns [`StoreError::HashMismatch`] and the
    /// temp file is dropped without being committed.
    ///
    /// This is the download path: we know the OID upfront and must verify
    /// what the server sent.
    pub fn insert_verified(&self, expected: Oid, src: &mut impl Read) -> Result<u64, StoreError> {
        let (actual, size, tmp) = self.stream_to_tmp(src)?;
        if actual != expected {
            // Drop the tmp file; it goes away on Drop.
            return Err(StoreError::HashMismatch { expected, actual });
        }
        self.commit(actual, tmp)?;
        Ok(size)
    }

    fn stream_to_tmp(&self, src: &mut impl Read) -> io::Result<(Oid, u64, NamedTempFile)> {
        std::fs::create_dir_all(self.tmp_dir())?;
        let mut tmp = NamedTempFile::new_in(self.tmp_dir())?;
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        let mut buf = vec![0u8; COPY_BUFFER];
        let file = tmp.as_file_mut();
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            total += n as u64;
        }
        file.flush()?;
        let bytes: [u8; 32] = hasher.finalize().into();
        Ok((Oid::from_bytes(bytes), total, tmp))
    }

    fn commit(&self, oid: Oid, tmp: NamedTempFile) -> io::Result<()> {
        // The empty object lives at /dev/null — never persist it.
        if oid == Oid::EMPTY {
            return Ok(());
        }
        let dest = self.object_path(oid);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomic rename, *clobbering* any existing file at the target
        // path. The store is content-addressed: anything already there
        // is either the same content (no-op overwrite) or corrupt
        // (truncated, half-written) — and the latter is exactly what
        // `git lfs fetch --refetch` exists to recover from.
        tmp.persist(&dest).map(|_| ()).map_err(|e| e.error)
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

    /// Sample non-empty OID used across tests (SHA-256 of "abc").
    const ABC_OID_HEX: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn abc_oid() -> Oid {
        ABC_OID_HEX.parse().unwrap()
    }

    #[test]
    fn object_path_is_sharded() {
        let (_tmp, store) = fixture();
        let oid: Oid = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393"
            .parse()
            .unwrap();
        let path = store.object_path(oid);
        let suffix: PathBuf = ["objects", "4d", "7a", &oid.to_string()].iter().collect();
        assert!(
            path.ends_with(&suffix),
            "{path:?} does not end with {suffix:?}"
        );
    }

    #[test]
    fn empty_oid_short_circuits() {
        let (_tmp, store) = fixture();
        assert_eq!(store.object_path(Oid::EMPTY), PathBuf::from(NULL_DEVICE));
        assert!(store.contains(Oid::EMPTY));
        assert!(store.contains_with_size(Oid::EMPTY, 0));
        assert!(!store.contains_with_size(Oid::EMPTY, 1));
        // Opening the empty OID yields zero bytes.
        let mut buf = Vec::new();
        store
            .open(Oid::EMPTY)
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn insert_round_trip() {
        let (_tmp, store) = fixture();
        let content = b"hello world!";
        let (oid, size) = store.insert(&mut content.as_slice()).unwrap();
        assert_eq!(size, content.len() as u64);
        assert!(store.contains(oid));
        assert!(store.contains_with_size(oid, size));
        let mut readback = Vec::new();
        store.open(oid).unwrap().read_to_end(&mut readback).unwrap();
        assert_eq!(readback, content);
    }

    #[test]
    fn insert_computes_correct_sha256() {
        let (_tmp, store) = fixture();
        let (oid, _) = store.insert(&mut b"abc".as_slice()).unwrap();
        assert_eq!(oid, abc_oid());
    }

    #[test]
    fn insert_empty_yields_empty_oid_and_no_object_file() {
        let (_tmp, store) = fixture();
        let (oid, size) = store.insert(&mut [].as_slice()).unwrap();
        assert_eq!(oid, Oid::EMPTY);
        assert_eq!(size, 0);
        // Critically: nothing was persisted under objects/.
        assert!(!store.root.join("objects").exists());
    }

    #[test]
    fn insert_idempotent() {
        let (_tmp, store) = fixture();
        let (oid1, _) = store.insert(&mut b"abc".as_slice()).unwrap();
        let (oid2, _) = store.insert(&mut b"abc".as_slice()).unwrap();
        assert_eq!(oid1, oid2);
        assert!(store.contains(oid1));
    }

    #[test]
    fn insert_verified_succeeds_on_match() {
        let (_tmp, store) = fixture();
        let size = store
            .insert_verified(abc_oid(), &mut b"abc".as_slice())
            .unwrap();
        assert_eq!(size, 3);
        assert!(store.contains(abc_oid()));
    }

    #[test]
    fn insert_verified_errors_on_mismatch_and_leaves_no_file() {
        let (_tmp, store) = fixture();
        let wrong: Oid = "0000000000000000000000000000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let err = store
            .insert_verified(wrong, &mut b"abc".as_slice())
            .unwrap_err();
        match err {
            StoreError::HashMismatch { expected, actual } => {
                assert_eq!(expected, wrong);
                assert_eq!(actual, abc_oid());
            }
            other => panic!("expected HashMismatch, got {other:?}"),
        }
        // Neither the wrong OID nor the actual OID should be present —
        // a failed verify must not leak a half-committed file.
        assert!(!store.contains(wrong));
        assert!(!store.contains(abc_oid()));
        // And no leftover tmp file.
        let tmp_entries: Vec<_> = std::fs::read_dir(store.tmp_dir())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(tmp_entries.is_empty(), "tmp dir not empty: {tmp_entries:?}");
    }

    #[test]
    fn open_missing_oid_is_not_found() {
        let (_tmp, store) = fixture();
        let oid: Oid = "0000000000000000000000000000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let err = store.open(oid).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn streaming_megabyte_input() {
        let (_tmp, store) = fixture();
        // ~1 MiB to exercise the streaming loop across many buffer fills.
        let content: Vec<u8> = (0..1_048_576u32).map(|i| (i ^ (i >> 5)) as u8).collect();
        let (oid, size) = store.insert(&mut content.as_slice()).unwrap();
        assert_eq!(size, content.len() as u64);
        let mut readback = Vec::new();
        store.open(oid).unwrap().read_to_end(&mut readback).unwrap();
        assert_eq!(readback, content);
    }

    #[test]
    fn each_object_returns_empty_when_no_objects_dir() {
        let (_tmp, store) = fixture();
        // Store dir doesn't exist yet.
        assert!(store.each_object().unwrap().is_empty());
    }

    #[test]
    fn each_object_finds_inserted_objects_with_correct_size() {
        let (_tmp, store) = fixture();
        let (oid_a, _) = store.insert(&mut b"hello".as_slice()).unwrap();
        let (oid_b, _) = store.insert(&mut b"world!!!".as_slice()).unwrap();
        let mut got = store.each_object().unwrap();
        got.sort_by_key(|(_, size)| *size);
        assert_eq!(got.len(), 2);
        // Order by size: "hello" (5 bytes) first, then "world!!!" (8 bytes).
        assert_eq!(got[0].0, oid_a);
        assert_eq!(got[0].1, 5);
        assert_eq!(got[1].0, oid_b);
        assert_eq!(got[1].1, 8);
    }

    #[test]
    fn each_object_skips_unrecognized_filenames() {
        let (_tmp, store) = fixture();
        let (oid, _) = store.insert(&mut b"hi".as_slice()).unwrap();
        // Drop a stray file in the same shard directory that isn't a
        // 64-char hex name — must not crash or be reported.
        let shard = store
            .root()
            .join("objects")
            .join(&oid.to_string()[0..2])
            .join(&oid.to_string()[2..4]);
        std::fs::write(shard.join("README"), b"ignored").unwrap();
        let got = store.each_object().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, oid);
    }

    #[test]
    fn insert_verified_overwrites_corrupt_existing_file() {
        // Mirrors the scenario t-fetch's `--refetch` test exercises:
        // a previous fetch landed an object, then the file got
        // truncated (cp /dev/null over it). A subsequent verified
        // insert must replace the corrupt file rather than silently
        // skipping the write.
        let (_tmp, store) = fixture();
        let dest = store.object_path(abc_oid());
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"").unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 0);

        store
            .insert_verified(abc_oid(), &mut b"abc".as_slice())
            .unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn insert_creates_dirs_on_demand() {
        let (_tmp, store) = fixture();
        // Before any insert, neither objects/ nor tmp/ exists.
        assert!(!store.root.exists());
        let (oid, _) = store.insert(&mut b"abc".as_slice()).unwrap();
        assert!(store.tmp_dir().is_dir());
        assert!(store.object_path(oid).is_file());
    }
}
