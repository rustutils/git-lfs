//! `git lfs track`: manage LFS-tracked patterns in `.gitattributes`.

use std::fs;
use std::io;
use std::path::Path;

use tempfile::NamedTempFile;

const ATTRIBUTES_FILE: &str = ".gitattributes";

/// The attribute fragment we append to mark a pattern as LFS-tracked. Matches
/// upstream's format byte-for-byte.
const LFS_ATTR_TAIL: &str = " filter=lfs diff=lfs merge=lfs -text";

#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("failed to commit .gitattributes: {0}")]
    Persist(String),
}

/// In-memory view of a `.gitattributes` file. Preserves the original line
/// order and any non-LFS content (so `* text=auto`, comments, etc. survive
/// a track operation untouched).
pub struct Attributes {
    lines: Vec<String>,
}

impl Attributes {
    /// Read `.gitattributes` from `cwd`. Empty if the file doesn't exist.
    pub fn read(cwd: &Path) -> Result<Self, TrackError> {
        let path = cwd.join(ATTRIBUTES_FILE);
        match fs::read_to_string(&path) {
            Ok(s) => Ok(Self {
                lines: s.lines().map(String::from).collect(),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self { lines: Vec::new() }),
            Err(e) => Err(e.into()),
        }
    }

    /// All patterns marked with `filter=lfs` (i.e. tracked by LFS), in file
    /// order. Negative-attribute lines (`-filter`) are not included.
    pub fn lfs_patterns(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| {
                // Strip an inline `#` comment, then tokenize.
                let body = line.split('#').next().unwrap_or(line).trim();
                if body.is_empty() {
                    return None;
                }
                let mut tokens = body.split_whitespace();
                let pattern = tokens.next()?;
                if tokens.any(|t| t == "filter=lfs") {
                    Some(pattern)
                } else {
                    None
                }
            })
            .collect()
    }

    fn is_lfs_tracked(&self, pattern: &str) -> bool {
        self.lfs_patterns().contains(&pattern)
    }

    /// Append a pattern as LFS-tracked. Returns `true` if added, `false` if
    /// it was already tracked (no-op).
    pub fn track(&mut self, pattern: &str) -> bool {
        if self.is_lfs_tracked(pattern) {
            return false;
        }
        self.lines.push(format!("{pattern}{LFS_ATTR_TAIL}"));
        true
    }

    /// Remove every LFS-tracked line for `pattern`. Returns `true` if at
    /// least one line was removed, `false` if the pattern wasn't tracked.
    /// Non-LFS lines (no `filter=lfs`) are always preserved, even if their
    /// first token matches `pattern`.
    pub fn untrack(&mut self, pattern: &str) -> bool {
        let before = self.lines.len();
        self.lines.retain(|line| {
            let body = line.split('#').next().unwrap_or(line).trim();
            if body.is_empty() {
                return true;
            }
            let mut tokens = body.split_whitespace();
            let Some(first) = tokens.next() else {
                return true;
            };
            let is_lfs = tokens.any(|t| t == "filter=lfs");
            !(is_lfs && first == pattern)
        });
        self.lines.len() != before
    }

    /// Persist back to `.gitattributes` via tempfile + atomic rename.
    pub fn write(&self, cwd: &Path) -> Result<(), TrackError> {
        let mut content = String::new();
        for line in &self.lines {
            content.push_str(line);
            content.push('\n');
        }
        let tmp = NamedTempFile::new_in(cwd)?;
        fs::write(tmp.path(), content)?;
        let target = cwd.join(ATTRIBUTES_FILE);
        tmp.persist(target)
            .map_err(|e| TrackError::Persist(e.to_string()))?;
        Ok(())
    }
}

/// Outcome of a [`track`] call: which patterns were added vs. already
/// tracked. Used by the cli to render its output.
pub struct TrackOutcome {
    pub added: Vec<String>,
    pub already: Vec<String>,
}

/// Add each `pattern` to `.gitattributes` as LFS-tracked, idempotently.
pub fn track(cwd: &Path, patterns: &[String]) -> Result<TrackOutcome, TrackError> {
    let mut attrs = Attributes::read(cwd)?;
    let mut added = Vec::new();
    let mut already = Vec::new();
    for pattern in patterns {
        if attrs.track(pattern) {
            added.push(pattern.clone());
        } else {
            already.push(pattern.clone());
        }
    }
    if !added.is_empty() {
        attrs.write(cwd)?;
    }
    Ok(TrackOutcome { added, already })
}

/// Return the LFS-tracked patterns from `.gitattributes` in `cwd`.
pub fn list(cwd: &Path) -> Result<Vec<String>, TrackError> {
    Ok(Attributes::read(cwd)?
        .lfs_patterns()
        .into_iter()
        .map(String::from)
        .collect())
}

