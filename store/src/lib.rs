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

use std::collections::HashMap;
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
///
/// May reference any number of alternate stores — typically the LFS
/// objects of a `git clone --shared` source — and will materialize a
/// hit from one of them into the local store on demand. See
/// [`Store::with_references`].
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    /// Paths to alternate `lfs/objects/` directories. Each maps to a
    /// `.git/objects/info/alternates` entry: when the local store
    /// misses, [`Store::contains_with_size`] / [`Store::open`] walk
    /// these in order and hardlink (or copy) any hit into `root`.
    references: Vec<PathBuf>,
    /// File/directory mode policy for objects committed into the
    /// store. Defaults to "honor process umask"; set via
    /// [`Store::with_shared_repository`] to override (e.g. to apply
    /// `core.sharedRepository=group` semantics).
    mode_policy: ModePolicy,
}

/// File-mode rule used when committing objects and creating their
/// containing directories. Mirrors git's `core.sharedRepository`
/// semantics — see `config/config.go::getMask` upstream.
#[derive(Debug, Clone, Copy)]
struct ModePolicy {
    /// Mask: target file mode is `0o666 & !mask`. None means "no
    /// explicit policy — chmod committed files to `0o666 & !umask`
    /// since the tempfile crate uses mode 0o600 unconditionally."
    mask: u32,
}

impl ModePolicy {
    fn from_umask() -> Self {
        Self {
            mask: process_umask(),
        }
    }

    /// Parse a `core.sharedRepository` config value into a mask.
    /// Recognized: `umask`/`false`/`0`/unset → process umask;
    /// `group`/`true`/`1` → 0o007; `all`/`world`/`everybody`/`2` →
    /// 0o002; any other octal value N → `0o666 & !N`. Unrecognized
    /// strings fall back to umask.
    fn from_shared_repository(value: &str) -> Self {
        let v = value.trim().to_ascii_lowercase();
        let mask = match v.as_str() {
            "group" | "true" | "1" => 0o007,
            "all" | "world" | "everybody" | "2" => 0o002,
            "umask" | "false" | "0" | "" => process_umask(),
            other => {
                // Try octal interpretation. Strip any leading `0` to
                // match git's `strconv.ParseInt(v, 8, ...)` behavior.
                match u32::from_str_radix(other.trim_start_matches('0'), 8) {
                    Ok(mode) if mode <= 0o777 => 0o666 & !mode,
                    _ => process_umask(),
                }
            }
        };
        Self { mask: mask & 0o777 }
    }

    /// Target file mode for committed objects (and the temp files
    /// they're persisted from).
    fn file_mode(self) -> u32 {
        0o666 & !self.mask & 0o777
    }

    /// Target directory mode. Matches git's
    /// `tools.ExecutablePermissions`: copy read bits to execute bits.
    fn dir_mode(self) -> u32 {
        let f = self.file_mode();
        (f | ((f & 0o444) >> 2)) & 0o777
    }
}

/// Read the process umask without permanently changing it. POSIX's
/// `umask` syscall is read-modify-write; the bracketed `(0,prev)`
/// dance is the standard way to capture it without races.
#[cfg(unix)]
fn process_umask() -> u32 {
    // SAFETY: `libc::umask` is signal-safe and thread-safe to call;
    // the brief window where umask is 0 only matters if another
    // thread creates a file in that interval. Stores live for the
    // duration of a single command and are constructed before any
    // worker threads spawn.
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

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("expected OID {expected}, got {actual}")]
    HashMismatch { expected: Oid, actual: Oid },
}

