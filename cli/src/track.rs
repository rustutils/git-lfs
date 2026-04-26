//! `git lfs track`: manage LFS-tracked patterns in `.gitattributes`.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use tempfile::NamedTempFile;

const ATTRIBUTES_FILE: &str = ".gitattributes";

/// The attribute fragment that marks a pattern as LFS-tracked. Matches
/// upstream's format byte-for-byte.
const LFS_FILTER_TAIL: &str = "filter=lfs diff=lfs merge=lfs -text";

/// Files we refuse to LFS-track because doing so would corrupt the
/// repository (the file itself controls how git understands every other
/// file).
const FORBIDDEN: &[&str] = &[
    ".gitattributes",
    ".gitignore",
    ".gitmodules",
    ".lfsconfig",
];

#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("failed to commit .gitattributes: {0}")]
    Persist(String),
}

/// `--lockable` / `--not-lockable` / neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockableMode {
    /// `--lockable` — ensure the line carries the `lockable` attribute.
    Yes,
    /// `--not-lockable` — ensure the line does *not* carry it.
    No,
    /// Neither flag — leave existing lines as-is; new lines get no
    /// `lockable` attribute.
    Default,
}

/// Line-ending choice when writing `.gitattributes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
}

impl Eol {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// Outcome for a single tracked pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackResult {
    /// Pattern was new — line appended.
    Added,
    /// Pattern already had a line, but its `lockable` state changed —
    /// line replaced in place.
    Replaced,
    /// Pattern already had a line with the requested state — no-op.
    AlreadyTracked,
}

pub struct TrackedPattern {
    pub pattern: String,
    pub result: TrackResult,
}

pub struct TrackOutcome {
    pub patterns: Vec<TrackedPattern>,
}

#[derive(Debug, Clone, Copy)]
pub struct TrackOptions {
    pub lockable: LockableMode,
    pub dry_run: bool,
}

impl Default for TrackOptions {
    fn default() -> Self {
        Self {
            lockable: LockableMode::Default,
            dry_run: false,
        }
    }
}

/// In-memory view of `.gitattributes`. Preserves the original line order
/// and any non-LFS content (so `* text=auto`, comments, etc. survive a
/// track operation untouched). Tracks whether the file used CRLF on
/// disk, so writes can preserve that style by default.
pub struct Attributes {
    lines: Vec<String>,
    had_crlf: bool,
}

