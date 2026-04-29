//! `git lfs status` — show staged + unstaged LFS-tracked changes.
//!
//! Output mirrors upstream's three modes:
//! - default: human-readable, one section per (committed | not staged), each
//!   line classifying its blobs as LFS / Git / File with a short content-hash
//!   prefix.
//! - `--porcelain`: one line per change, deduplicated.
//! - `--json`: stable structured output for scripts; only LFS entries are
//!   reported.
//!
//! For v0 we omit the "Objects to be pushed to <remote/branch>" section that
//! upstream prints; that's a separate scan against the upstream tracking ref
//! and is deferred — see NOTES.md.

use std::collections::HashSet;
use std::path::Path;

use git_lfs_git::{CatFileBatch, DiffEntry, PointerEntry, diff_index, scan_pointers};
use git_lfs_pointer::Pointer;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("could not serialize JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Command was run outside any git repo. Caller should print the
    /// message and exit 128 (mirrors `git lfs fetch` / `pull`).
    #[error("Not in a Git repository.")]
    NotInRepo,
    /// Command was run inside a bare repo. Status needs a work tree
    /// to compare against; mirror upstream's exact wording so the
    /// `t-status` "without a working copy" test grep matches.
    #[error("This operation must be run in a work tree.")]
    NotInWorkTree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Default,
    Porcelain,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobKind {
    Lfs,
    Git,
    File,
    /// Blob referenced by the diff but missing from the local object
    /// database (e.g. a partial clone, or the user `rm`'d
    /// `.git/objects/...`). Renders as `?: <missing>`.
    Missing,
}

impl BlobKind {
    fn label(self) -> &'static str {
        match self {
            BlobKind::Lfs => "LFS",
            BlobKind::Git => "Git",
            BlobKind::File => "File",
            BlobKind::Missing => "?",
        }
    }
}

/// One blob's classification + a short content-hash prefix for display.
#[derive(Debug, Clone)]
struct BlobInfo {
    kind: BlobKind,
    /// `Some` with a 7-char hex prefix, or the literal string `"deleted"`
    /// for the working-tree deletion sentinel. `None` is reserved for
    /// the unusual case where neither applies.
    sha7: Option<String>,
}

impl BlobInfo {
    /// Working-tree file is missing. Upstream formats this as
    /// `File: deleted` rather than a hex prefix.
    fn deleted() -> Self {
        Self {
            kind: BlobKind::File,
            sha7: Some("deleted".to_owned()),
        }
    }
}

/// Git's well-known empty-tree hash. Used as a stand-in for HEAD when
/// the repo has no commits yet, so `diff-index` can still tell us which
/// files have been staged.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub fn run(cwd: &Path, format: Format) -> Result<(), StatusError> {
    // Disambiguate three states for clean error messages:
    //   1. not inside a repo at all   → "Not in a Git repository." (128)
    //   2. inside a bare repo         → "This operation must be run in a work tree."
    //   3. inside a regular repo with a worktree → proceed
    if !is_in_git_repo(cwd) {
        return Err(StatusError::NotInRepo);
    }
    let Some(repo_root) = repo_root(cwd) else {
        return Err(StatusError::NotInWorkTree);
    };
    let head = current_head(cwd);
    // No HEAD yet: diff against the empty tree so freshly-staged files
    // surface as additions. Upstream does the same — its "before
    // initial commit" test expects the normal section layout
    // populated with the staged blobs.
    let has_head = head.is_some();
    let refname: &str = head.as_deref().unwrap_or(EMPTY_TREE_SHA);
    let staged = diff_index(cwd, refname, true)?;
    let combined = diff_index(cwd, refname, false)?;
    let unstaged = subtract(&combined, &staged);

    // "Objects to be pushed" only appears in the default format and
    // only when the current branch tracks an upstream that we can
    // resolve. Failures here are non-fatal — upstream omits the
    // section if it can't compute the diff cleanly.
    let push = if has_head && format == Format::Default {
        upstream_tracking_ref(cwd).and_then(|upstream| {
            scan_pointers(&repo_root, &["HEAD"], &[upstream.full_ref.as_str()])
                .ok()
                .map(|pointers| (upstream.display, pointers))
        })
    } else {
        None
    };

    match format {
        Format::Default => emit_default(
            cwd,
            &repo_root,
            refname,
            has_head,
            &staged,
            &unstaged,
            push.as_ref(),
        ),
        Format::Porcelain => emit_porcelain(&staged, &unstaged),
        Format::Json => emit_json(&repo_root, &staged, &unstaged),
    }
}

