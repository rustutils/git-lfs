//! `git diff-index -z` parser.
//!
//! Used by `git lfs status` to enumerate staged + unstaged changes
//! against HEAD. The `-z` form is mandatory for correctness: paths can
//! contain spaces, newlines, and quoting metacharacters; without `-z`,
//! git would render those quoted and we'd have to undo the encoding.
//!
//! Output format (one record per change):
//! ```text
//! :<src-mode> <dst-mode> <src-sha> <dst-sha> <status>\0<src>\0[<dst>\0]
//! ```
//! `<status>` is a single letter A/M/D/R/C/T/U/X, optionally followed by
//! a 1–3 digit similarity score for `R` and `C`. The trailing `<dst>`
//! field is only present for renames and copies.

use std::path::Path;
use std::process::Command;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub src_sha: String,
    pub dst_sha: String,
    pub status: char,
    /// Present only for `R` (rename) and `C` (copy); the similarity
    /// score git computed.
    pub similarity: Option<u16>,
    pub src_name: String,
    /// Present only for `R` and `C`.
    pub dst_name: Option<String>,
}

impl DiffEntry {
    /// The "current" path of this entry — `dst_name` for renames/copies
    /// (which is the path the diff lands at), `src_name` otherwise.
    pub fn path(&self) -> &str {
        self.dst_name.as_deref().unwrap_or(&self.src_name)
    }
}

