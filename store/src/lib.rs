//! Content-addressable on-disk store for Git LFS objects.
//!
//! A [`Store`] manages the LFS directory, usually `.git/lfs`. Each object is
//! stored under `objects/` at a path made from its OID, as in the [spec].
//!
//! There are two ways to add an object. [`Store::insert`] hashes the content
//! as it's written and returns the OID, which is what the clean filter needs.
//! [`Store::insert_verified`] checks the content against an OID you already
//! have, which is what downloads need. Both write to a temporary file first,
//! so an interrupted write never leaves a partial object.
//!
//! Resumable downloads instead write to [`Store::incomplete_path`] and move
//! the finished file into place with [`Store::commit_partial`].
//!
//! [`Store::with_references`] adds other stores to fall back on, such as the
//! source of a `git clone --shared`. [`Store::with_shared_repository`] sets
//! file permissions from `core.sharedRepository`.
//!
//! ```
//! use git_lfs_store::Store;
//!
//! # let _tmp = tempfile::TempDir::new().unwrap();
//! # let lfs_dir = _tmp.path().join("lfs");
//! let store = Store::new(&lfs_dir);
//! let (oid, size) = store.insert(&mut &b"hello world"[..]).unwrap();
//! assert!(store.contains(oid));
//! assert_eq!(size, 11);
//! ```
//!
//! [spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/spec.md

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use git_lfs_pointer::Oid;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

/// The null device, which [`Store::object_path`] returns for [`Oid::EMPTY`].
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

const COPY_BUFFER: usize = 64 * 1024;

/// A local LFS object store.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    /// Other stores' `objects` directories, searched in order when an object
    /// is missing locally.
    references: Vec<PathBuf>,
    /// Permissions for new files and directories.
    mode_policy: ModePolicy,
}

/// Permissions for new files and directories, following
/// `core.sharedRepository` like upstream's `getMask` in `config/config.go`.
#[derive(Debug, Clone, Copy)]
struct ModePolicy {
    /// Permission bits to clear, like a umask.
    ///
    /// Files are always chmodded, because `tempfile` creates them with mode
    /// 0o600 whatever the umask is.
    mask: u32,
}

impl ModePolicy {
    fn from_umask() -> Self {
        Self {
            mask: process_umask(),
        }
    }

    /// Parse a `core.sharedRepository` value.
    ///
    /// `group` (or `true`, `1`) makes files group-writable, and `all` (or
    /// `world`, `everybody`, `2`) also makes them world-readable. An octal
    /// mode such as `0660` is used as given. Anything else, including
    /// `umask`, uses the process umask.
    fn from_shared_repository(value: &str) -> Self {
        let v = value.trim().to_ascii_lowercase();
        let mask = match v.as_str() {
            "group" | "true" | "1" => 0o007,
            "all" | "world" | "everybody" | "2" => 0o002,
            "umask" | "false" | "0" | "" => process_umask(),
            other => {
                // Parse as octal, like upstream's strconv.ParseInt(v, 8, ...).
                match u32::from_str_radix(other.trim_start_matches('0'), 8) {
                    Ok(mode) if mode <= 0o777 => 0o666 & !mode,
                    _ => process_umask(),
                }
            }
        };
        Self { mask: mask & 0o777 }
    }

    /// Mode for object files.
    fn file_mode(self) -> u32 {
        0o666 & !self.mask & 0o777
    }

    /// Mode for directories: the file mode, with each read bit also granting
    /// execute, like upstream's `tools.ExecutablePermissions`.
    fn dir_mode(self) -> u32 {
        let f = self.file_mode();
        (f | ((f & 0o444) >> 2)) & 0o777
    }
}

/// Read the process umask.
///
/// POSIX can only read the umask by setting it, so this sets it and then
/// restores the old value.
#[cfg(unix)]
fn process_umask() -> u32 {
    // SAFETY: `umask` is always safe to call. The umask is briefly 0o022,
    // which only matters to a thread creating a file at that moment. Stores
    // are created before any worker threads start.
    unsafe {
        let prev = libc::umask(0o022);
        libc::umask(prev);
        (prev as u32) & 0o777
    }
}

#[cfg(not(unix))]
fn process_umask() -> u32 {
    0o022
}

