//! Reading and writing the LFS patterns in a `.gitattributes` file, for
//! `git lfs track` and `git lfs untrack`.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use tempfile::NamedTempFile;

const ATTRIBUTES_FILE: &str = ".gitattributes";

/// The attributes `track` writes after a pattern, as upstream writes them.
const LFS_FILTER_TAIL: &str = "filter=lfs diff=lfs merge=lfs -text";

/// File name prefixes that must not be tracked, such as `.gitattributes`
/// and `.lfsconfig`. Upstream's `prefixBlocklist`.
const FORBIDDEN_PREFIXES: &[&str] = &[".git", ".lfs"];

#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("failed to commit .gitattributes: {0}")]
    Persist(String),
}

/// Whether `--lockable` or `--not-lockable` was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockableMode {
    /// `--lockable`: the line must have the `lockable` attribute.
    Yes,
    /// `--not-lockable`: the line must not have it.
    No,
    /// Neither: existing lines are kept, and new lines aren't lockable.
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
    /// A new line was appended.
    Added,
    /// The existing line was rewritten to change its `lockable` state.
    Replaced,
    /// The existing line was already as requested.
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
    /// `--filename`: escape glob characters so each pattern matches only
    /// the file of that name.
    pub literal_filename: bool,
}

impl Default for TrackOptions {
    fn default() -> Self {
        Self {
            lockable: LockableMode::Default,
            dry_run: false,
            literal_filename: false,
        }
    }
}

/// The lines of a `.gitattributes` file.
///
/// Lines that aren't changed are written back as they were, apart from
/// line endings.
pub struct Attributes {
    lines: Vec<String>,
    /// The file's line ending, if any line sets `filter` or `lockable`.
    eol: Option<Eol>,
}

