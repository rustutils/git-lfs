//! `.gitattributes` parsing and matching.
//!
//! Matching uses `gix-attributes` and `gix-glob`, which implement Git's
//! wildmatch patterns and macros. This parses files only. Git operations
//! still shell out to `git`.
//!
//! An [`AttrSet`] is built in one of three ways:
//!
//! - [`AttrSet::from_workdir`] loads every `.gitattributes` in the working
//!   tree, plus the repository's `info/attributes`. As in Git, deeper
//!   directories win over shallower ones, and `info/attributes` wins over
//!   them all.
//! - [`AttrSet::from_buffer`] loads a single in-memory buffer.
//! - [`AttrSet::empty`] followed by [`AttrSet::add_buffer_at`] for each
//!   directory, for reading attributes out of a commit rather than the
//!   working tree.
//!
//! Query it with [`AttrSet::value`] and [`AttrSet::is_set`], or the LFS
//! helpers [`AttrSet::is_lfs_tracked`] and [`AttrSet::is_lockable`].
//!
//! [`list_lfs_patterns`] is separate. It lists the raw LFS pattern lines
//! for `git lfs track`, without matching any paths.
//!
//! [`PathPatterns`] matches paths against patterns given on their own,
//! such as `git lfs migrate --include`, by the same rules.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use bstr::ByteSlice;
use gix_attributes::{
    Search, StateRef,
    search::{MetadataCollection, Outcome},
};
use gix_glob::pattern::Case;

/// A queryable set of `.gitattributes` patterns.
pub struct AttrSet {
    search: Search,
    collection: MetadataCollection,
    /// Attribute keys each macro expands to, by macro name.
    ///
    /// `gix-attributes` does not expand `!<macro>`, so
    /// [`Self::intake_buffer`] uses this to expand it before the buffer is
    /// handed over.
    macros: HashMap<String, Vec<String>>,
}

impl AttrSet {
    /// Create a set with no patterns, only Git's built-in `binary` macro.
    pub fn empty() -> Self {
        let mut collection = MetadataCollection::default();
        let mut search = Search::default();
        search.add_patterns_buffer(
            b"[attr]binary -diff -merge -text",
            "[builtin]".into(),
            None,
            &mut collection,
            true,
        );
        let mut macros = HashMap::new();
        macros.insert(
            "binary".to_string(),
            vec!["diff".into(), "merge".into(), "text".into()],
        );
        Self {
            search,
            collection,
            macros,
        }
    }

    /// Build a set from one buffer in `.gitattributes` format.
    pub fn from_buffer(bytes: &[u8]) -> Self {
        let mut me = Self::empty();
        let rewritten = me.intake_buffer(bytes);
        me.search.add_patterns_buffer(
            &rewritten,
            "<memory>".into(),
            None,
            &mut me.collection,
            true,
        );
        me
    }

    /// Add a `.gitattributes` buffer that applies to paths under `dir`.
    ///
    /// `dir` uses `/` separators with no trailing slash, and is `""` for
    /// the repository root. Use this to evaluate attributes from a commit
    /// rather than the working tree.
    ///
    /// Buffers added later win, so add shallower directories before deeper
    /// ones to get Git's precedence.
    pub fn add_buffer_at(&mut self, bytes: &[u8], dir: &str) {
        let virtual_root = std::path::PathBuf::from("/__lfs_virt");
        let source = if dir.is_empty() {
            virtual_root.join(".gitattributes")
        } else {
            virtual_root.join(dir).join(".gitattributes")
        };
        let rewritten = self.intake_buffer(bytes);
        self.search.add_patterns_buffer(
            &rewritten,
            source,
            Some(&virtual_root),
            &mut self.collection,
            true,
        );
    }