/// The upstream-tracking ref for the current branch, both as the
/// human-readable `<remote>/<branch>` form (for the section header) and
/// the full ref name (for `scan_pointers`'s exclude list).
struct UpstreamRef {
    /// e.g. `"origin/main"`.
    display: String,
    /// e.g. `"refs/remotes/origin/main"`.
    full_ref: String,
}

fn upstream_tracking_ref(cwd: &Path) -> Option<UpstreamRef> {
    let abbrev = run_git(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])?;
    if abbrev.is_empty() || abbrev == "@{u}" {
        return None;
    }
    let full = run_git(cwd, &["rev-parse", "--symbolic-full-name", "@{u}"])?;
    Some(UpstreamRef {
        display: abbrev,
        full_ref: full,
    })
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

fn emit_default(
    cwd: &Path,
    repo_root: &Path,
    refname: &str,
    has_head: bool,
    staged: &[DiffEntry],
    unstaged: &[DiffEntry],
    push: Option<&(String, Vec<PointerEntry>)>,
) -> Result<(), StatusError> {
    // Branch / detached HEAD line is suppressed before the initial
    // commit — upstream omits any header in that case so the output
    // starts with a blank line and goes straight into sections.
    if has_head {
        if let Some(branch) = current_branch(cwd) {
            println!("On branch {branch}");
        } else {
            // Detached or otherwise; show the ref we resolved against.
            println!("HEAD detached at {}", &refname[..refname.len().min(7)]);
        }
    }

    let mut batch = CatFileBatch::spawn(cwd)?;

    // "Objects to be pushed to <remote>/<branch>:" comes first when
    // present. No leading blank — upstream butts it against the
    // branch header. Each entry uses the full LFS OID, not a 7-char
    // prefix (matches t-status expected output).
    if let Some((remote_branch, pointers)) = push {
        println!("Objects to be pushed to {remote_branch}:");
        println!();
        for p in pointers {
            let path = p.path.as_deref().map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            println!("\t{path} ({})", p.oid);
        }
    }

    // Sections always have a blank line between header and entries
    // (upstream layout). Empty sections still get the blank.
    println!();
    println!("Objects to be committed:");
    println!();
    for e in staged {
        println!("\t{}", format_entry_line(cwd, repo_root, &mut batch, e)?);
    }

    println!();
    println!("Objects not staged for commit:");
    println!();
    for e in unstaged {
        println!("\t{}", format_entry_line(cwd, repo_root, &mut batch, e)?);
    }
    Ok(())
}

fn format_entry_line(
    cwd: &Path,
    repo_root: &Path,
    batch: &mut CatFileBatch,
    e: &DiffEntry,
) -> Result<String, StatusError> {
    let from = blob_info_from(repo_root, batch, e)?;
    let to = blob_info_to(repo_root, batch, e)?;

    let render_from = render_blob(&from);
    let render_to = render_blob(&to);

    let info = if e.status == 'A' {
        // For pure additions, the "from" side is empty. Show only the
        // dst classification, sourced via blob_info_from (which falls
        // back to dst_sha when src is zero).
        format!("({render_from})")
    } else {
        format!("({render_from} -> {render_to})")
    };

    // diff-index always emits repo-relative paths; for display we want
    // them relative to the user's cwd so a `git lfs status` from a
    // subdirectory matches what `git status` would show.
    let display_src = display_path(cwd, repo_root, &e.src_name);
    let path_part = match e.status {
        'R' | 'C' => format!(
            "{} -> {}",
            display_src,
            display_path(cwd, repo_root, e.dst_name.as_deref().unwrap_or(&e.src_name))
        ),
        _ => display_src,
    };
    Ok(format!("{path_part} {info}"))
}

fn render_blob(b: &BlobInfo) -> String {
    match &b.sha7 {
        Some(sha) => format!("{}: {sha}", b.kind.label()),
        None => b.kind.label().to_owned(),
    }
}

