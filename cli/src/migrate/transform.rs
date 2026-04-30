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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plain blob → LFS pointer. `--above` threshold respected.
    Import,
    /// LFS pointer → raw content from local store. `--above` ignored
    /// since pointer files are tiny by definition.
    Export,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub include: Option<GlobSet>,
    pub exclude: Option<GlobSet>,
    /// Only consulted in [`Mode::Import`].
    pub above: u64,
    /// `.gitattributes` lines to add up-front, before any per-blob
    /// transformation runs. Used by [`Mode::Export`] to seed the
    /// rewritten attributes from the user's `--include`/`--exclude`
    /// CLI patterns (see `migrate/export.rs`).
    pub attrs_add_initial: Vec<String>,
    /// `.gitattributes` lines to drop up-front. Same use as
    /// `attrs_add_initial`.
    pub attrs_remove_initial: Vec<String>,
    /// Print one `  commit <sha>: <path>` line per converted blob to
    /// stderr. Wired in from `--verbose` on import / export.
    pub verbose: bool,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub blobs_converted: u64,
    pub bytes_converted: u64,
    pub commits_seen: u64,
    pub patterns: BTreeSet<String>,
    /// `(mark, original_oid)` for each commit we forwarded. Pairs with
    /// fast-import's `--export-marks` output to build the
    /// `--object-map` file.
    pub commit_marks: Vec<(u32, String)>,
}

pub struct Transform<'a> {
    store: &'a Store,
    opts: Options,
    mode: Mode,
    /// Buffered blobs keyed by their input mark, awaiting the first
    /// commit to reveal their path.
    blob_buffer: HashMap<u32, Vec<u8>>,
    /// Marks for which we've emitted output. Subsequent references
    /// pass through unchanged (the blob is already in the output
    /// stream).
    emitted: HashSet<u32>,
    /// Next free mark for our own injected blobs.
    next_fresh: u32,
    /// `.gitattributes` lines to ensure are present, in stable order.
    /// Import: `*.<ext> filter=lfs diff=lfs merge=lfs -text`.
    /// Export: `*.<ext> !text !filter !merge !diff`.
    attrs_add: BTreeSet<String>,
    /// `.gitattributes` lines to drop. Only populated in
    /// [`Mode::Export`] — those patterns are no longer LFS-tracked.
    attrs_remove: BTreeSet<String>,
    pub stats: Stats,
}

impl<'a> Transform<'a> {
    pub fn new(store: &'a Store, opts: Options, mode: Mode) -> Self {
        let mut attrs_add: BTreeSet<String> = BTreeSet::new();
        for line in &opts.attrs_add_initial {
            attrs_add.insert(line.clone());
        }
        let mut attrs_remove: BTreeSet<String> = BTreeSet::new();
        for line in &opts.attrs_remove_initial {
            attrs_remove.insert(line.clone());
        }
        Self {
            store,
            opts,
            mode,
            blob_buffer: HashMap::new(),
            emitted: HashSet::new(),
            next_fresh: FRESH_MARK_BASE,
            attrs_add,
            attrs_remove,
            stats: Stats::default(),
        }
    }

    /// Drive the full pipeline: read every command from `r`, transform,
    /// write to `w`. Consumes `self`.
    pub fn run<R: Read, W: Write>(mut self, r: R, w: W) -> io::Result<Stats> {
        let mut reader = Reader::new(r);
        let mut writer = Writer::new(w);
        while let Some(cmd) = reader.next()? {
            self.process(cmd, &mut writer)?;
        }
        writer.flush()?;
        self.stats.patterns = self.attrs_add.clone();
        Ok(self.stats)
    }

