//! The blob rewriting between `git fast-export` and `git fast-import`.
//!
//! A blob's path is only known once a commit references it, so blobs
//! are buffered by mark and written out, converted or not, just before
//! the first commit that uses them. If the same content is at two paths
//! and only one of them is converted, the other path gets the blob's
//! other version: the original blob, by its ID, or the converted one,
//! written under a new mark.
//!
//! In import and export mode, each commit also gets a rewritten
//! top-level `.gitattributes`: its own lines, minus the lines being
//! removed, plus the patterns added so far. As in upstream, early
//! commits don't get patterns for files that only appear later.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use git_lfs_git::attr::PathPatterns;
use git_lfs_pointer::{Oid, Pointer};
use git_lfs_store::Store;
use sha2::{Digest, Sha256};

use super::fast_export::{Blob, Command, Commit, DataRef, FileChange, Reader};
use super::fast_import::Writer;
use super::path_matches;

const ATTRS_PATH: &str = ".gitattributes";

/// The first mark for the rewritten `.gitattributes` blobs.
///
/// fast-export numbers its marks up from 1, so these don't collide.
const FRESH_MARK_BASE: u32 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Convert matching blobs to pointers.
    Import,
    /// Replace matching pointers with their objects from the local store.
    Export,
    /// Convert blobs that the commit's own attributes say are LFS files,
    /// for `migrate import --fixup`.
    ///
    /// `.gitattributes` is left as it is, and the include and exclude
    /// patterns aren't used.
    Fixup,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub include: Option<PathPatterns>,
    pub exclude: Option<PathPatterns>,
    /// Minimum blob size to convert, in [`Mode::Import`] only.
    pub above: u64,
    /// `.gitattributes` lines to add to every commit, built from the
    /// `--include` and `--exclude` patterns.
    pub attrs_add_initial: Vec<String>,
    /// `.gitattributes` lines to remove from every commit.
    pub attrs_remove_initial: Vec<String>,
    /// Print `  commit <sha>: <path>` to stderr for each converted blob.
    pub verbose: bool,
    /// Don't add a `.gitattributes` line for each converted path.
    ///
    /// Set when `attrs_add_initial` already has the user's patterns, so
    /// `--include "a file.txt"` doesn't also add `*.txt`.
    pub skip_path_derived_attrs: bool,
    /// The repository's `info/attributes`, for [`Mode::Fixup`].
    ///
    /// It takes precedence over every `.gitattributes`.
    pub info_attrs: Vec<u8>,
    /// The global attributes file, for [`Mode::Fixup`].
    ///
    /// It has the lowest precedence.
    pub global_attrs: Vec<u8>,
    /// The repository, to read a blob again when a second path needs its
    /// other version.
    pub cwd: PathBuf,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub blobs_converted: u64,
    pub bytes_converted: u64,
    pub commits_seen: u64,
    pub patterns: BTreeSet<String>,
    /// `(mark, original commit SHA)` for each commit.
    ///
    /// With fast-import's exported marks, this maps old commits to new
    /// ones.
    pub commit_marks: Vec<(u32, String)>,
}

pub struct Transform<'a> {
    store: &'a Store,
    opts: Options,
    mode: Mode,
    /// Blobs not written out yet, by mark.
    blob_buffer: HashMap<u32, Vec<u8>>,
    /// Marks of blobs already written out.
    emitted: HashSet<u32>,
    /// The content of each `.gitattributes` [`Mode::Fixup`] has seen, by
    /// mark or ID, kept for later commits that use the same blob.
    attrs_contents: HashMap<String, Vec<u8>>,
    /// Original blob IDs, by mark, from `--show-original-ids`.
    original_ids: HashMap<u32, String>,
    /// How each blob written out by [`Self::process_commit`] was written.
    written: HashMap<u32, Written>,
    /// The other version of a written blob, and whether it's converted.
    alternates: HashMap<u32, (DataRef, bool)>,
    /// Next free mark for a rewritten `.gitattributes`.
    next_fresh: u32,
    /// `.gitattributes` lines for the paths converted so far, in
    /// [`Mode::Import`].
    attrs_add: BTreeSet<String>,
    /// [`Options::attrs_add_initial`].
    attrs_add_initial: Vec<String>,
    /// [`Options::attrs_remove_initial`].
    attrs_remove: BTreeSet<String>,
    /// Tags and resets of `refs/tags/`, held back until the end of the
    /// stream by [`Self::write_tags`].
    tags: Vec<Command>,
    pub stats: Stats,
}

