//! `git cat-file --batch[-check]` long-running subprocess wrappers.
//!
//! Both flavors keep one git subprocess alive across many queries, which
//! is critical for scanners that need to inspect thousands of OIDs (one
//! fork per object would dominate runtime). Send `<oid>\n` on stdin,
//! parse `<oid> <type> <size>\n` (or `<oid> missing\n`) from stdout.
//! `--batch` additionally streams `<size>` bytes of content + a trailing
//! newline after the header.
//!
//! See `git-cat-file(1)` § "BATCH OUTPUT".

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::Error;

/// One header response from `cat-file --batch[-check]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatFileHeader {
    /// Object exists. `size` is the in-repo content length in bytes.
    Found { oid: String, kind: String, size: u64 },
    /// Git replied with `<requested-oid> missing`.
    Missing { oid: String },
}

/// Full response from `cat-file --batch`: a header plus exactly `size`
/// bytes of content (only present when the header is [`CatFileHeader::Found`]).
#[derive(Debug, Clone)]
pub struct BlobContent {
    pub oid: String,
    pub kind: String,
    pub size: u64,
    pub content: Vec<u8>,
}

/// `git cat-file --batch-check` — header-only mode. Use this to decide
/// whether to spend the I/O on reading a blob's content (e.g. filter to
/// blobs ≤ MAX_POINTER_SIZE before paying the read cost).
pub struct CatFileBatchCheck {
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    child: Child,
}

impl CatFileBatchCheck {
    pub fn spawn(cwd: &Path) -> Result<Self, Error> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["cat-file", "--batch-check"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        Ok(Self { stdin: Some(stdin), stdout, child })
    }

    /// Look up one OID. Returns the parsed header.
    pub fn check(&mut self, oid: &str) -> Result<CatFileHeader, Error> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::Failed("cat-file --batch-check stdin closed".into()))?;
        writeln!(stdin, "{oid}")?;
        stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        if line.is_empty() {
            return Err(Error::Failed(
                "cat-file --batch-check exited unexpectedly".into(),
            ));
        }
        parse_header(line.trim_end_matches('\n'))
    }
}

impl Drop for CatFileBatchCheck {
    fn drop(&mut self) {
        // Closing stdin signals cat-file to exit cleanly.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// `git cat-file --batch` — header + content mode. Use this once you've
/// narrowed candidates with [`CatFileBatchCheck`] (typically by size).
pub struct CatFileBatch {
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    child: Child,
}

impl CatFileBatch {
    pub fn spawn(cwd: &Path) -> Result<Self, Error> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        Ok(Self { stdin: Some(stdin), stdout, child })
    }

    /// Read one OID. Returns `Ok(None)` if git replied "missing"; otherwise
    /// the full blob content. Reads exactly `size` bytes after the header,
    /// then consumes the trailing newline git emits between objects.
    pub fn read(&mut self, oid: &str) -> Result<Option<BlobContent>, Error> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::Failed("cat-file --batch stdin closed".into()))?;
        writeln!(stdin, "{oid}")?;
        stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        if line.is_empty() {
            return Err(Error::Failed("cat-file --batch exited unexpectedly".into()));
        }
        match parse_header(line.trim_end_matches('\n'))? {
            CatFileHeader::Missing { .. } => Ok(None),
            CatFileHeader::Found { oid, kind, size } => {
                let mut content = vec![0u8; size as usize];
                self.stdout.read_exact(&mut content)?;
                let mut nl = [0u8; 1];
                self.stdout.read_exact(&mut nl)?;
                if nl[0] != b'\n' {
                    return Err(Error::Failed(format!(
                        "cat-file --batch: expected trailing newline, got byte 0x{:02x}",
                        nl[0]
                    )));
                }
                Ok(Some(BlobContent { oid, kind, size, content }))
            }
        }
    }
}