fn emit_porcelain(staged: &[DiffEntry], unstaged: &[DiffEntry]) -> Result<(), StatusError> {
    // Order: unstaged first, then staged, deduping by name. Matches
    // upstream: a file present in both surfaces under its unstaged
    // entry, so a `git mv` followed by an edit reads as the unstaged
    // form. Test 2 is the canonical example.
    let mut seen: HashSet<String> = HashSet::new();
    for e in unstaged.iter().chain(staged.iter()) {
        let name = e.dst_name.as_deref().unwrap_or(&e.src_name).to_owned();
        if !seen.insert(name) {
            continue;
        }
        println!("{}", porcelain_line(e));
    }
    Ok(())
}

fn porcelain_line(e: &DiffEntry) -> String {
    match e.status {
        'R' | 'C' => format!(
            "{}  {} -> {}",
            e.status,
            e.src_name,
            e.dst_name.as_deref().unwrap_or(&e.src_name)
        ),
        'M' => format!(" {} {}", e.status, e.src_name),
        _ => format!("{}  {}", e.status, e.src_name),
    }
}

#[derive(Debug, Serialize)]
struct JsonOutput {
    files: std::collections::BTreeMap<String, JsonEntry>,
}

#[derive(Debug, Serialize)]
struct JsonEntry {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
}

fn emit_json(
    repo_root: &Path,
    staged: &[DiffEntry],
    unstaged: &[DiffEntry],
) -> Result<(), StatusError> {
    let mut batch = CatFileBatch::spawn(repo_root)?;
    let mut files = std::collections::BTreeMap::new();
    for e in unstaged.iter().chain(staged.iter()) {
        // Upstream JSON output only reports LFS-related entries.
        let from = blob_info_from(repo_root, &mut batch, e)?;
        if from.kind != BlobKind::Lfs {
            continue;
        }
        let key = e.dst_name.as_deref().unwrap_or(&e.src_name).to_owned();
        let entry = match e.status {
            'R' | 'C' => JsonEntry {
                status: e.status.to_string(),
                from: Some(e.src_name.clone()),
            },
            _ => JsonEntry {
                status: e.status.to_string(),
                from: None,
            },
        };
        files.entry(key).or_insert(entry);
    }
    println!("{}", serde_json::to_string(&JsonOutput { files })?);
    Ok(())
}

/// Subtract `b` from `a` by the (src_sha, dst_sha, name) key upstream
/// uses. Preserves the order of `a`.
fn subtract(a: &[DiffEntry], b: &[DiffEntry]) -> Vec<DiffEntry> {
    let key = |e: &DiffEntry| {
        format!(
            "{}:{}:{}",
            e.src_sha,
            e.dst_sha,
            e.dst_name.as_deref().unwrap_or(&e.src_name)
        )
    };
    let exclude: HashSet<String> = b.iter().map(key).collect();
    a.iter().filter(|e| !exclude.contains(&key(e))).cloned().collect()
}

fn is_in_git_repo(cwd: &Path) -> bool {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

fn repo_root(cwd: &Path) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(s))
    }
}

/// Convert a repo-relative path to one relative to `cwd` for display.
/// Falls back to the repo-relative form on canonicalization errors —
/// the path will still be readable, just less convenient.
fn display_path(cwd: &Path, repo_root: &Path, repo_rel: &str) -> String {
    let (Ok(cwd_abs), Ok(root_abs)) = (cwd.canonicalize(), repo_root.canonicalize()) else {
        return repo_rel.to_owned();
    };
    let Ok(rel_in_repo) = cwd_abs.strip_prefix(&root_abs) else {
        return repo_rel.to_owned();
    };
    // How many components of cwd live under the repo root? That's the
    // number of `..` steps we need to climb back to the root before
    // descending to the file.
    let depth = rel_in_repo.components().count();
    if depth == 0 {
        return repo_rel.to_owned();
    }
    let mut prefix = String::new();
    for _ in 0..depth {
        prefix.push_str("../");
    }
    format!("{prefix}{repo_rel}")
}

