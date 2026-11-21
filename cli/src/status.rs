//! `git lfs status`: show staged and unstaged changes, and which side of
//! each is an LFS pointer.
//!
//! The default output lists the LFS objects to push to the upstream branch,
//! then the staged and unstaged changes. Each side of a change is shown as
//! LFS (the pointer's OID), Git (the SHA-256 of the blob), or File (the
//! SHA-256 of the working-tree file), as upstream does. `--porcelain` prints
//! one line per path, and `--json` lists only changes to LFS files.

use std::collections::HashSet;
use std::path::Path;

use git_lfs_git::cat_file::CatFileBatch;
use git_lfs_git::diff_index::{DiffEntry, diff_index};
use git_lfs_git::scanner::{PointerEntry, scan_pointers};
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
    /// Run outside any repository. The caller exits 128.
    #[error("Not in a Git repository.")]
    NotInRepo,
    /// Run inside a bare repository. t-status.sh checks this message.
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
    /// A blob missing from the object database, such as in a partial
    /// clone.
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

/// One side of a change, as shown in the default output.
#[derive(Debug, Clone)]
struct BlobInfo {
    kind: BlobKind,
    /// The first seven hex digits of the hash, or a word such as
    /// `deleted` in its place.
    sha7: Option<String>,
}

impl BlobInfo {
    /// The working-tree file is missing.
    fn deleted() -> Self {
        Self {
            kind: BlobKind::File,
            sha7: Some("deleted".to_owned()),
        }
    }
}

/// Git's empty tree, which stands in for HEAD before the first commit.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub fn run(cwd: &Path, format: Format) -> Result<(), StatusError> {
    if !is_in_git_repo(cwd) {
        return Err(StatusError::NotInRepo);
    }
    let Some(repo_root) = repo_root(cwd) else {
        return Err(StatusError::NotInWorkTree);
    };
    let head = current_head(cwd);
    // Before the first commit, every staged file is an addition.
    let has_head = head.is_some();
    let refname: &str = head.as_deref().unwrap_or(EMPTY_TREE_SHA);
    let staged = diff_index(cwd, refname, true)?;
    let combined = diff_index(cwd, refname, false)?;
    let unstaged = subtract(&combined, &staged);

    // Without an upstream branch the section is left out, as upstream
    // does. A failed scan also leaves it out, where upstream fails.
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

/// The current branch's upstream (`@{u}`).
struct UpstreamRef {
    /// Short name, such as `origin/main`.
    display: String,
    /// Full name, such as `refs/remotes/origin/main`.
    full_ref: String,
}

fn upstream_tracking_ref(cwd: &Path) -> Option<UpstreamRef> {
    let abbrev = run_git(
        cwd,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )?;
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
    // Like upstream, there's no header line before the first commit.
    if has_head {
        if let Some(branch) = current_branch(cwd) {
            println!("On branch {branch}");
        } else {
            println!("HEAD detached at {}", &refname[..refname.len().min(7)]);
        }
    }

    let mut batch = CatFileBatch::spawn(cwd)?;

    // Entries here show the full OID, unlike the sections below.
    if let Some((remote_branch, pointers)) = push {
        println!("Objects to be pushed to {remote_branch}:");
        println!();
        for p in pointers {
            let path = p
                .path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            println!("\t{path} ({})", p.oid);
        }
    }

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
        // An addition has one side, which `blob_info_from` takes from
        // the destination.
        format!("({render_from})")
    } else {
        format!("({render_from} -> {render_to})")
    };

    // diff-index paths are repo-relative. Show them relative to `cwd`.
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
    // Like upstream, a path that is both staged and unstaged is shown
    // with its unstaged change.
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

/// Remove the entries of `b` from `a`, keyed on both blob IDs and the
/// path, as upstream does.
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
    a.iter()
        .filter(|e| !exclude.contains(&key(e)))
        .cloned()
        .collect()
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

/// Make a repo-relative path relative to `cwd`.
///
/// Climbs to the root with one `..` per level below it, so a file under
/// `cwd` shows as `../dir/file`. Falls back to the repo-relative path if
/// either directory can't be canonicalized.
fn display_path(cwd: &Path, repo_root: &Path, repo_rel: &str) -> String {
    let (Ok(cwd_abs), Ok(root_abs)) = (cwd.canonicalize(), repo_root.canonicalize()) else {
        return repo_rel.to_owned();
    };
    let Ok(rel_in_repo) = cwd_abs.strip_prefix(&root_abs) else {
        return repo_rel.to_owned();
    };
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

/// Classify the source side of a change.
///
/// For an addition, whose source is all zeros, the destination is used,
/// as upstream does.
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

/// Classify the destination side of a change.
///
/// An all-zero destination means the working-tree file.
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
            return Ok(BlobInfo {
                kind: BlobKind::Missing,
                sha7: Some("<missing>".to_owned()),
            });
        };
        if let Ok(p) = Pointer::parse(&blob.content) {
            return Ok(BlobInfo {
                kind: BlobKind::Lfs,
                sha7: Some(short(&p.oid.to_string())),
            });
        }
        // Upstream shows the content's SHA-256, not the blob ID.
        let mut hasher = Sha256::new();
        hasher.update(&blob.content);
        let sha = hex32(hasher.finalize().into());
        return Ok(BlobInfo {
            kind: BlobKind::Git,
            sha7: Some(short(&sha)),
        });
    }

    // An all-zero ID means the working-tree file.
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
            // Replaced by a directory. Upstream shows this as deleted.
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