    /// Load every `.gitattributes` under `repo_root`, plus the
    /// repository's `info/attributes` if it exists.
    ///
    /// Directories named `.git` are skipped at any depth. See the
    /// [module docs](self) for precedence.
    pub fn from_workdir(repo_root: &Path) -> io::Result<Self> {
        let mut me = Self::empty();

        // Read first, so its macros are known to every `.gitattributes`,
        // but added last.
        let info = info_attributes_path(repo_root);
        let info_patterns = match fs::read(&info) {
            Ok(bytes) if patterns_parse(&bytes, info.display()) => Some(me.intake_buffer(&bytes)),
            Ok(_) => None,
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };

        let mut found = Vec::new();
        walk_for_gitattributes(repo_root, &mut found)?;
        // gix-attributes searches pattern lists last to first, so adding
        // shallow before deep makes deeper files win.
        found.sort_by_key(|p| p.components().count());
        for path in found {
            let bytes = fs::read(&path)?;
            if !patterns_parse(&bytes, path.display()) {
                continue;
            }
            let rewritten = me.intake_buffer(&bytes);
            // gix-glob strips `repo_root` from the file's parent to get the
            // directory its patterns are relative to, such as `sub/` for
            // `sub/.gitattributes`.
            me.search.add_patterns_buffer(
                &rewritten,
                path,
                Some(repo_root),
                &mut me.collection,
                true,
            );
        }

        // Added last, so it wins over every `.gitattributes`, as in Git.
        if let Some(rewritten) = info_patterns {
            me.search
                .add_patterns_buffer(&rewritten, info, None, &mut me.collection, true);
        }
        Ok(me)
    }

    /// Record the macros declared in `bytes` and expand `!<macro>` tokens.
    ///
    /// Returns a copy of `bytes` where each `!<macro>` on a pattern line is
    /// replaced with `!attr1 !attr2 ...` for the macro's attributes.
    /// `gix-attributes` does not expand macro negations itself, so without
    /// this `b.dat !lfs` would leave `filter=lfs` in place (t-fsck test 11).
    /// Lines are processed in order, so a macro must be declared before it
    /// is negated, as in upstream's `MacroProcessor`.
    fn intake_buffer(&mut self, bytes: &[u8]) -> Vec<u8> {
        let Ok(s) = std::str::from_utf8(bytes) else {
            // Pass non-UTF-8 input through unchanged, without expanding
            // negations, rather than risk corrupting it.
            return bytes.to_vec();
        };
        let mut out = Vec::with_capacity(bytes.len());
        for line in s.split('\n') {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("[attr]") {
                // Record the macro, and pass the line through so
                // gix-attributes knows it too.
                let mut tokens = rest.split_whitespace();
                if let Some(name) = tokens.next() {
                    let attrs: Vec<String> = tokens
                        .map(|t| {
                            // Negation only needs the bare key.
                            let key = t.trim_start_matches(['-', '!']);
                            key.split_once('=')
                                .map(|(k, _)| k)
                                .unwrap_or(key)
                                .to_string()
                        })
                        .filter(|k| !k.is_empty())
                        .collect();
                    if !attrs.is_empty() {
                        self.macros.insert(name.to_string(), attrs);
                    }
                }
                out.extend_from_slice(line.as_bytes());
                out.push(b'\n');
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                out.extend_from_slice(line.as_bytes());
                out.push(b'\n');
                continue;
            }
            // The literal `!<macro>` token is dropped after expansion. If
            // it were kept, gix-attributes would expand the macro again at
            // lookup time and undo the expanded `!filter`. The cost is
            // that the macro's own name stays set on the negated path,
            // which nothing here queries.
            let leading_ws_len = line.len() - trimmed.len();
            out.extend_from_slice(&line.as_bytes()[..leading_ws_len]);
            let mut tokens = trimmed.split_whitespace();
            if let Some(pattern) = tokens.next() {
                out.extend_from_slice(pattern.as_bytes());
                for tok in tokens {
                    if let Some(name) = tok.strip_prefix('!')
                        && let Some(macro_attrs) = self.macros.get(name)
                    {
                        for k in macro_attrs {
                            out.push(b' ');
                            out.push(b'!');
                            out.extend_from_slice(k.as_bytes());
                        }
                        continue;
                    }
                    out.push(b' ');
                    out.extend_from_slice(tok.as_bytes());
                }
            }
            out.push(b'\n');
        }
        out
    }

