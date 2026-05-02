//! `.gitattributes` parsing and matching.
//!
//! Backed by `gix-attributes` + `gix-glob`, which together implement Git's
//! wildmatch + macro + per-directory precedence semantics. The "shell out
//! to git, not gix" rule in CLAUDE.md applies to runtime git operations
//! (rev-list, cat-file, etc.), not to using gix-* crates as parsing libs.
//!
//! Two construction modes:
//!
//! - [`AttrSet::from_workdir`] — discover and load every `.gitattributes`
//!   in the working tree, plus `.git/info/attributes`. Per-directory files
//!   take precedence over `info/attributes`; deeper directories win over
//!   shallower (Git's standard "more specific wins").
//! - [`AttrSet::from_buffer`] — load from a single in-memory buffer. For
//!   tests and one-shot matching that doesn't need a workdir.
//!
//! Once built, query with [`AttrSet::value`] / [`AttrSet::is_set`], plus
//! the LFS-specific helpers [`AttrSet::is_lfs_tracked`] /
//! [`AttrSet::is_lockable`].

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
    /// Macro name → list of attribute keys that macro expands to.
    /// Tracked alongside gix-attributes' internal macro state so we
    /// can work around its lack of `!<macro>` expansion: when we see
    /// `<pattern> !<macro>` in a buffer, we rewrite it to
    /// `<pattern> !attr1 !attr2 … !<macro>` before handing the bytes
    /// off, since gix only honors the `!<macro>` token literally.
    macros: HashMap<String, Vec<String>>,
}

impl AttrSet {
    /// Empty set, seeded only with Git's built-in `[attr]binary` macro
    /// (so patterns referencing `binary` resolve correctly).
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

    /// Build from a single `.gitattributes`-format buffer.
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

    /// Add a `.gitattributes` buffer that should match paths under
    /// `dir` (forward-slash separated, no trailing slash, `""` for the
    /// repo root). For per-commit evaluation during streaming
    /// rewrites where the on-disk working tree isn't authoritative.
    /// Order of calls matters — gix-attributes iterates lists in
    /// reverse, so deeper directories should be added *after*
    /// shallower ones to win precedence (matching Git's "more
    /// specific path overrides shallower" semantics).
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

    /// Discover every `.gitattributes` reachable from `repo_root` (skipping
    /// the `.git/` directory) and load them along with `.git/info/attributes`
    /// if it exists.
    pub fn from_workdir(repo_root: &Path) -> io::Result<Self> {
        let mut me = Self::empty();

        let info = repo_root.join(".git").join("info").join("attributes");
        if info.exists() {
            let bytes = fs::read(&info)?;
            let rewritten = me.intake_buffer(&bytes);
            me.search
                .add_patterns_buffer(&rewritten, info, None, &mut me.collection, true);
        }

        let mut found = Vec::new();
        walk_for_gitattributes(repo_root, &mut found)?;
        // Shallow → deep: gix-attributes iterates pattern lists in reverse
        // when matching, so the last-added (deepest) wins — matching Git's
        // "more specific path overrides shallower" semantics.
        found.sort_by_key(|p| p.components().count());
        for path in found {
            let bytes = fs::read(&path)?;
            let rewritten = me.intake_buffer(&bytes);
            // `root` is always the repo root. gix-glob computes each file's
            // relative `base` by stripping the repo-root prefix from
            // `source.parent()` — so root.gitattributes ends up with no base
            // (matches paths directly) while sub/.gitattributes ends up with
            // base `sub/` (strips `sub/` before matching).
            me.search.add_patterns_buffer(
                &rewritten,
                path,
                Some(repo_root),
                &mut me.collection,
                true,
            );
        }
        Ok(me)
    }

