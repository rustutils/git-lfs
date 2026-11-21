//! Find the LFS pointers in history, a tree, the index, or the stash.
//!
//! Most scanners list candidate blobs with git, drop those too large to be
//! a pointer using [`CatFileBatchCheck`], then read the rest with
//! [`CatFileBatch`] and keep the ones that parse as a [`Pointer`].
//!
//! - [`scan_pointers`] walks the history reachable from some refs, with
//!   [`rev_list`](crate::rev_list::rev_list), and returns each LFS object
//!   once.
//! - [`scan_tree`] returns one entry per pointer path in a single tree.
//! - [`scan_index_lfs`] reads the index entries whose path has the
//!   `filter=lfs` attribute.
//! - [`scan_index_pointers`] reads what's staged or changed relative to a
//!   ref.
//! - [`scan_previous_versions`] and [`scan_stashed`] read pointer text out
//!   of `git log -p` instead of reading blobs.
//! - [`scan_tree_blobs`] lists every blob in a tree, pointer or not.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use git_lfs_pointer::{Extension, MAX_POINTER_SIZE, Oid, Pointer};

use crate::Error;
use crate::cat_file::{CatFileBatch, CatFileBatchCheck, CatFileHeader};

/// One LFS pointer discovered by the scanner.
#[derive(Debug, Clone)]
pub struct PointerEntry {
    /// The LFS object's OID, from the pointer's `oid` line.
    pub oid: Oid,
    /// The LFS object's size in bytes, from the pointer's `size` line.
    pub size: u64,
    /// The first path the pointer was found at.
    ///
    /// Fine for progress output. To filter by path, use
    /// [`paths`](Self::paths).
    pub path: Option<PathBuf>,
    /// Every path the pointer was found at.
    ///
    /// Filter by path (`--include`, `--exclude`) against all of these, not
    /// just [`path`](Self::path). Otherwise an object stored at two paths is
    /// dropped whenever the first path seen happens to be excluded. Never
    /// empty when `path` is `Some`.
    pub paths: Vec<PathBuf>,
    /// Whether the pointer text was exactly the canonical encoding.
    ///
    /// `git lfs fsck --pointers` reports pointers that parse but aren't
    /// canonical.
    pub canonical: bool,
    /// The pointer's extensions, sorted by priority as in
    /// [`Pointer::extensions`].
    pub extensions: Vec<Extension>,
}

/// Find the LFS pointers reachable from `include` but not from `exclude`.
///
/// Like upstream's `ScanRefs`, this looks at every blob in history, not
/// just the tips, so it also finds pointers that were later modified or
/// deleted. Each LFS object appears once, with every path it was seen at
/// in [`PointerEntry::paths`]. The order of the result is unspecified.
pub fn scan_pointers(
    cwd: &Path,
    include: &[&str],
    exclude: &[&str],
) -> Result<Vec<PointerEntry>, Error> {
    scan_pointers_with_args(cwd, include, exclude, &[])
}

/// [`scan_pointers`] with extra `git rev-list` arguments.
///
/// See [`rev_list_with_args`](crate::rev_list::rev_list_with_args).
pub fn scan_pointers_with_args(
    cwd: &Path,
    include: &[&str],
    exclude: &[&str],
    extra_cmdline_args: &[&str],
) -> Result<Vec<PointerEntry>, Error> {
    let entries = crate::rev_list::rev_list_with_args(cwd, include, exclude, extra_cmdline_args)?;

    let mut bcheck = CatFileBatchCheck::spawn(cwd)?;
    let mut candidates: Vec<(String, Option<String>)> = Vec::new();
    for entry in entries {
        match bcheck.check(&entry.oid)? {
            CatFileHeader::Found { kind, size, .. }
                if kind == "blob" && (size as usize) < MAX_POINTER_SIZE =>
            {
                candidates.push((entry.oid, entry.name));
            }
            _ => {}
        }
    }
    drop(bcheck);

    let mut batch = CatFileBatch::spawn(cwd)?;
    let mut by_oid: std::collections::HashMap<Oid, usize> = std::collections::HashMap::new();
    let mut out: Vec<PointerEntry> = Vec::new();
    for (oid, name) in candidates {
        let Some(blob) = batch.read(&oid)? else {
            continue;
        };
        let Ok(pointer) = Pointer::parse(&blob.content) else {
            continue;
        };
        let path_buf = name.map(PathBuf::from);
        if let Some(&idx) = by_oid.get(&pointer.oid) {
            if let Some(p) = path_buf
                && !out[idx].paths.contains(&p)
            {
                out[idx].paths.push(p);
            }
            continue;
        }
        let paths: Vec<PathBuf> = path_buf.iter().cloned().collect();
        by_oid.insert(pointer.oid, out.len());
        out.push(PointerEntry {
            oid: pointer.oid,
            size: pointer.size,
            path: path_buf,
            paths,
            canonical: pointer.canonical,
            extensions: pointer.extensions.clone(),
        });
    }
    Ok(out)
}

