//! Stream transform that converts plain blobs into LFS pointers and
//! rewrites `.gitattributes` so the resulting history is properly
//! filter=lfs-tracked.
//!
//! Operates on a Reader → Writer pipeline:
//!
//! ```text
//! git fast-export --full-tree
//!     | <Transform::run reads Commands, emits Commands>
//!     | git fast-import --force
//! ```
//!
//! Two state-tracking nuances worth knowing:
//!
//! 1. **Blob buffering.** `git fast-export` emits every blob before any
//!    commit references it. We can't decide whether to convert a blob
//!    until we know its path, which only arrives via a commit's `M`
//!    directive. So we buffer blob contents indexed by mark and emit
//!    them lazily on first reference.
//!
//! 2. **Per-commit `.gitattributes`.** With `--full-tree`, commits
//!    re-state every file every time. Each commit gets a freshly
//!    emitted `.gitattributes` blob with the patterns accumulated *so
//!    far* — matching upstream's behavior where early commits don't
//!    yet know about later ones' file types.
//!
//! ## First-commit-wins for shared blobs
//!
//! If the same blob OID appears at two paths with conflicting filter
//! outcomes (e.g. one matches `--include` and the other doesn't), the
//! first commit to reference it wins. v0 behavior; documented in
//! NOTES.md.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Read, Write};

use git_lfs_pointer::{Oid, Pointer};
use git_lfs_store::Store;
use globset::GlobSet;
use sha2::{Digest, Sha256};

use super::fast_export::{Blob, Command, Commit, DataRef, FileChange, Reader};
use super::fast_import::Writer;

const ATTRS_PATH: &str = ".gitattributes";

/// Marks we emit for our own freshly-created blobs (the rewritten
/// `.gitattributes`). Set high enough that a real `git fast-export`
/// stream won't collide — fast-export starts at :1 and increments.
const FRESH_MARK_BASE: u32 = 1 << 30;

#[derive(Debug, Clone)]
pub struct Options {
    pub include: Option<GlobSet>,
    pub exclude: Option<GlobSet>,
    pub above: u64,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub blobs_converted: u64,
    pub bytes_converted: u64,
    pub commits_seen: u64,
    pub patterns: BTreeSet<String>,
}

pub struct Transform<'a> {
    store: &'a Store,
    opts: Options,
    /// Buffered blobs keyed by their input mark, awaiting the first
    /// commit to reveal their path.
    blob_buffer: HashMap<u32, Vec<u8>>,
    /// Marks for which we've emitted output. Subsequent references
    /// pass through unchanged (the blob is already in the output
    /// stream).
    emitted: HashSet<u32>,
    /// Next free mark for our own injected blobs.
    next_fresh: u32,
    /// `*.<ext> filter=lfs diff=lfs merge=lfs -text` lines, stable
    /// alphabetical order so the rewritten `.gitattributes` is
    /// deterministic.
    patterns: BTreeSet<String>,
    pub stats: Stats,
}

impl<'a> Transform<'a> {
    pub fn new(store: &'a Store, opts: Options) -> Self {
        Self {
            store,
            opts,
            blob_buffer: HashMap::new(),
            emitted: HashSet::new(),
            next_fresh: FRESH_MARK_BASE,
            patterns: BTreeSet::new(),
            stats: Stats::default(),
        }
    }

    /// Drive the full pipeline: read every command from `r`, transform,
    /// write to `w`. Consumes `self`.
    pub fn run<R: Read, W: Write>(
        mut self,
        r: R,
        w: W,
    ) -> io::Result<Stats> {
        let mut reader = Reader::new(r);
        let mut writer = Writer::new(w);
        while let Some(cmd) = reader.next()? {
            self.process(cmd, &mut writer)?;
        }
        writer.flush()?;
        self.stats.patterns = self.patterns;
        Ok(self.stats)
    }

    fn process<W: Write>(
        &mut self,
        cmd: Command,
        writer: &mut Writer<W>,
    ) -> io::Result<()> {
        match cmd {
            Command::Blob(b) => {
                if let Some(mark) = b.mark {
                    self.blob_buffer.insert(mark, b.data);
                } else {
                    // Mark-less blobs can't be referenced; just pass
                    // through (rare but valid).
                    writer.write(&Command::Blob(b))?;
                }
                Ok(())
            }
            Command::Commit(c) => self.process_commit(c, writer),
            other => writer.write(&other),
        }
    }