    /// Single-pass macro intake: scans `bytes` for `[attr]<name> ...`
    /// declarations to update [`Self::macros`] and returns a rewritten
    /// copy with each `<pattern> !<macro>` token expanded to the
    /// underlying `!attr1 !attr2 … !<macro>` sequence. Lets us work
    /// around `gix-attributes` not expanding macro negations
    /// (test 11 of t-fsck: `b.dat !lfs` should leave `filter`
    /// unspecified, not just unset the literal `lfs` attribute).
    /// Macros are processed in declaration order — same constraint
    /// upstream's `MacroProcessor` documents — so a buffer that
    /// declares and immediately uses a macro is fine.
    fn intake_buffer(&mut self, bytes: &[u8]) -> Vec<u8> {
        let Ok(s) = std::str::from_utf8(bytes) else {
            // Non-UTF-8 buffer: pass through. We'd rather miss the
            // negation expansion than corrupt the bytes. Real
            // .gitattributes files are UTF-8 in practice.
            return bytes.to_vec();
        };
        let mut out = Vec::with_capacity(bytes.len());
        for line in s.split('\n') {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("[attr]") {
                // `[attr]<name> <attr>...` — register macro, pass line
                // through verbatim so gix-attributes also knows about it.
                let mut tokens = rest.split_whitespace();
                if let Some(name) = tokens.next() {
                    let attrs: Vec<String> = tokens
                        .map(|t| {
                            // Strip leading `-`/`!` and any `=value` suffix
                            // — we only need the bare key for negation
                            // expansion later.
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
            // Pattern line: tokenize and expand any `!<macro>` references.
            // First whitespace-separated token is the pattern; remainder
            // are attribute settings. When we expand `!<macro>`, we
            // *drop* the literal `!<macro>` token from the rewritten
            // line — feeding gix-attributes both the expanded
            // `!filter !diff …` set *and* the literal `!lfs` makes it
            // re-trigger its own macro expansion at lookup time and
            // wipe out our `!filter`. The trade-off is that the macro
            // *name* itself stays Set rather than Unspecified for the
            // negated path; nothing we ship currently looks the macro
            // name up directly, so that's acceptable.
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
                        // Drop the literal `!<macro>` (see comment above).
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

    /// Return the resolved value of `attr` for `path` (relative to the
    /// repo root, with `/` separators). `None` for unspecified or unset.
    /// `Set`/`Value(v)` map to `Some("true")` / `Some(v)`.
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

    /// True iff `attr` is set for `path` — that is, `attr` or `attr=<v>`
    /// where `v` is anything other than the literal `"false"`.
    pub fn is_set(&self, path: &str, attr: &str) -> bool {
        matches!(self.value(path, attr).as_deref(), Some(v) if v != "false")
    }

    /// True iff `path` matches a `filter=lfs` line.
    pub fn is_lfs_tracked(&self, path: &str) -> bool {
        self.value(path, "filter").as_deref() == Some("lfs")
    }

    /// True iff `path` matches a `lockable` line.
    pub fn is_lockable(&self, path: &str) -> bool {
        self.is_set(path, "lockable")
    }
}

/// A single LFS-related pattern line discovered while listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternEntry {
    /// The pattern text exactly as it appears in the file (with any
    /// surrounding `"..."` quotes stripped).
    pub pattern: String,
    /// Path of the `.gitattributes` (or `.git/info/attributes`) file the
    /// pattern was found in, relative to the repo root and with `/`
    /// separators.
    pub source: String,
    /// True if the line establishes LFS tracking (`filter=lfs`); false if
    /// it explicitly removes / unspecifies the filter (`-filter`,
    /// `!filter`, `-filter=...`).
    pub tracked: bool,
    /// True if the same line carries the `lockable` attribute (in `set`
    /// form — `lockable=false` is treated as not lockable).
    pub lockable: bool,
}

/// All LFS-related patterns visible in a workdir, in load order
/// (`.git/info/attributes` first, then `.gitattributes` from shallow to
/// deep).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PatternListing {
    pub patterns: Vec<PatternEntry>,
}

impl PatternListing {
    /// Lines that establish LFS tracking (`filter=lfs`).
    pub fn tracked(&self) -> impl Iterator<Item = &PatternEntry> {
        self.patterns.iter().filter(|p| p.tracked)
    }

    /// Lines that explicitly remove / unspecify the LFS filter.
    pub fn excluded(&self) -> impl Iterator<Item = &PatternEntry> {
        self.patterns.iter().filter(|p| !p.tracked)
    }
}

/// Walk `.gitattributes` across the workdir plus `.git/info/attributes`
/// and the user's `core.attributesfile` (if configured), extracting
/// LFS-related pattern lines for `git lfs track`'s listing mode.
///
/// Pattern matching is *not* needed here — we're just enumerating the raw
/// pattern text per source file — so this uses a simple line tokenizer
/// rather than [`AttrSet`]'s full wildmatch machinery.
pub fn list_lfs_patterns(repo_root: &Path) -> io::Result<PatternListing> {
    let mut listing = PatternListing::default();

    // The user-level attributes file (`core.attributesfile`, default
    // `~/.config/git/attributes`). Looked up before `.git/info/attributes`
    // and the per-tree files so it shows up first in the listing —
    // upstream lists global → repo-local → per-dir.
    if let Ok(Some(path)) = crate::config::get_effective(repo_root, "core.attributesfile") {
        let expanded = expand_tilde(&path);
        if let Ok(bytes) = fs::read(&expanded) {
            scan_attr_lines(&bytes, &path, &mut listing);
        }
    }

    let info = repo_root.join(".git").join("info").join("attributes");
    if info.exists() {
        let bytes = fs::read(&info)?;
        scan_attr_lines(&bytes, ".git/info/attributes", &mut listing);
    }

    let mut found = Vec::new();
    walk_for_gitattributes(repo_root, &mut found)?;
    found.sort_by_key(|p| p.components().count());
    for path in found {
        let bytes = fs::read(&path)?;
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        scan_attr_lines(&bytes, &rel, &mut listing);
    }
    Ok(listing)
}

/// Resolve a leading `~` / `~/` to the user's home directory. Git's
/// `core.attributesfile` accepts both forms, but Rust's `Path` doesn't
/// expand them itself.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if path == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(path)
}

fn scan_attr_lines(bytes: &[u8], source: &str, listing: &mut PatternListing) {
    for raw in bytes.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        // Per `gitattributes(5)`, `#` only starts a comment when it's
        // the first non-whitespace character on the line — patterns like
        // `\#` are valid and must not be cropped here.
        let body = line.trim();
        if body.is_empty() || body.starts_with('#') || body.starts_with("[attr]") {
            continue;
        }
        let mut tokens = body.split_whitespace();
        let Some(pattern) = tokens.next() else {
            continue;
        };
        let mut filter: Option<bool> = None;
        let mut lockable = false;
        for tok in tokens {
            if tok == "filter=lfs" {
                filter = Some(true);
            } else if tok == "-filter" || tok == "!filter" || tok.starts_with("-filter=") {
                filter = Some(false);
            } else if tok == "lockable" {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
        // .gitattributes + .git/info/attributes + nested per-directory
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
    fn workdir_skips_dotgit_directory() {
        // A .gitattributes inside .git/ must NOT be picked up — only
        // .git/info/attributes is, and it's loaded explicitly above.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join(".git/.gitattributes"), "*.bin filter=lfs\n").unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(!s.is_lfs_tracked("a.bin"));
    }
}