/// Why inserting an object failed.
///
/// Other operations return [`io::Error`], since they can't fail any other
/// way.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A filesystem error.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The content given to [`Store::insert_verified`] doesn't match the
    /// expected OID. Nothing is stored.
    #[error("expected OID {expected}, got {actual}")]
    HashMismatch { expected: Oid, actual: Oid },
}

impl Store {
    /// Create a store for an LFS directory.
    ///
    /// Directories are created when they're first needed.
    pub fn new(lfs_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: lfs_dir.into(),
            references: Vec::new(),
            mode_policy: ModePolicy::from_umask(),
        }
    }

    /// Set permissions from a `core.sharedRepository` value, such as `group`
    /// or `0660`.
    ///
    /// Unrecognized values use the process umask.
    #[must_use]
    pub fn with_shared_repository(mut self, value: &str) -> Self {
        self.mode_policy = ModePolicy::from_shared_repository(value);
        self
    }

    /// Add other stores' `objects` directories to fall back on.
    ///
    /// When an object is missing locally but one of these has it, it's
    /// hardlinked into this store, or copied if that fails. The CLI passes
    /// the LFS directories of the repository's Git alternates, from
    /// `git_lfs_git::lfs_alternate_dirs`.
    #[must_use]
    pub fn with_references(mut self, refs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.references = refs.into_iter().collect();
        self
    }

    /// The LFS directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory for temporary files.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// The directory for partial downloads, which are kept between runs so
    /// they can be resumed.
    pub fn incomplete_dir(&self) -> PathBuf {
        self.root.join("incomplete")
    }

    /// Where the partial download of `oid` goes. The file may not exist.
    pub fn incomplete_path(&self, oid: Oid) -> PathBuf {
        self.incomplete_dir().join(format!("{oid}.part"))
    }

    /// Move a finished download into the store.
    ///
    /// This doesn't check the content, so verify it against `oid` first. Any
    /// existing file for `oid` is replaced.
    pub fn commit_partial(&self, oid: Oid, partial: &Path) -> io::Result<()> {
        if oid == Oid::EMPTY {
            return Ok(());
        }
        let dest = self.object_path(oid);
        if let Some(parent) = dest.parent() {
            self.create_dir_all_with_mode(parent)?;
        }
        std::fs::rename(partial, &dest)?;
        self.set_file_mode(&dest)?;
        Ok(())
    }

    /// Delete temporary files left behind by interrupted runs.
    ///
    /// Meant to run when a command starts, like upstream's cleanup in
    /// `fs/cleanup.go`. Errors are ignored. A file is deleted if:
    ///
    /// - its name is an OID followed by `-`, and that object is already in
    ///   the store, or
    /// - it's more than an hour old, and so is its directory. Another process
    ///   may still be using old files in a recently changed subdirectory.
    ///   Files directly in the temporary directory don't need this check.
    pub fn cleanup_tmp_objects(&self) {
        let tmp = self.root.join("tmp");
        if !tmp.exists() {
            return;
        }
        let cutoff =
            match std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(3600)) {
                Some(t) => t,
                None => return,
            };
        // Directory modification times, so each is read only once.
        let mut dir_mtimes: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();
        self.walk_tmp(&tmp, &tmp, cutoff, &mut dir_mtimes);
    }

    fn walk_tmp(
        &self,
        root: &Path,
        dir: &Path,
        cutoff: std::time::SystemTime,
        dir_mtimes: &mut HashMap<PathBuf, std::time::SystemTime>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                self.walk_tmp(root, &path, cutoff, dir_mtimes);
                continue;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // An object that's already stored. The prefix isn't checked for
            // hex, because upstream's tests use names like `good...`.
            if name_str.len() > 64 && name_str.as_bytes()[64] == b'-' && name_str[..64].is_ascii() {
                let oid_str = &name_str[..64];
                let object_path = self
                    .root
                    .join("objects")
                    .join(&oid_str[0..2])
                    .join(&oid_str[2..4])
                    .join(oid_str);
                if object_path.is_file() {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            }
            // Skip files in recently changed subdirectories. The top-level
            // directory changes too often for this check to be useful.
            if dir != root {
                let dir_mtime = *dir_mtimes.entry(dir.to_path_buf()).or_insert_with(|| {
                    std::fs::metadata(dir)
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH)
                });
                if dir_mtime > cutoff {
                    continue;
                }
            }
            // Delete files older than the cutoff.
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            if mtime < cutoff {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Where the object for `oid` is stored.
    ///
    /// For [`Oid::EMPTY`], this is the null device, so opening the empty object
    /// needs no special case.
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

    /// Whether the object is in the store.
    ///
    /// The empty object always is. An object found in a fallback store is
    /// linked into this one first.
    pub fn contains(&self, oid: Oid) -> bool {
        if oid == Oid::EMPTY {
            return true;
        }
        if self.object_path(oid).is_file() {
            return true;
        }
        self.materialize_from_reference(oid, None)
    }

    /// Whether the object is in the store with the expected size.
    ///
    /// A different size means the local copy is incomplete or corrupt. Like
    /// [`Store::contains`], this uses fallback stores.
    pub fn contains_with_size(&self, oid: Oid, size: u64) -> bool {
        if oid == Oid::EMPTY {
            return size == 0;
        }
        let local = std::fs::metadata(self.object_path(oid))
            .map(|m| m.is_file() && m.len() == size)
            .unwrap_or(false);
        if local {
            return true;
        }
        self.materialize_from_reference(oid, Some(size))
    }

    /// Link or copy an object from the first fallback store that has it, with
    /// the given size if there is one. Returns whether it worked.
    fn materialize_from_reference(&self, oid: Oid, size: Option<u64>) -> bool {
        if self.references.is_empty() {
            return false;
        }
        let hex = oid.to_string();
        for refdir in &self.references {
            let src = refdir.join(&hex[0..2]).join(&hex[2..4]).join(&hex);
            let Ok(meta) = std::fs::metadata(&src) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            if let Some(want) = size
                && meta.len() != want
            {
                continue;
            }
            let dest = self.object_path(oid);
            if let Some(parent) = dest.parent() {
                let _ = self.create_dir_all_with_mode(parent);
            }
            // A hardlink fails across filesystems, so fall back to copying.
            if std::fs::hard_link(&src, &dest).is_ok() || std::fs::copy(&src, &dest).is_ok() {
                let _ = self.set_file_mode(&dest);
                return true;
            }
        }
        false
    }

    /// List every object in the store, with its size on disk.
    ///
    /// Files that aren't named like an OID are skipped. A store that doesn't
    /// exist yet is empty.
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

    /// Open an object for reading.
    ///
    /// Uses fallback stores, and fails with [`io::ErrorKind::NotFound`] if
    /// the object isn't in any of them.
    pub fn open(&self, oid: Oid) -> io::Result<File> {
        let path = self.object_path(oid);
        match File::open(&path) {
            Ok(f) => Ok(f),
            Err(e) if e.kind() == io::ErrorKind::NotFound && oid != Oid::EMPTY => {
                if self.materialize_from_reference(oid, None) {
                    File::open(&path)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Add content to the store, returning its OID and size.
    ///
    /// If the object is already stored, the existing file is left alone, since
    /// it may be a hardlink into a fallback store.
    pub fn insert(&self, src: &mut impl Read) -> Result<(Oid, u64), StoreError> {
        let (oid, size, tmp) = self.stream_to_tmp(src)?;
        if oid != Oid::EMPTY && self.object_path(oid).is_file() {
            drop(tmp);
            return Ok((oid, size));
        }
        self.commit(oid, tmp)?;
        Ok((oid, size))
    }

    /// Add content that should have the OID `expected`, returning its size.
    ///
    /// Fails with [`StoreError::HashMismatch`] if it doesn't, and stores
    /// nothing. Any existing file for the OID is replaced, which is how
    /// `git lfs fetch --refetch` repairs a corrupt copy.
    pub fn insert_verified(&self, expected: Oid, src: &mut impl Read) -> Result<u64, StoreError> {
        let (actual, size, tmp) = self.stream_to_tmp(src)?;
        if actual != expected {
            return Err(StoreError::HashMismatch { expected, actual });
        }
        self.commit(actual, tmp)?;
        Ok(size)
    }

    fn stream_to_tmp(&self, src: &mut impl Read) -> io::Result<(Oid, u64, NamedTempFile)> {
        self.create_dir_all_with_mode(&self.tmp_dir())?;
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
        // The empty object is the null device, so there's nothing to store.
        if oid == Oid::EMPTY {
            return Ok(());
        }
        let dest = self.object_path(oid);
        if let Some(parent) = dest.parent() {
            self.create_dir_all_with_mode(parent)?;
        }
        // Replace any existing file. It either has the same content or is
        // corrupt.
        tmp.persist(&dest).map_err(|e| e.error)?;
        self.set_file_mode(&dest)?;
        Ok(())
    }

    /// Create a directory and its parents, setting the permissions of those
    /// inside the LFS directory.
    fn create_dir_all_with_mode(&self, target: &Path) -> io::Result<()> {
        std::fs::create_dir_all(target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = self.mode_policy.dir_mode();
            // Chmod the LFS directory and each directory below it on the
            // way to `target`. A `target` outside the LFS directory is left
            // alone.
            let mut cursor = self.root.clone();
            if cursor.is_dir() {
                let _ = std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(mode));
            }
            if let Ok(rel) = target.strip_prefix(&self.root) {
                for component in rel.components() {
                    cursor.push(component);
                    if cursor.is_dir() {
                        let _ = std::fs::set_permissions(
                            &cursor,
                            std::fs::Permissions::from_mode(mode),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Create the directory for partial downloads, with the right
    /// permissions.
    pub fn prepare_incomplete_dir(&self) -> io::Result<()> {
        self.create_dir_all_with_mode(&self.incomplete_dir())
    }

    /// Set an object file's permissions. Does nothing on Windows.
    fn set_file_mode(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = self.mode_policy.file_mode();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
        Ok(())
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
        // Neither the wrong OID nor the actual OID should be present:
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
        // 64-char hex name: must not crash or be reported.
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

    /// Build a "source" store with an object pre-installed, plus an
    /// empty "shared" store that references it. Mirrors the
    /// `git clone --shared` setup from t-fetch's init.
    fn shared_fixture() -> (TempDir, Store, Store, Oid) {
        let tmp = TempDir::new().unwrap();
        let source = Store::new(tmp.path().join("src/lfs"));
        let (oid, _) = source.insert(&mut b"abc".as_slice()).unwrap();
        let shared = Store::new(tmp.path().join("shared/lfs"))
            .with_references([source.root().join("objects")]);
        (tmp, source, shared, oid)
    }

    #[test]
    fn contains_finds_object_via_reference() {
        let (_tmp, _source, shared, oid) = shared_fixture();
        // Object lives only in the source's lfs/objects/ at this
        // point. `contains` should report it as present (and fault
        // it in along the way).
        assert!(shared.contains(oid));
        assert!(shared.object_path(oid).is_file());
    }

    #[test]
    fn open_faults_in_from_reference() {
        let (_tmp, _source, shared, oid) = shared_fixture();
        let mut buf = Vec::new();
        shared.open(oid).unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"abc");
        // After open, the object is materialized locally so future
        // reads are independent of the alternate.
        assert!(shared.object_path(oid).is_file());
    }

    #[test]
    fn contains_with_size_rejects_size_mismatch_in_reference() {
        let (_tmp, _source, shared, oid) = shared_fixture();
        // Real size is 3; ask for 4 → reference hit gets rejected.
        assert!(!shared.contains_with_size(oid, 4));
        assert!(!shared.object_path(oid).is_file());
    }

    #[test]
    fn store_without_references_misses() {
        // Sanity: same OID that the shared fixture finds via
        // alternates is genuinely absent in a plain store.
        let (_tmp, store) = fixture();
        let oid = abc_oid();
        assert!(!store.contains(oid));
        assert!(matches!(
            store.open(oid).unwrap_err().kind(),
            io::ErrorKind::NotFound,
        ));
    }

    #[test]
    fn cleanup_ignores_non_ascii_names() {
        let (_tmp, store) = fixture();
        std::fs::create_dir_all(store.tmp_dir()).unwrap();
        // Byte 64 is `-`, but byte 2 is inside a multi-byte character.
        let name = format!("€{}-x", "a".repeat(61));
        std::fs::write(store.tmp_dir().join(name), b"x").unwrap();
        store.cleanup_tmp_objects();
    }
}