    /// Return the value of `attr` for `path`.
    ///
    /// `path` is relative to the repository root, with `/` separators. A
    /// bare `attr` gives `"true"`, `attr=v` gives `v`, and an unset or
    /// unspecified attribute gives `None`.
    pub fn value(&self, path: &str, attr: &str) -> Option<String> {
        let mut out = Outcome::default();
        out.initialize_with_selection(&self.collection, [attr]);
        self.search
            .pattern_matching_relative_path(path.into(), Case::Sensitive, None, &mut out);
        for m in out.iter_selected() {
            if m.assignment.name.as_str() != attr {
                continue;
            }
            return match m.assignment.state {
                StateRef::Set => Some("true".into()),
                StateRef::Value(v) => Some(v.as_bstr().to_str_lossy().into_owned()),
                StateRef::Unset | StateRef::Unspecified => None,
            };
        }
        None
    }

    /// Whether `attr` is set for `path` to anything other than `false`.
    pub fn is_set(&self, path: &str, attr: &str) -> bool {
        matches!(self.value(path, attr).as_deref(), Some(v) if v != "false")
    }

    /// Whether `path` resolves to `filter=lfs`.
    pub fn is_lfs_tracked(&self, path: &str) -> bool {
        self.value(path, "filter").as_deref() == Some("lfs")
    }

    /// Whether `lockable` is set for `path`, as with [`Self::is_set`].
    pub fn is_lockable(&self, path: &str) -> bool {
        self.is_set(path, "lockable")
    }
}

/// One LFS pattern line found by [`list_lfs_patterns`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternEntry {
    /// The pattern, exactly as written in the file.
    pub pattern: String,
    /// The file the line came from.
    ///
    /// Working tree files are relative to the repository root with `/`
    /// separators. `.git/info/attributes` is given as that string. The
    /// user's global attributes file is given by its configured or default
    /// path.
    pub source: String,
    /// Whether the line sets `filter=lfs`.
    ///
    /// False means it clears the filter, with `-filter`, `!filter`,
    /// `-filter=...`, or by negating a macro that sets `filter=lfs`.
    pub tracked: bool,
    /// Whether the line has a bare `lockable` token.
    pub lockable: bool,
}

/// The LFS pattern lines found by [`list_lfs_patterns`], in load order.
///
/// The user's global attributes file comes first, then
/// `.git/info/attributes`, then each `.gitattributes` from shallow to deep.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PatternListing {
    pub patterns: Vec<PatternEntry>,
}

impl PatternListing {
    /// Lines that set `filter=lfs`.
    pub fn tracked(&self) -> impl Iterator<Item = &PatternEntry> {
        self.patterns.iter().filter(|p| p.tracked)
    }

    /// Lines that clear the filter.
    pub fn excluded(&self) -> impl Iterator<Item = &PatternEntry> {
        self.patterns.iter().filter(|p| !p.tracked)
    }
}

/// Macros seen so far while listing patterns, across files.
#[derive(Default)]
struct MacroState {
    /// Names of macros whose expansion sets `filter=lfs`, directly or
    /// through another macro.
    enables_lfs: std::collections::HashSet<String>,
}

impl MacroState {
    /// Record whether the macro declared by `line` sets `filter=lfs`.
    ///
    /// Does nothing unless `line` is an `[attr]<name>` declaration.
    fn ingest(&mut self, line: &str) {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("[attr]") else {
            return;
        };
        let mut tokens = rest.split_whitespace();
        let Some(name) = tokens.next() else {
            return;
        };
        // The last token that affects the filter wins.
        let mut enables = false;
        for tok in tokens {
            match self.classify(tok) {
                FilterEffect::SetLfs => enables = true,
                FilterEffect::Clear => enables = false,
                FilterEffect::None => {}
            }
        }
        if enables {
            self.enables_lfs.insert(name.to_owned());
        } else {
            self.enables_lfs.remove(name);
        }
    }