impl Attributes {
    pub fn read(cwd: &Path) -> Result<Self, TrackError> {
        let path = cwd.join(ATTRIBUTES_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                let had_crlf = bytes.windows(2).any(|w| w == b"\r\n");
                let s = String::from_utf8_lossy(&bytes).into_owned();
                Ok(Self {
                    lines: s.lines().map(String::from).collect(),
                    had_crlf,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self {
                lines: Vec::new(),
                had_crlf: false,
            }),
            Err(e) => Err(e.into()),
        }
    }

    pub fn had_crlf(&self) -> bool {
        self.had_crlf
    }

    fn find_lfs_line(&self, pattern: &str) -> Option<usize> {
        self.lines.iter().position(|line| {
            let Some(body) = uncommented(line) else {
                return false;
            };
            let mut tokens = body.split_whitespace();
            let Some(first) = tokens.next() else {
                return false;
            };
            first == pattern && tokens.any(|t| t == "filter=lfs")
        })
    }

    fn line_is_lockable(&self, idx: usize) -> bool {
        let Some(body) = uncommented(&self.lines[idx]) else {
            return false;
        };
        body.split_whitespace().any(|t| t == "lockable")
    }

    fn build_line(pattern: &str, lockable: bool) -> String {
        if lockable {
            format!("{pattern} {LFS_FILTER_TAIL} lockable")
        } else {
            format!("{pattern} {LFS_FILTER_TAIL}")
        }
    }

    /// Track `pattern` (already escaped). Returns whether the line was
    /// added, replaced (lockable state changed), or unchanged.
    pub fn track(&mut self, pattern: &str, lockable: LockableMode) -> TrackResult {
        let existing = self.find_lfs_line(pattern);
        match (existing, lockable) {
            (Some(idx), LockableMode::Yes) if !self.line_is_lockable(idx) => {
                self.lines[idx] = Self::build_line(pattern, true);
                TrackResult::Replaced
            }
            (Some(idx), LockableMode::No) if self.line_is_lockable(idx) => {
                self.lines[idx] = Self::build_line(pattern, false);
                TrackResult::Replaced
            }
            (Some(_), _) => TrackResult::AlreadyTracked,
            (None, mode) => {
                let lockable = matches!(mode, LockableMode::Yes);
                self.lines.push(Self::build_line(pattern, lockable));
                TrackResult::Added
            }
        }
    }

    /// Remove every LFS-tracked line for `pattern`. Returns `true` if at
    /// least one line was removed. Non-LFS lines are preserved even if
    /// their first token matches `pattern`.
    pub fn untrack(&mut self, pattern: &str) -> bool {
        let before = self.lines.len();
        self.lines.retain(|line| {
            let Some(body) = uncommented(line) else {
                return true;
            };
            let mut tokens = body.split_whitespace();
            let Some(first) = tokens.next() else {
                return true;
            };
            let is_lfs = tokens.any(|t| t == "filter=lfs");
            !(is_lfs && first == pattern)
        });
        self.lines.len() != before
    }

    pub fn write(&self, cwd: &Path, eol: Eol) -> Result<(), TrackError> {
        let term = eol.as_str();
        let mut content = String::new();
        for line in &self.lines {
            content.push_str(line);
            content.push_str(term);
        }
        let tmp = NamedTempFile::new_in(cwd)?;
        fs::write(tmp.path(), content)?;
        let target = cwd.join(ATTRIBUTES_FILE);
        tmp.persist(target)
            .map_err(|e| TrackError::Persist(e.to_string()))?;
        Ok(())
    }
}

/// Pick the line-ending policy for writing `.gitattributes`. Honors
/// `core.autocrlf` first, then existing-file detection, then defaults to
/// LF. With `core.autocrlf=input`, only Windows hosts use CRLF.
pub fn detect_eol(cwd: &Path, attrs: &Attributes) -> Eol {
    let autocrlf = git_autocrlf(cwd);
    match autocrlf.as_deref() {
        Some("true") => return Eol::Crlf,
        Some("input") if cfg!(windows) => return Eol::Crlf,
        _ => {}
    }
    if attrs.had_crlf() {
        Eol::Crlf
    } else {
        Eol::Lf
    }
}

/// Return the trimmed body of a `.gitattributes` line, or `None` if the
/// line is blank or a comment. Per `gitattributes(5)`, `#` only starts a
/// comment when it's the line's first non-whitespace character — so a
/// pattern like `\#` (an escaped literal `#`) is *not* a comment.
fn uncommented(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    Some(trimmed)
}

fn git_autocrlf(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "core.autocrlf"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

/// Escape a user-supplied pattern for safe insertion into
/// `.gitattributes`. Spaces become `[[:space:]]`; a leading `#` is
/// backslash-escaped so it isn't read as a comment.
pub fn escape_attr_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for (i, c) in pattern.chars().enumerate() {
        match c {
            ' ' => out.push_str("[[:space:]]"),
            '#' if i == 0 => out.push_str("\\#"),
            _ => out.push(c),
        }
    }
    out
}

/// Strip a leading `./` prefix and apply `.gitattributes` escaping.
pub fn normalize_pattern(pattern: &str) -> String {
    let trimmed = pattern.strip_prefix("./").unwrap_or(pattern);
    escape_attr_pattern(trimmed)
}

/// If `pattern` would (textually or via globbing) match one of the
/// forbidden filenames, return that filename. Otherwise `None`.
pub fn forbidden_match(pattern: &str) -> Option<&'static str> {
    let stripped = pattern.trim_start_matches("./");
    for f in FORBIDDEN {
        if stripped == *f {
            return Some(*f);
        }
    }
    if let Ok(glob) = globset::GlobBuilder::new(stripped)
        .literal_separator(false)
        .build()
    {
        let m = glob.compile_matcher();
        for f in FORBIDDEN {
            if m.is_match(f) {
                return Some(*f);
            }
        }
    }
    None
}