fn current_head(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

fn current_branch(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

/// Classify the "from" side of a diff entry. For additions (src zero) we
/// fall back to the dst sha; this matches upstream so additions report
/// their content classification on the single line they emit.
fn blob_info_from(
    repo_root: &Path,
    batch: &mut CatFileBatch,
    e: &DiffEntry,
) -> Result<BlobInfo, StatusError> {
    let blob_sha = if is_zero_sha(&e.src_sha) {
        &e.dst_sha
    } else {
        &e.src_sha
    };
    blob_info(repo_root, batch, blob_sha, &e.src_name)
}

/// Classify the "to" side. Falls back to the working-tree file when
/// dst_sha is zero (e.g. unstaged modifications, where the new content
/// isn't yet in any git object).
fn blob_info_to(
    repo_root: &Path,
    batch: &mut CatFileBatch,
    e: &DiffEntry,
) -> Result<BlobInfo, StatusError> {
    let name = e.dst_name.as_deref().unwrap_or(&e.src_name);
    blob_info(repo_root, batch, &e.dst_sha, name)
}

fn blob_info(
    repo_root: &Path,
    batch: &mut CatFileBatch,
    sha: &str,
    name: &str,
) -> Result<BlobInfo, StatusError> {
    if !is_zero_sha(sha) {
        let Some(blob) = batch.read(sha)? else {
            // Object referenced by the diff but absent from the local
            // object database (e.g. partial clone, or
            // `.git/objects/<aa>/<rest>` was deleted). Surface as
            // `?: <missing>` so users know the source content can't
            // be inspected.
            return Ok(BlobInfo {
                kind: BlobKind::Missing,
                sha7: Some("<missing>".to_owned()),
            });
        };
        if let Ok(p) = Pointer::parse(&blob.content) {
            // Pointer's OID is the LFS content sha; matches what
            // upstream reports as ContentsSha for pointer blobs.
            return Ok(BlobInfo {
                kind: BlobKind::Lfs,
                sha7: Some(short(&p.oid.to_string())),
            });
        }
        // Non-pointer blob — use sha256 of the blob's bytes for display
        // so we have something stable that doesn't depend on the git
        // object hash format (sha1 vs sha256 repos).
        let mut hasher = Sha256::new();
        hasher.update(&blob.content);
        let sha = hex32(hasher.finalize().into());
        return Ok(BlobInfo { kind: BlobKind::Git, sha7: Some(short(&sha)) });
    }

    // Zero src/dst sha: read the working-tree file directly. `name`
    // is repo-relative, so resolve from the repo root regardless of
    // where the user invoked status.
    let path = repo_root.join(name);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let sha = hex32(hasher.finalize().into());
            Ok(BlobInfo {
                kind: BlobKind::File,
                sha7: Some(short(&sha)),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BlobInfo::deleted()),
        Err(e) if e.kind() == std::io::ErrorKind::IsADirectory => {
            // The working-tree path is now a directory (test 16:
            // `git rm test && mkdir test`). Upstream classifies this
            // as deleted for the file's perspective.
            Ok(BlobInfo::deleted())
        }
        Err(e) => Err(e.into()),
    }
}

fn is_zero_sha(sha: &str) -> bool {
    sha.bytes().all(|b| b == b'0')
}

fn short(s: &str) -> String {
    s.chars().take(7).collect()
}

fn hex32(bytes: [u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_zero_sha_handles_lengths() {
        assert!(is_zero_sha("0000000"));
        assert!(is_zero_sha("0000000000000000000000000000000000000000"));
        assert!(!is_zero_sha("0000001"));
        assert!(!is_zero_sha("abc"));
    }

    #[test]
    fn porcelain_modification_has_leading_space() {
        let e = DiffEntry {
            src_sha: "a".into(),
            dst_sha: "b".into(),
            status: 'M',
            similarity: None,
            src_name: "f.txt".into(),
            dst_name: None,
        };
        assert_eq!(porcelain_line(&e), " M f.txt");
    }

    #[test]
    fn porcelain_rename_has_two_paths() {
        let e = DiffEntry {
            src_sha: "a".into(),
            dst_sha: "b".into(),
            status: 'R',
            similarity: Some(86),
            src_name: "old".into(),
            dst_name: Some("new".into()),
        };
        assert_eq!(porcelain_line(&e), "R  old -> new");
    }

    #[test]
    fn subtract_removes_matching_keys_only() {
        let mk = |status: char, src: &str| DiffEntry {
            src_sha: "src".into(),
            dst_sha: "dst".into(),
            status,
            similarity: None,
            src_name: src.into(),
            dst_name: None,
        };
        let a = vec![mk('M', "a"), mk('M', "b"), mk('M', "c")];
        let b = vec![mk('M', "b")];
        let r = subtract(&a, &b);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].src_name, "a");
        assert_eq!(r[1].src_name, "c");
    }
}