impl Store {
    /// Create a store rooted at the given LFS directory. The directory is not
    /// created eagerly; subdirectories are created on demand as objects land.
    pub fn new(lfs_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: lfs_dir.into(),
            references: Vec::new(),
            mode_policy: ModePolicy::from_umask(),
        }
    }

    /// Apply `core.sharedRepository` semantics to objects this store
    /// commits. `value` is the literal string from `git config`
    /// (`group`, `everybody`, octal `0660`, …); unrecognized values
    /// fall back to honoring the process umask. Resets any prior
    /// policy on this `Store`.
    #[must_use]
    pub fn with_shared_repository(mut self, value: &str) -> Self {
        self.mode_policy = ModePolicy::from_shared_repository(value);
        self
    }

    /// Attach alternate `lfs/objects/` directories that the store may
    /// hardlink-or-copy from when a local lookup misses. Used by
    /// `git clone --shared` setups so the new repo can read the
    /// source's existing LFS objects without re-downloading.
    ///
    /// Pass [`git_lfs_git::lfs_alternate_dirs`](https://docs.rs/git-lfs-git)
    /// (`<git-dir>/objects/info/alternates` resolved to LFS-objects
    /// dirs) at construction.
    #[must_use]
    pub fn with_references(mut self, refs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.references = refs.into_iter().collect();
        self
    }

    /// Root LFS directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding temp files for in-flight inserts.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Directory holding partial / in-progress downloads. Files are
    /// named `<oid>.part` and persist across process invocations so a
    /// later attempt can pick up where a prior one left off (issuing
    /// a `Range:` request). Mirrors upstream's `incomplete/` layout.
    pub fn incomplete_dir(&self) -> PathBuf {
        self.root.join("incomplete")
    }

    /// Path to the partial-download file for `oid`. The file may not
    /// exist; the caller is responsible for creating + writing it.
    pub fn incomplete_path(&self, oid: Oid) -> PathBuf {
        self.incomplete_dir().join(format!("{oid}.part"))
    }

    /// Atomically move a fully-downloaded partial file into its final
    /// object-path location. The caller is responsible for confirming
    /// the file's bytes hash to `oid` first; this is a pure rename.
    /// Clobbers any existing file at the destination — see
    /// [`insert_verified`](Self::insert_verified) for the rationale.
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

    /// Sweep `<root>/tmp/objects/` (upstream's path for in-flight
    /// download temp files: `<oid>-<random>`) and remove any whose
    /// leading 64-char OID is already complete in the store, and
    /// prune anything else under `tmp/` older than an hour.
    ///
    /// Best-effort — the dir not existing, or any individual remove
    /// failing, is silently ignored. Intended to run once per
    /// command invocation, before the command's main work, so an
    /// interrupted prior run doesn't leak temp files indefinitely
    /// (matches upstream's `lfs.cleanupTempFiles` startup task in
    /// `fs/cleanup.go`).
    ///
    /// Per-file rules — mirroring upstream:
    /// 1. Filenames starting with `<64-hex>-` whose object is already
    ///    complete in the store are removed unconditionally
    ///    (interrupted-rename leftovers).
    /// 2. Otherwise, files older than 1 hour are removed — *unless*
    ///    they live in a subdirectory whose own mtime is fresher than
    ///    1 hour, since active processes may have stale-looking files
    ///    they still hold open (hard-linked across repos). Files
    ///    directly under `tmp/` are exempt from the subdir-age
    ///    short-circuit since we modify the top-level tmp dir often
    ///    enough that it would never expire.
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
        // Cache subdir mtimes so the 1-hour exemption check doesn't
        // re-stat the same dir per file.
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
            // Rule 1: "<oid>-..." file whose object is already complete.
            // The filesystem-level cleanup accepts any 64-char prefix
            // (no hex validation) so upstream test sentinels like
            // `good...` / `bad...` round-trip.
            if name_str.len() > 64 && name_str.as_bytes().get(64) == Some(&b'-') {
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
            // Rule 2a: skip files in young subdirectories. The
            // top-level tmp/ itself is exempt (otherwise it'd never
            // expire). Cache the dir's mtime so we don't restat per
            // file.
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
            // Rule 2b: remove file if older than the cutoff.
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            if mtime < cutoff {
                let _ = std::fs::remove_file(&path);
            }
        }
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
    /// OID is always considered present. If the local copy is missing but
    /// an alternate store has the object, materializes it locally first.
    pub fn contains(&self, oid: Oid) -> bool {
        if oid == Oid::EMPTY {
            return true;
        }
        if self.object_path(oid).is_file() {
            return true;
        }
        self.materialize_from_reference(oid, None)
    }

    /// `true` if the object is present and its on-disk size matches `size`.
    /// Used to detect partial/corrupted local copies. Like
    /// [`contains`](Self::contains), will fault in a matching alternate-store
    /// object on demand.
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

    /// Walk reference stores looking for `oid`; the first hit (matching
    /// `size` if specified) is hardlinked — or copied, on cross-device
    /// fallback — into the local store. Returns `true` if the object
    /// is now present locally as a result.
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
            // Hardlink first (free, O(1), shares inode); fall back to
            // copy on EXDEV / NotSupported (e.g. alternate on a
            // different filesystem).
            if std::fs::hard_link(&src, &dest).is_ok() || std::fs::copy(&src, &dest).is_ok() {
                let _ = self.set_file_mode(&dest);
                return true;
            }
        }
        false
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
    /// if the object isn't in the store. Faults in from a reference
    /// store if needed.
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

    /// Stream `src` into the store, computing SHA-256 as we go.
    /// Returns the resulting OID and byte count.
    ///
    /// This is the clean-filter path: we don't know the OID until after the
    /// content is hashed.
    ///
    /// If the resulting OID is already present locally, the temp file is
    /// dropped without persisting. The store is content-addressed, so an
    /// existing file at that path is necessarily the same bytes; skipping
    /// `tmp.persist` here preserves any hardlink already at the
    /// destination (a rename swaps a fresh inode in, which would break
    /// the link to the alternate-store source).
    pub fn insert(&self, src: &mut impl Read) -> Result<(Oid, u64), StoreError> {
        let (oid, size, tmp) = self.stream_to_tmp(src)?;
        if oid != Oid::EMPTY && self.object_path(oid).is_file() {
            drop(tmp);
            return Ok((oid, size));
        }
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
        // The empty object lives at /dev/null — never persist it.
        if oid == Oid::EMPTY {
            return Ok(());
        }
        let dest = self.object_path(oid);
        if let Some(parent) = dest.parent() {
            self.create_dir_all_with_mode(parent)?;
        }
        // Atomic rename, *clobbering* any existing file at the target
        // path. The store is content-addressed: anything already there
        // is either the same content (no-op overwrite) or corrupt
        // (truncated, half-written) — and the latter is exactly what
        // `git lfs fetch --refetch` exists to recover from.
        tmp.persist(&dest).map_err(|e| e.error)?;
        self.set_file_mode(&dest)?;
        Ok(())
    }

    /// `mkdir -p` walking the path, chmoding each directory under
    /// `root` to the configured `mode_policy.dir_mode()`. Components
    /// outside `root` (e.g. the user's home directory) are left
    /// alone — we only own the LFS subtree.
    fn create_dir_all_with_mode(&self, target: &Path) -> io::Result<()> {
        std::fs::create_dir_all(target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = self.mode_policy.dir_mode();
            // Walk from root → target, chmoding each component that
            // exists under our LFS root. The check `starts_with(root)`
            // guards against calls with an unrelated path.
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

    /// Ensure `<root>/incomplete/` exists with the right directory
    /// mode. Public so the transfer crate can stage `.part` files
    /// under it without bypassing the `core.sharedRepository` policy.
    pub fn prepare_incomplete_dir(&self) -> io::Result<()> {
        self.create_dir_all_with_mode(&self.incomplete_dir())
    }

    /// Chmod a committed object file to the configured file mode.
    /// No-op on non-unix.
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
        // point — `contains` should report it as present (and fault
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
}
