//! Emitter for the `git fast-import` wire format.
//!
//! Inverse of [`crate::migrate::fast_export::Reader`]: takes [`Command`]
//! values and writes them back as bytes that `git fast-import` can
//! ingest. Round-tripping a command through Reader → Writer → Reader
//! should yield equal values; see the round-trip tests.

use std::io::{self, Write};

use super::fast_export::{Command, Commit, DataRef, FileChange, Reset, Tag};

pub struct Writer<W: Write> {
    inner: W,
}

impl<W: Write> Writer<W> {
    pub fn new(w: W) -> Self {
        Self { inner: w }
    }

    pub fn write(&mut self, cmd: &Command) -> io::Result<()> {
        match cmd {
            Command::Blob(b) => self.write_blob(b),
            Command::Commit(c) => self.write_commit(c),
            Command::Tag(t) => self.write_tag(t),
            Command::Reset(r) => self.write_reset(r),
            Command::Feature(s) => writeln!(self.inner, "feature {s}"),
            Command::Option(s) => writeln!(self.inner, "option {s}"),
            Command::Progress(s) => writeln!(self.inner, "progress {s}"),
            Command::Checkpoint => writeln!(self.inner, "checkpoint"),
            Command::Done => writeln!(self.inner, "done"),
        }
    }

    fn write_blob(&mut self, b: &super::fast_export::Blob) -> io::Result<()> {
        writeln!(self.inner, "blob")?;
        if let Some(m) = b.mark {
            writeln!(self.inner, "mark :{m}")?;
        }
        if let Some(oid) = &b.original_oid {
            writeln!(self.inner, "original-oid {oid}")?;
        }
        self.write_data_block(&b.data)
    }

    fn write_commit(&mut self, c: &Commit) -> io::Result<()> {
        writeln!(self.inner, "commit {}", c.ref_name)?;
        if let Some(m) = c.mark {
            writeln!(self.inner, "mark :{m}")?;
        }
        if let Some(oid) = &c.original_oid {
            writeln!(self.inner, "original-oid {oid}")?;
        }
        if let Some(a) = &c.author {
            writeln!(self.inner, "author {a}")?;
        }
        writeln!(self.inner, "committer {}", c.committer)?;
        if let Some(e) = &c.encoding {
            writeln!(self.inner, "encoding {e}")?;
        }
        self.write_data_block(&c.message)?;
        if let Some(f) = &c.from {
            writeln!(self.inner, "from {f}")?;
        }
        for m in &c.merges {
            writeln!(self.inner, "merge {m}")?;
        }
        for change in &c.file_changes {
            self.write_file_change(change)?;
        }
        // Trailing blank line. fast-import accepts either blank or the
        // start of the next command; emitting blank is unambiguous.
        writeln!(self.inner)
    }

    fn write_tag(&mut self, t: &Tag) -> io::Result<()> {
        writeln!(self.inner, "tag {}", t.name)?;
        if let Some(m) = t.mark {
            writeln!(self.inner, "mark :{m}")?;
        }
        if let Some(oid) = &t.original_oid {
            writeln!(self.inner, "original-oid {oid}")?;
        }
        writeln!(self.inner, "from {}", t.from)?;
        if let Some(tagger) = &t.tagger {
            writeln!(self.inner, "tagger {tagger}")?;
        }
        self.write_data_block(&t.message)
    }

    fn write_reset(&mut self, r: &Reset) -> io::Result<()> {
        writeln!(self.inner, "reset {}", r.ref_name)?;
        if let Some(f) = &r.from {
            writeln!(self.inner, "from {f}")?;
        }
        writeln!(self.inner)
    }

    fn write_file_change(&mut self, c: &FileChange) -> io::Result<()> {
        match c {
            FileChange::Modify { mode, dataref, path } => {
                let dr = match dataref {
                    DataRef::Mark(id) => format!(":{id}"),
                    DataRef::Sha(s) => s.clone(),
                };
                writeln!(self.inner, "M {mode} {dr} {path}")
            }
            FileChange::ModifyInline { mode, path, data } => {
                writeln!(self.inner, "M {mode} inline {path}")?;
                self.write_data_block(data)
            }
            FileChange::Delete { path } => writeln!(self.inner, "D {path}"),
            FileChange::Rename { src, dst } => writeln!(self.inner, "R {src} {dst}"),
            FileChange::Copy { src, dst } => writeln!(self.inner, "C {src} {dst}"),
            FileChange::DeleteAll => writeln!(self.inner, "deleteall"),
            FileChange::Raw(s) => writeln!(self.inner, "{s}"),
        }
    }

