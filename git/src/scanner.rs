//! Scanner: walk git history, find LFS pointer blobs.
//!
//! This is the entry point used by `git lfs fetch`/`pull`/`push` to
//! enumerate the LFS pointers reachable from a set of refs. The pipeline
//! mirrors upstream:
//!
//! 1. [`rev_list`](crate::rev_list::rev_list) emits every reachable object
//!    (commits, trees, blobs).
//! 2. [`CatFileBatchCheck`] filters those to blobs whose size could fit in
//!    a pointer file (≤ [`MAX_POINTER_SIZE`]). Blobs are read from index;
//!    cheap header-only check, no content I/O.
//! 3. [`CatFileBatch`] reads the surviving candidates' content. Each is
//!    parsed as a [`Pointer`]; non-pointers are silently skipped.
//! 4. The output is deduplicated by LFS OID (the pointer's content OID,
//!    not the git blob OID): the same LFS object can appear in many
//!    blobs/paths, but we only need to fetch it once.

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
    /// LFS object OID (the `oid sha256:...` field of the pointer file).
    pub oid: Oid,
    /// Object size in bytes (per the pointer's `size` field).
    pub size: u64,
    /// First working-tree path the pointer was found at. A single LFS
    /// object can appear under many paths in history; we keep the first.
    /// Useful for progress display ("downloading foo/bar.bin"); not the
    /// authoritative source — caller should not rely on it for routing.
    pub path: Option<PathBuf>,
    /// Every working-tree path the pointer was seen at (across history
    /// and refs). Callers that filter by path (`--include`/`--exclude`)
    /// must check this set rather than just `path`, otherwise an LFS
    /// OID shared between two paths gets filtered out whenever the
    /// scanner happens to dedup down to the wrong one. Always
    /// non-empty when `path` is `Some`.
    pub paths: Vec<PathBuf>,
    /// `true` if the pointer's source bytes were byte-canonical. Used by
    /// `git lfs fsck --pointers` to flag pointers that parse but don't
    /// match the canonical encoding.
    pub canonical: bool,
    /// Pointer extensions in priority-ascending order, mirroring
    /// `Pointer::extensions`. Empty for plain pointers; non-empty when
    /// the file was committed through a configured `lfs.extension.<n>`
    /// chain. The materialize/checkout paths replay these in reverse to
    /// reconstruct the working-tree content.
    pub extensions: Vec<Extension>,
}

/// Walk history reachable from `include` minus `exclude`, return unique
/// LFS pointers.
///
/// Order is undefined and should not be relied on. Callers that want a
/// stable order should sort the result.
///
/// **History semantics**: matches upstream's `ScanRefs` — every blob in
/// every commit's tree is examined, including blobs that have since been
/// deleted or modified. This catches LFS objects from the full history
/// of the named refs, which is what `git lfs fetch <ref>` is documented
/// to do.
pub fn scan_pointers(
    cwd: &Path,
    include: &[&str],
    exclude: &[&str],
) -> Result<Vec<PointerEntry>, Error> {
    scan_pointers_with_args(cwd, include, exclude, &[])
}

