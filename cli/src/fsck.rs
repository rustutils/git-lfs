//! `git lfs fsck` — integrity check for LFS objects + pointers.
//!
//! Two independent checks driven by `--objects` and `--pointers`:
//!
//! - **`--objects`**: for every LFS pointer reachable from the named ref's
//!   history, verify the local store has its bytes and that re-hashing
//!   them matches the pointer's OID. Missing or corrupt files are
//!   reported and (unless `--dry-run`) moved to `<lfs>/bad/<oid>`.
//! - **`--pointers`**: for every blob in the named ref's tree, classify
//!   it against `.gitattributes`. A blob whose path matches `filter=lfs`
//!   is expected to be a canonical pointer; if it parses but isn't
//!   canonical we report `nonCanonicalPointer`, and if it doesn't parse
//!   at all (or is too big to be a pointer) we report
//!   `unexpectedGitObject`.
//!
//! Defaults to running both. Exit code: 0 if everything is fine,
//! 1 otherwise.
//!
//! Out of scope (see NOTES.md): the `<a>..<b>` range form, scanning the
//! index alongside HEAD when no ref is given, `lfs.fetchexclude` honor.

use std::io::Read;
use std::path::Path;

use git_lfs_git::AttrSet;
use git_lfs_git::cat_file::CatFileBatch;
use git_lfs_git::scanner::{scan_pointers, scan_tree_blobs};
use git_lfs_pointer::{MAX_POINTER_SIZE, Oid, Pointer};
use git_lfs_store::Store;
use sha2::{Digest, Sha256};

use crate::fetch::fetch_filter_set;

/// Loosened "inside a repo" check: anything `git rev-parse
/// --absolute-git-dir` can resolve counts. Earlier we required
/// `--is-inside-work-tree`, but that rejected the
/// `GIT_DIR=… GIT_WORK_TREE=… GIT_OBJECT_DIRECTORY=…` invocation in
/// t-fsck 7 (cwd is the parent of the work tree, not inside it). The
/// outside-repo case (test 4) still surfaces because `git_dir` fails
/// when nothing on the cwd path is a git repo and no env vars steer
/// us at one.
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
    /// User-facing error printed verbatim. Used for things like
    /// `Git can't resolve ref: "..."` that the test suite greps.
    #[error("{0}")]
    Other(String),
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
/// problem found, 128 = not in a git repo).
pub fn run(cwd: &Path, refspec: Option<&str>, opts: &Options) -> Result<i32, FsckError> {
    // Outside-a-repo guard. `t-fsck.sh::fsck: outside git repository`
    // greps `Not in a Git repository` on stdout (`2>&1 > fsck.log`
    // captures stdout only) and asserts exit 128.
    if !is_in_git_repo(cwd) {
        println!("Not in a Git repository.");
        return Ok(128);
    }
    let store = Store::new(git_lfs_git::lfs_dir(cwd)?);
    let r = refspec.unwrap_or("HEAD");

    // Validate the ref upfront so a typo surfaces as the upstream-
    // format `Git can't resolve ref: "<r>"` line (t-fsck 16) instead
    // of a stack of internal `git rev-list failed` messages.
    if !crate::fetch::is_resolvable_ref(cwd, r) {
        return Err(FsckError::Other(format!("Git can't resolve ref: {r:?}")));
    }

    let mut corrupt_oids: Vec<Oid> = Vec::new();
    let mut non_canonical: usize = 0;

    if matches!(opts.mode, Mode::Objects | Mode::Both) {
        // Scan the full reachable history; this is what upstream's
        // ScanRef does, and it's what most users mean by "fsck this
        // ref's LFS objects."
        //
        // Honor `lfs.fetchinclude` / `lfs.fetchexclude`: pointers
        // whose working-tree path matches an exclude (or doesn't
        // match an include) are skipped, because the user has
        // explicitly opted out of having those objects locally.
        // `t-fsck.sh::fsck detects invalid objects except in
        // excluded paths` and `t-fetch.sh::fetch with exclude
        // filters in gitconfig` both rely on this.
        let include_set = fetch_filter_set(cwd, "lfs.fetchinclude")?;
        let exclude_set = fetch_filter_set(cwd, "lfs.fetchexclude")?;
        let pointers = scan_pointers(cwd, &[r], &[])?;
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
        // Build attrs from the *tree*, not the working directory: when
        // someone runs `git lfs fsck` against a repo populated only by
        // env vars (`GIT_DIR=… GIT_WORK_TREE=… GIT_OBJECT_DIRECTORY=…`,
        // see t-fsck 7) the work tree may be empty even though HEAD
        // has a `.gitattributes`. Read each `.gitattributes` blob from
        // the tree and feed it to AttrSet at the right directory.
        let attrs = build_tree_attrs(cwd, &blobs, &mut batch)?;
        for blob in &blobs {
            // Symlinks store their target path as the blob content;
            // they're not LFS pointers regardless of `.gitattributes`
            // and upstream's fsck (`fsck does not detect invalid
            // pointers with symlinks`) expects them to pass through.
            if blob.mode == "120000" {
                continue;
            }
            // Use forward slashes for path matching — gix-attributes
            // works in repo-relative `/`-separated paths.
            let path_str = blob.path.to_string_lossy().replace('\\', "/");
            if !attrs.is_lfs_tracked(&path_str) {
                continue;
            }
            // Blobs above the pointer size ceiling can't be pointers; flag
            // and skip the read entirely.
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

/// Walk every `.gitattributes` blob in the tree-blob list and feed its
/// contents to a fresh [`AttrSet`] at the right per-directory base.
/// Unlike [`AttrSet::from_workdir`], this reads from the git tree
/// itself, so an empty / out-of-sync working directory is fine.
/// Falls back silently when a blob can't be read (the only consumer
/// is fsck, which would just report nothing extra rather than fail).
fn build_tree_attrs(
    cwd: &Path,
    blobs: &[git_lfs_git::scanner::TreeBlob],
    batch: &mut CatFileBatch,
) -> std::io::Result<AttrSet> {
    let mut attrs = AttrSet::empty();
    let _ = cwd;
    // Sort by path-component depth so root .gitattributes is added
    // first, mirroring AttrSet::from_workdir's "shallow → deep" order
    // (gix-attributes' last-added wins).
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
        assert_eq!(
            verify_object(&store, oid, 1).unwrap(),
            ObjectVerify::Missing
        );
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
        assert_eq!(
            verify_object(&store, Oid::EMPTY, 0).unwrap(),
            ObjectVerify::Ok
        );
    }
}