/// Find the LFS pointers in the index whose path has the `filter=lfs`
/// attribute.
///
/// Runs `git ls-files --stage -z -- :(attr:filter=lfs)` from the top of
/// the working tree, or from the Git directory in a bare repository.
/// Symlinks are skipped. In a sparse checkout or a bare repository,
/// entries whose parent directory doesn't exist in the working tree are
/// skipped too. Each LFS object appears once.
///
/// Upstream's pull and fetch list pointers this way on Git 2.42 and later.
/// It avoids a history walk, which is slow in a partial clone made with
/// `--filter=tree:0`.
pub fn scan_index_lfs(cwd: &Path) -> Result<Vec<PointerEntry>, Error> {
    // `git ls-files` run from a subdirectory lists only that subdirectory.
    let scan_cwd = match crate::run_git(cwd, &["rev-parse", "--show-toplevel"]) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => crate::run_git(cwd, &["rev-parse", "--absolute-git-dir"])
            .map(PathBuf::from)
            .unwrap_or_else(|_| cwd.to_path_buf()),
    };
    // Cone-mode sparse checkout leaves out-of-cone directories
    // uncreated, so a missing parent directory means "out of cone". In a
    // bare repository only top-level entries pass. In an ordinary
    // checkout the check stays off, so a file the user deleted is still
    // restored by `git lfs pull`.
    let filter_by_parent_dir = is_bare_repo(&scan_cwd) || is_sparse_checkout(&scan_cwd);

    let out = Command::new("git")
        .arg("-C")
        .arg(&scan_cwd)
        .args(["ls-files", "--stage", "-z", "--", ":(attr:filter=lfs)"])
        .output()?;
    if !out.status.success() {
        return Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }

    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    for record in out.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let s = match std::str::from_utf8(record) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // `<mode> SP <oid> SP <stage>\t<path>`
        let Some((meta, path)) = s.split_once('\t') else {
            continue;
        };
        let parts: Vec<&str> = meta.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let mode = parts[0];
        let oid = parts[1];
        if mode == "120000" {
            continue;
        }
        let path = PathBuf::from(path);
        if filter_by_parent_dir
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !scan_cwd.join(parent).is_dir()
        {
            continue;
        }
        candidates.push((oid.to_string(), path));
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut batch = CatFileBatch::spawn(cwd)?;
    let mut by_oid: std::collections::HashMap<Oid, usize> = std::collections::HashMap::new();
    let mut out: Vec<PointerEntry> = Vec::new();
    for (oid, path) in candidates {
        let Some(blob) = batch.read(&oid)? else {
            continue;
        };
        let Ok(pointer) = Pointer::parse(&blob.content) else {
            continue;
        };
        if let Some(&idx) = by_oid.get(&pointer.oid) {
            if !out[idx].paths.contains(&path) {
                out[idx].paths.push(path);
            }
            continue;
        }
        by_oid.insert(pointer.oid, out.len());
        out.push(PointerEntry {
            oid: pointer.oid,
            size: pointer.size,
            path: Some(path.clone()),
            paths: vec![path],
            canonical: pointer.canonical,
            extensions: pointer.extensions.clone(),
        });
    }
    Ok(out)
}