/// [`scan_pointers`] with extra rev-list cmdline args. See
/// [`rev_list_with_args`](crate::rev_list_with_args).
pub fn scan_pointers_with_args(
    cwd: &Path,
    include: &[&str],
    exclude: &[&str],
    extra_cmdline_args: &[&str],
) -> Result<Vec<PointerEntry>, Error> {
    let entries = crate::rev_list::rev_list_with_args(cwd, include, exclude, extra_cmdline_args)?;

    // Phase 1: header-only check. Filter to blobs whose size could plausibly
    // be a pointer file. Tracking name alongside so we can report it.
    let mut bcheck = CatFileBatchCheck::spawn(cwd)?;
    let mut candidates: Vec<(String, Option<String>)> = Vec::new();
    for entry in entries {
        match bcheck.check(&entry.oid)? {
            CatFileHeader::Found { kind, size, .. }
                if kind == "blob" && (size as usize) < MAX_POINTER_SIZE =>
            {
                candidates.push((entry.oid, entry.name));
            }
            // Trees, commits, oversized blobs, missing — all skipped.
            _ => {}
        }
    }
    drop(bcheck);

    // Phase 2: read content of each candidate, parse as pointer, dedup
    // by LFS OID. Same LFS object referenced from multiple paths/commits
    // collapses to one entry — but we accumulate every path it appeared
    // at so include/exclude filters can match any of them.
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

/// Scan the index for LFS pointers via
/// `git ls-files --stage -z -- :(attr:filter=lfs)`.
///
/// Honors sparse-checkout (only entries in the sparse cone are listed)
/// and works in bare repos against whatever's been written into the
/// index. Empty result when the index is empty or no path matches the
/// `filter=lfs` attribute. Symlinks (mode 120000) are skipped — they
/// can never be LFS pointers.
///
/// This is the discovery path upstream's pull / fetch use on Git 2.42+;
/// it sidesteps the rev-list traversal that's expensive on partial
/// clones with `--filter=tree:0` and over-broad in bare repos with no
/// committed `.gitattributes` reachable via the index.
pub fn scan_index_lfs(cwd: &Path) -> Result<Vec<PointerEntry>, Error> {
    // Run from the work-tree top (or git-dir for bare): `git ls-files`
    // from a subdir restricts output to that subdir's entries, so
    // running from `repo/dir1/` would miss `repo/a.dat`. Resolve via
    // `--show-toplevel` first; fall back to the git-dir for bare repos
    // (which legitimately have no work tree).
    let scan_cwd = match crate::run_git(cwd, &["rev-parse", "--show-toplevel"]) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => crate::run_git(cwd, &["rev-parse", "--absolute-git-dir"])
            .map(PathBuf::from)
            .unwrap_or_else(|_| cwd.to_path_buf()),
    };
    // Apply a parent-dir-existence filter only when there's a reason
    // to: cone-mode sparse-checkout marks out-of-cone entries by
    // omitting their working-tree parents, and bare repos have no
    // working-tree subdirs at all. For ordinary checkouts where the
    // user just `rm`'d a file, we want to fetch and restore — not
    // skip — so the filter stays off.
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
        // Skip paths whose parent dir isn't materialized in the work
        // tree: that's how cone-mode sparse-checkout marks out-of-cone
        // entries when ls-files emits the *expanded* index (the trees
        // are local but the working-tree dirs were never created).
        // The same check naturally drops non-root entries in bare
        // repos, where only the top-level scan_cwd exists as a
        // directory. Skipped on plain checkouts so a user `rm`'d
        // file still gets restored by `git lfs pull`.
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

/// One blob found while walking a tree, before any pointer-parsing or
/// size-based filtering. Paths and OIDs are reported verbatim from
/// `git ls-tree`.
#[derive(Debug, Clone)]
pub struct TreeBlob {
    /// Working-tree path of the blob.
    pub path: PathBuf,
    /// Git blob OID (the SHA-1 of the blob in the object database).
    pub blob_oid: String,
    /// Size of the blob in bytes, per `cat-file --batch-check`.
    pub size: u64,
    /// Git tree-entry mode in octal (e.g. `100644`, `100755`,
    /// `120000` for symlinks). Callers that classify entries by
    /// mode (e.g. `fsck --pointers` skipping symlinks) read this.
    pub mode: String,
}

/// Walk the tree at `reference` and return *every* blob — no size filter,
/// no pointer parsing. Used by `fsck --pointers` for its full-tree sweep
/// when classifying paths against `.gitattributes`.
pub fn scan_tree_blobs(cwd: &Path, reference: &str) -> Result<Vec<TreeBlob>, Error> {
    // `git ls-tree` only takes a tree-ish, not a range. For a `<a>..<b>`
    // reference (used by `git lfs fsck HEAD^..HEAD`), walk every commit
    // in the range and union their tree blobs (deduped by path+oid). A
    // bare ref still takes the cheap one-shot path.
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

/// Expand a `<a>..<b>` rev-range into the concrete commits it names
/// and union their tree blobs (deduped by path + blob OID). Mirrors
/// upstream's behavior for `git lfs fsck HEAD^..HEAD`: every blob
/// reachable from any commit in the range is checked once.
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

/// Walk the tree at `reference`, returning one entry per LFS pointer blob.
///
/// Unlike [`scan_pointers`], this does *not* walk history and does *not*
/// dedupe by LFS OID — each path in the tree that points at an LFS
/// pointer becomes its own entry. Multiple paths pointing at the same
/// LFS object yield multiple entries, with their working-tree paths
/// preserved. This matches upstream's `ScanTree` semantics, used by
/// `ls-files` and `status`.
///
/// Paths are read from `git ls-tree -r -z` so embedded newlines or
/// quoting metacharacters round-trip cleanly.
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

    // Phase 1: parse `<mode> <type> <oid>\t<path>` records, keep blobs
    // small enough to be a pointer.
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

    // Phase 2: read each candidate blob, parse as pointer, emit one
    // entry per path. No OID dedup — that's intentional, callers may
    // want to know every path an object lives at in this tree.
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

/// Walk `git log -G "oid sha256:" -p <ref>` since `since`, returning
/// every LFS pointer that appears as the **previous** state of a
/// modified file (i.e. lives on the `-` side of a unified diff).
///
/// Mirrors upstream's `lfs/gitscanner_log.go::logPreviousSHAs`. Used by
/// fetch-recent (to download pre-images of recently-modified files) and
/// by prune retention (to keep them on disk).
///
/// `-U12` is requested so a small in-place edit still surfaces enough
/// surrounding context to capture the full pointer body (version, oid,
/// size, optional ext-N lines).
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
    /// `+` lines (new state of the file in this commit).
    #[allow(dead_code)]
    Additions,
    /// `-` lines (previous state, before this commit modified it).
    Deletions,
}

/// Line-by-line state machine for `git log -p` output formatted with
/// `--format=lfs-commit-sha: %H %P`. Mirrors upstream's `logScanner`
/// (`lfs/gitscanner_log.go`).
struct LogScanner {
    direction: LogDiffDirection,
    /// Path of the file currently being diffed, or `None` between
    /// commits / before the first `diff --git` header.
    current_filename: Option<String>,
    /// Buffered pointer-body lines (with the diff marker stripped).
    /// Flushed into a [`PointerEntry`] when the next file/commit
    /// boundary arrives or the stream ends.
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

    /// Feed one log line. Returns `Some(entry)` when a complete pointer
    /// body just got flushed (i.e. a file or commit boundary just
    /// arrived after we'd buffered some pointer data).
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
            // Strip the leading diff marker (`+`, `-`, or ` `).
            self.pointer_data.extend_from_slice(&line.as_bytes()[1..]);
            self.pointer_data.push(b'\n');
        }
        None
    }

    /// Drain the pending pointer buffer into a [`PointerEntry`] if it
    /// parses as a valid LFS pointer. Resets the buffer either way.
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