impl<'a> Transform<'a> {
    pub fn new(store: &'a Store, opts: Options, mode: Mode) -> Self {
        let attrs_add_initial = opts.attrs_add_initial.clone();
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
            attrs_contents: HashMap::new(),
            original_ids: HashMap::new(),
            written: HashMap::new(),
            alternates: HashMap::new(),
            next_fresh: FRESH_MARK_BASE,
            attrs_add: BTreeSet::new(),
            attrs_add_initial,
            attrs_remove,
            tags: Vec::new(),
            stats: Stats::default(),
        }
    }

    /// Read a fast-export stream from `r` and write the rewritten stream
    /// to `w`.
    pub fn run<R: Read, W: Write>(mut self, r: R, w: W) -> io::Result<Stats> {
        let mut reader = Reader::new(r);
        let mut writer = Writer::new(w);
        while let Some(cmd) = reader.next()? {
            self.process(cmd, &mut writer)?;
        }
        self.write_tags(&mut writer)?;
        writer.flush()?;
        self.stats.patterns = self.attrs_add.clone();
        for p in &self.attrs_add_initial {
            self.stats.patterns.insert(p.clone());
        }
        Ok(self.stats)
    }

    fn process<W: Write>(&mut self, cmd: Command, writer: &mut Writer<W>) -> io::Result<()> {
        match cmd {
            Command::Blob(b) => {
                if let Some(mark) = b.mark {
                    if let Some(id) = b.original_oid {
                        self.original_ids.insert(mark, id);
                    }
                    self.blob_buffer.insert(mark, b.data);
                } else {
                    // Nothing can refer to a blob without a mark.
                    writer.write(&Command::Blob(b))?;
                }
                Ok(())
            }
            Command::Commit(c) => match self.mode {
                Mode::Fixup => self.process_commit_fixup(c, writer),
                _ => self.process_commit(c, writer),
            },
            Command::Tag(t) => {
                self.tags.push(Command::Tag(t));
                Ok(())
            }
            Command::Reset(r) if r.ref_name.starts_with("refs/tags/") => {
                self.tags.push(Command::Reset(r));
                Ok(())
            }
            other => writer.write(&other),
        }
    }

    /// Write the held-back tags, with each tag of a tag fixed.
    ///
    /// fast-export writes the tag that another tag points at under the
    /// outer tag's name, so fast-import would store it with that name and
    /// the outer tag wouldn't point at the rewritten inner one. Each inner
    /// tag gets its own name back from the original object. The inner
    /// tag's own entry later in the stream is then dropped, since
    /// fast-import refuses two updates to one ref, and references to its
    /// mark go to the first entry.
    ///
    /// Only tags can refer to a tag, so holding them back is safe.
    fn write_tags<W: Write>(&mut self, writer: &mut Writer<W>) -> io::Result<()> {
        let inner: HashSet<&str> = self
            .tags
            .iter()
            .filter_map(|c| match c {
                Command::Tag(t) => t.from.strip_prefix(':'),
                _ => None,
            })
            .collect();
        let mut renames = Vec::new();
        for (i, cmd) in self.tags.iter().enumerate() {
            if let Command::Tag(t) = cmd
                && let (Some(mark), Some(oid)) = (t.mark, &t.original_oid)
                && inner.contains(mark.to_string().as_str())
            {
                let name = tag_name(&self.opts.cwd, oid)?;
                if name != t.name {
                    renames.push((i, name));
                }
            }
        }
        for (i, name) in renames {
            if let Command::Tag(t) = &mut self.tags[i] {
                t.name = name;
            }
        }
        // The mark each original tag was written under, and the name it
        // was written with.
        let mut written: HashMap<String, (u32, String)> = HashMap::new();
        let mut aliases: HashMap<String, String> = HashMap::new();
        for mut cmd in std::mem::take(&mut self.tags) {
            if let Command::Tag(t) = &mut cmd {
                if let Some(to) = aliases.get(&t.from) {
                    t.from = to.clone();
                }
                if let (Some(mark), Some(oid)) = (t.mark, &t.original_oid) {
                    match written.get(oid) {
                        Some((first, name)) if *name == t.name => {
                            aliases.insert(format!(":{mark}"), format!(":{first}"));
                            continue;
                        }
                        Some(_) => {}
                        None => {
                            written.insert(oid.clone(), (mark, t.name.clone()));
                        }
                    }
                }
            }
            writer.write(&cmd)?;
        }
        Ok(())
    }

    /// Convert the commit's new blobs that its attributes say are LFS
    /// files.
    ///
    /// The attributes come from the commit's `.gitattributes` files that
    /// are still in the blob buffer. One already written out for an
    /// earlier commit is missing, so files added after the commit that
    /// added `.gitattributes` aren't converted.
    fn process_commit_fixup<W: Write>(
        &mut self,
        c: Commit,
        writer: &mut Writer<W>,
    ) -> io::Result<()> {
        self.stats.commits_seen += 1;
        if let (Some(mark), Some(oid)) = (c.mark, c.original_oid.as_ref()) {
            self.stats.commit_marks.push((mark, oid.clone()));
        }

        // With `--full-tree`, every commit lists each `.gitattributes`,
        // also when an earlier commit already wrote its blob out.
        let mut attrs_dirs: Vec<(String, Vec<u8>)> = Vec::new();
        for ch in &c.file_changes {
            match ch {
                FileChange::Modify { dataref, path, .. } if is_attrs_path(path) => {
                    if let Some(content) = self.attrs_content(dataref)? {
                        attrs_dirs.push((dir_of(path), content));
                    }
                }
                FileChange::ModifyInline { path, data, .. } if is_attrs_path(path) => {
                    attrs_dirs.push((dir_of(path), data.clone()));
                }
                _ => {}
            }
        }
        // The last list added takes precedence, so add from lowest to
        // highest: the global file, `.gitattributes` from shallow to
        // deep, then `info/attributes`.
        attrs_dirs.sort_by_key(|(d, _)| d.matches('/').count());
        let mut attrs = git_lfs_git::AttrSet::empty();
        if !self.opts.global_attrs.is_empty() {
            attrs.add_buffer_at(&self.opts.global_attrs, "");
        }
        for (dir, content) in &attrs_dirs {
            attrs.add_buffer_at(content, dir);
        }
        if !self.opts.info_attrs.is_empty() {
            attrs.add_buffer_at(&self.opts.info_attrs, "");
        }

        for change in &c.file_changes {
            if let FileChange::Modify {
                dataref: DataRef::Mark(m),
                path,
                mode,
            } = change
                && !is_attrs_path(path)
                && !self.emitted.contains(m)
                && let Some(content) = self.blob_buffer.remove(m)
            {
                // A symlink's blob is its target, never an LFS file.
                if mode != "120000" && attrs.is_lfs_tracked(path) {
                    let (out, _) = self.import_blob(path, content)?;
                    writer.write(&Command::Blob(Blob {
                        mark: Some(*m),
                        original_oid: None,
                        data: out,
                    }))?;
                } else {
                    writer.write(&Command::Blob(Blob {
                        mark: Some(*m),
                        original_oid: None,
                        data: content,
                    }))?;
                }
                self.emitted.insert(*m);
            }
        }

        for change in &c.file_changes {
            if let FileChange::Modify {
                dataref: DataRef::Mark(m),
                path,
                ..
            } = change
                && is_attrs_path(path)
                && !self.emitted.contains(m)
                && let Some(content) = self.blob_buffer.remove(m)
            {
                writer.write(&Command::Blob(Blob {
                    mark: Some(*m),
                    original_oid: None,
                    data: content,
                }))?;
                self.emitted.insert(*m);
            }
        }

        writer.write(&Command::Commit(c))
    }

    /// The content of the `.gitattributes` blob `dataref`, from the
    /// buffer, from an earlier commit, or from the repository for an ID.
    fn attrs_content(&mut self, dataref: &DataRef) -> io::Result<Option<Vec<u8>>> {
        let key = match dataref {
            DataRef::Mark(m) => format!(":{m}"),
            DataRef::Sha(id) => id.clone(),
        };
        if let Some(content) = self.attrs_contents.get(&key) {
            return Ok(Some(content.clone()));
        }
        let content = match dataref {
            DataRef::Mark(m) => match self.blob_buffer.get(m) {
                Some(content) => content.clone(),
                None => return Ok(None),
            },
            DataRef::Sha(id) => read_blob(&self.opts.cwd, id)?,
        };
        self.attrs_contents.insert(key, content.clone());
        Ok(Some(content))
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

        // No `.gitattributes` is converted, as in upstream. The original
        // top-level one is never written out: it's replaced below. Nested
        // ones, and symlinks, whose blob is their target, are written out
        // unchanged afterwards.
        for change in &mut c.file_changes {
            let FileChange::Modify {
                dataref,
                path,
                mode,
            } = change
            else {
                continue;
            };
            let DataRef::Mark(m) = *dataref else {
                continue;
            };
            if is_attrs_path(path) || mode == "120000" {
                continue;
            }
            let (converted, new) = if let Some(content) = self.blob_buffer.remove(&m) {
                let written = Written {
                    converted: false,
                    size: content.len() as u64,
                    pointer: Pointer::parse(&content).is_ok(),
                };
                let (out, converted) = self.transform_blob(path, content)?;
                writer.write(&Command::Blob(Blob {
                    mark: Some(m),
                    original_oid: None,
                    data: out,
                }))?;
                self.emitted.insert(m);
                self.written.insert(
                    m,
                    Written {
                        converted,
                        ..written
                    },
                );
                (converted, true)
            } else if let Some(written) = self.written.get(&m).copied() {
                if self.converts(path, &written) == written.converted {
                    (written.converted, false)
                } else {
                    let (alternate, converted, new) = self.alternate(m, path, writer)?;
                    *dataref = alternate;
                    (converted, new)
                }
            } else {
                continue;
            };
            if converted {
                self.add_pattern_for_path(path);
                if new
                    && self.opts.verbose
                    && let Some(oid) = c.original_oid.as_deref()
                {
                    eprintln!("  commit {oid}: {path}");
                }
            }
        }
        for change in &c.file_changes {
            if let FileChange::Modify {
                dataref: DataRef::Mark(m),
                path,
                mode,
            } = change
                && (mode == "120000" || (is_attrs_path(path) && path != ATTRS_PATH))
                && !self.emitted.contains(m)
                && let Some(content) = self.blob_buffer.remove(m)
            {
                writer.write(&Command::Blob(Blob {
                    mark: Some(*m),
                    original_oid: None,
                    data: content,
                }))?;
                self.emitted.insert(*m);
            }
        }

        let existing_attrs = self.read_existing_attrs(&c);
        let new_attrs = build_attrs(
            &existing_attrs,
            &self.attrs_add,
            &self.attrs_add_initial,
            &self.attrs_remove,
        );
        // The original blob isn't written out, so a commit that changes
        // the top-level file needs the rewrite even when it's empty.
        let has_attrs = c.file_changes.iter().any(|ch| {
            matches!(ch, FileChange::Modify { path, .. }
                | FileChange::ModifyInline { path, .. } if path == ATTRS_PATH)
        });
        if !new_attrs.is_empty() || has_attrs {
            let attrs_mark = self.alloc_fresh();
            writer.write(&Command::Blob(Blob {
                mark: Some(attrs_mark),
                original_oid: None,
                data: new_attrs.into_bytes(),
            }))?;
            replace_or_insert_attrs(&mut c.file_changes, attrs_mark);
        }

        writer.write(&Command::Commit(c))
    }

    /// Whether a blob written as `written` would be converted at `path`.
    fn converts(&self, path: &str, written: &Written) -> bool {
        let matches = path_matches(path, &self.opts.include, &self.opts.exclude);
        match self.mode {
            Mode::Import => matches && !written.pointer && written.size >= self.opts.above,
            Mode::Export => matches && written.pointer,
            Mode::Fixup => written.converted,
        }
    }

    /// The other version of blob `m`, for a path that needs it, as the
    /// dataref to use, whether it's converted, and whether it was just
    /// made.
    ///
    /// The original blob is referred to by its ID. The converted one is
    /// made from the original, read with `git cat-file`, and written
    /// under a new mark. Without the original ID, or if the blob can't
    /// be converted, such as a pointer whose object is missing, `m` is
    /// used as it is.
    fn alternate<W: Write>(
        &mut self,
        m: u32,
        path: &str,
        writer: &mut Writer<W>,
    ) -> io::Result<(DataRef, bool, bool)> {
        if let Some((dataref, converted)) = self.alternates.get(&m) {
            return Ok((dataref.clone(), *converted, false));
        }
        let converted_before = self.written[&m].converted;
        let alternate = match self.original_ids.get(&m).cloned() {
            None => (DataRef::Mark(m), converted_before),
            Some(id) if converted_before => (DataRef::Sha(id), false),
            Some(id) => {
                let content = read_blob(&self.opts.cwd, &id)?;
                match self.transform_blob(path, content)? {
                    (out, true) => {
                        let mark = self.alloc_fresh();
                        writer.write(&Command::Blob(Blob {
                            mark: Some(mark),
                            original_oid: None,
                            data: out,
                        }))?;
                        (DataRef::Mark(mark), true)
                    }
                    (_, false) => (DataRef::Mark(m), false),
                }
            }
        };
        self.alternates.insert(m, alternate.clone());
        Ok((alternate.0, alternate.1, true))
    }

    /// Convert a blob if its path matches, returning the content to write
    /// and whether it was converted.
    fn transform_blob(&mut self, path: &str, content: Vec<u8>) -> io::Result<(Vec<u8>, bool)> {
        if !path_matches(path, &self.opts.include, &self.opts.exclude) {
            return Ok((content, false));
        }
        match self.mode {
            Mode::Import => self.import_blob(path, content),
            Mode::Export => self.export_blob(path, content),
            // Fixup commits go through `process_commit_fixup`.
            Mode::Fixup => Ok((content, false)),
        }
    }

    fn import_blob(&mut self, _path: &str, content: Vec<u8>) -> io::Result<(Vec<u8>, bool)> {
        let size = content.len() as u64;
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
        let pointer = match Pointer::parse(&content) {
            Ok(p) => p,
            Err(_) => return Ok((content, false)),
        };
        // Without the object, the pointer stays. Nothing is downloaded.
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
        if !matches!(self.mode, Mode::Import) {
            return;
        }
        if self.opts.skip_path_derived_attrs {
            return;
        }
        // With `--above`, track the exact path, not every file with
        // its extension. t-migrate-import checks for lines like
        // `/a.md filter=lfs ...`.
        if self.opts.above > 0 {
            let escaped = super::import::escape_attr_path(path);
            self.attrs_add
                .insert(format!("/{escaped} filter=lfs diff=lfs merge=lfs -text"));
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

/// How a blob was written out.
#[derive(Debug, Clone, Copy)]
struct Written {
    converted: bool,
    /// The original blob's size.
    size: u64,
    /// Whether the original blob is a pointer.
    pointer: bool,
}

/// Read blob `id` from the repository at `cwd`.
fn read_blob(cwd: &Path, id: &str) -> io::Result<Vec<u8>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["cat-file", "blob", id])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git cat-file blob {id} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// The name in the header of the tag object `id`.
fn tag_name(cwd: &Path, id: &str) -> io::Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["cat-file", "tag", id])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git cat-file tag {id} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.strip_prefix("tag ").map(str::to_owned))
        .ok_or_else(|| io::Error::other(format!("tag {id} has no name")))
}