/// Add each `pattern` to `.gitattributes` as LFS-tracked, idempotently.
/// Honors CRLF detection and `--dry-run`. The caller is expected to have
/// already vetted patterns against [`forbidden_match`] and printed any
/// blocklist diagnostics.
pub fn track(
    cwd: &Path,
    patterns: &[String],
    opts: TrackOptions,
) -> Result<TrackOutcome, TrackError> {
    let mut attrs = Attributes::read(cwd)?;
    let eol = detect_eol(cwd, &attrs);
    let mut out = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let normalized = normalize_pattern(pattern);
        let result = attrs.track(&normalized, opts.lockable);
        out.push(TrackedPattern {
            pattern: pattern.clone(),
            result,
        });
    }
    let any_changes = out
        .iter()
        .any(|p| matches!(p.result, TrackResult::Added | TrackResult::Replaced));
    if any_changes && !opts.dry_run {
        attrs.write(cwd, eol)?;
    }
    Ok(TrackOutcome { patterns: out })
}

/// Outcome of an [`untrack`] call.
pub struct UntrackOutcome {
    pub removed: Vec<String>,
    pub missing: Vec<String>,
}

/// Remove each `pattern` from `.gitattributes`. Idempotent.
pub fn untrack(cwd: &Path, patterns: &[String]) -> Result<UntrackOutcome, TrackError> {
    let mut attrs = Attributes::read(cwd)?;
    let eol = detect_eol(cwd, &attrs);
    let mut removed = Vec::new();
    let mut missing = Vec::new();
    for pattern in patterns {
        let normalized = normalize_pattern(pattern);
        if attrs.untrack(&normalized) {
            removed.push(pattern.clone());
        } else {
            missing.push(pattern.clone());
        }
    }
    if !removed.is_empty() {
        attrs.write(cwd, eol)?;
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

    fn write_bytes(dir: &Path, bytes: &[u8]) {
        fs::write(dir.join(ATTRIBUTES_FILE), bytes).unwrap();
    }

    #[test]
    fn track_creates_file_when_missing() {
        let tmp = TempDir::new().unwrap();
        let outcome = track(tmp.path(), &["*.jpg".into()], TrackOptions::default()).unwrap();
        assert_eq!(outcome.patterns.len(), 1);
        assert!(matches!(outcome.patterns[0].result, TrackResult::Added));
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content, "*.jpg filter=lfs diff=lfs merge=lfs -text\n");
    }

    #[test]
    fn track_appends_and_preserves_existing_content() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "* text=auto\n#*.cs diff=csharp\n");
        track(tmp.path(), &["*.jpg".into()], TrackOptions::default()).unwrap();
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
        let first = track(tmp.path(), &["*.jpg".into()], TrackOptions::default()).unwrap();
        assert!(matches!(first.patterns[0].result, TrackResult::Added));

        let second = track(tmp.path(), &["*.jpg".into()], TrackOptions::default()).unwrap();
        assert!(matches!(
            second.patterns[0].result,
            TrackResult::AlreadyTracked
        ));

        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content.matches("*.jpg").count(), 1);
    }

    #[test]
    fn dry_run_does_not_write_file() {
        let tmp = TempDir::new().unwrap();
        let outcome = track(
            tmp.path(),
            &["*.jpg".into()],
            TrackOptions {
                lockable: LockableMode::Default,
                dry_run: true,
            },
        )
        .unwrap();
        assert!(matches!(outcome.patterns[0].result, TrackResult::Added));
        assert!(!tmp.path().join(ATTRIBUTES_FILE).exists());
    }

    #[test]
    fn lockable_yes_replaces_existing_non_lockable_line() {
        let tmp = TempDir::new().unwrap();
        track(tmp.path(), &["*.png".into()], TrackOptions::default()).unwrap();
        let outcome = track(
            tmp.path(),
            &["*.png".into()],
            TrackOptions {
                lockable: LockableMode::Yes,
                dry_run: false,
            },
        )
        .unwrap();
        assert!(matches!(outcome.patterns[0].result, TrackResult::Replaced));
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content.matches("*.png").count(), 1);
        assert!(content.contains("lockable"));
    }

    #[test]
    fn lockable_no_strips_lockable_attribute() {
        let tmp = TempDir::new().unwrap();
        track(
            tmp.path(),
            &["*.png".into()],
            TrackOptions {
                lockable: LockableMode::Yes,
                dry_run: false,
            },
        )
        .unwrap();
        let outcome = track(
            tmp.path(),
            &["*.png".into()],
            TrackOptions {
                lockable: LockableMode::No,
                dry_run: false,
            },
        )
        .unwrap();
        assert!(matches!(outcome.patterns[0].result, TrackResult::Replaced));
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert!(!content.contains("lockable"));
    }

    #[test]
    fn lockable_default_preserves_existing_state() {
        let tmp = TempDir::new().unwrap();
        track(
            tmp.path(),
            &["*.jpg".into()],
            TrackOptions {
                lockable: LockableMode::Yes,
                dry_run: false,
            },
        )
        .unwrap();
        let outcome = track(tmp.path(), &["*.jpg".into()], TrackOptions::default()).unwrap();
        assert!(matches!(
            outcome.patterns[0].result,
            TrackResult::AlreadyTracked
        ));
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert!(content.contains("lockable"));
    }

    #[test]
    fn forbidden_match_blocks_literal_gitattributes() {
        assert_eq!(forbidden_match(".gitattributes"), Some(".gitattributes"));
        assert_eq!(forbidden_match("./.gitattributes"), Some(".gitattributes"));
    }

    #[test]
    fn forbidden_match_blocks_glob_against_dotfiles() {
        assert!(forbidden_match(".git*").is_some());
        assert!(forbidden_match("*").is_some());
    }

    #[test]
    fn forbidden_match_allows_normal_patterns() {
        assert_eq!(forbidden_match("*.jpg"), None);
        assert_eq!(forbidden_match("data/*.bin"), None);
    }

    #[test]
    fn escape_pattern_handles_spaces_and_leading_hash() {
        assert_eq!(escape_attr_pattern(" "), "[[:space:]]");
        assert_eq!(escape_attr_pattern("foo bar/*"), "foo[[:space:]]bar/*");
        assert_eq!(escape_attr_pattern("#"), "\\#");
        assert_eq!(escape_attr_pattern("foo#bar"), "foo#bar");
    }

    #[test]
    fn normalize_strips_leading_dot_slash() {
        assert_eq!(normalize_pattern("./a.dat"), "a.dat");
    }

    #[test]
    fn write_preserves_existing_crlf_terminators() {
        let tmp = TempDir::new().unwrap();
        // Existing file with CRLF lines, no trailing newline at all on
        // the last line — mirrors the upstream "track without trailing
        // linebreak" + "track with existing crlf" fixtures.
        write_bytes(tmp.path(), b"*.mov filter=lfs -text\r\n");
        let attrs = Attributes::read(tmp.path()).unwrap();
        assert!(attrs.had_crlf());
        let mut attrs = attrs;
        attrs.track("*.gif", LockableMode::Default);
        attrs.write(tmp.path(), Eol::Crlf).unwrap();
        let bytes = fs::read(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(
            bytes,
            b"*.mov filter=lfs -text\r\n*.gif filter=lfs diff=lfs merge=lfs -text\r\n"
        );
    }

    #[test]
    fn write_uses_lf_when_no_crlf_seen() {
        let tmp = TempDir::new().unwrap();
        write_bytes(tmp.path(), b"*.mov filter=lfs -text");
        let mut attrs = Attributes::read(tmp.path()).unwrap();
        assert!(!attrs.had_crlf());
        attrs.track("*.gif", LockableMode::Default);
        attrs.write(tmp.path(), Eol::Lf).unwrap();
        let bytes = fs::read(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(
            bytes,
            b"*.mov filter=lfs -text\n*.gif filter=lfs diff=lfs merge=lfs -text\n"
        );
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

    #[test]
    fn untrack_does_not_remove_negative_filter_line() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "*.gif -filter -text\n");
        let outcome = untrack(tmp.path(), &["*.gif".into()]).unwrap();
        // -filter line isn't an LFS-tracked line, so untrack shouldn't
        // remove it; the pattern is reported as missing.
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.missing, vec!["*.gif"]);
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content, "*.gif -filter -text\n");
    }
}
