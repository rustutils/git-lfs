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
        Self { search, collection }
    }

    /// Build from a single `.gitattributes`-format buffer.
    pub fn from_buffer(bytes: &[u8]) -> Self {
        let mut me = Self::empty();
        me.search.add_patterns_buffer(
            bytes,
            "<memory>".into(),
            None,
            &mut me.collection,
            true,
        );
        me
    }

    /// Discover every `.gitattributes` reachable from `repo_root` (skipping
    /// the `.git/` directory) and load them along with `.git/info/attributes`
    /// if it exists.
    pub fn from_workdir(repo_root: &Path) -> io::Result<Self> {
        let mut me = Self::empty();
        let mut buf = Vec::new();

        let info = repo_root.join(".git").join("info").join("attributes");
        if info.exists() {
            me.search.add_patterns_file(
                info,
                true,
                None,
                &mut buf,
                &mut me.collection,
                true,
            )?;
        }

        let mut found = Vec::new();
        walk_for_gitattributes(repo_root, &mut found)?;
        // Shallow → deep: gix-attributes iterates pattern lists in reverse
        // when matching, so the last-added (deepest) wins — matching Git's
        // "more specific path overrides shallower" semantics.
        found.sort_by_key(|p| p.components().count());
        for path in found {
            // `root` is always the repo root. gix-glob computes each file's
            // relative `base` by stripping the repo-root prefix from
            // `source.parent()` — so root.gitattributes ends up with no base
            // (matches paths directly) while sub/.gitattributes ends up with
            // base `sub/` (strips `sub/` before matching).
            me.search.add_patterns_file(
                path,
                true,
                Some(repo_root),
                &mut buf,
                &mut me.collection,
                true,
            )?;
        }
        Ok(me)
    }

    /// Return the resolved value of `attr` for `path` (relative to the
    /// repo root, with `/` separators). `None` for unspecified or unset.
    /// `Set`/`Value(v)` map to `Some("true")` / `Some(v)`.
    pub fn value(&self, path: &str, attr: &str) -> Option<String> {
        let mut out = Outcome::default();
        out.initialize_with_selection(&self.collection, [attr]);
        self.search.pattern_matching_relative_path(
            path.into(),
            Case::Sensitive,
            None,
            &mut out,
        );
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
}

/// All LFS-related patterns visible in a workdir, partitioned into ones
/// configured *as* LFS (`filter=lfs`) and ones that explicitly *exclude*
/// LFS (`-filter` / `-filter=lfs`).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PatternListing {
    /// `filter=lfs` lines.
    pub tracked: Vec<PatternEntry>,
    /// `-filter` or `-filter=lfs` lines (intentionally excluded from LFS).
    pub excluded: Vec<PatternEntry>,
}

/// Walk `.gitattributes` across the workdir plus `.git/info/attributes`,
/// extracting LFS-related pattern lines for `git lfs track`'s listing mode.
///
/// Pattern matching is *not* needed here — we're just enumerating the raw
/// pattern text per source file — so this uses a simple line tokenizer
/// rather than [`AttrSet`]'s full wildmatch machinery.
pub fn list_lfs_patterns(repo_root: &Path) -> io::Result<PatternListing> {
    let mut listing = PatternListing::default();

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

fn scan_attr_lines(bytes: &[u8], source: &str, listing: &mut PatternListing) {
    for raw in bytes.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        let body = line.split('#').next().unwrap_or(&line).trim();
        if body.is_empty() || body.starts_with("[attr]") {
            continue;
        }
        let mut tokens = body.split_whitespace();
        let Some(pattern) = tokens.next() else {
            continue;
        };
        let mut filter = None;
        for tok in tokens {
            if tok == "filter=lfs" {
                filter = Some(true);
            } else if tok == "-filter" || tok.starts_with("-filter=") {
                filter = Some(false);
            }
        }
        match filter {
            Some(true) => listing.tracked.push(PatternEntry {
                pattern: pattern.to_owned(),
                source: source.to_owned(),
            }),
            Some(false) => listing.excluded.push(PatternEntry {
                pattern: pattern.to_owned(),
                source: source.to_owned(),
            }),
            None => {}
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
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "*.bin filter=lfs\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("sub/.gitattributes"),
            "*.bin -filter\n",
        )
        .unwrap();

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
            .tracked
            .iter()
            .map(|p| (p.pattern.as_str(), p.source.as_str()))
            .collect();
        let excluded: Vec<(&str, &str)> = listing
            .excluded
            .iter()
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
        assert_eq!(listing.tracked.len(), 1);
        assert_eq!(listing.tracked[0].pattern, "*.bin");
    }

    #[test]
    fn workdir_skips_dotgit_directory() {
        // A .gitattributes inside .git/ must NOT be picked up — only
        // .git/info/attributes is, and it's loaded explicitly above.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(
            tmp.path().join(".git/.gitattributes"),
            "*.bin filter=lfs\n",
        )
        .unwrap();

        let s = AttrSet::from_workdir(tmp.path()).unwrap();
        assert!(!s.is_lfs_tracked("a.bin"));
    }
}