impl Drop for CatFileBatch {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// Parse a `cat-file --batch[-check]` header line.
///
/// Lines come in two flavors:
/// - `<oid> <type> <size>` — object found
/// - `<oid> missing` — object not in the repo
fn parse_header(line: &str) -> Result<CatFileHeader, Error> {
    let mut parts = line.splitn(3, ' ');
    let oid = parts
        .next()
        .ok_or_else(|| Error::Failed(format!("cat-file: empty header line {line:?}")))?
        .to_owned();
    let second = parts
        .next()
        .ok_or_else(|| Error::Failed(format!("cat-file: malformed header {line:?}")))?;
    if second == "missing" {
        return Ok(CatFileHeader::Missing { oid });
    }
    let size_str = parts
        .next()
        .ok_or_else(|| Error::Failed(format!("cat-file: missing size in {line:?}")))?;
    let size = size_str
        .parse::<u64>()
        .map_err(|e| Error::Failed(format!("cat-file: bad size {size_str:?}: {e}")))?;
    Ok(CatFileHeader::Found {
        oid,
        kind: second.to_owned(),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::commit_helper::*;

    #[test]
    fn parse_header_found() {
        let h = parse_header("abc123 blob 42").unwrap();
        match h {
            CatFileHeader::Found { oid, kind, size } => {
                assert_eq!(oid, "abc123");
                assert_eq!(kind, "blob");
                assert_eq!(size, 42);
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn parse_header_missing() {
        let h = parse_header("abc123 missing").unwrap();
        assert!(matches!(h, CatFileHeader::Missing { oid } if oid == "abc123"));
    }

    #[test]
    fn parse_header_malformed() {
        assert!(parse_header("").is_err());
        assert!(parse_header("only-one-token").is_err());
        assert!(parse_header("oid blob not-a-size").is_err());
    }

    #[test]
    fn batch_check_known_blob() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"hello");
        // Find the blob OID via ls-tree (cheap shell).
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["ls-tree", "-r", "HEAD"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let blob_oid = stdout.split_whitespace().nth(2).unwrap();

        let mut bc = CatFileBatchCheck::spawn(repo.path()).unwrap();
        let h = bc.check(blob_oid).unwrap();
        match h {
            CatFileHeader::Found { kind, size, .. } => {
                assert_eq!(kind, "blob");
                assert_eq!(size, 5); // "hello"
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn batch_check_missing_oid() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"x");
        let mut bc = CatFileBatchCheck::spawn(repo.path()).unwrap();
        let nope = "0000000000000000000000000000000000000001";
        match bc.check(nope).unwrap() {
            CatFileHeader::Missing { oid } => assert_eq!(oid, nope),
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn batch_reads_content_and_trailing_newline() {
        let repo = init_repo();
        // Use bytes that include a literal newline in the middle so we
        // exercise the read_exact path rather than relying on read_line.
        let content = b"line one\nline two\n";
        commit_file(&repo, "multi.txt", content);
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["ls-tree", "-r", "HEAD"])
            .output()
            .unwrap();
        let blob_oid = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .nth(2)
            .unwrap()
            .to_owned();

        let mut b = CatFileBatch::spawn(repo.path()).unwrap();
        let blob = b.read(&blob_oid).unwrap().unwrap();
        assert_eq!(blob.kind, "blob");
        assert_eq!(blob.size, content.len() as u64);
        assert_eq!(blob.content, content);
    }

    #[test]
    fn batch_returns_none_for_missing() {
        let repo = init_repo();
        commit_file(&repo, "x.txt", b"x");
        let mut b = CatFileBatch::spawn(repo.path()).unwrap();
        let r = b.read("0000000000000000000000000000000000000001").unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn batch_handles_many_queries_in_one_session() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"AAA");
        commit_file(&repo, "b.txt", b"BBBB");
        commit_file(&repo, "c.txt", b"CCCCC");

        // Collect all blob OIDs.
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["ls-tree", "-r", "HEAD"])
            .output()
            .unwrap();
        let oids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.split_whitespace().nth(2).unwrap().to_owned())
            .collect();
        assert_eq!(oids.len(), 3);

        let mut b = CatFileBatch::spawn(repo.path()).unwrap();
        let mut sizes = Vec::new();
        for oid in &oids {
            let blob = b.read(oid).unwrap().unwrap();
            sizes.push(blob.size);
        }
        sizes.sort_unstable();
        assert_eq!(sizes, vec![3, 4, 5]);
    }
}