    /// Classify one attribute token by its effect on the filter.
    ///
    /// Macro references are resolved against the macros seen so far.
    fn classify(&self, tok: &str) -> FilterEffect {
        if tok == "filter=lfs" {
            return FilterEffect::SetLfs;
        }
        if tok == "-filter" || tok == "!filter" || tok.starts_with("-filter=") {
            return FilterEffect::Clear;
        }
        // `-<macro>` and `!<macro>` clear whatever the macro sets.
        if let Some(name) = tok.strip_prefix('-').or_else(|| tok.strip_prefix('!')) {
            if self.enables_lfs.contains(name) {
                return FilterEffect::Clear;
            }
            return FilterEffect::None;
        }
        if self.enables_lfs.contains(tok) {
            return FilterEffect::SetLfs;
        }
        FilterEffect::None
    }
}

enum FilterEffect {
    SetLfs,
    Clear,
    None,
}

/// List the pattern lines that set or clear `filter=lfs`, for
/// `git lfs track` with no arguments.
///
/// Reads the user's global attributes file, `.git/info/attributes`, and
/// every `.gitattributes` under `repo_root`, in that order. Lines whose
/// tokens do not touch the filter are skipped. Macros are followed, and a
/// macro declared in one file applies to the files read after it.
///
/// This only tokenizes lines. It does not match patterns against paths.
pub fn list_lfs_patterns(repo_root: &Path) -> io::Result<PatternListing> {
    let mut listing = PatternListing::default();
    let mut macros = MacroState::default();

    // The global file comes first, as in upstream's listing.
    if let Some((path, bytes)) = read_global_attributes(repo_root)
        && patterns_parse(&bytes, &path)
    {
        scan_attr_lines(&bytes, &path, &mut listing, &mut macros, true);
    }

    let info = info_attributes_path(repo_root);
    if info.exists() {
        let bytes = fs::read(&info)?;
        if patterns_parse(&bytes, info.display()) {
            scan_attr_lines(
                &bytes,
                ".git/info/attributes",
                &mut listing,
                &mut macros,
                true,
            );
        }
    }

    let mut found = Vec::new();
    walk_for_gitattributes(repo_root, &mut found)?;
    found.sort_by_key(|p| p.components().count());
    for path in found {
        let bytes = fs::read(&path)?;
        if !patterns_parse(&bytes, path.display()) {
            continue;
        }
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        // Git ignores macros declared below the root `.gitattributes`.
        let is_root = !rel.contains('/');
        scan_attr_lines(&bytes, &rel, &mut listing, &mut macros, is_root);
    }
    Ok(listing)
}

/// Read the user's global attributes file, returning its display path and
/// contents.
///
/// Tries `core.attributesfile` from the effective config, then
/// `$XDG_CONFIG_HOME/git/attributes`, or `$HOME/.config/git/attributes`
/// when `XDG_CONFIG_HOME` is unset or empty. Unlike Git, a configured
/// `core.attributesfile` that can't be read falls through to the XDG
/// path.
fn read_global_attributes(repo_root: &Path) -> Option<(String, Vec<u8>)> {
    if let Ok(Some(path)) = crate::config::get_effective(repo_root, "core.attributesfile") {
        let expanded = crate::path::expand_tilde(&path);
        if let Ok(bytes) = fs::read(&expanded) {
            return Some((path, bytes));
        }
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let path = xdg.join("git").join("attributes");
    let bytes = fs::read(&path).ok()?;
    Some((path.to_string_lossy().into_owned(), bytes))
}

fn scan_attr_lines(
    bytes: &[u8],
    source: &str,
    listing: &mut PatternListing,
    macros: &mut MacroState,
    allow_macros: bool,
) {
    for raw in bytes.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        // `#` starts a comment only as the first non-blank character, so
        // a pattern like `\#` is kept.
        let body = line.trim();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if body.starts_with("[attr]") {
            if allow_macros {
                macros.ingest(body);
            }
            continue;
        }
        let mut tokens = body.split_whitespace();
        let Some(pattern) = tokens.next() else {
            continue;
        };
        let mut filter: Option<bool> = None;
        let mut lockable = false;
        for tok in tokens {
            match macros.classify(tok) {
                FilterEffect::SetLfs => filter = Some(true),
                FilterEffect::Clear => filter = Some(false),
                FilterEffect::None => {}
            }
            if tok == "lockable" {
                lockable = true;
            }
        }
        if let Some(tracked) = filter {
            listing.patterns.push(PatternEntry {
                pattern: pattern.to_owned(),
                source: source.to_owned(),
                tracked,
                lockable,
            });
        }
    }
}