    fn process_commit<W: Write>(
        &mut self,
        mut c: Commit,
        writer: &mut Writer<W>,
    ) -> io::Result<()> {
        self.stats.commits_seen += 1;

        // Pass 1: emit any buffered blobs this commit references at
        // non-`.gitattributes` paths, deciding conversion based on path.
        for change in &c.file_changes {
            if let FileChange::Modify {
                dataref: DataRef::Mark(m),
                path,
                ..
            } = change
                && path != ATTRS_PATH
                && !self.emitted.contains(m)
                && let Some(content) = self.blob_buffer.remove(m)
            {
                let (out, was_converted) = self.transform_blob(path, content)?;
                writer.write(&Command::Blob(Blob {
                    mark: Some(*m),
                    original_oid: None,
                    data: out,
                }))?;
                self.emitted.insert(*m);
                if was_converted {
                    self.add_pattern_for_path(path);
                }
            }
        }

        // Pass 2: rewrite `.gitattributes` for this commit. The new
        // content is the union of (a) whatever the commit's existing
        // `.gitattributes` had, and (b) every pattern accumulated so
        // far in stream order.
        let existing_attrs = self.read_existing_attrs(&c);
        let new_attrs = build_attrs(&existing_attrs, &self.patterns);
        let needs_attrs = !new_attrs.is_empty();
        if needs_attrs {
            let attrs_mark = self.alloc_fresh();
            writer.write(&Command::Blob(Blob {
                mark: Some(attrs_mark),
                original_oid: None,
                data: new_attrs.into_bytes(),
            }))?;
            // Replace existing M directive or insert a new one.
            replace_or_insert_attrs(&mut c.file_changes, attrs_mark);
        }

        writer.write(&Command::Commit(c))
    }

    /// Decide whether to clean a blob given its path, and run the
    /// conversion if so. Returns `(content_to_emit, was_converted)`.
    fn transform_blob(
        &mut self,
        path: &str,
        content: Vec<u8>,
    ) -> io::Result<(Vec<u8>, bool)> {
        let size = content.len() as u64;

        // Don't convert pointer-already-encoded blobs — they're already
        // LFS. Detect by parse success.
        if Pointer::parse(&content).is_ok() {
            return Ok((content, false));
        }

        if !path_matches(path, &self.opts.include, &self.opts.exclude) {
            return Ok((content, false));
        }
        if size < self.opts.above {
            return Ok((content, false));
        }

        // Hash → store → pointer text.
        let oid_bytes: [u8; 32] = Sha256::digest(&content).into();
        let oid = Oid::from_bytes(oid_bytes);
        // Store::insert_verified writes the bytes and verifies the
        // hash matches. We pass the just-computed OID so a mid-write
        // disk corruption surfaces here.
        self.store
            .insert_verified(oid, &mut content.as_slice())
            .map_err(|e| io::Error::other(format!("storing object: {e}")))?;
        let pointer_text = Pointer::new(oid, size).encode().into_bytes();
        self.stats.blobs_converted += 1;
        self.stats.bytes_converted += size;
        Ok((pointer_text, true))
    }

    fn read_existing_attrs(&self, c: &Commit) -> String {
        for ch in &c.file_changes {
            if let FileChange::Modify {
                dataref: DataRef::Mark(m),
                path,
                ..
            } = ch
                && path == ATTRS_PATH
                && let Some(bytes) = self.blob_buffer.get(m)
            {
                return String::from_utf8_lossy(bytes).into_owned();
            }
            if let FileChange::ModifyInline { path, data, .. } = ch
                && path == ATTRS_PATH
            {
                return String::from_utf8_lossy(data).into_owned();
            }
        }
        String::new()
    }

    fn add_pattern_for_path(&mut self, path: &str) {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        if let Some(idx) = leaf.rfind('.')
            && idx > 0
            && idx < leaf.len() - 1
        {
            self.patterns.insert(format!(
                "*{} filter=lfs diff=lfs merge=lfs -text",
                &leaf[idx..]
            ));
        }
        // Files without an extension don't get a pattern in v0 —
        // matching upstream's behavior of preferring `*.<ext>` over
        // path-literal entries.
    }

