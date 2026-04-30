//! Parser for the `git fast-export` / `git fast-import` wire format.
//!
//! The format is documented in `git-fast-import(1)`. We parse the subset
//! that `git fast-export --full-tree` actually produces:
//!
//! - `blob` — content blob with optional `mark` and `original-oid`.
//! - `commit` — commit metadata + a list of file-change directives.
//! - `tag` — annotated tag.
//! - `reset` — branch reset (often paired with a `from`).
//! - `feature`, `option`, `progress`, `checkpoint`, `done` — control
//!   directives, kept verbatim.
//!
//! Streaming: each call to [`Reader::next`] reads exactly one [`Command`]
//! without buffering the whole stream. Blob content sits in a `Vec<u8>`
//! because the transform stage needs it; everything else stays as a
//! handful of strings.
//!
//! ## Wire-format quirks we accept
//!
//! - Lines inside a command end with `\n`. CR-LF is *not* part of the
//!   format; we don't strip it.
//! - The optional trailing `\n` after a `data <N>\n<bytes>` block is
//!   consumed if present; absent is also fine.
//! - A commit's file-change list terminates at a blank line *or* at the
//!   next recognized top-level keyword. We peek one line ahead to
//!   distinguish.

use std::io::{self, BufRead, BufReader, Read};