fn is_bare_repo(cwd: &Path) -> bool {
    crate::run_git(cwd, &["rev-parse", "--is-bare-repository"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

fn is_sparse_checkout(cwd: &Path) -> bool {
    crate::run_git(cwd, &["config", "--get", "core.sparseCheckout"])
        .map(|s| s.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// One blob from [`scan_tree_blobs`], whether or not it's a pointer.
#[derive(Debug, Clone)]
pub struct TreeBlob {
    /// Path of the blob in the tree.
    pub path: PathBuf,
    /// Git object ID of the blob, in hex.
    pub blob_oid: String,
    /// Size of the blob in bytes.
    pub size: u64,
    /// Tree entry mode in octal, such as `100644`, or `120000` for a
    /// symlink.
    pub mode: String,
}

/// List every blob in the tree at `reference`, pointer or not.
///
/// `reference` may also be a range such as `HEAD^..HEAD`. Then the trees of
/// all commits in the range are listed, and each path and blob pair
/// appears once.
pub fn scan_tree_blobs(cwd: &Path, reference: &str) -> Result<Vec<TreeBlob>, Error> {
    if reference.contains("..") {
        return scan_blobs_in_range(cwd, reference);
    }
    scan_tree_blobs_for_ref(cwd, reference)
}

fn scan_tree_blobs_for_ref(cwd: &Path, reference: &str) -> Result<Vec<TreeBlob>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-tree", "--full-tree", "-r", "-z", reference])
        .output()?;
    if !out.status.success() {
        return Err(Error::Failed(format!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut bcheck = CatFileBatchCheck::spawn(cwd)?;
    let mut blobs = Vec::new();
    for record in out.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let s = std::str::from_utf8(record)
            .map_err(|e| Error::Failed(format!("ls-tree: non-utf8 record: {e}")))?;
        let (header, path) = s
            .split_once('\t')
            .ok_or_else(|| Error::Failed(format!("ls-tree: malformed record {s:?}")))?;
        let mut parts = header.split_whitespace();
        let mode = parts
            .next()
            .ok_or_else(|| Error::Failed(format!("ls-tree: missing mode in {s:?}")))?;
        let kind = parts.next();
        let oid = parts
            .next()
            .ok_or_else(|| Error::Failed(format!("ls-tree: missing oid in {s:?}")))?;
        if kind != Some("blob") {
            continue;
        }
        if let CatFileHeader::Found { kind, size, .. } = bcheck.check(oid)?
            && kind == "blob"
        {
            blobs.push(TreeBlob {
                path: PathBuf::from(path),
                blob_oid: oid.to_owned(),
                size,
                mode: mode.to_owned(),
            });
        }
    }
    Ok(blobs)
}

/// List the blobs in the tree of every commit in `range`, each path and
/// blob pair once.
fn scan_blobs_in_range(cwd: &Path, range: &str) -> Result<Vec<TreeBlob>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-list", range])
        .output()?;
    if !out.status.success() {
        return Err(Error::Failed(format!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut seen: std::collections::HashSet<(PathBuf, String)> = std::collections::HashSet::new();
    let mut all = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let commit = line.trim();
        if commit.is_empty() {
            continue;
        }
        for blob in scan_tree_blobs_for_ref(cwd, commit)? {
            if seen.insert((blob.path.clone(), blob.blob_oid.clone())) {
                all.push(blob);
            }
        }
    }
    Ok(all)
}

/// Find the LFS pointers in the tree at `reference`, one entry per path.
///
/// Unlike [`scan_pointers`], this doesn't walk history, and an LFS object
/// stored at several paths gets an entry for each, like upstream's
/// `ScanTree`.
pub fn scan_tree(cwd: &Path, reference: &str) -> Result<Vec<PointerEntry>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-tree", "--full-tree", "-r", "-z", reference])
        .output()?;
    if !out.status.success() {
        return Err(Error::Failed(format!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    // Records are `<mode> <type> <oid>\t<path>`.
    let mut bcheck = CatFileBatchCheck::spawn(cwd)?;
    let mut candidates: Vec<(String, String)> = Vec::new();
    for record in out.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let s = std::str::from_utf8(record)
            .map_err(|e| Error::Failed(format!("ls-tree: non-utf8 record: {e}")))?;
        let (header, path) = s
            .split_once('\t')
            .ok_or_else(|| Error::Failed(format!("ls-tree: malformed record {s:?}")))?;
        let mut parts = header.split_whitespace();
        let _mode = parts.next();
        let kind = parts.next();
        let oid = parts
            .next()
            .ok_or_else(|| Error::Failed(format!("ls-tree: missing oid in {s:?}")))?;
        if kind != Some("blob") {
            continue;
        }
        if let CatFileHeader::Found { kind, size, .. } = bcheck.check(oid)?
            && kind == "blob"
            && (size as usize) < MAX_POINTER_SIZE
        {
            candidates.push((oid.to_owned(), path.to_owned()));
        }
    }
    drop(bcheck);

    let mut batch = CatFileBatch::spawn(cwd)?;
    let mut entries = Vec::new();
    for (oid, path) in candidates {
        let Some(blob) = batch.read(&oid)? else {
            continue;
        };
        let Ok(pointer) = Pointer::parse(&blob.content) else {
            continue;
        };
        let path_buf = PathBuf::from(path);
        entries.push(PointerEntry {
            oid: pointer.oid,
            size: pointer.size,
            path: Some(path_buf.clone()),
            paths: vec![path_buf],
            canonical: pointer.canonical,
            extensions: pointer.extensions.clone(),
        });
    }
    Ok(entries)
}

/// Find the LFS pointers in the index that differ from `reference`.
///
/// Runs `git diff-index` and `git diff-index --cached` against
/// `reference`, like upstream's `scanIndex` in `lfs/gitscanner_index.go`.
/// Content that git hasn't hashed (an unstaged edit) has no blob to read,
/// so in practice this finds staged pointers. Deletions, symlinks, and
/// submodules are skipped. Each LFS object appears once. If `reference`
/// doesn't resolve, as in a repository with no commits, the result is
/// empty.
pub fn scan_index_pointers(cwd: &Path, reference: &str) -> Result<Vec<PointerEntry>, Error> {
    let scan_cwd = match crate::run_git(cwd, &["rev-parse", "--show-toplevel"]) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => crate::run_git(cwd, &["rev-parse", "--absolute-git-dir"])
            .map(PathBuf::from)
            .unwrap_or_else(|_| cwd.to_path_buf()),
    };

    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: std::collections::HashSet<(String, PathBuf)> = std::collections::HashSet::new();
    for cached_arg in [&[][..], &["--cached"][..]] {
        let mut args = vec!["diff-index", "-z"];
        args.extend_from_slice(cached_arg);
        args.push(reference);
        let out = Command::new("git")
            .arg("-C")
            .arg(&scan_cwd)
            .args(&args)
            .output()?;
        if !out.status.success() {
            // Most likely `reference` doesn't exist yet.
            continue;
        }
        // Records are `:<src_mode> <dst_mode> <src_sha> <dst_sha>
        // <status>\0<path>\0`, with a second path for renames (R) and
        // copies (C). See `crate::diff_index` for a full parser.
        let bytes = &out.stdout;
        let mut i = 0;
        while i < bytes.len() {
            let meta_end = bytes[i..]
                .iter()
                .position(|&b| b == 0)
                .map(|p| i + p)
                .unwrap_or(bytes.len());
            let Ok(meta) = std::str::from_utf8(&bytes[i..meta_end]) else {
                i = meta_end + 1;
                continue;
            };
            i = meta_end + 1;
            let parts: Vec<&str> = meta.trim_start_matches(':').split_whitespace().collect();
            if parts.len() < 5 {
                continue;
            }
            let dst_mode = parts[1];
            let dst_sha = parts[3];
            let status = parts[4];
            // Skip symlinks (120000), submodules (160000), deletions,
            // and an all-zero dst sha, which means the working tree
            // content differs and git hasn't hashed it.
            if dst_mode == "120000"
                || dst_mode == "160000"
                || status.starts_with('D')
                || dst_sha.bytes().all(|b| b == b'0')
            {
                // Still consume the paths.
                let path_count = if status.starts_with('R') || status.starts_with('C') {
                    2
                } else {
                    1
                };
                for _ in 0..path_count {
                    let end = bytes[i..]
                        .iter()
                        .position(|&b| b == 0)
                        .map(|p| i + p)
                        .unwrap_or(bytes.len());
                    i = end + 1;
                }
                continue;
            }
            // Keep the last path, which is the destination.
            let path_count = if status.starts_with('R') || status.starts_with('C') {
                2
            } else {
                1
            };
            let mut path: PathBuf = PathBuf::new();
            for n in 0..path_count {
                let end = bytes[i..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| i + p)
                    .unwrap_or(bytes.len());
                if n + 1 == path_count {
                    path = PathBuf::from(String::from_utf8_lossy(&bytes[i..end]).into_owned());
                }
                i = end + 1;
            }
            let key = (dst_sha.to_owned(), path.clone());
            if seen.insert(key) {
                candidates.push((dst_sha.to_owned(), path));
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut bcheck = CatFileBatchCheck::spawn(cwd)?;
    let mut sized: Vec<(String, PathBuf)> = Vec::new();
    for (oid, path) in candidates {
        match bcheck.check(&oid)? {
            CatFileHeader::Found { kind, size, .. }
                if kind == "blob" && (size as usize) < MAX_POINTER_SIZE =>
            {
                sized.push((oid, path));
            }
            _ => {}
        }
    }
    drop(bcheck);

    let mut batch = CatFileBatch::spawn(cwd)?;
    let mut by_oid: std::collections::HashMap<Oid, usize> = std::collections::HashMap::new();
    let mut out: Vec<PointerEntry> = Vec::new();
    for (oid, path) in sized {
        let Some(blob) = batch.read(&oid)? else {
            continue;
        };
        let Ok(pointer) = Pointer::parse(&blob.content) else {
            continue;
        };
        if let Some(&idx) = by_oid.get(&pointer.oid) {
            if !out[idx].paths.contains(&path) {
                out[idx].paths.push(path);
            }
            continue;
        }
        by_oid.insert(pointer.oid, out.len());
        out.push(PointerEntry {
            oid: pointer.oid,
            size: pointer.size,
            path: Some(path.clone()),
            paths: vec![path],
            canonical: pointer.canonical,
            extensions: pointer.extensions.clone(),
        });
    }
    Ok(out)
}

/// Find the LFS pointers added by every stash entry.
///
/// Covers the stashed working tree, the stashed index, and the untracked
/// files of `git stash -u`, like upstream's `scanStashed` in
/// `lfs/gitscanner_log.go`. Returns an empty list when there is no stash.
/// The same LFS object can appear more than once.
pub fn scan_stashed(cwd: &Path) -> Result<Vec<PointerEntry>, Error> {
    let stash_shas: Vec<String> = match Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["log", "-g", "--format=%h", "refs/stash", "--"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => return Ok(Vec::new()),
    };
    if stash_shas.is_empty() {
        return Ok(Vec::new());
    }
    // A stash entry is a merge commit whose first parent is HEAD at
    // stash time. Its other parents are the index commit and, with
    // `git stash -u`, the untracked files commit. The first pass, with
    // `-m --first-parent`, diffs the stash commit against HEAD. The
    // second pass skips merge diffs, so it shows the index and untracked
    // commits.
    let mut entries: Vec<PointerEntry> = Vec::new();
    for extra in [&["-m", "--first-parent"][..], &[][..]] {
        let mut args: Vec<String> = vec!["log".into()];
        for a in extra {
            args.push((*a).to_owned());
        }
        for a in [
            "--no-ext-diff",
            "--no-textconv",
            "--color=never",
            "-G",
            "oid sha256:",
            "-p",
            "-U12",
            "--format=lfs-commit-sha: %H %P",
        ] {
            args.push(a.to_owned());
        }
        for sha in &stash_shas {
            args.push(format!("{sha}^..{sha}"));
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut child = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(&arg_refs)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let mut parser = LogScanner::new(LogDiffDirection::Additions);
        for line in BufReader::new(stdout).lines() {
            let line = line?;
            if let Some(entry) = parser.feed(&line) {
                entries.push(entry);
            }
        }
        if let Some(entry) = parser.flush() {
            entries.push(entry);
        }
        // Upstream's `scanStashed` ignores this exit status too.
        let _ = child.wait();
    }
    Ok(entries)
}

/// Find the LFS pointers that commits on `reference` since `since`
/// replaced or deleted.
///
/// Reads the removed side of `git log -p` diffs, like upstream's
/// `logPreviousSHAs` in `lfs/gitscanner_log.go`. Fetch uses this for
/// `lfs.fetchrecentcommitsdays` and prune to decide what to keep. The
/// same LFS object can appear more than once.
pub fn scan_previous_versions(
    cwd: &Path,
    reference: &str,
    since: SystemTime,
) -> Result<Vec<PointerEntry>, Error> {
    let since_unix = since
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let since_arg = format!("--since=@{since_unix}");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "log",
            "--no-ext-diff",
            "--no-textconv",
            "--color=never",
            "-G",
            "oid sha256:",
            "-p",
            "-U12",
            "--format=lfs-commit-sha: %H %P",
            &since_arg,
            reference,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let mut parser = LogScanner::new(LogDiffDirection::Deletions);
    let mut entries = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Some(entry) = parser.feed(&line) {
            entries.push(entry);
        }
    }
    if let Some(entry) = parser.flush() {
        entries.push(entry);
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(Error::Failed(format!(
            "git log failed: exit {:?}",
            status.code()
        )));
    }
    Ok(entries)
}

/// Which side of a unified diff to capture pointer bodies from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogDiffDirection {
    /// `+` lines, the file's state after the commit.
    Additions,
    /// `-` lines, the file's state before the commit.
    Deletions,
}

/// Parser for `git log -p --format=lfs-commit-sha: %H %P` output, fed one
/// line at a time.
///
/// Mirrors upstream's `logScanner` in `lfs/gitscanner_log.go`.
struct LogScanner {
    direction: LogDiffDirection,
    /// Path of the file being diffed, or `None` before the first file
    /// header or when the header couldn't be parsed.
    current_filename: Option<String>,
    /// Pointer lines seen so far for the current file, without their diff
    /// marker.
    pointer_data: Vec<u8>,
}

impl LogScanner {
    fn new(direction: LogDiffDirection) -> Self {
        Self {
            direction,
            current_filename: None,
            pointer_data: Vec::new(),
        }
    }

    /// Feed one line of output.
    ///
    /// At a commit or file header, returns the previous file's pointer if
    /// it parsed.
    fn feed(&mut self, line: &str) -> Option<PointerEntry> {
        if line.starts_with("lfs-commit-sha: ") {
            return self.flush();
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let entry = self.flush();
            self.current_filename = parse_diff_git_header(rest, self.direction);
            return entry;
        }
        if let Some(rest) = line.strip_prefix("diff --cc ") {
            let entry = self.flush();
            self.current_filename = Some(rest.to_owned());
            return entry;
        }
        if self.current_filename.is_some() && is_pointer_data_line(line, self.direction) {
            self.pointer_data.extend_from_slice(&line.as_bytes()[1..]);
            self.pointer_data.push(b'\n');
        }
        None
    }

    /// Parse the buffered lines as a pointer and clear the buffer.
    ///
    /// Call once more after the last line.
    fn flush(&mut self) -> Option<PointerEntry> {
        if self.pointer_data.is_empty() {
            return None;
        }
        let parsed = Pointer::parse(&self.pointer_data);
        let path = self.current_filename.as_ref().map(PathBuf::from);
        self.pointer_data.clear();
        let pointer = parsed.ok()?;
        Some(PointerEntry {
            oid: pointer.oid,
            size: pointer.size,
            paths: path.iter().cloned().collect(),
            path,
            canonical: pointer.canonical,
            extensions: pointer.extensions,
        })
    }
}

/// Whether a diff line is a pointer line on the side `dir` reads.
///
/// Context lines count for both sides, since an edit usually changes only
/// the `oid` and `size` lines. The callers ask for `-U12` so the rest of
/// the pointer, extensions included, is in the context.
fn is_pointer_data_line(line: &str, dir: LogDiffDirection) -> bool {
    let mut chars = line.chars();
    let Some(marker) = chars.next() else {
        return false;
    };
    let dir_match = matches!(
        (marker, dir),
        ('+', LogDiffDirection::Additions) | ('-', LogDiffDirection::Deletions) | (' ', _)
    );
    if !dir_match {
        return false;
    }
    let body = chars.as_str();
    body.starts_with("version https://git-lfs")
        || body.starts_with("oid sha256")
        || body.starts_with("size")
        || body.starts_with("ext-")
}

/// Get the path from the rest of a `diff --git a/<old> b/<new>` header.
///
/// Returns the new path for additions and the old path for deletions.
/// Like upstream's `a\/(.+?)\s+"?b\/(.+)`, the old path ends at the first
/// whitespace followed by `b/`, so paths may contain spaces. Paths that
/// git quotes (control characters, and non-ASCII unless `core.quotePath`
/// is off) are returned still quoted and escaped.
fn parse_diff_git_header(rest: &str, dir: LogDiffDirection) -> Option<String> {
    let rest = rest.trim();
    let after_a = rest.strip_prefix('"').unwrap_or(rest).strip_prefix("a/")?;
    // Skip the first character: the old path can't be empty.
    for (i, c) in after_a.char_indices().skip(1) {
        if !c.is_whitespace() {
            continue;
        }
        let tail = after_a[i..].trim_start();
        if let Some(path_b) = tail.strip_prefix('"').unwrap_or(tail).strip_prefix("b/") {
            let path = match dir {
                LogDiffDirection::Additions => path_b,
                LogDiffDirection::Deletions => &after_a[..i],
            };
            return Some(path.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::commit_helper::*;

    /// Build a canonical pointer text for a known content. Mirrors what
    /// `git lfs clean` would emit, so we don't need to wire the filter
    /// crate into git's tests.
    fn pointer_text(content: &[u8]) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let oid_bytes: [u8; 32] = Sha256::digest(content).into();
        let oid_hex = oid_bytes.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        });
        format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid_hex}\nsize {}\n",
            content.len()
        )
        .into_bytes()
    }

    #[test]
    fn empty_repo_returns_no_pointers() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"plain content");
        let result = scan_pointers(repo.path(), &["HEAD"], &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn finds_pointer_blobs_skips_plain_blobs() {
        let repo = init_repo();
        // Plain content and an LFS pointer side by side.
        commit_file(&repo, "plain.txt", b"just text");
        let pointer = pointer_text(b"this would be the actual binary content");
        commit_file(&repo, "big.bin", &pointer);

        let result = scan_pointers(repo.path(), &["HEAD"], &[]).unwrap();
        assert_eq!(result.len(), 1, "{result:?}");
        assert_eq!(
            result[0].size,
            b"this would be the actual binary content".len() as u64,
        );
        assert_eq!(result[0].path.as_deref(), Some(Path::new("big.bin")));
    }

    #[test]
    fn dedups_same_lfs_oid_in_multiple_paths() {
        let repo = init_repo();
        let pointer = pointer_text(b"shared payload");
        commit_file(&repo, "first.bin", &pointer);
        commit_file(&repo, "second.bin", &pointer);

        let result = scan_pointers(repo.path(), &["HEAD"], &[]).unwrap();
        // Same content → same pointer text → same git blob OID, but we
        // also want to verify dedup at the LFS-OID layer.
        assert_eq!(result.len(), 1, "{result:?}");
    }

    #[test]
    fn finds_pointers_in_history_not_just_tip() {
        let repo = init_repo();
        // A pointer that is later overwritten by plain content. ScanRefs
        // semantics require we still find it, since older commits are part
        // of the history reachable from HEAD.
        let pointer = pointer_text(b"deleted later");
        commit_file(&repo, "x.bin", &pointer);
        commit_file(&repo, "x.bin", b"plain text now");

        let result = scan_pointers(repo.path(), &["HEAD"], &[]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].size, b"deleted later".len() as u64);
    }

    #[test]
    fn excludes_filter_history_walk() {
        let repo = init_repo();
        commit_file(&repo, "old.bin", &pointer_text(b"old payload"));
        let first = head_oid(&repo);
        commit_file(&repo, "new.bin", &pointer_text(b"new payload"));

        // Include HEAD, exclude the first commit → only new.bin's pointer.
        let result = scan_pointers(repo.path(), &["HEAD"], &[&first]).unwrap();
        assert_eq!(result.len(), 1, "{result:?}");
        assert_eq!(result[0].size, b"new payload".len() as u64);
    }

    #[test]
    fn skips_blobs_that_look_like_pointers_but_dont_parse() {
        let repo = init_repo();
        // Small, but malformed pointer-shaped content.
        commit_file(&repo, "fake.bin", b"version foo\nbut not really a pointer");

        let result = scan_pointers(repo.path(), &["HEAD"], &[]).unwrap();
        assert!(result.is_empty(), "{result:?}");
    }

    #[test]
    fn scan_tree_returns_only_tree_entries_not_history() {
        let repo = init_repo();
        // A pointer that exists historically but is gone at HEAD must
        // NOT show up in scan_tree (this is the point of the helper:
        // ls-files should only see what's in the named tree).
        let pointer = pointer_text(b"deleted later");
        commit_file(&repo, "x.bin", &pointer);
        commit_file(&repo, "x.bin", b"plain text now");

        let result = scan_tree(repo.path(), "HEAD").unwrap();
        assert!(result.is_empty(), "{result:?}");
    }

    #[test]
    fn scan_tree_emits_one_entry_per_path_not_per_oid() {
        let repo = init_repo();
        // Same pointer at two paths in the current tree → two entries.
        // (scan_pointers would dedupe to one, scan_tree must not.)
        let pointer = pointer_text(b"shared payload");
        commit_file(&repo, "first.bin", &pointer);
        commit_file(&repo, "second.bin", &pointer);

        let mut result = scan_tree(repo.path(), "HEAD").unwrap();
        result.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(result.len(), 2, "{result:?}");
        assert_eq!(result[0].path.as_deref(), Some(Path::new("first.bin")));
        assert_eq!(result[1].path.as_deref(), Some(Path::new("second.bin")));
        // Same OID under both paths.
        assert_eq!(result[0].oid, result[1].oid);
    }

    #[test]
    fn scan_tree_skips_plain_blobs_and_keeps_pointers() {
        let repo = init_repo();
        commit_file(&repo, "plain.txt", b"just text");
        let pointer = pointer_text(b"binary content");
        commit_file(&repo, "big.bin", &pointer);

        let result = scan_tree(repo.path(), "HEAD").unwrap();
        assert_eq!(result.len(), 1, "{result:?}");
        assert_eq!(result[0].path.as_deref(), Some(Path::new("big.bin")));
    }

    #[test]
    fn scan_tree_unknown_ref_errors() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"x");
        let err = scan_tree(repo.path(), "does-not-exist").unwrap_err();
        match err {
            Error::Failed(msg) => assert!(
                msg.contains("does-not-exist") || msg.contains("Not a valid"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected Failed, got {err:?}"),
        }
    }

    fn feed_log<'a, I: IntoIterator<Item = &'a str>>(
        dir: LogDiffDirection,
        lines: I,
    ) -> Vec<PointerEntry> {
        let mut s = LogScanner::new(dir);
        let mut out = Vec::new();
        for line in lines {
            if let Some(e) = s.feed(line) {
                out.push(e);
            }
        }
        if let Some(e) = s.flush() {
            out.push(e);
        }
        out
    }

    #[test]
    fn log_scanner_extracts_deleted_pointer_body() {
        // Two commits: first adds the pointer (its old state at HEAD~1
        // is empty), second replaces it with a different OID. The
        // Deletions side captures the second diff's `-` lines = the
        // pointer at HEAD~1.
        let lines = [
            "lfs-commit-sha: cccccccccccccccccccccccccccccccccccccccc bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "diff --git a/foo.bin b/foo.bin",
            "@@ -1,3 +1,3 @@",
            " version https://git-lfs.github.com/spec/v1",
            "-oid sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "-size 100",
            "+oid sha256:2222222222222222222222222222222222222222222222222222222222222222",
            "+size 200",
        ];
        let out = feed_log(LogDiffDirection::Deletions, lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size, 100);
        assert_eq!(
            out[0]
                .path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("foo.bin".to_owned())
        );
    }

    #[test]
    fn log_scanner_handles_multi_file_commit() {
        let lines = [
            "lfs-commit-sha: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "diff --git a/a.bin b/a.bin",
            " version https://git-lfs.github.com/spec/v1",
            "-oid sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "-size 1",
            "+oid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "+size 2",
            "diff --git a/b.bin b/b.bin",
            " version https://git-lfs.github.com/spec/v1",
            "-oid sha256:3333333333333333333333333333333333333333333333333333333333333333",
            "-size 3",
            "+oid sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "+size 4",
        ];
        let out = feed_log(LogDiffDirection::Deletions, lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].size, 1);
        assert_eq!(out[1].size, 3);
    }

    #[test]
    fn log_scanner_skips_non_pointer_diffs() {
        // The pointer-data regex only matches lines starting with one
        // of the four LFS keywords, so random source-file edits don't
        // accumulate.
        let lines = [
            "lfs-commit-sha: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "diff --git a/main.c b/main.c",
            "-int old() { return 1; }",
            "+int new() { return 2; }",
        ];
        let out = feed_log(LogDiffDirection::Deletions, lines);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn parse_diff_git_header_picks_correct_side() {
        let h = "a/foo.bin b/foo.bin";
        assert_eq!(
            parse_diff_git_header(h, LogDiffDirection::Additions).as_deref(),
            Some("foo.bin")
        );
        assert_eq!(
            parse_diff_git_header(h, LogDiffDirection::Deletions).as_deref(),
            Some("foo.bin")
        );
        // Rename: paths differ, and deletions sees the old name.
        let renamed = "a/old.bin b/new.bin";
        assert_eq!(
            parse_diff_git_header(renamed, LogDiffDirection::Additions).as_deref(),
            Some("new.bin")
        );
        assert_eq!(
            parse_diff_git_header(renamed, LogDiffDirection::Deletions).as_deref(),
            Some("old.bin")
        );
    }

    #[test]
    fn parse_diff_git_header_allows_spaces_in_paths() {
        let h = "a/my file.bin b/my file.bin";
        assert_eq!(
            parse_diff_git_header(h, LogDiffDirection::Additions).as_deref(),
            Some("my file.bin")
        );
        assert_eq!(
            parse_diff_git_header(h, LogDiffDirection::Deletions).as_deref(),
            Some("my file.bin")
        );
    }

    #[test]
    fn log_scanner_finds_pointer_in_path_with_spaces() {
        let lines = [
            "lfs-commit-sha: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "diff --git a/my file.bin b/my file.bin",
            " version https://git-lfs.github.com/spec/v1",
            "-oid sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "-size 1",
            "+oid sha256:2222222222222222222222222222222222222222222222222222222222222222",
            "+size 2",
        ];
        let out = feed_log(LogDiffDirection::Deletions, lines);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].path.as_deref(), Some(Path::new("my file.bin")));
    }
}