/// Run `git diff-index -z [--cached] <ref>` and return the parsed entries.
///
/// `cached = true` reports staged changes (HEAD vs index); `cached = false`
/// reports working-tree changes (HEAD vs working tree, including unstaged).
///
/// `-M` (rename detection) matches upstream's `git lfs status` behavior;
/// without it, a `git mv` shows up as a delete + add pair instead of an `R`
/// entry, which the JSON-shape tests rely on.
pub fn diff_index(cwd: &Path, refname: &str, cached: bool) -> Result<Vec<DiffEntry>, Error> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args(["diff-index", "-M", "-z"]);
    if cached {
        cmd.arg("--cached");
    }
    cmd.arg(refname);
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(Error::Failed(format!(
            "git diff-index failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    parse(&out.stdout)
}

fn parse(bytes: &[u8]) -> Result<Vec<DiffEntry>, Error> {
    // Strip the trailing NUL git always emits so the iterator below
    // doesn't see a phantom empty token at the end.
    let trimmed = bytes.strip_suffix(b"\0").unwrap_or(bytes);
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut tokens = trimmed.split(|&b| b == 0);
    let mut entries = Vec::new();
    while let Some(meta) = tokens.next() {
        let meta_s = std::str::from_utf8(meta)
            .map_err(|e| Error::Failed(format!("diff-index: non-utf8 metadata: {e}")))?;
        let body = meta_s
            .strip_prefix(':')
            .ok_or_else(|| Error::Failed(format!("diff-index: missing ':' in {meta_s:?}")))?;
        let parts: Vec<&str> = body.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(Error::Failed(format!(
                "diff-index: expected 5 metadata fields in {meta_s:?}, got {}",
                parts.len()
            )));
        }
        let src_sha = parts[2].to_owned();
        let dst_sha = parts[3].to_owned();
        let status_field = parts[4];
        let status = status_field
            .chars()
            .next()
            .ok_or_else(|| Error::Failed(format!("diff-index: empty status in {meta_s:?}")))?;
        let similarity = if status_field.len() > 1 {
            status_field[1..].parse::<u16>().ok()
        } else {
            None
        };

        let src = tokens
            .next()
            .ok_or_else(|| Error::Failed(format!("diff-index: missing src name for {meta_s:?}")))?;
        let src_name = std::str::from_utf8(src)
            .map_err(|e| Error::Failed(format!("diff-index: non-utf8 src name: {e}")))?
            .to_owned();

        let dst_name = if matches!(status, 'R' | 'C') {
            let dst = tokens.next().ok_or_else(|| {
                Error::Failed(format!(
                    "diff-index: missing dst name for {status} record {meta_s:?}"
                ))
            })?;
            Some(
                std::str::from_utf8(dst)
                    .map_err(|e| Error::Failed(format!("diff-index: non-utf8 dst name: {e}")))?
                    .to_owned(),
            )
        } else {
            None
        };

        entries.push(DiffEntry {
            src_sha,
            dst_sha,
            status,
            similarity,
            src_name,
            dst_name,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_input() {
        assert!(parse(b"").unwrap().is_empty());
        assert!(parse(b"\0").unwrap().is_empty());
    }

    #[test]
    fn parse_modification() {
        let raw = b":100644 100644 abc 123 M\0file.txt\0";
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.src_sha, "abc");
        assert_eq!(e.dst_sha, "123");
        assert_eq!(e.status, 'M');
        assert_eq!(e.similarity, None);
        assert_eq!(e.src_name, "file.txt");
        assert_eq!(e.dst_name, None);
    }

    #[test]
    fn parse_addition_has_zero_src_sha() {
        let raw = b":000000 100644 0000000 1234567 A\0new.bin\0";
        let entries = parse(raw).unwrap();
        assert_eq!(entries[0].status, 'A');
        assert_eq!(entries[0].src_sha, "0000000");
        assert_eq!(entries[0].dst_sha, "1234567");
    }

    #[test]
    fn parse_rename_with_score_and_two_paths() {
        let raw = b":100644 100644 abc 123 R86\0old/path.txt\0new/path.txt\0";
        let entries = parse(raw).unwrap();
        let e = &entries[0];
        assert_eq!(e.status, 'R');
        assert_eq!(e.similarity, Some(86));
        assert_eq!(e.src_name, "old/path.txt");
        assert_eq!(e.dst_name.as_deref(), Some("new/path.txt"));
        assert_eq!(e.path(), "new/path.txt");
    }

    #[test]
    fn parse_multiple_records() {
        let raw = b":100644 100644 a 1 M\0a.txt\0\
                   :100644 100644 b 2 M\0b.txt\0\
                   :100644 100644 c 3 R100\0c.txt\0d.txt\0";
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].src_name, "a.txt");
        assert_eq!(entries[1].src_name, "b.txt");
        assert_eq!(entries[2].status, 'R');
        assert_eq!(entries[2].dst_name.as_deref(), Some("d.txt"));
    }

    #[test]
    fn parse_path_with_embedded_special_chars() {
        // With -z, paths are emitted raw — including newlines and tabs
        // that would normally be quote-escaped without -z.
        let raw = b":100644 100644 a 1 M\0name with\nnewline\0";
        let entries = parse(raw).unwrap();
        assert_eq!(entries[0].src_name, "name with\nnewline");
    }

    #[test]
    fn parse_missing_colon_errors() {
        let raw = b"100644 100644 a 1 M\0file\0";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn parse_truncated_record_errors() {
        // Status R, but no dst name follows — malformed.
        let raw = b":100644 100644 a 1 R86\0only-src\0";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn diff_index_against_real_repo_finds_staged_modification() {
        use crate::tests::commit_helper::*;
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"first");
        // Modify and stage.
        std::fs::write(repo.path().join("a.txt"), b"second").unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "a.txt"])
            .status()
            .unwrap();

        let staged = diff_index(repo.path(), "HEAD", true).unwrap();
        assert_eq!(staged.len(), 1, "{staged:?}");
        assert_eq!(staged[0].status, 'M');
        assert_eq!(staged[0].src_name, "a.txt");

        // Working tree matches index, so unstaged diff is empty.
        let unstaged = diff_index(repo.path(), "HEAD", false).unwrap();
        // diff_index without --cached compares HEAD vs working tree, so
        // this includes the staged change — the caller is responsible
        // for deduping (which `status` does).
        assert_eq!(unstaged.len(), 1);
    }
}