/// One top-level fast-export command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Blob(Blob),
    Commit(Commit),
    Tag(Tag),
    Reset(Reset),
    Feature(String),
    Option(String),
    Progress(String),
    Checkpoint,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub mark: Option<u32>,
    pub original_oid: Option<String>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub ref_name: String,
    pub mark: Option<u32>,
    pub original_oid: Option<String>,
    /// Raw `<name> <email> <when>` content (we don't parse the timestamp).
    pub author: Option<String>,
    pub committer: String,
    pub encoding: Option<String>,
    pub message: Vec<u8>,
    pub from: Option<String>,
    pub merges: Vec<String>,
    pub file_changes: Vec<FileChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub mark: Option<u32>,
    pub original_oid: Option<String>,
    pub from: String,
    pub tagger: Option<String>,
    pub message: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reset {
    pub ref_name: String,
    pub from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    /// `M <mode> <dataref> <path>` — modify a tree entry.
    Modify {
        mode: String,
        dataref: DataRef,
        path: String,
    },
    /// `M <mode> inline <path>\n<data>` — inline modify.
    ModifyInline {
        mode: String,
        path: String,
        data: Vec<u8>,
    },
    /// `D <path>` (or the spelled-out `filedelete`).
    Delete { path: String },
    /// `R <src> <dst>`
    Rename { src: String, dst: String },
    /// `C <src> <dst>`
    Copy { src: String, dst: String },
    /// `deleteall` — `--full-tree` emits this at the start of every
    /// commit; we record it so the emitter can re-emit it.
    DeleteAll,
    /// Anything else (notes, etc.) — emit verbatim.
    Raw(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataRef {
    /// `:<id>` — a mark referencing a previous blob in this stream.
    Mark(u32),
    /// `<sha>` — an existing object's SHA-1.
    Sha(String),
}

/// Streaming reader for fast-export output.
pub struct Reader<R: Read> {
    inner: BufReader<R>,
    /// One-line look-ahead used by commits to know when the
    /// file-change list ends.
    pending: Option<String>,
}

impl<R: Read> Reader<R> {
    pub fn new(r: R) -> Self {
        Self {
            inner: BufReader::new(r),
            pending: None,
        }
    }

    /// Read the next command, or `None` at EOF.
    pub fn next(&mut self) -> io::Result<Option<Command>> {
        loop {
            let Some(line) = self.read_line()? else {
                return Ok(None);
            };
            if line.is_empty() {
                // Blank lines between commands; skip.
                continue;
            }
            return self.parse_command(&line).map(Some);
        }
    }

    fn parse_command(&mut self, first: &str) -> io::Result<Command> {
        if first == "blob" {
            return Ok(Command::Blob(self.read_blob_body()?));
        }
        if let Some(rest) = first.strip_prefix("commit ") {
            return Ok(Command::Commit(self.read_commit_body(rest)?));
        }
        if let Some(rest) = first.strip_prefix("tag ") {
            return Ok(Command::Tag(self.read_tag_body(rest)?));
        }
        if let Some(rest) = first.strip_prefix("reset ") {
            return Ok(Command::Reset(self.read_reset_body(rest)?));
        }
        if let Some(rest) = first.strip_prefix("feature ") {
            return Ok(Command::Feature(rest.to_owned()));
        }
        if let Some(rest) = first.strip_prefix("option ") {
            return Ok(Command::Option(rest.to_owned()));
        }
        if let Some(rest) = first.strip_prefix("progress ") {
            return Ok(Command::Progress(rest.to_owned()));
        }
        if first == "checkpoint" {
            return Ok(Command::Checkpoint);
        }
        if first == "done" {
            return Ok(Command::Done);
        }
        Err(io::Error::other(format!(
            "unrecognized fast-export command: {first:?}"
        )))
    }

    fn read_blob_body(&mut self) -> io::Result<Blob> {
        let mut mark = None;
        let mut original_oid = None;
        loop {
            let line = self
                .read_line()?
                .ok_or_else(|| io::Error::other("unexpected EOF in blob"))?;
            if let Some(rest) = line.strip_prefix("mark :") {
                mark = Some(parse_mark(rest)?);
            } else if let Some(rest) = line.strip_prefix("original-oid ") {
                original_oid = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("data ") {
                let data = self.read_data_block(rest)?;
                return Ok(Blob {
                    mark,
                    original_oid,
                    data,
                });
            } else {
                return Err(io::Error::other(format!(
                    "unexpected line in blob: {line:?}"
                )));
            }
        }
    }

    fn read_commit_body(&mut self, ref_name: &str) -> io::Result<Commit> {
        let mut mark = None;
        let mut original_oid = None;
        let mut author = None;
        let mut committer: Option<String> = None;
        let mut encoding = None;
        let mut message = Vec::new();
        let mut from = None;
        let mut merges = Vec::new();
        let mut have_message = false;

        // Header lines until we hit `data`.
        while !have_message {
            let line = self
                .read_line()?
                .ok_or_else(|| io::Error::other("unexpected EOF in commit header"))?;
            if let Some(rest) = line.strip_prefix("mark :") {
                mark = Some(parse_mark(rest)?);
            } else if let Some(rest) = line.strip_prefix("original-oid ") {
                original_oid = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("author ") {
                author = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("committer ") {
                committer = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("encoding ") {
                encoding = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("data ") {
                message = self.read_data_block(rest)?;
                have_message = true;
            } else {
                return Err(io::Error::other(format!(
                    "unexpected commit-header line: {line:?}"
                )));
            }
        }

        // After the message, optional `from` / `merge` / file changes.
        // These continue until a blank line or the next command keyword.
        let mut file_changes = Vec::new();
        while let Some(line) = self.peek_line()? {
            if line.is_empty() {
                self.consume_peek();
                break;
            }
            if is_top_level_keyword(&line) {
                // Don't consume — this is the start of the next command.
                break;
            }
            self.consume_peek();
            if let Some(rest) = line.strip_prefix("from ") {
                from = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("merge ") {
                merges.push(rest.to_owned());
            } else if let Some(rest) = parse_file_change_start(&line) {
                let change = self.complete_file_change(rest)?;
                file_changes.push(change);
            } else {
                return Err(io::Error::other(format!(
                    "unexpected line inside commit body: {line:?}"
                )));
            }
        }

        let committer =
            committer.ok_or_else(|| io::Error::other("commit missing committer line"))?;
        Ok(Commit {
            ref_name: ref_name.to_owned(),
            mark,
            original_oid,
            author,
            committer,
            encoding,
            message,
            from,
            merges,
            file_changes,
        })
    }

    /// `start` is the file-change line we already consumed. For inline
    /// modifies we read a follow-up `data` block.
    fn complete_file_change(&mut self, start: FileChangeStart) -> io::Result<FileChange> {
        match start {
            FileChangeStart::Modify {
                mode,
                dataref,
                path,
            } => Ok(FileChange::Modify {
                mode,
                dataref,
                path,
            }),
            FileChangeStart::ModifyInline { mode, path } => {
                let line = self
                    .read_line()?
                    .ok_or_else(|| io::Error::other("unexpected EOF after `M ... inline`"))?;
                let count = line.strip_prefix("data ").ok_or_else(|| {
                    io::Error::other(format!("expected `data <N>` after inline M, got {line:?}"))
                })?;
                let data = self.read_data_block(count)?;
                Ok(FileChange::ModifyInline { mode, path, data })
            }
            FileChangeStart::Delete(path) => Ok(FileChange::Delete { path }),
            FileChangeStart::Rename(src, dst) => Ok(FileChange::Rename { src, dst }),
            FileChangeStart::Copy(src, dst) => Ok(FileChange::Copy { src, dst }),
            FileChangeStart::DeleteAll => Ok(FileChange::DeleteAll),
            FileChangeStart::Raw(raw) => Ok(FileChange::Raw(raw)),
        }
    }

    fn read_tag_body(&mut self, name: &str) -> io::Result<Tag> {
        let mut mark = None;
        let mut original_oid = None;
        let mut from: Option<String> = None;
        let mut tagger = None;
        let mut message = Vec::new();
        let mut have_message = false;

        while !have_message {
            let line = self
                .read_line()?
                .ok_or_else(|| io::Error::other("unexpected EOF in tag header"))?;
            if let Some(rest) = line.strip_prefix("mark :") {
                mark = Some(parse_mark(rest)?);
            } else if let Some(rest) = line.strip_prefix("original-oid ") {
                original_oid = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("from ") {
                from = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("tagger ") {
                tagger = Some(rest.to_owned());
            } else if let Some(rest) = line.strip_prefix("data ") {
                message = self.read_data_block(rest)?;
                have_message = true;
            } else {
                return Err(io::Error::other(format!(
                    "unexpected tag-header line: {line:?}"
                )));
            }
        }

        let from = from.ok_or_else(|| io::Error::other("tag missing from line"))?;
        Ok(Tag {
            name: name.to_owned(),
            mark,
            original_oid,
            from,
            tagger,
            message,
        })
    }

    fn read_reset_body(&mut self, ref_name: &str) -> io::Result<Reset> {
        // The `from` line is optional; an immediate blank line / next
        // keyword ends the reset.
        let mut from = None;
        if let Some(line) = self.peek_line()? {
            if line.is_empty() {
                self.consume_peek();
            } else if let Some(rest) = line.strip_prefix("from ") {
                let rest = rest.to_owned();
                self.consume_peek();
                from = Some(rest);
                // Optional trailing blank line.
                if let Some(next) = self.peek_line()?
                    && next.is_empty()
                {
                    self.consume_peek();
                }
            }
            // Else: a top-level keyword starts the next command; leave alone.
        }
        Ok(Reset {
            ref_name: ref_name.to_owned(),
            from,
        })
    }

    fn read_data_block(&mut self, count_str: &str) -> io::Result<Vec<u8>> {
        if let Some(rest) = count_str.strip_prefix("<<") {
            // `data <<DELIM\n<lines>\nDELIM\n` form. Used by humans;
            // fast-export doesn't emit it. Refuse rather than half-parse.
            return Err(io::Error::other(format!(
                "data <<DELIM form not supported: {rest:?}"
            )));
        }
        let count: u64 = count_str
            .parse()
            .map_err(|_| io::Error::other(format!("invalid data count {count_str:?}")))?;
        let mut buf = vec![0u8; count as usize];
        self.inner.read_exact(&mut buf)?;
        // Optional trailing LF after the data block. fast-export always
        // emits one; consume if present.
        let mut peek = [0u8; 1];
        match self.inner.read(&mut peek)? {
            0 => {} // EOF — fine.
            1 => {
                if peek[0] != b'\n' {
                    // Not the optional LF — must be the start of the
                    // next line. Stash it.
                    self.pending = Some(String::from_utf8_lossy(&peek).into_owned());
                }
            }
            _ => unreachable!(),
        }
        Ok(buf)
    }

    /// Read one line and strip the trailing `\n`. Returns `None` at EOF.
    fn read_line(&mut self) -> io::Result<Option<String>> {
        if let Some(p) = self.pending.take() {
            // The pending buffer might be a partial line we stashed
            // mid-byte. Append more until we hit a newline or EOF.
            let mut s = p;
            if !s.ends_with('\n') {
                let mut tail = String::new();
                self.inner.read_line(&mut tail)?;
                s.push_str(&tail);
            }
            return Ok(Some(strip_lf(s)));
        }
        let mut buf = String::new();
        let n = self.inner.read_line(&mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(strip_lf(buf)))
    }

    fn peek_line(&mut self) -> io::Result<Option<String>> {
        match self.pending.take() {
            None => {
                let mut buf = String::new();
                let n = self.inner.read_line(&mut buf)?;
                if n == 0 {
                    return Ok(None);
                }
                self.pending = Some(buf);
            }
            Some(p) => {
                // Complete a partial pending fragment (e.g. the single
                // byte the data-block reader stashed when it found a
                // non-LF character after the data section).
                if p.ends_with('\n') {
                    self.pending = Some(p);
                } else {
                    let mut s = p;
                    let mut tail = String::new();
                    self.inner.read_line(&mut tail)?;
                    s.push_str(&tail);
                    self.pending = Some(s);
                }
            }
        }
        Ok(Some(strip_lf(self.pending.clone().unwrap())))
    }

    fn consume_peek(&mut self) {
        self.pending = None;
    }
}

fn strip_lf(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

fn parse_mark(s: &str) -> io::Result<u32> {
    s.parse()
        .map_err(|_| io::Error::other(format!("invalid mark :{s}")))
}

fn is_top_level_keyword(line: &str) -> bool {
    matches!(
        line.split_whitespace().next(),
        Some(
            "blob"
                | "commit"
                | "tag"
                | "reset"
                | "feature"
                | "option"
                | "progress"
                | "checkpoint"
                | "done"
        )
    )
}

/// Intermediate type so we can read the start-of-line of a file-change
/// before deciding whether more bytes (an inline `data` block) follow.
enum FileChangeStart {
    Modify {
        mode: String,
        dataref: DataRef,
        path: String,
    },
    ModifyInline {
        mode: String,
        path: String,
    },
    Delete(String),
    Rename(String, String),
    Copy(String, String),
    DeleteAll,
    Raw(String),
}

/// Decode git's C-style path quoting in a fast-export `M` directive.
/// fast-export emits paths quoted (`"a file.txt"`) when they contain
/// space, double-quote, control chars, or non-ASCII bytes; everything
/// else comes through verbatim. Mirrors `unquote_c_style` in git
/// itself. Unknown escapes are preserved as a best-effort fallback so
/// we don't lose data on malformed input.
fn unquote_path(raw: &str) -> String {
    if !raw.starts_with('"') || !raw.ends_with('"') || raw.len() < 2 {
        return raw.to_owned();
    }
    let inner = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('v') => out.push('\x0b'),
            Some('0') => out.push('\0'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_file_change_start(line: &str) -> Option<FileChangeStart> {
    if line == "deleteall" {
        return Some(FileChangeStart::DeleteAll);
    }
    if let Some(rest) = line.strip_prefix("M ") {
        // Format: `<mode> <dataref> <path>` where dataref is `:N`,
        // a sha, or the literal `inline`. Paths with special chars
        // (e.g. spaces) come C-quoted; unquote before storing so
        // include/exclude globs match unwrapped paths.
        let mut parts = rest.splitn(3, ' ');
        let mode = parts.next()?.to_owned();
        let dataref_or_inline = parts.next()?;
        let path = unquote_path(parts.next()?);
        if dataref_or_inline == "inline" {
            return Some(FileChangeStart::ModifyInline { mode, path });
        }
        let dataref = if let Some(id) = dataref_or_inline.strip_prefix(':') {
            DataRef::Mark(id.parse().ok()?)
        } else {
            DataRef::Sha(dataref_or_inline.to_owned())
        };
        return Some(FileChangeStart::Modify {
            mode,
            dataref,
            path,
        });
    }
    if let Some(path) = line.strip_prefix("D ") {
        return Some(FileChangeStart::Delete(path.to_owned()));
    }
    if let Some(path) = line.strip_prefix("filedelete ") {
        return Some(FileChangeStart::Delete(path.to_owned()));
    }
    if let Some(rest) = line.strip_prefix("R ") {
        let (src, dst) = rest.split_once(' ')?;
        return Some(FileChangeStart::Rename(src.to_owned(), dst.to_owned()));
    }
    if let Some(rest) = line.strip_prefix("C ") {
        let (src, dst) = rest.split_once(' ')?;
        return Some(FileChangeStart::Copy(src.to_owned(), dst.to_owned()));
    }
    // Notes, etc. Pass through verbatim.
    if line.starts_with("N ") {
        return Some(FileChangeStart::Raw(line.to_owned()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(input: &[u8]) -> Vec<Command> {
        let mut reader = Reader::new(input);
        let mut out = Vec::new();
        while let Some(cmd) = reader.next().unwrap() {
            out.push(cmd);
        }
        out
    }

    #[test]
    fn parses_simple_blob() {
        let s = b"blob\nmark :1\ndata 5\nhello\n";
        let cmds = read_all(s);
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            Command::Blob(b) => {
                assert_eq!(b.mark, Some(1));
                assert_eq!(b.data, b"hello");
            }
            other => panic!("expected blob, got {other:?}"),
        }
    }

    #[test]
    fn parses_blob_with_original_oid() {
        let s = b"blob\nmark :1\noriginal-oid abc123\ndata 3\nfoo\n";
        let cmds = read_all(s);
        match &cmds[0] {
            Command::Blob(b) => {
                assert_eq!(b.original_oid.as_deref(), Some("abc123"));
                assert_eq!(b.data, b"foo");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_blob_with_binary_data() {
        // 4-byte blob containing a NUL and an LF — neither should
        // confuse the framing.
        let s: Vec<u8> = b"blob\nmark :1\ndata 4\n"
            .iter()
            .copied()
            .chain([0u8, b'\n', 0xff, 0u8])
            .chain(*b"\n")
            .collect();
        let cmds = read_all(&s);
        match &cmds[0] {
            Command::Blob(b) => {
                assert_eq!(b.data, vec![0u8, b'\n', 0xff, 0u8]);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_simple_commit() {
        let s = b"commit refs/heads/main\n\
                  mark :2\n\
                  author Alice <a@example> 1234567890 +0000\n\
                  committer Alice <a@example> 1234567890 +0000\n\
                  data 11\n\
                  initial msg\n\
                  M 100644 :1 hello.txt\n\
                  \n";
        let cmds = read_all(s);
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            Command::Commit(c) => {
                assert_eq!(c.ref_name, "refs/heads/main");
                assert_eq!(c.mark, Some(2));
                assert_eq!(
                    c.author.as_deref(),
                    Some("Alice <a@example> 1234567890 +0000")
                );
                assert_eq!(c.message, b"initial msg");
                assert_eq!(c.file_changes.len(), 1);
                match &c.file_changes[0] {
                    FileChange::Modify {
                        mode,
                        dataref,
                        path,
                    } => {
                        assert_eq!(mode, "100644");
                        assert_eq!(dataref, &DataRef::Mark(1));
                        assert_eq!(path, "hello.txt");
                    }
                    other => panic!("got {other:?}"),
                }
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_commit_with_full_tree_deleteall() {
        let s = b"commit refs/heads/main\n\
                  committer A <a@b> 1 +0000\n\
                  data 1\n\
                  m\n\
                  from :1\n\
                  deleteall\n\
                  M 100644 :2 a.txt\n\
                  M 100644 :3 b.txt\n\
                  \n";
        let cmds = read_all(s);
        match &cmds[0] {
            Command::Commit(c) => {
                assert_eq!(c.from.as_deref(), Some(":1"));
                assert_eq!(c.file_changes.len(), 3);
                assert!(matches!(c.file_changes[0], FileChange::DeleteAll));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_commit_with_inline_modify() {
        let s = b"commit refs/heads/main\n\
                  committer A <a@b> 1 +0000\n\
                  data 1\n\
                  m\n\
                  M 100644 inline notes.txt\n\
                  data 5\n\
                  hello\n";
        let cmds = read_all(s);
        match &cmds[0] {
            Command::Commit(c) => {
                assert_eq!(c.file_changes.len(), 1);
                match &c.file_changes[0] {
                    FileChange::ModifyInline { mode, path, data } => {
                        assert_eq!(mode, "100644");
                        assert_eq!(path, "notes.txt");
                        assert_eq!(data, b"hello");
                    }
                    other => panic!("got {other:?}"),
                }
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_reset_with_from() {
        let s = b"reset refs/heads/main\nfrom :42\n\n";
        let cmds = read_all(s);
        match &cmds[0] {
            Command::Reset(r) => {
                assert_eq!(r.ref_name, "refs/heads/main");
                assert_eq!(r.from.as_deref(), Some(":42"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_reset_without_from() {
        let s = b"reset refs/heads/main\n\nblob\nmark :1\ndata 0\n\n";
        let cmds = read_all(s);
        assert_eq!(cmds.len(), 2);
        match &cmds[0] {
            Command::Reset(r) => {
                assert_eq!(r.ref_name, "refs/heads/main");
                assert_eq!(r.from, None);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_tag() {
        let s = b"tag v1.0\n\
                  from :5\n\
                  tagger Bob <b@example> 1234 +0000\n\
                  data 8\n\
                  release.\n";
        let cmds = read_all(s);
        match &cmds[0] {
            Command::Tag(t) => {
                assert_eq!(t.name, "v1.0");
                assert_eq!(t.from, ":5");
                assert_eq!(t.message, b"release.");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_feature_option_progress_done() {
        let s = b"feature done\noption git config a=b\nprogress halfway\ndone\n";
        let cmds = read_all(s);
        assert!(matches!(&cmds[0], Command::Feature(s) if s == "done"));
        assert!(matches!(&cmds[1], Command::Option(s) if s == "git config a=b"));
        assert!(matches!(&cmds[2], Command::Progress(s) if s == "halfway"));
        assert!(matches!(&cmds[3], Command::Done));
    }

    #[test]
    fn handles_multiple_commands_back_to_back() {
        let s = b"blob\nmark :1\ndata 1\na\n\
                  blob\nmark :2\ndata 1\nb\n\
                  commit refs/heads/main\n\
                  committer A <a@b> 1 +0000\n\
                  data 1\nm\n\
                  M 100644 :1 a\n\
                  M 100644 :2 b\n\
                  \n";
        let cmds = read_all(s);
        assert_eq!(cmds.len(), 3);
        assert!(matches!(&cmds[0], Command::Blob(_)));
        assert!(matches!(&cmds[1], Command::Blob(_)));
        match &cmds[2] {
            Command::Commit(c) => assert_eq!(c.file_changes.len(), 2),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn errors_on_unknown_command() {
        let s = b"flapdoodle\n";
        let mut reader = Reader::new(&s[..]);
        let err = reader.next().unwrap_err();
        assert!(err.to_string().contains("unrecognized"));
    }

    #[test]
    fn parses_delete_and_rename_and_copy() {
        let s = b"commit refs/heads/main\n\
                  committer A <a@b> 1 +0000\n\
                  data 1\nm\n\
                  D old.txt\n\
                  R from.txt to.txt\n\
                  C base.txt copy.txt\n\
                  \n";
        let cmds = read_all(s);
        match &cmds[0] {
            Command::Commit(c) => {
                assert_eq!(c.file_changes.len(), 3);
                assert!(
                    matches!(&c.file_changes[0], FileChange::Delete { path } if path == "old.txt")
                );
                assert!(matches!(
                    &c.file_changes[1],
                    FileChange::Rename { src, dst } if src == "from.txt" && dst == "to.txt"
                ));
                assert!(matches!(
                    &c.file_changes[2],
                    FileChange::Copy { src, dst } if src == "base.txt" && dst == "copy.txt"
                ));
            }
            other => panic!("got {other:?}"),
        }
    }
}
