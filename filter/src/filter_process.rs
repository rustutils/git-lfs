//! The git filter-process protocol.
//!
//! Implements the long-running-filter side of git's `gitprotocol-common`(5)
//! framing: a one-time handshake + capability negotiation, then a loop of
//! request/response pairs over packet-line framing on stdin/stdout. Same
//! business logic as per-invocation `clean`/`smudge`, just batched in one
//! subprocess for the duration of a checkout/commit.

use std::collections::HashMap;
use std::io::{self, Read, Write};

use git_lfs_git::pktline;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;

use crate::{CleanExtension, FetchError, SmudgeExtension, SmudgeOutcome, clean, smudge_with_fetch};

#[derive(Debug, thiserror::Error)]
pub enum FilterProcessError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("filter-process handshake: {0}")]
    Handshake(String),
    #[error("filter-process: missing required header {0:?}")]
    MissingHeader(&'static str),
    #[error("filter-process: unknown command {0:?}")]
    UnknownCommand(String),
}

/// Run the filter-process protocol against `input`/`output` (typically
/// stdin/stdout). Returns when git closes its end of the pipe.
///
/// `fetch` is called when a smudge request hits an object that isn't in
/// the local store; see [`smudge_with_fetch`] for semantics. If fetching
/// is not supported in the caller's context, pass a closure that always
/// errors — the protocol will then surface those smudges as `status=error`
/// to git, same as if [`smudge`](crate::smudge) hit `ObjectMissing`.
///
/// `skip_smudge` reflects the upstream `GIT_LFS_SKIP_SMUDGE` env var.
/// When true, smudge requests pass the pointer text through unchanged
/// — the working-tree file ends up holding pointer text and `git lfs
/// pull` (or another smudge run) is the recovery path. Clean requests
/// are unaffected.
#[allow(clippy::too_many_arguments)]
pub fn filter_process<R, W, F>(
    store: &Store,
    input: R,
    output: W,
    mut fetch: F,
    skip_smudge: bool,
    clean_extensions: &[CleanExtension],
    smudge_extensions: &[SmudgeExtension],
    smudge_path_filter: &dyn Fn(&str) -> bool,
) -> Result<(), FilterProcessError>
where
    R: Read,
    W: Write,
    F: FnMut(&Pointer) -> Result<(), FetchError>,
{
    let mut reader = pktline::Reader::new(input);
    let mut writer = pktline::Writer::new(output);

    handshake(&mut reader, &mut writer)?;

    let mut malformed: Vec<String> = Vec::new();

    loop {
        // A read error here at packet-boundary normally means git closed the
        // pipe — that's the protocol's "we're done" signal, not a real error.
        let headers = match read_headers(&mut reader) {
            Ok(Some(h)) => h,
            Ok(None) => break,
            Err(FilterProcessError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(e),
        };

        let payload = read_payload(&mut reader)?;
        let command = headers
            .get("command")
            .ok_or(FilterProcessError::MissingHeader("command"))?
            .clone();
        let pathname = headers.get("pathname").map(String::as_str).unwrap_or("");

        match command.as_str() {
            "clean" => process_clean(store, &mut writer, &payload, pathname, clean_extensions)?,
            "smudge" if skip_smudge => process_smudge_passthrough(&mut writer, &payload)?,
            // `lfs.fetchinclude` / `lfs.fetchexclude` excluded the
            // path: pass the pointer text through unchanged. Mirrors
            // upstream's clone-time include/exclude (test 2 of
            // t-filter-process); the user can run `git lfs pull`
            // later to materialize.
            "smudge" if !smudge_path_filter(pathname) => {
                process_smudge_passthrough(&mut writer, &payload)?;
            }
            "smudge" => {
                let outcome = process_smudge(
                    store,
                    &mut writer,
                    &payload,
                    pathname,
                    smudge_extensions,
                    &mut fetch,
                )?;
                if matches!(outcome, Some(SmudgeOutcome::Passthrough)) {
                    malformed.push(pathname.to_owned());
                }
            }
            other => return Err(FilterProcessError::UnknownCommand(other.into())),
        }
        writer.flush()?;
    }

    if !malformed.is_empty() {
        report_malformed(&malformed);
    }
    Ok(())
}

fn report_malformed(malformed: &[String]) {
    let stderr = io::stderr();
    let mut out = stderr.lock();
    let header = if malformed.len() == 1 {
        format!(
            "Encountered {} file that should have been a pointer, but wasn't:\n",
            malformed.len()
        )
    } else {
        format!(
            "Encountered {} files that should have been pointers, but weren't:\n",
            malformed.len()
        )
    };
    let _ = out.write_all(header.as_bytes());
    for name in malformed {
        let _ = writeln!(out, "\t{name}");
    }
}

fn handshake<R: Read, W: Write>(
    reader: &mut pktline::Reader<R>,
    writer: &mut pktline::Writer<W>,
) -> Result<(), FilterProcessError> {
    // Welcome.
    let welcome = reader
        .read_text()?
        .ok_or_else(|| FilterProcessError::Handshake("expected welcome, got flush".into()))?;
    if welcome != "git-filter-client" {
        return Err(FilterProcessError::Handshake(format!(
            "expected git-filter-client, got {welcome:?}"
        )));
    }
    let mut versions = Vec::new();
    while let Some(line) = reader.read_text()? {
        versions.push(line);
    }
    if !versions.iter().any(|v| v == "version=2") {
        return Err(FilterProcessError::Handshake(format!(
            "client doesn't advertise version=2 (got {versions:?})"
        )));
    }
    writer.write_text("git-filter-server")?;
    writer.write_text("version=2")?;
    writer.write_flush()?;

    // Send our capabilities *before* reading the client's. The protocol
    // doc reads as if the client speaks first ("capability=…" then a
    // flush, then server replies), but real git serializes the two
    // exchanges and won't send its capabilities until it has seen the
    // server's. Upstream Go's filter-process advertises preemptively for
    // the same reason — diverging from that reordering deadlocks any
    // shell-test that does `git add` of an LFS-tracked path.
    writer.write_text("capability=clean")?;
    writer.write_text("capability=smudge")?;
    writer.write_flush()?;
    writer.flush()?;

    // Drain the client's advertised capabilities (informational — we
    // don't gate on them). We *do* require clean + smudge to be in the
    // set so that misconfigured callers get a clear error rather than
    // silent "command not understood" later.
    let mut caps = Vec::new();
    while let Some(line) = reader.read_text()? {
        caps.push(line);
    }
    for required in ["capability=clean", "capability=smudge"] {
        if !caps.iter().any(|c| c == required) {
            return Err(FilterProcessError::Handshake(format!(
                "client missing required {required} (got {caps:?})"
            )));
        }
    }

    Ok(())
}

fn read_headers<R: Read>(
    reader: &mut pktline::Reader<R>,
) -> Result<Option<HashMap<String, String>>, FilterProcessError> {
    let first = reader.read_text()?;
    let Some(first) = first else {
        // Bare flush at top of loop is unexpected from git; treat as shutdown.
        return Ok(None);
    };
    let mut map = HashMap::new();
    insert_kv(&mut map, &first);
    while let Some(line) = reader.read_text()? {
        insert_kv(&mut map, &line);
    }
    Ok(Some(map))
}

fn insert_kv(map: &mut HashMap<String, String>, line: &str) {
    if let Some((k, v)) = line.split_once('=') {
        map.insert(k.to_owned(), v.to_owned());
    }
}

fn read_payload<R: Read>(reader: &mut pktline::Reader<R>) -> Result<Vec<u8>, FilterProcessError> {
    let mut payload = Vec::new();
    while let Some(packet) = reader.read_packet()? {
        payload.extend_from_slice(&packet);
    }
    Ok(payload)
}

/// Run one clean request through the protocol envelope:
/// `status=success` + flush, content packets + flush, final `status=...` + flush.
fn process_clean<W: Write>(
    store: &Store,
    writer: &mut pktline::Writer<W>,
    payload: &[u8],
    pathname: &str,
    extensions: &[CleanExtension],
) -> Result<(), FilterProcessError> {
    write_initial_status(writer)?;
    let result = run_through_sink(writer, |sink| {
        clean(store, &mut { payload }, sink, pathname, extensions)
            .map(|_| ())
            .map_err(|e| io::Error::other(e.to_string()))
    });
    write_final_status(writer, result.is_ok())?;
    Ok(())
}

/// `GIT_LFS_SKIP_SMUDGE=1` mode: emit the pointer payload unchanged.
fn process_smudge_passthrough<W: Write>(
    writer: &mut pktline::Writer<W>,
    payload: &[u8],
) -> Result<(), FilterProcessError> {
    write_initial_status(writer)?;
    let result = run_through_sink(writer, |sink| sink.write_all(payload));
    write_final_status(writer, result.is_ok())?;
    Ok(())
}

fn process_smudge<W, F>(
    store: &Store,
    writer: &mut pktline::Writer<W>,
    payload: &[u8],
    pathname: &str,
    smudge_extensions: &[SmudgeExtension],
    fetch: &mut F,
) -> Result<Option<SmudgeOutcome>, FilterProcessError>
where
    W: Write,
    F: FnMut(&Pointer) -> Result<(), FetchError>,
{
    write_initial_status(writer)?;
    let mut outcome: Option<SmudgeOutcome> = None;
    let result = run_through_sink(writer, |sink| {
        // The protocol only differentiates success vs. error at this layer;
        // the specific reason (ObjectMissing, FetchFailed, ExtensionFailed,
        // …) is logged by the caller's stderr if they care.
        match smudge_with_fetch(
            store,
            &mut { payload },
            sink,
            pathname,
            smudge_extensions,
            |p| fetch(p),
        ) {
            Ok(o) => {
                outcome = Some(o);
                Ok(())
            }
            Err(e) => Err(io::Error::other(e.to_string())),
        }
    });
    write_final_status(writer, result.is_ok())?;
    Ok(outcome)
}

fn write_initial_status<W: Write>(writer: &mut pktline::Writer<W>) -> io::Result<()> {
    writer.write_text("status=success")?;
    writer.write_flush()
}

fn write_final_status<W: Write>(writer: &mut pktline::Writer<W>, ok: bool) -> io::Result<()> {
    // End-of-content flush comes from the sink runner; this is the
    // post-content "trailer" status that tells git "all done, no errors"
    // (or "I lied, error happened").
    writer.write_text(if ok { "status=success" } else { "status=error" })?;
    writer.write_flush()
}

/// Runs `f` with a packet-line sink, then flushes the sink and emits the
/// end-of-content flush regardless of `f`'s result. The result of `f` is
/// returned for the caller's status decision.
fn run_through_sink<W, F>(writer: &mut pktline::Writer<W>, f: F) -> io::Result<()>
where
    W: Write,
    F: FnOnce(&mut pktline::Sink<'_, W>) -> io::Result<()>,
{
    let result = {
        let mut sink = pktline::Sink::new(writer);
        let r = f(&mut sink);
        sink.flush()?;
        r
    };
    writer.write_flush()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_lfs_pointer::VERSION_LATEST;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("lfs"));
        (tmp, store)
    }

    /// Build a stream of pktline packets the way git would send them.
    struct PktBuilder(Vec<u8>);

    impl PktBuilder {
        fn new() -> Self {
            Self(Vec::new())
        }
        fn text(mut self, s: &str) -> Self {
            let body = format!("{s}\n");
            let total = body.len() + 4;
            self.0.extend_from_slice(format!("{total:04x}").as_bytes());
            self.0.extend_from_slice(body.as_bytes());
            self
        }
        fn data(mut self, b: &[u8]) -> Self {
            let total = b.len() + 4;
            self.0.extend_from_slice(format!("{total:04x}").as_bytes());
            self.0.extend_from_slice(b);
            self
        }
        fn flush(mut self) -> Self {
            self.0.extend_from_slice(b"0000");
            self
        }
        fn build(self) -> Vec<u8> {
            self.0
        }
    }

    /// Decode the response stream into a flat Vec of "packet or flush" tokens
    /// for assertions.
    #[derive(Debug, PartialEq)]
    enum Tok {
        Text(String),
        Bin(Vec<u8>),
        Flush,
    }

    fn decode(bytes: &[u8]) -> Vec<Tok> {
        let mut r = pktline::Reader::new(Cursor::new(bytes));
        let mut out = Vec::new();
        loop {
            match r.read_packet() {
                Ok(Some(p)) => match String::from_utf8(p.clone()) {
                    Ok(s) => out.push(Tok::Text(s.trim_end_matches('\n').to_owned())),
                    Err(_) => out.push(Tok::Bin(p)),
                },
                Ok(None) => out.push(Tok::Flush),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return out,
                Err(e) => panic!("decode error: {e}"),
            }
        }
    }

    fn handshake_input() -> PktBuilder {
        PktBuilder::new()
            .text("git-filter-client")
            .text("version=2")
            .flush()
            .text("capability=clean")
            .text("capability=smudge")
            .flush()
    }

    /// Fetcher that always errors — the right default for tests where
    /// we don't expect any miss. Lies surface as `status=error` to git.
    fn no_fetch(_p: &Pointer) -> Result<(), FetchError> {
        Err("test: no fetcher configured".into())
    }

    fn run(store: &Store, input: Vec<u8>) -> Vec<u8> {
        let mut output = Vec::new();
        filter_process(
            store,
            Cursor::new(input),
            &mut output,
            no_fetch,
            false,
            &[],
            &[],
            &|_| true,
        )
        .unwrap();
        output
    }

    #[test]
    fn handshake_only_then_clean_shutdown() {
        let (_t, store) = fixture();
        let output = run(&store, handshake_input().build());
        let toks = decode(&output);
        // Server welcome + 2 caps + their respective flushes.
        assert_eq!(
            toks,
            vec![
                Tok::Text("git-filter-server".into()),
                Tok::Text("version=2".into()),
                Tok::Flush,
                Tok::Text("capability=clean".into()),
                Tok::Text("capability=smudge".into()),
                Tok::Flush,
            ],
        );
    }

    #[test]
    fn clean_request_emits_pointer() {
        let (_t, store) = fixture();
        let input = handshake_input()
            .text("command=clean")
            .text("pathname=hello.bin")
            .flush()
            .data(b"hello world\n")
            .flush()
            .build();
        let output = run(&store, input);

        // Skip past handshake (6 tokens) and find the response.
        let toks = decode(&output);
        let rest = &toks[6..];
        assert_eq!(rest[0], Tok::Text("status=success".into()));
        assert_eq!(rest[1], Tok::Flush);
        // Next packet(s) are the pointer text. Should fit in one packet.
        if let Tok::Text(t) = &rest[2] {
            assert!(t.starts_with("version https://git-lfs.github.com/spec/v1\n"));
            assert!(t.contains("oid sha256:"));
            assert!(t.contains("size 12"));
        } else {
            panic!("expected text pointer, got {:?}", rest[2]);
        }
        assert_eq!(rest[3], Tok::Flush); // end-of-content
        assert_eq!(rest[4], Tok::Text("status=success".into()));
        assert_eq!(rest[5], Tok::Flush);
    }

    #[test]
    fn smudge_request_emits_content() {
        let (_t, store) = fixture();
        // Pre-populate the store via clean(), then ask filter-process to smudge.
        let mut pointer = Vec::new();
        clean(&store, &mut { &b"smudge a\n"[..] }, &mut pointer, "", &[]).unwrap();

        let input = handshake_input()
            .text("command=smudge")
            .text("pathname=a.dat")
            .flush()
            .data(&pointer)
            .flush()
            .build();
        let output = run(&store, input);
        let toks = decode(&output);
        let rest = &toks[6..];
        assert_eq!(rest[0], Tok::Text("status=success".into()));
        assert_eq!(rest[1], Tok::Flush);
        // Content "smudge a\n" is short text, so it'll round-trip as a Text token.
        assert_eq!(rest[2], Tok::Text("smudge a".into()));
        assert_eq!(rest[3], Tok::Flush);
        assert_eq!(rest[4], Tok::Text("status=success".into()));
    }

    #[test]
    fn smudge_missing_object_emits_status_error() {
        let (_t, store) = fixture();
        let unknown = "0000000000000000000000000000000000000000000000000000000000000001";
        let pointer = format!("version {VERSION_LATEST}\noid sha256:{unknown}\nsize 5\n");
        let input = handshake_input()
            .text("command=smudge")
            .text("pathname=missing.dat")
            .flush()
            .data(pointer.as_bytes())
            .flush()
            .build();
        let output = run(&store, input);
        let toks = decode(&output);
        let rest = &toks[6..];
        assert_eq!(rest[0], Tok::Text("status=success".into())); // initial
        assert_eq!(rest[1], Tok::Flush);
        // No content was written; next is end-of-content flush, then error trailer.
        assert_eq!(rest[2], Tok::Flush);
        assert_eq!(rest[3], Tok::Text("status=error".into()));
        assert_eq!(rest[4], Tok::Flush);
    }

    #[test]
    fn smudge_invokes_fetcher_when_object_missing() {
        let (_t, store) = fixture();
        let content = b"fetched on demand\n";
        // Build the pointer text by cleaning, then wipe the object so the
        // smudge will miss and exercise the fetch path.
        let mut pointer = Vec::new();
        clean(&store, &mut { &content[..] }, &mut pointer, "", &[]).unwrap();
        let parsed = git_lfs_pointer::Pointer::parse(&pointer).unwrap();
        std::fs::remove_file(store.object_path(parsed.oid)).unwrap();

        let input = handshake_input()
            .text("command=smudge")
            .text("pathname=a.dat")
            .flush()
            .data(&pointer)
            .flush()
            .build();

        let mut output = Vec::new();
        let store_ref = &store;
        filter_process(
            &store,
            Cursor::new(input),
            &mut output,
            |p: &Pointer| {
                // Stand in for a real download — re-insert the bytes.
                store_ref.insert(&mut { &content[..] }).unwrap();
                assert_eq!(p.oid, parsed.oid);
                Ok(())
            },
            false,
            &[],
            &[],
            &|_| true,
        )
        .unwrap();

        let toks = decode(&output);
        let rest = &toks[6..];
        assert_eq!(rest[0], Tok::Text("status=success".into()));
        assert_eq!(rest[1], Tok::Flush);
        // Content "fetched on demand\n" comes back as a Text token.
        assert_eq!(rest[2], Tok::Text("fetched on demand".into()));
        assert_eq!(rest[3], Tok::Flush);
        assert_eq!(rest[4], Tok::Text("status=success".into()));
    }

    #[test]
    fn multiple_requests_in_one_session() {
        let (_t, store) = fixture();
        let input = handshake_input()
            .text("command=clean")
            .text("pathname=a.bin")
            .flush()
            .data(b"AAA")
            .flush()
            .text("command=clean")
            .text("pathname=b.bin")
            .flush()
            .data(b"BBB")
            .flush()
            .build();
        let output = run(&store, input);
        let toks = decode(&output);
        // Handshake is 6 tokens; each clean response is 6 tokens.
        // (status=success, flush, content, flush, status=success, flush)
        assert_eq!(toks.len(), 6 + 6 + 6, "got tokens: {toks:?}");
    }
}