impl Attributes {
    pub fn read(cwd: &Path) -> Result<Self, TrackError> {
        let path = cwd.join(ATTRIBUTES_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                let s = String::from_utf8_lossy(&bytes).into_owned();
                let lines: Vec<String> = s.lines().map(String::from).collect();
                let eol = lines
                    .iter()
                    .any(|l| sets_filter_or_lockable(l))
                    .then(|| majority_eol(&s))
                    .flatten();
                Ok(Self { lines, eol })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self {
                lines: Vec::new(),
                eol: None,
            }),
            Err(e) => Err(e.into()),
        }
    }

    /// The file's line ending, if any line sets `filter` or `lockable`.
    pub fn eol(&self) -> Option<Eol> {
        self.eol
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

    /// Add or update the line for `pattern`, which is already escaped.
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

    /// Remove every line for `pattern` that sets `filter=lfs`.
    ///
    /// Returns whether any line was removed. Both sides are compared with
    /// [`canonical_attr_pattern`], so `a b` matches `./a[[:space:]]b`.
    pub fn untrack(&mut self, pattern: &str) -> bool {
        let want = canonical_attr_pattern(pattern);
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
            !(is_lfs && canonical_attr_pattern(first) == want)
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

/// Pick the line ending for writing `.gitattributes`, in upstream's order.
///
/// The file's own line ending wins if it has a line that sets `filter` or
/// `lockable`. Otherwise CRLF if `core.autocrlf` is true, and then the
/// platform's: CRLF on Windows, LF elsewhere.
pub fn detect_eol(cwd: &Path, attrs: &Attributes) -> Eol {
    if let Some(eol) = attrs.eol() {
        return eol;
    }
    let autocrlf = git_autocrlf(cwd).map(|v| v.to_ascii_lowercase());
    if matches!(autocrlf.as_deref(), Some("true" | "t" | "1")) || cfg!(windows) {
        Eol::Crlf
    } else {
        Eol::Lf
    }
}

/// CRLF if more lines end in CRLF than in a bare LF, otherwise LF. `None`
/// if no line ends in either.
///
/// A last line without a line ending isn't counted, as in upstream.
fn majority_eol(text: &str) -> Option<Eol> {
    let (mut lf, mut crlf) = (0usize, 0usize);
    for line in text.split_inclusive('\n').filter(|l| l.ends_with('\n')) {
        if line.ends_with("\r\n") {
            crlf += 1;
        } else {
            lf += 1;
        }
    }
    if crlf > lf {
        Some(Eol::Crlf)
    } else if lf == 0 {
        None
    } else {
        Some(Eol::Lf)
    }
}

/// Whether an attributes line sets `filter`, in any form, or `lockable`.
///
/// Macro definitions are skipped, and macros aren't expanded.
fn sets_filter_or_lockable(line: &str) -> bool {
    let Some(body) = uncommented(line) else {
        return false;
    };
    if body.starts_with("[attr]") {
        return false;
    }
    body.split_whitespace().skip(1).any(|attr| {
        let name = attr.trim_start_matches(['-', '!']);
        let name = name.split_once('=').map_or(name, |(k, _)| k);
        name == "filter" || attr == "lockable" || attr == "lockable=true"
    })
}

/// The trimmed line, or `None` for a blank line or comment.
///
/// A comment starts with `#`, so an escaped `\#` pattern isn't one.
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

/// Escape a pattern for `.gitattributes`.
///
/// Spaces become `[[:space:]]`, and a leading `#` is escaped so it isn't
/// read as a comment.
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

/// A pattern without a leading `./` and with [`unescape_attr_pattern`]
/// applied, for comparing a pattern argument with a file's.
fn canonical_attr_pattern(pattern: &str) -> String {
    let trimmed = pattern.strip_prefix("./").unwrap_or(pattern);
    unescape_attr_pattern(trimmed)
}

/// Undo the escaping of spaces, `#`, and backslashes.
///
/// Turns `[[:space:]]` into a space, `\#` into `#`, and `\\` into `\`.
/// Escaped glob characters such as `\*` are kept.
pub fn unescape_attr_pattern(escaped: &str) -> String {
    // Undo the doubled backslashes last, or `\\#` would become `#`.
    let mut s = escaped.replace("[[:space:]]", " ");
    s = s.replace("\\#", "#");
    s = s.replace("\\\\", "\\");
    s
}

/// Escape a file name for `.gitattributes`, for `track --filename`.
///
/// Backslashes are doubled, then `*`, `?`, `[`, and `]` are escaped, and
/// spaces and a leading `#` are escaped as in [`escape_attr_pattern`]. On
/// Windows, upstream turns backslashes into slashes instead. This doesn't.
pub fn escape_glob_characters(pattern: &str) -> String {
    // Double backslashes first, so the escapes added below stay single.
    let mut step1 = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        if c == '\\' {
            step1.push_str("\\\\");
        } else {
            step1.push(c);
        }
    }
    let mut out = String::with_capacity(step1.len());
    for (i, c) in step1.chars().enumerate() {
        match c {
            '*' | '?' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            ' ' => out.push_str("[[:space:]]"),
            '#' if i == 0 => out.push_str("\\#"),
            _ => out.push(c),
        }
    }
    out
}

/// The name of a file in the index that `pattern` matches and that starts
/// with `.git` or `.lfs`, if there is one.
///
/// Like upstream's `blocklistItem`, this checks files Git lists for the
/// pattern, not the pattern's text, so `*` is fine until such a file is
/// added.
pub fn forbidden_match(cwd: &Path, pattern: &str) -> Option<String> {
    // `git ls-files -x` doesn't handle a leading `/`.
    let safe = pattern.strip_prefix('/').unwrap_or(pattern);
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-files", "--ignored", "--cached", "-z", "-x", safe])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    for record in out.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let path = std::str::from_utf8(record).ok()?;
        let base = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if FORBIDDEN_PREFIXES.iter().any(|p| base.starts_with(p)) {
            return Some(base);
        }
    }
    None
}

