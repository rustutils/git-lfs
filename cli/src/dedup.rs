//! `git lfs dedup`: replace LFS files in the working tree with
//! copy-on-write clones of their stored objects.

use std::fs;
use std::io;
use std::path::Path;

use git_lfs_store::Store;

/// Run `git lfs dedup`, or with `test`, only check for support.
///
/// Messages and their order of checks follow upstream, whose shell tests
/// match them. Refusals exit 2.
pub fn run(cwd: &Path, test: bool) -> Result<u8, Box<dyn std::error::Error>> {
    let lfs_dir = git_lfs_git::lfs_dir(cwd)?;
    let store = Store::new(&lfs_dir);

    if test {
        let tmp = lfs_dir.join("tmp");
        if let Err(e) = fs::create_dir_all(&tmp).and_then(|()| check_clone_support(&tmp)) {
            eprintln!("This system does not support de-duplication: {e}");
            return Ok(2);
        }
        if extensions_configured(cwd) {
            eprintln!("{EXTENSIONS_CONFIGURED}");
            return Ok(2);
        }
        println!("OK: This platform and repository support file de-duplication.");
        return Ok(0);
    }

    if check_clone_support(&git_lfs_git::git_dir(cwd)?).is_err() {
        eprintln!("This system does not support de-duplication.");
        return Ok(2);
    }
    if extensions_configured(cwd) {
        eprintln!("{EXTENSIONS_CONFIGURED}");
        return Ok(2);
    }
    if crate::migrate::working_tree_dirty(cwd)? {
        eprintln!("Working tree is dirty. Please commit or reset your change.");
        return Ok(2);
    }

    let root = git_lfs_git::work_tree_root(cwd)?;
    let (mut count, mut bytes) = (0u64, 0u64);
    for entry in git_lfs_git::scanner::scan_tree(cwd, "HEAD")? {
        for path in &entry.paths {
            let name = path.display();
            match dedup_file(&store, entry.oid, entry.size, &root.join(path)) {
                Ok(()) => {
                    println!("Success: {name} (Size: {})", entry.size);
                    count += 1;
                    bytes += entry.size;
                }
                Err(e) => {
                    eprintln!("Skipped: {name} (Size: {})\n          {e}", entry.size);
                }
            }
        }
    }

    let unit = if bytes == 1 { "byte" } else { "bytes" };
    println!(
        "\n\nFinished successfully.\n  De-duplicated  size: {bytes} {unit}\n                count: {count}"
    );
    Ok(0)
}

const EXTENSIONS_CONFIGURED: &str = "This platform supports file de-duplication, however, \
     Git LFS extensions are configured and therefore de-duplication can not be used.";

/// Whether any `lfs.extension.<name>.*` setting exists.
fn extensions_configured(cwd: &Path) -> bool {
    !git_lfs_git::extension::list_extensions(cwd).is_empty()
}

/// Clone a scratch file within `dir`, to see if its file system can.
fn check_clone_support(dir: &Path) -> io::Result<()> {
    let scratch = tempfile::Builder::new().prefix("dedup").tempdir_in(dir)?;
    let src = scratch.path().join("src");
    fs::write(&src, b"")?;
    reflink_copy::reflink(&src, scratch.path().join("dst"))
}

/// Replace `dest` with a clone of the object `oid`, keeping its mode.
///
/// The clone is made next to `dest` and renamed over it, so a failure
/// leaves the original file as it was.
fn dedup_file(store: &Store, oid: git_lfs_pointer::Oid, size: u64, dest: &Path) -> io::Result<()> {
    if !store.contains_with_size(oid, size) {
        return Err(io::Error::other("Git LFS object file does not exist"));
    }
    let permissions = fs::metadata(dest)?.permissions();
    // The empty object has no file to clone, and an empty file shares
    // nothing anyway.
    if size == 0 {
        return Ok(());
    }
    let dir = dest
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?;
    let scratch = tempfile::Builder::new()
        .prefix(".git-lfs-dedup")
        .tempdir_in(dir)?;
    let clone = scratch.path().join("clone");
    reflink_copy::reflink(store.object_path(oid), &clone)?;
    fs::set_permissions(&clone, permissions)?;
    fs::rename(&clone, dest)
}