    fn alloc_fresh(&mut self) -> u32 {
        let m = self.next_fresh;
        self.next_fresh += 1;
        m
    }
}

fn path_matches(
    path: &str,
    include: &Option<GlobSet>,
    exclude: &Option<GlobSet>,
) -> bool {
    if let Some(ex) = exclude
        && ex.is_match(path)
    {
        return false;
    }
    match include {
        Some(inc) => inc.is_match(path),
        None => true,
    }
}

/// Union the existing `.gitattributes` content (preserved verbatim
/// modulo our pattern lines) with the accumulated LFS pattern lines.
/// Output is line-stable: existing lines first in their original
/// order, then any new patterns that weren't already present.
fn build_attrs(existing: &str, patterns: &BTreeSet<String>) -> String {
    let mut have: HashSet<String> = existing
        .lines()
        .map(|l| l.trim().to_owned())
        .collect();

    let mut out = String::with_capacity(existing.len() + patterns.len() * 64);
    for line in existing.lines() {
        out.push_str(line);
        out.push('\n');
    }
    for p in patterns {
        if have.insert(p.clone()) {
            out.push_str(p);
            out.push('\n');
        }
    }
    out
}

fn replace_or_insert_attrs(changes: &mut Vec<FileChange>, attrs_mark: u32) {
    for ch in changes.iter_mut() {
        match ch {
            FileChange::Modify { path, dataref, .. } if path == ATTRS_PATH => {
                *dataref = DataRef::Mark(attrs_mark);
                return;
            }
            FileChange::ModifyInline { path, .. } if path == ATTRS_PATH => {
                // Replace inline form with a mark reference.
                *ch = FileChange::Modify {
                    mode: "100644".into(),
                    dataref: DataRef::Mark(attrs_mark),
                    path: ATTRS_PATH.into(),
                };
                return;
            }
            _ => {}
        }
    }
    // No existing entry — insert at the end. (Some commits emit
    // `deleteall` first; we want our M to come after.)
    changes.push(FileChange::Modify {
        mode: "100644".into(),
        dataref: DataRef::Mark(attrs_mark),
        path: ATTRS_PATH.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use globset::{Glob, GlobSetBuilder};
    use tempfile::TempDir;

    fn fixture_store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("lfs"));
        (tmp, store)
    }

    fn glob(pat: &str) -> GlobSet {
        let mut b = GlobSetBuilder::new();
        b.add(Glob::new(pat).unwrap());
        b.build().unwrap()
    }

    fn run_transform(input: &[u8], opts: Options) -> (Vec<u8>, Stats) {
        let (_tmp, store) = fixture_store();
        let mut out: Vec<u8> = Vec::new();
        let stats = Transform::new(&store, opts).run(input, &mut out).unwrap();
        (out, stats)
    }

    #[test]
    fn passes_through_streams_with_no_matching_blobs() {
        let input = b"blob\n\
                      mark :1\n\
                      data 5\n\
                      hello\n\
                      commit refs/heads/main\n\
                      mark :2\n\
                      author A <a@b> 1 +0000\n\
                      committer A <a@b> 1 +0000\n\
                      data 1\nm\n\
                      M 100644 :1 plain.txt\n\
                      \n";
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 0,
        };
        let (_, stats) = run_transform(input, opts);
        assert_eq!(stats.blobs_converted, 0);
        assert_eq!(stats.commits_seen, 1);
        assert!(stats.patterns.is_empty());
    }

    #[test]
    fn converts_matching_blob_to_pointer_and_accumulates_pattern() {
        let input = b"blob\n\
                      mark :1\n\
                      data 12\n\
                      hello world\n\
                      commit refs/heads/main\n\
                      mark :2\n\
                      author A <a@b> 1 +0000\n\
                      committer A <a@b> 1 +0000\n\
                      data 1\nm\n\
                      M 100644 :1 data.bin\n\
                      \n";
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 0,
        };
        let (out, stats) = run_transform(input, opts);
        assert_eq!(stats.blobs_converted, 1);
        assert_eq!(stats.bytes_converted, 12);
        assert!(stats
            .patterns
            .contains("*.bin filter=lfs diff=lfs merge=lfs -text"));

        // The output should re-parse cleanly. The blob with mark :1
        // now contains pointer text; a fresh blob (high mark) carries
        // .gitattributes; the commit's M for data.bin still references
        // :1, and a new M for .gitattributes is appended.
        let s = String::from_utf8(out).expect("utf-8 stream");
        assert!(s.contains("oid sha256:"), "expected pointer text: {s}");
        assert!(
            s.contains("*.bin filter=lfs diff=lfs merge=lfs -text"),
            "expected attrs blob: {s}",
        );
        assert!(
            s.contains(".gitattributes"),
            "expected commit to gain a .gitattributes M: {s}",
        );
    }

    #[test]
    fn respects_above_threshold() {
        // 5-byte blob, threshold 100 → leave alone.
        let input = b"blob\n\
                      mark :1\n\
                      data 5\n\
                      hello\n\
                      commit refs/heads/main\n\
                      committer A <a@b> 1 +0000\n\
                      data 1\nm\n\
                      M 100644 :1 a.bin\n\
                      \n";
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 100,
        };
        let (_, stats) = run_transform(input, opts);
        assert_eq!(stats.blobs_converted, 0);
    }

    #[test]
    fn does_not_double_convert_existing_pointer_blob() {
        let oid = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 11\n"
        );
        let blob_line = format!("data {}\n{pointer}", pointer.len());
        let input = format!(
            "blob\nmark :1\n{blob_line}\
             commit refs/heads/main\n\
             committer A <a@b> 1 +0000\n\
             data 1\nm\n\
             M 100644 :1 data.bin\n\n"
        );
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 0,
        };
        let (_, stats) = run_transform(input.as_bytes(), opts);
        // Already a pointer → not re-converted.
        assert_eq!(stats.blobs_converted, 0);
    }

    #[test]
    fn rewrites_existing_gitattributes_with_union() {
        let input = b"blob\n\
                      mark :1\n\
                      data 16\n\
                      *.txt diff=text\n\
                      blob\n\
                      mark :2\n\
                      data 5\n\
                      hello\n\
                      commit refs/heads/main\n\
                      committer A <a@b> 1 +0000\n\
                      data 1\nm\n\
                      M 100644 :1 .gitattributes\n\
                      M 100644 :2 a.bin\n\
                      \n";
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 0,
        };
        let (out, _) = run_transform(input, opts);
        let s = String::from_utf8(out).unwrap();
        // Existing line preserved, new pattern added.
        assert!(s.contains("*.txt diff=text"), "{s}");
        assert!(
            s.contains("*.bin filter=lfs diff=lfs merge=lfs -text"),
            "{s}",
        );
    }

    #[test]
    fn build_attrs_unions_without_duplicating_existing_pattern() {
        let existing = "*.bin filter=lfs diff=lfs merge=lfs -text\n*.txt diff=text\n";
        let mut patterns = BTreeSet::new();
        patterns.insert("*.bin filter=lfs diff=lfs merge=lfs -text".to_string());
        patterns.insert("*.png filter=lfs diff=lfs merge=lfs -text".to_string());
        let out = build_attrs(existing, &patterns);
        let bin_count = out
            .lines()
            .filter(|l| *l == "*.bin filter=lfs diff=lfs merge=lfs -text")
            .count();
        assert_eq!(bin_count, 1, "should not duplicate existing pattern");
        assert!(out.contains("*.png filter=lfs"));
    }

    #[test]
    fn replace_or_insert_attrs_inserts_when_missing() {
        let mut changes = vec![FileChange::Modify {
            mode: "100644".into(),
            dataref: DataRef::Mark(7),
            path: "data.bin".into(),
        }];
        replace_or_insert_attrs(&mut changes, 99);
        assert_eq!(changes.len(), 2);
        match &changes[1] {
            FileChange::Modify { path, dataref, .. } => {
                assert_eq!(path, ".gitattributes");
                assert_eq!(dataref, &DataRef::Mark(99));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn replace_or_insert_attrs_updates_existing_dataref() {
        let mut changes = vec![FileChange::Modify {
            mode: "100644".into(),
            dataref: DataRef::Mark(42),
            path: ".gitattributes".into(),
        }];
        replace_or_insert_attrs(&mut changes, 99);
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            FileChange::Modify { dataref, .. } => {
                assert_eq!(dataref, &DataRef::Mark(99));
            }
            other => panic!("got {other:?}"),
        }
    }
}