    fn write_data_block(&mut self, data: &[u8]) -> io::Result<()> {
        writeln!(self.inner, "data {}", data.len())?;
        self.inner.write_all(data)?;
        // Optional trailing LF after the data block. fast-export emits
        // it; we do too so the next command's keyword starts on its
        // own line.
        self.inner.write_all(b"\n")?;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::super::fast_export::{Blob, Commit, Reader};
    use super::*;

    fn round_trip(input: &[u8]) -> Vec<u8> {
        let mut reader = Reader::new(input);
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = Writer::new(&mut buf);
        while let Some(cmd) = reader.next().unwrap() {
            writer.write(&cmd).unwrap();
        }
        writer.flush().unwrap();
        buf
    }

    fn read_back(bytes: &[u8]) -> Vec<Command> {
        let mut reader = Reader::new(bytes);
        let mut out = Vec::new();
        while let Some(cmd) = reader.next().unwrap() {
            out.push(cmd);
        }
        out
    }

    #[test]
    fn round_trip_simple_blob() {
        let input = b"blob\nmark :1\ndata 5\nhello\n";
        let cmds_in = read_back(input);
        let bytes = round_trip(input);
        let cmds_out = read_back(&bytes);
        assert_eq!(cmds_in, cmds_out);
    }

    #[test]
    fn round_trip_blob_with_binary_content() {
        let mut input = b"blob\nmark :1\ndata 4\n".to_vec();
        input.extend_from_slice(&[0u8, b'\n', 0xff, 0u8]);
        input.push(b'\n');
        let cmds_in = read_back(&input);
        let bytes = round_trip(&input);
        let cmds_out = read_back(&bytes);
        assert_eq!(cmds_in, cmds_out);
    }

    #[test]
    fn round_trip_full_tree_commit() {
        let input = b"commit refs/heads/main\n\
                      mark :2\n\
                      author Alice <a@example> 1234567890 +0000\n\
                      committer Alice <a@example> 1234567890 +0000\n\
                      data 11\n\
                      initial msg\n\
                      from :prev\n\
                      deleteall\n\
                      M 100644 :1 a.txt\n\
                      M 100644 :3 b/c.txt\n\
                      \n";
        let cmds_in = read_back(input);
        let bytes = round_trip(input);
        let cmds_out = read_back(&bytes);
        assert_eq!(cmds_in, cmds_out);
    }

    #[test]
    fn round_trip_commit_with_inline_modify() {
        let input = b"commit refs/heads/main\n\
                      committer A <a@b> 1 +0000\n\
                      data 1\nm\n\
                      M 100644 inline note.txt\n\
                      data 5\nhello\n";
        let cmds_in = read_back(input);
        let bytes = round_trip(input);
        let cmds_out = read_back(&bytes);
        assert_eq!(cmds_in, cmds_out);
    }

    #[test]
    fn round_trip_reset_tag_done() {
        let input = b"reset refs/heads/main\nfrom :7\n\n\
                      tag v1.0\nfrom :7\ntagger A <a@b> 1 +0000\ndata 4\nrel!\n\
                      done\n";
        let cmds_in = read_back(input);
        let bytes = round_trip(input);
        let cmds_out = read_back(&bytes);
        assert_eq!(cmds_in, cmds_out);
    }

    #[test]
    fn write_blob_directly() {
        let blob = Blob {
            mark: Some(7),
            original_oid: None,
            data: b"abc".to_vec(),
        };
        let mut buf: Vec<u8> = Vec::new();
        Writer::new(&mut buf).write(&Command::Blob(blob)).unwrap();
        assert_eq!(&buf[..], b"blob\nmark :7\ndata 3\nabc\n");
    }

    /// Round-trip a stream that mirrors what `git fast-export
    /// --full-tree` emits in practice: `reset` first, then a chain of
    /// commits referencing earlier ones via marks, with `deleteall` on
    /// every commit including the empty initial.
    #[test]
    fn round_trip_realistic_full_tree_stream() {
        let input = b"reset refs/heads/main\n\
                      commit refs/heads/main\n\
                      mark :1\n\
                      author t <t@t> 1 +0000\n\
                      committer t <t@t> 1 +0000\n\
                      data 6\nfirst\n\
                      deleteall\n\
                      \n\
                      blob\n\
                      mark :2\n\
                      data 7\nbinary\n\
                      commit refs/heads/main\n\
                      mark :3\n\
                      author t <t@t> 1 +0000\n\
                      committer t <t@t> 1 +0000\n\
                      data 7\nsecond\n\
                      from :1\n\
                      deleteall\n\
                      M 100644 :2 foo.bin\n\
                      \n";
        let cmds_in = read_back(input);
        let bytes = round_trip(input);
        let cmds_out = read_back(&bytes);
        assert_eq!(cmds_in, cmds_out, "round-trip should be lossless");
        assert_eq!(cmds_in.len(), 4, "expected reset + 2 commits + 1 blob");
    }

    #[test]
    fn write_commit_includes_all_metadata_fields() {
        let commit = Commit {
            ref_name: "refs/heads/x".into(),
            mark: Some(3),
            original_oid: Some("abc".into()),
            author: Some("A <a@b> 1 +0000".into()),
            committer: "B <b@c> 2 +0000".into(),
            encoding: Some("UTF-8".into()),
            message: b"hi".to_vec(),
            from: Some(":2".into()),
            merges: vec![":99".into()],
            file_changes: vec![FileChange::Delete { path: "old".into() }],
        };
        let mut buf: Vec<u8> = Vec::new();
        Writer::new(&mut buf).write(&Command::Commit(commit)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        for needle in &[
            "commit refs/heads/x\n",
            "mark :3\n",
            "original-oid abc\n",
            "author A <a@b> 1 +0000\n",
            "committer B <b@c> 2 +0000\n",
            "encoding UTF-8\n",
            "data 2\nhi\n",
            "from :2\n",
            "merge :99\n",
            "D old\n",
        ] {
            assert!(s.contains(needle), "missing {needle:?} in:\n{s}");
        }
    }
}