/// Outcome of an [`untrack`] call: which patterns were removed vs. weren't
/// tracked to begin with.
pub struct UntrackOutcome {
    pub removed: Vec<String>,
    pub missing: Vec<String>,
}

/// Remove each `pattern` from `.gitattributes`. Idempotent: untracking a
/// pattern that wasn't tracked is recorded under `missing` and is not an
/// error.
pub fn untrack(cwd: &Path, patterns: &[String]) -> Result<UntrackOutcome, TrackError> {
    let mut attrs = Attributes::read(cwd)?;
    let mut removed = Vec::new();
    let mut missing = Vec::new();
    for pattern in patterns {
        if attrs.untrack(pattern) {
            removed.push(pattern.clone());
        } else {
            missing.push(pattern.clone());
        }
    }
    if !removed.is_empty() {
        attrs.write(cwd)?;
    }
    Ok(UntrackOutcome { removed, missing })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, content: &str) {
        fs::write(dir.join(ATTRIBUTES_FILE), content).unwrap();
    }

    #[test]
    fn list_empty_when_no_file() {
        let tmp = TempDir::new().unwrap();
        assert!(list(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn list_returns_only_filter_lfs_patterns() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "* text=auto\n\
             *.jpg filter=lfs diff=lfs merge=lfs -text\n\
             # comment\n\
             *.zip filter=lfs -text\n\
             *.cs diff=csharp\n",
        );
        assert_eq!(list(tmp.path()).unwrap(), vec!["*.jpg", "*.zip"]);
    }

    #[test]
    fn track_creates_file_when_missing() {
        let tmp = TempDir::new().unwrap();
        let outcome = track(tmp.path(), &["*.jpg".into()]).unwrap();
        assert_eq!(outcome.added, vec!["*.jpg"]);
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content, "*.jpg filter=lfs diff=lfs merge=lfs -text\n");
    }

    #[test]
    fn track_appends_and_preserves_existing_content() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "* text=auto\n#*.cs diff=csharp\n");
        track(tmp.path(), &["*.jpg".into()]).unwrap();
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(
            content,
            "* text=auto\n\
             #*.cs diff=csharp\n\
             *.jpg filter=lfs diff=lfs merge=lfs -text\n",
        );
    }

    #[test]
    fn track_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let first = track(tmp.path(), &["*.jpg".into()]).unwrap();
        assert_eq!(first.added, vec!["*.jpg"]);
        assert!(first.already.is_empty());

        let second = track(tmp.path(), &["*.jpg".into()]).unwrap();
        assert!(second.added.is_empty());
        assert_eq!(second.already, vec!["*.jpg"]);

        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        // Pattern appears exactly once.
        assert_eq!(content.matches("*.jpg").count(), 1);
    }

    #[test]
    fn track_multiple_patterns() {
        let tmp = TempDir::new().unwrap();
        let outcome = track(
            tmp.path(),
            &["*.jpg".into(), "*.png".into(), "*.zip".into()],
        )
        .unwrap();
        assert_eq!(outcome.added.len(), 3);
        assert_eq!(list(tmp.path()).unwrap(), vec!["*.jpg", "*.png", "*.zip"]);
    }

    #[test]
    fn negative_filter_lines_are_not_tracked() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "*.gif -filter -text\n");
        assert!(list(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn untrack_removes_only_lfs_lines_for_pattern() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "* text=auto\n\
             *.jpg filter=lfs diff=lfs merge=lfs -text\n\
             *.png filter=lfs diff=lfs merge=lfs -text\n",
        );
        let outcome = untrack(tmp.path(), &["*.jpg".into()]).unwrap();
        assert_eq!(outcome.removed, vec!["*.jpg"]);
        assert!(outcome.missing.is_empty());
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(
            content,
            "* text=auto\n\
             *.png filter=lfs diff=lfs merge=lfs -text\n",
        );
    }

    #[test]
    fn untrack_unknown_pattern_is_recorded_as_missing() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "*.jpg filter=lfs diff=lfs merge=lfs -text\n");
        let outcome = untrack(tmp.path(), &["*.png".into()]).unwrap();
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.missing, vec!["*.png"]);
        // File untouched.
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content, "*.jpg filter=lfs diff=lfs merge=lfs -text\n");
    }

    #[test]
    fn untrack_preserves_non_lfs_line_with_same_first_token() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "*.cs diff=csharp\n");
        let outcome = untrack(tmp.path(), &["*.cs".into()]).unwrap();
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.missing, vec!["*.cs"]);
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content, "*.cs diff=csharp\n");
    }

    #[test]
    fn untrack_no_file_is_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let outcome = untrack(tmp.path(), &["*.jpg".into()]).unwrap();
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.missing, vec!["*.jpg"]);
        assert!(!tmp.path().join(ATTRIBUTES_FILE).exists());
    }
}