    fn process<W: Write>(&mut self, cmd: Command, writer: &mut Writer<W>) -> io::Result<()> {
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
        if let (Some(mark), Some(oid)) = (c.mark, c.original_oid.as_ref()) {
            self.stats.commit_marks.push((mark, oid.clone()));
        }

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
                    if self.opts.verbose {
                        if let Some(oid) = c.original_oid.as_deref() {
                            eprintln!("  commit {oid}: {path}");
                        }
                    }
                }
            }
        }

        // Pass 2: rewrite `.gitattributes` for this commit. The new
        // content is the existing content with the `attrs_remove`
        // lines stripped, then any `attrs_add` lines appended.
        let existing_attrs = self.read_existing_attrs(&c);
        let new_attrs = build_attrs(&existing_attrs, &self.attrs_add, &self.attrs_remove);
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

    /// Decide whether to convert a blob given its path, and run the
    /// conversion if so. Returns `(content_to_emit, was_converted)`.
    fn transform_blob(&mut self, path: &str, content: Vec<u8>) -> io::Result<(Vec<u8>, bool)> {
        if !path_matches(path, &self.opts.include, &self.opts.exclude) {
            return Ok((content, false));
        }
        match self.mode {
            Mode::Import => self.import_blob(path, content),
            Mode::Export => self.export_blob(path, content),
        }
    }

    fn import_blob(&mut self, _path: &str, content: Vec<u8>) -> io::Result<(Vec<u8>, bool)> {
        let size = content.len() as u64;
        // Don't re-convert blobs that already encode an LFS pointer.
        if Pointer::parse(&content).is_ok() {
            return Ok((content, false));
        }
        if size < self.opts.above {
            return Ok((content, false));
        }
        let oid_bytes: [u8; 32] = Sha256::digest(&content).into();
        let oid = Oid::from_bytes(oid_bytes);
        self.store
            .insert_verified(oid, &mut content.as_slice())
            .map_err(|e| io::Error::other(format!("storing object: {e}")))?;
        let pointer_text = Pointer::new(oid, size).encode().into_bytes();
        self.stats.blobs_converted += 1;
        self.stats.bytes_converted += size;
        Ok((pointer_text, true))
    }

    fn export_blob(&mut self, _path: &str, content: Vec<u8>) -> io::Result<(Vec<u8>, bool)> {
        // Only pointer-encoded blobs convert; everything else passes
        // through (matching upstream's `IsNotAPointerError` skip).
        let pointer = match Pointer::parse(&content) {
            Ok(p) => p,
            Err(_) => return Ok((content, false)),
        };
        // Resolve the LFS object's bytes from the local store. If
        // we can't find them we can't expand — leave the pointer in
        // place so the user can re-fetch and try again.
        let mut file = match self.store.open(pointer.oid) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok((content, false));
            }
            Err(e) => return Err(e),
        };
        let mut buf = Vec::with_capacity(pointer.size as usize);
        std::io::Read::read_to_end(&mut file, &mut buf)?;
        self.stats.blobs_converted += 1;
        self.stats.bytes_converted += pointer.size;
        Ok((buf, true))
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
        // Export mode pre-seeds attrs from the user's include/exclude
        // CLI patterns (see `migrate/export.rs::build_export_attrs`),
        // so per-path derivation is import-only.
        if !matches!(self.mode, Mode::Import) {
            return;
        }
        let leaf = path.rsplit('/').next().unwrap_or(path);
        let Some(idx) = leaf.rfind('.') else { return };
        if idx == 0 || idx >= leaf.len() - 1 {
            return;
        }
        let ext = &leaf[idx..];
        self.attrs_add
            .insert(format!("*{ext} filter=lfs diff=lfs merge=lfs -text"));
    }

    fn alloc_fresh(&mut self) -> u32 {
        let m = self.next_fresh;
        self.next_fresh += 1;
        m
    }
}