/// The repository's `info/attributes`.
///
/// It's in the common Git directory, so a linked worktree, whose `.git` is
/// a file, shares the main worktree's.
fn info_attributes_path(repo_root: &Path) -> PathBuf {
    let dot_git = repo_root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        crate::path::git_common_dir(repo_root).unwrap_or(dot_git)
    };
    git_dir.join("info").join("attributes")
}

/// Whether the patterns in the attributes file `path` all parse.
///
/// Like upstream, a file with a pattern its matcher rejects is ignored as
/// a whole, with a `GIT_TRACE` line saying why.
fn patterns_parse(bytes: &[u8], path: impl std::fmt::Display) -> bool {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        let body = line.trim();
        if body.is_empty() || body.starts_with('#') || body.starts_with("[attr]") {
            continue;
        }
        let Some(pattern) = body.split_whitespace().next() else {
            continue;
        };
        if let Some(err) = crate::wildmatch::pattern_error(pattern) {
            if crate::trace_enabled() {
                eprintln!(
                    "Error parsing attributes from {path}: invalid attribute pattern {pattern:?}: {err}"
                );
            }
            return false;
        }
    }
    true
}

fn walk_for_gitattributes(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        if name == OsStr::new(".git") {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            walk_for_gitattributes(&path, out)?;
        } else if ft.is_file() && name == OsStr::new(".gitattributes") {
            out.push(path);
        }
    }
    Ok(())
}

/// Patterns matched against paths the way `.gitattributes` patterns are.
///
/// A pattern without a slash matches the file name at any depth, and
/// one with a slash matches the whole path from the repository root. A
/// leading slash only anchors the pattern. A `*` doesn't match a slash,
/// but `**` does. A pattern ending in a slash matches only directories,
/// so never a file.
#[derive(Debug, Clone)]
pub struct PathPatterns {
    patterns: Vec<gix_glob::Pattern>,
}

impl PathPatterns {
    /// Parse `patterns`, skipping empty ones and comments, which match
    /// nothing.
    pub fn new<S: AsRef<str>>(patterns: impl IntoIterator<Item = S>) -> Self {
        let patterns = patterns
            .into_iter()
            .filter_map(|p| gix_glob::Pattern::from_bytes_without_negation(p.as_ref().as_bytes()))
            .collect();
        Self { patterns }
    }