/// Whether `path` is a `.gitattributes` file in any directory.
pub(super) fn is_attrs_path(path: &str) -> bool {
    path == ATTRS_PATH
        || path
            .rsplit_once('/')
            .is_some_and(|(_, leaf)| leaf == ATTRS_PATH)
}

/// The directory of a tree path, or an empty string at the top level.
pub(super) fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => String::new(),
    }
}

/// Build a commit's new `.gitattributes`.
///
/// Drops the existing lines that are in `remove` (ignoring surrounding
/// whitespace), then appends `add` and then `add_initial`, skipping
/// lines already present.
fn build_attrs(
    existing: &str,
    add: &BTreeSet<String>,
    add_initial: &[String],
    remove: &BTreeSet<String>,
) -> String {
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
    // The user's patterns go last, after the per-extension lines, as in
    // upstream. t-migrate-import compares the whole file.
    for p in add_initial {
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
            FileChange::Modify {
                path,
                dataref,
                mode,
                ..
            } if path == ATTRS_PATH => {
                *dataref = DataRef::Mark(attrs_mark);
                // Always a regular, non-executable file.
                // t-migrate-export checks that an executable one loses
                // its executable bit.
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
    use tempfile::TempDir;

    fn fixture_store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("lfs"));
        (tmp, store)
    }

    fn glob(pat: &str) -> PathPatterns {
        PathPatterns::new([pat])
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

        // Blob :1 now holds pointer text, a new blob holds the
        // `.gitattributes`, and the commit gains an M line for it.
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

    /// A stream with one blob, with `original-oid` `id`, at two paths.
    fn shared_blob_stream(id: &str, first: &str, second: &str) -> Vec<u8> {
        format!(
            "blob\nmark :1\noriginal-oid {id}\ndata 12\nhello world\n\
             commit refs/heads/main\nmark :2\n\
             committer A <a@b> 1 +0000\ndata 1\nm\n\
             M 100644 :1 {first}\nM 100644 :1 {second}\n\n"
        )
        .into_bytes()
    }

    #[test]
    fn shared_blob_keeps_original_at_unmatched_path() {
        let id = "3b18e512dba79e4c8300dd08aeb37f8e728b8dad";
        let opts = Options {
            include: Some(glob("*.bin")),
            ..Default::default()
        };
        let (out, stats) = run_transform(&shared_blob_stream(id, "a.bin", "a.txt"), opts);
        assert_eq!(stats.blobs_converted, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("M 100644 :1 a.bin"), "{s}");
        assert!(s.contains(&format!("M 100644 {id} a.txt")), "{s}");
    }

    #[test]
    fn shared_blob_converts_at_matching_path_after_unmatched_one() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str], input: Option<&[u8]>| {
            let mut child = std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(input) = input {
                child.stdin.take().unwrap().write_all(input).unwrap();
            }
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "--quiet"], None);
        let id = git(&["hash-object", "-w", "--stdin"], Some(b"hello world"));
        let opts = Options {
            include: Some(glob("*.bin")),
            cwd: repo.path().to_path_buf(),
            ..Default::default()
        };
        let (out, stats) = run_transform(&shared_blob_stream(&id, "a.txt", "a.bin"), opts);
        assert_eq!(stats.blobs_converted, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("M 100644 :1 a.txt"), "{s}");
        let fresh = format!("M 100644 :{FRESH_MARK_BASE} a.bin");
        assert!(s.contains(&fresh), "{s}");
        assert!(
            stats
                .patterns
                .contains("*.bin filter=lfs diff=lfs merge=lfs -text")
        );
    }

    #[test]
    fn respects_above_threshold() {
        // A 5-byte blob is below the threshold of 100.
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
    fn keeps_empty_gitattributes_when_nothing_is_converted() {
        // The commit refers to the empty file's blob, which is replaced by
        // a rewrite. Without one, fast-import fails on the missing mark.
        let input = b"blob\n\
                      mark :1\n\
                      data 0\n\
                      blob\n\
                      mark :2\n\
                      data 5\n\
                      hello\n\
                      commit refs/heads/main\n\
                      committer A <a@b> 1 +0000\n\
                      data 1\nm\n\
                      M 100644 :1 .gitattributes\n\
                      M 100644 :2 a.txt\n\
                      \n";
        let opts = Options {
            above: 100,
            ..Default::default()
        };
        let (out, stats) = run_transform(input, opts);
        assert_eq!(stats.blobs_converted, 0);
        let s = String::from_utf8(out).unwrap();
        let line = s
            .lines()
            .find(|l| l.ends_with(" .gitattributes"))
            .expect("commit keeps .gitattributes");
        let mark = line.split(' ').nth(2).unwrap();
        assert!(
            s.contains(&format!("mark {mark}\ndata 0\n")),
            "no empty blob for {mark}: {s}"
        );
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
        // Already a pointer, so not converted again.
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
        let out = build_attrs(existing, &add, &[], &remove);
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
        let out = build_attrs(existing, &add, &[], &remove);
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
            // `build_export_attrs` makes these from the patterns. The
            // transform doesn't add lines in export mode.
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