fn path_matches(path: &str, include: &Option<GlobSet>, exclude: &Option<GlobSet>) -> bool {
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

/// Combine the existing `.gitattributes` content with our accumulated
/// `add` / `remove` policy.
///
/// - Lines whose trimmed form matches anything in `remove` are dropped.
/// - Lines in `add` are appended after the surviving existing content,
///   preserving alphabetical order from the input `BTreeSet`. Lines
///   already present in the existing content are not duplicated.
fn build_attrs(existing: &str, add: &BTreeSet<String>, remove: &BTreeSet<String>) -> String {
    let mut have: HashSet<String> = HashSet::new();
    let mut out = String::with_capacity(existing.len() + add.len() * 64);
    for line in existing.lines() {
        let trimmed = line.trim();
        if remove.contains(trimmed) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        have.insert(trimmed.to_owned());
    }
    for p in add {
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
            FileChange::Modify { path, dataref, mode, .. } if path == ATTRS_PATH => {
                *dataref = DataRef::Mark(attrs_mark);
                // Normalize: `.gitattributes` is a config file, not an
                // executable. Strip the +x bit if the source happened
                // to commit it as 0755 (t-migrate-export's permissions
                // test asserts the rewritten attrs is non-executable).
                // Symlinks (120000) drop here too — we always emit a
                // regular file because the upfront symlink check at the
                // CLI rejected the input long before this.
                *mode = "100644".into();
                return;
            }
            FileChange::ModifyInline { path, .. } if path == ATTRS_PATH => {
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
        let stats = Transform::new(&store, opts, Mode::Import)
            .run(input, &mut out)
            .unwrap();
        (out, stats)
    }

    fn run_export(input: &[u8], opts: Options, store: &Store) -> (Vec<u8>, Stats) {
        let mut out: Vec<u8> = Vec::new();
        let stats = Transform::new(store, opts, Mode::Export)
            .run(input, &mut out)
            .unwrap();
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
            ..Default::default()
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
            ..Default::default()
        };
        let (out, stats) = run_transform(input, opts);
        assert_eq!(stats.blobs_converted, 1);
        assert_eq!(stats.bytes_converted, 12);
        assert!(
            stats
                .patterns
                .contains("*.bin filter=lfs diff=lfs merge=lfs -text")
        );

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
            ..Default::default()
        };
        let (_, stats) = run_transform(input, opts);
        assert_eq!(stats.blobs_converted, 0);
    }

    #[test]
    fn does_not_double_convert_existing_pointer_blob() {
        let oid = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
        let pointer =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 11\n");
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
            ..Default::default()
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
            ..Default::default()
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
        let mut add = BTreeSet::new();
        add.insert("*.bin filter=lfs diff=lfs merge=lfs -text".to_string());
        add.insert("*.png filter=lfs diff=lfs merge=lfs -text".to_string());
        let remove = BTreeSet::new();
        let out = build_attrs(existing, &add, &remove);
        let bin_count = out
            .lines()
            .filter(|l| *l == "*.bin filter=lfs diff=lfs merge=lfs -text")
            .count();
        assert_eq!(bin_count, 1, "should not duplicate existing pattern");
        assert!(out.contains("*.png filter=lfs"));
    }

    #[test]
    fn build_attrs_drops_removed_patterns() {
        let existing = "*.bin filter=lfs diff=lfs merge=lfs -text\n*.txt diff=text\n";
        let add = BTreeSet::new();
        let mut remove = BTreeSet::new();
        remove.insert("*.bin filter=lfs diff=lfs merge=lfs -text".to_string());
        let out = build_attrs(existing, &add, &remove);
        assert!(
            !out.contains("*.bin filter=lfs"),
            "removed line still present: {out}"
        );
        assert!(
            out.contains("*.txt diff=text"),
            "preserved line missing: {out}"
        );
    }

    #[test]
    fn export_expands_pointer_blob_to_real_content() {
        let (_tmp, store) = fixture_store();
        // Seed the store with the bytes the pointer references.
        let real = b"hello world\n";
        let (oid, _) = store.insert(&mut real.as_slice()).unwrap();
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\n\
             oid sha256:{oid}\n\
             size {}\n",
            real.len(),
        );

        let input = format!(
            "blob\nmark :1\ndata {n}\n{pointer}\
             commit refs/heads/main\n\
             committer A <a@b> 1 +0000\n\
             data 1\nm\n\
             M 100644 :1 data.bin\n\n",
            n = pointer.len(),
        );
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 0,
            // CLI seeds these from `--include`/`--exclude` patterns —
            // see `migrate/export.rs::build_export_attrs`. The transform
            // itself doesn't derive them in export mode.
            attrs_add_initial: vec!["*.bin !text !filter !merge !diff".into()],
            ..Default::default()
        };
        let (out, stats) = run_export(input.as_bytes(), opts, &store);
        assert_eq!(stats.blobs_converted, 1);
        let s = String::from_utf8_lossy(&out);
        // Output should contain the raw "hello world\n" bytes for
        // blob :1 (no longer pointer text).
        assert!(
            s.contains("\nhello world\n"),
            "expected raw content in stream: {s}"
        );
        assert!(
            !s.contains("oid sha256:"),
            "pointer text should be gone: {s}"
        );
        // Tracked-as-not-LFS line in the rewritten .gitattributes,
        // seeded from `attrs_add_initial`.
        assert!(
            s.contains("*.bin !text !filter !merge !diff"),
            "expected un-track line: {s}",
        );
    }

    #[test]
    fn export_passes_through_non_pointer_blobs() {
        let (_tmp, store) = fixture_store();
        let input = b"blob\nmark :1\ndata 5\nhello\n\
                      commit refs/heads/main\n\
                      committer A <a@b> 1 +0000\n\
                      data 1\nm\n\
                      M 100644 :1 plain.txt\n\n";
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 0,
            ..Default::default()
        };
        let (_, stats) = run_export(input, opts, &store);
        assert_eq!(stats.blobs_converted, 0);
    }

    #[test]
    fn export_leaves_pointer_alone_when_object_missing_from_store() {
        let (_tmp, store) = fixture_store();
        // Pointer references an OID we never put in the store.
        let oid = "1111111111111111111111111111111111111111111111111111111111111111";
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\n\
             oid sha256:{oid}\nsize 5\n",
        );
        let input = format!(
            "blob\nmark :1\ndata {n}\n{pointer}\
             commit refs/heads/main\n\
             committer A <a@b> 1 +0000\n\
             data 1\nm\n\
             M 100644 :1 data.bin\n\n",
            n = pointer.len(),
        );
        let opts = Options {
            include: Some(glob("*.bin")),
            exclude: None,
            above: 0,
            ..Default::default()
        };
        let (_, stats) = run_export(input.as_bytes(), opts, &store);
        // No conversion when object isn't locally available.
        assert_eq!(stats.blobs_converted, 0);
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