    /// Whether any pattern matches the file at `path`, relative to the
    /// repository root and separated by slashes.
    pub fn is_match(&self, path: &str) -> bool {
        let basename = path.rfind('/').map(|i| i + 1);
        self.patterns.iter().any(|p| {
            p.matches_repo_relative_path(
                path.into(),
                basename,
                Some(false),
                Case::Sensitive,
                gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn path_patterns_match_like_gitattributes() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("a.txt", &["a.txt", "dir/a.txt"], &["b.txt", "a.txt/b"]),
            ("/a.txt", &["a.txt"], &["dir/a.txt"]),
            ("*.txt", &["a.txt", "x/y/a.txt"], &["bar.txt/b.md"]),
            ("dir/*.txt", &["dir/a.txt"], &["dir/x/a.txt", "x/dir/a.txt"]),
            ("foo/**", &["foo/a", "foo/b/c"], &["a/foo/b"]),
            ("**/*ar/**", &["foo/bar/baz/a.txt"], &["foo/baz/a.txt"]),
            ("a[[:space:]]b", &["a b"], &["ab"]),
            ("dir/", &[], &["dir", "dir/a"]),
        ];
        for (pattern, matching, other) in cases {
            let set = PathPatterns::new([pattern]);
            for path in *matching {
                assert!(set.is_match(path), "{pattern} should match {path}");
            }
            for path in *other {
                assert!(!set.is_match(path), "{pattern} matched {path}");
            }
        }
        assert!(!PathPatterns::new(["", "# comment"]).is_match(""));
    }

    #[test]
    fn negated_macro_unsets_constituent_attrs() {
        // Regression for t-fsck 11. `[attr]lfs ...` declares a macro,
        // `*.dat lfs` applies it (so .dat files become filter=lfs),
        // `b.dat !lfs` unsets it. After our intake-time rewrite
        // expands `!lfs` into `!filter !diff !merge !text`, gix
        // reports filter=None for b.dat and is_lfs_tracked returns
        // false for it.
        let s = AttrSet::from_buffer(
            b"[attr]lfs filter=lfs diff=lfs merge=lfs -text\n\
              *.dat lfs\n\
              b.dat !lfs\n",
        );
        assert_eq!(s.value("a.dat", "filter").as_deref(), Some("lfs"));
        assert_eq!(s.value("b.dat", "filter"), None);
        assert!(s.is_lfs_tracked("a.dat"));
        assert!(!s.is_lfs_tracked("b.dat"));
    }

    #[test]
    fn empty_set_has_no_matches() {
        let s = AttrSet::empty();
        assert_eq!(s.value("foo.txt", "filter"), None);
        assert!(!s.is_lfs_tracked("foo.txt"));
        assert!(!s.is_lockable("foo.txt"));
    }

    #[test]
    fn buffer_basename_match() {
        let s = AttrSet::from_buffer(b"*.bin filter=lfs diff=lfs merge=lfs -text\n");
        assert!(s.is_lfs_tracked("foo.bin"));
        assert!(s.is_lfs_tracked("nested/dir/foo.bin"));
        assert!(!s.is_lfs_tracked("foo.txt"));
    }

    #[test]
    fn value_returns_raw_string() {
        let s = AttrSet::from_buffer(b"*.txt eol=lf\n");
        assert_eq!(s.value("a.txt", "eol").as_deref(), Some("lf"));
    }

    #[test]
    fn unset_attribute_via_dash_prefix() {
        let s = AttrSet::from_buffer(
            b"*.txt filter=lfs\n\
              special.txt -filter\n",
        );
        assert!(s.is_lfs_tracked("a.txt"));
        // `special.txt -filter` removes the filter attribute → value is None.
        assert_eq!(s.value("special.txt", "filter"), None);
        assert!(!s.is_lfs_tracked("special.txt"));
    }

    #[test]
    fn lockable_set_form() {
        let s = AttrSet::from_buffer(b"*.psd lockable\n");
        assert!(s.is_lockable("art/cover.psd"));
        assert!(!s.is_lockable("readme.txt"));
    }

    #[test]
    fn is_set_treats_false_value_as_unset() {
        let s = AttrSet::from_buffer(
            b"truthy lockable\n\
              falsy  lockable=false\n",
        );
        assert!(s.is_set("truthy", "lockable"));
        assert!(!s.is_set("falsy", "lockable"));
    }

    #[test]
    fn rooted_pattern_only_matches_top_level() {
        let s = AttrSet::from_buffer(b"/top.bin filter=lfs\n");
        assert!(s.is_lfs_tracked("top.bin"));
        assert!(!s.is_lfs_tracked("nested/top.bin"));
    }

    #[test]
    fn workdir_loads_root_gitattributes() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "*.bin filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(s.is_lfs_tracked("a.bin"));
        assert!(s.is_lfs_tracked("sub/a.bin"));
    }

    #[test]
    fn deeper_gitattributes_overrides_root() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("sub/.git_placeholder")).unwrap();
        std::fs::write(tmp.path().join(".gitattributes"), "*.bin filter=lfs\n").unwrap();
        std::fs::write(tmp.path().join("sub/.gitattributes"), "*.bin -filter\n").unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(s.is_lfs_tracked("a.bin"));
        // Deeper -filter wins for paths within sub/.
        assert!(!s.is_lfs_tracked("sub/a.bin"));
    }

    #[test]
    fn info_attributes_loaded_from_dotgit() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::write(
            tmp.path().join(".git/info/attributes"),
            "*.bin filter=lfs\n",
        )
        .unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(s.is_lfs_tracked("a.bin"));
    }

    #[test]
    fn list_lfs_patterns_recursive() {
        // Mirror upstream t-track.sh's "track" test fixture: root
        // .gitattributes, .git/info/attributes, and nested per-directory
        // files, with one nested dir adding `-filter` exclusions.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "* text=auto\n\
             *.jpg filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join(".git/info/attributes"),
            "*.mov filter=lfs -text\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("a/.gitattributes"),
            "*.gif filter=lfs -text\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("a/b/.gitattributes"),
            "*.png filter=lfs -text\n\
             *.gif -filter -text\n\
             *.mov -filter=lfs -text\n",
        )
        .unwrap();

        let listing = list_lfs_patterns(tmp.path()).unwrap();
        let tracked: Vec<(&str, &str)> = listing
            .tracked()
            .map(|p| (p.pattern.as_str(), p.source.as_str()))
            .collect();
        let excluded: Vec<(&str, &str)> = listing
            .excluded()
            .map(|p| (p.pattern.as_str(), p.source.as_str()))
            .collect();

        // info/attributes is loaded first, then root → deepest .gitattributes.
        assert_eq!(
            tracked,
            vec![
                ("*.mov", ".git/info/attributes"),
                ("*.jpg", ".gitattributes"),
                ("*.gif", "a/.gitattributes"),
                ("*.png", "a/b/.gitattributes"),
            ]
        );
        assert_eq!(
            excluded,
            vec![
                ("*.gif", "a/b/.gitattributes"),
                ("*.mov", "a/b/.gitattributes"),
            ]
        );
    }

    #[test]
    fn list_lfs_patterns_skips_macros_and_comments() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "[attr]binary -diff -merge -text\n\
             # *.jpg filter=lfs\n\
             *.bin filter=lfs -text\n",
        )
        .unwrap();
        let listing = list_lfs_patterns(tmp.path()).unwrap();
        let tracked: Vec<&PatternEntry> = listing.tracked().collect();
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].pattern, "*.bin");
    }

    #[test]
    fn list_picks_up_lockable_attribute() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "*.psd filter=lfs diff=lfs merge=lfs lockable\n\
             *.bin filter=lfs diff=lfs merge=lfs\n",
        )
        .unwrap();
        let listing = list_lfs_patterns(tmp.path()).unwrap();
        assert_eq!(listing.patterns.len(), 2);
        assert_eq!(listing.patterns[0].pattern, "*.psd");
        assert!(listing.patterns[0].lockable);
        assert_eq!(listing.patterns[1].pattern, "*.bin");
        assert!(!listing.patterns[1].lockable);
    }

    #[test]
    fn list_expands_macro_to_lfs() {
        // `[attr]lfs filter=lfs ...` registers a macro, and `*.dat lfs`
        // applies it. Upstream's `git lfs track` lists `*.dat` as a
        // tracked pattern because the macro resolves to filter=lfs.
        // Lands t-attributes test 1.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "[attr]lfs filter=lfs diff=lfs merge=lfs -text\n\
             *.dat lfs\n",
        )
        .unwrap();
        let listing = list_lfs_patterns(tmp.path()).unwrap();
        let tracked: Vec<&str> = listing.tracked().map(|p| p.pattern.as_str()).collect();
        assert_eq!(tracked, vec!["*.dat"]);
    }

    #[test]
    fn list_expands_macro_defined_in_earlier_file() {
        // Macro defined in `.git/info/attributes`, referenced by a
        // pattern in `.gitattributes`. The pattern still resolves
        // because we accumulate macros across files in load order.
        // Models t-attributes test 3 (global → local).
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::write(
            tmp.path().join(".git/info/attributes"),
            "[attr]lfs filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join(".gitattributes"), "*.dat lfs\n").unwrap();
        let listing = list_lfs_patterns(tmp.path()).unwrap();
        let tracked: Vec<&str> = listing.tracked().map(|p| p.pattern.as_str()).collect();
        assert_eq!(tracked, vec!["*.dat"]);
    }

    #[test]
    fn list_negated_macro_marks_excluded() {
        // In `*.dat !lfs`, the `!` negation clears the macro's filter
        // attribute, so the pattern is recorded as excluded rather
        // than tracked. Models the dir-level override in t-attributes
        // "macros with unspecified flag".
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "[attr]lfs filter=lfs diff=lfs merge=lfs -text\n\
             **/*.dat lfs\n\
             other.dat !lfs\n",
        )
        .unwrap();
        let listing = list_lfs_patterns(tmp.path()).unwrap();
        let tracked: Vec<&str> = listing.tracked().map(|p| p.pattern.as_str()).collect();
        let excluded: Vec<&str> = listing.excluded().map(|p| p.pattern.as_str()).collect();
        assert_eq!(tracked, vec!["**/*.dat"]);
        assert_eq!(excluded, vec!["other.dat"]);
    }

    #[test]
    fn bang_filter_treated_as_excluded() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "*.dat filter=lfs\n\
             a.dat !filter\n",
        )
        .unwrap();
        let listing = list_lfs_patterns(tmp.path()).unwrap();
        assert_eq!(listing.patterns.len(), 2);
        assert!(listing.patterns[0].tracked);
        assert_eq!(listing.patterns[1].pattern, "a.dat");
        assert!(!listing.patterns[1].tracked);
    }

    #[test]
    fn info_attributes_win_over_gitattributes() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "*.bin filter=lfs lockable\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("sub/.gitattributes"), "*.bin filter=lfs\n").unwrap();
        std::fs::write(
            tmp.path().join(".git/info/attributes"),
            "*.bin -filter -lockable\n",
        )
        .unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(!s.is_lfs_tracked("a.bin"));
        assert!(!s.is_lfs_tracked("sub/a.bin"));
        assert!(!s.is_lockable("a.bin"));
    }

    #[test]
    fn info_attributes_macros_apply_to_gitattributes() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::write(
            tmp.path().join(".git/info/attributes"),
            "[attr]lfs filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join(".gitattributes"), "*.dat lfs\n*.tmp !lfs\n").unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(s.is_lfs_tracked("a.dat"));
        assert!(!s.is_lfs_tracked("a.tmp"));
    }

    #[test]
    fn linked_worktree_reads_the_common_info_attributes() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        let git = |cwd: &Path, args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(
            &main,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        );
        std::fs::write(main.join(".git/info/attributes"), "*.bin filter=lfs\n").unwrap();
        git(&main, &["worktree", "add", "-q", "../wt"]);

        let s = AttrSet::from_workdir(&tmp.path().join("wt")).unwrap();
        assert!(s.is_lfs_tracked("a.bin"));
    }

    #[test]
    fn workdir_skips_dotgit_directory() {
        // A .gitattributes inside .git/ must not be picked up. Only
        // .git/info/attributes is, and that is loaded explicitly.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join(".git/.gitattributes"), "*.bin filter=lfs\n").unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(!s.is_lfs_tracked("a.bin"));
    }
}