/// Pointer-body lines start with one of the diff markers (`+`, `-`,
/// ` `) followed by one of the four pointer-keyword prefixes. We
/// always include unchanged context lines (` `) so the version/size
/// lines bracket the changed `oid` line — `-U12` makes that reliable
/// for typical extension chains too.
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

/// Parse a `diff --git a/<path1> b/<path2>` header to the relevant
/// path. We pick the `b/` path for additions (the "new" name) and
/// the `a/` path for deletions (the "old" name). Renames are rare in
/// LFS-tracked binaries; on a rename, additions tracks the new path
/// and deletions tracks the old, which matches upstream.
///
/// Quoted / octal-escaped paths (those with spaces or non-ASCII)
/// aren't unescaped here — yet. The fetch-recent and prune tests
/// don't exercise them, so deferring keeps Slice 3 compact.
fn parse_diff_git_header(rest: &str, dir: LogDiffDirection) -> Option<String> {
    let trimmed = rest.trim();
    let a_idx = trimmed.find("a/")?;
    let after_a = &trimmed[a_idx + 2..];
    // Find the boundary between path1 and " b/path2". Upstream's regex
    // uses `\s+`, so any whitespace run terminates path1.
    let space_idx = after_a.find(|c: char| c.is_whitespace())?;
    let path_a = &after_a[..space_idx];
    let after_space = after_a[space_idx..].trim_start();
    let after_b = after_space.strip_prefix("b/")?;
    match dir {
        LogDiffDirection::Additions => Some(after_b.to_owned()),
        LogDiffDirection::Deletions => Some(path_a.to_owned()),
    }
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
        // Plain content + LFS pointer side-by-side.
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
        // semantics require we still find it — older commits are part of
        // history reachable from HEAD.
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
        // NOT show up in scan_tree (this is the point of the helper —
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
        // (scan_pointers would dedupe to one; scan_tree must not.)
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
        // of the four LFS keywords — random source-file edits don't
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
        // Rename — paths differ; deletions sees the old name.
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
}