/// Add a line for each pattern to `cwd/.gitattributes`.
///
/// The file is only written if a line changed and this isn't a dry run.
/// Patterns aren't checked with [`forbidden_match`] here.
pub fn track(
    cwd: &Path,
    patterns: &[String],
    opts: TrackOptions,
) -> Result<TrackOutcome, TrackError> {
    let mut attrs = Attributes::read(cwd)?;
    let eol = detect_eol(cwd, &attrs);
    let mut out = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let trimmed = pattern.strip_prefix("./").unwrap_or(pattern);
        let normalized = if opts.literal_filename {
            escape_glob_characters(trimmed)
        } else {
            escape_attr_pattern(trimmed)
        };
        let result = attrs.track(&normalized, opts.lockable);
        out.push(TrackedPattern {
            // The escaped form. The caller unescapes it for display,
            // which keeps escaped glob characters as upstream does, and
            // passes it to `git ls-files` as is.
            pattern: normalized.clone(),
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

/// Remove the lines for each pattern from `cwd/.gitattributes`.
pub fn untrack(cwd: &Path, patterns: &[String]) -> Result<UntrackOutcome, TrackError> {
    let mut attrs = Attributes::read(cwd)?;
    let eol = detect_eol(cwd, &attrs);
    let mut removed = Vec::new();
    let mut missing = Vec::new();
    for pattern in patterns {
        // Not escaped: `Attributes::untrack` unescapes both sides.
        if attrs.untrack(pattern) {
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
                literal_filename: false,
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
                literal_filename: false,
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
                literal_filename: false,
            },
        )
        .unwrap();
        let outcome = track(
            tmp.path(),
            &["*.png".into()],
            TrackOptions {
                lockable: LockableMode::No,
                dry_run: false,
                literal_filename: false,
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
                literal_filename: false,
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
    fn escape_pattern_handles_spaces_and_leading_hash() {
        assert_eq!(escape_attr_pattern(" "), "[[:space:]]");
        assert_eq!(escape_attr_pattern("foo bar/*"), "foo[[:space:]]bar/*");
        assert_eq!(escape_attr_pattern("#"), "\\#");
        assert_eq!(escape_attr_pattern("foo#bar"), "foo#bar");
    }

    #[test]
    fn escape_glob_characters_quotes_literal_metachars() {
        assert_eq!(escape_glob_characters("[foo]bar.txt"), "\\[foo\\]bar.txt");
        assert_eq!(escape_glob_characters("a*b?c.bin"), "a\\*b\\?c.bin");
    }

    #[test]
    fn escape_glob_characters_handles_backslash_then_metachars() {
        // Backslashes are doubled before the glob characters are
        // escaped.
        assert_eq!(
            escape_glob_characters("*[foo] \\n bar?.txt"),
            "\\*\\[foo\\][[:space:]]\\\\n[[:space:]]bar\\?.txt"
        );
    }

    #[test]
    fn unescape_attr_pattern_reverses_space_hash_and_double_backslash() {
        assert_eq!(unescape_attr_pattern("foo[[:space:]]bar"), "foo bar");
        assert_eq!(unescape_attr_pattern("\\#foo"), "#foo");
        assert_eq!(unescape_attr_pattern("a\\\\b"), "a\\b");
        // Escaped glob characters are kept.
        assert_eq!(unescape_attr_pattern("\\[foo\\]"), "\\[foo\\]");
    }

    #[test]
    fn write_preserves_existing_crlf_terminators() {
        let tmp = TempDir::new().unwrap();
        write_bytes(tmp.path(), b"*.mov filter=lfs -text\r\n");
        let attrs = Attributes::read(tmp.path()).unwrap();
        assert_eq!(attrs.eol(), Some(Eol::Crlf));
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
    fn eol_comes_from_lines_with_filter_or_lockable() {
        let tmp = TempDir::new().unwrap();
        let eol = |content: &[u8]| {
            write_bytes(tmp.path(), content);
            Attributes::read(tmp.path()).unwrap().eol()
        };
        // Without a filter or lockable line, the file doesn't decide.
        assert_eq!(eol(b"* text=auto\r\n"), None);
        assert_eq!(eol(b"*.a -filter\r\n"), Some(Eol::Crlf));
        assert_eq!(eol(b"*.a lockable\r\n"), Some(Eol::Crlf));
        // The majority wins, and a tie is LF.
        assert_eq!(
            eol(b"*.a filter=lfs\r\n*.b filter=lfs\r\n*.c filter=lfs\n"),
            Some(Eol::Crlf)
        );
        assert_eq!(eol(b"*.a filter=lfs\r\n*.b filter=lfs\n"), Some(Eol::Lf));
    }

    #[test]
    fn write_uses_lf_when_no_crlf_seen() {
        let tmp = TempDir::new().unwrap();
        write_bytes(tmp.path(), b"*.mov filter=lfs -text");
        let mut attrs = Attributes::read(tmp.path()).unwrap();
        // The only line has no line ending.
        assert_eq!(attrs.eol(), None);
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
        // A `-filter` line doesn't track the pattern.
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.missing, vec!["*.gif"]);
        let content = fs::read_to_string(tmp.path().join(ATTRIBUTES_FILE)).unwrap();
        assert_eq!(content, "*.gif -filter -text\n");
    }
}
