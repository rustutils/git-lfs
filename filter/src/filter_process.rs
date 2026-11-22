//! Git's long-running filter protocol.
//!
//! After a handshake, git sends clean and smudge requests over pkt-line
//! framing, and each one is answered with [`clean`] or [`smudge_with_fetch`].

use std::collections::HashMap;
use std::io::{self, Read, Write};

use git_lfs_git::pktline;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;

use crate::{CleanExtension, FetchError, SmudgeExtension, SmudgeOutcome, clean, smudge_with_fetch};

/// Things that can go wrong while running [`filter_process`].
#[derive(Debug, thiserror::Error)]
pub enum FilterProcessError {
    /// Reading from or writing to git failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Git's handshake was malformed, or didn't offer version 2 or both the
    /// clean and smudge capabilities.
    #[error("filter-process handshake: {0}")]
    Handshake(String),
    /// A request had no `command` header.
    #[error("filter-process: missing required header {0:?}")]
    MissingHeader(&'static str),
    /// A request's command wasn't `clean` or `smudge`.
    #[error("filter-process: unknown command {0:?}")]
    UnknownCommand(String),
}

/// Serve git's long-running filter protocol on `input` and `output`.
///
/// `input` and `output` are usually stdin and stdout. Returns when git
/// closes its end. Each request's content is read fully into memory before
/// it's processed. A failed clean or smudge is reported to git as
/// `status=error` and the session continues. A malformed request ends it
/// with an error.
///
/// `fetch` downloads missing objects, as for [`smudge_with_fetch`]. A
/// closure that always fails makes every missing object a `status=error`.
///
/// When `skip_smudge` is set (as by `GIT_LFS_SKIP_SMUDGE`), smudge requests
/// get the pointer back unchanged. So do paths for which
/// `smudge_path_filter` returns false. Clean requests are unaffected by
/// either.
///
/// Smudge requests whose content isn't a pointer are passed through, and
/// their paths are listed on stderr at the end of the session as files that
/// should have been pointers. The protocol's `delay` capability isn't
/// supported.
/// What Git said about a smudge request, for the `fetch` callback of
/// [`filter_process`].
#[derive(Debug, Clone, Copy)]
pub struct SmudgeRequest<'a> {
    /// The file's path in the working tree.
    pub pathname: &'a str,
    /// The commit or tree being checked out, from Git's `treeish`
    /// metadata. Git leaves it out in some cases, such as a sparse
    /// checkout.
    pub treeish: Option<&'a str>,
}

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
    F: FnMut(&Pointer, &SmudgeRequest) -> Result<(), FetchError>,
{
    let mut reader = pktline::Reader::new(input);
    let mut writer = pktline::Writer::new(output);

    handshake(&mut reader, &mut writer)?;

    let mut malformed: Vec<String> = Vec::new();

    loop {
        // EOF between requests is how git ends the session.
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
        let request = SmudgeRequest {
            pathname,
            treeish: headers.get("treeish").map(String::as_str),
        };

        match command.as_str() {
            "clean" => process_clean(store, &mut writer, &payload, pathname, clean_extensions)?,
            "smudge" if skip_smudge => process_smudge_passthrough(&mut writer, &payload)?,
            "smudge" if !smudge_path_filter(pathname) => {
                process_smudge_passthrough(&mut writer, &payload)?;
            }
            "smudge" => {
                let outcome = process_smudge(
                    store,
                    &mut writer,
                    &payload,
                    &request,
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

    // Git sends its capabilities only after reading our version reply, so
    // that has to be flushed before we read them. Ours go out with it,
    // ahead of git's, which git accepts.
    writer.write_text("capability=clean")?;
    writer.write_text("capability=smudge")?;
    writer.write_flush()?;
    writer.flush()?;

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
        // Git doesn't send a bare flush here. Treat it as the end.
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

/// Answer one clean request.
///
/// The reply is `status=success` and a flush, the content and a flush, then
/// the final status and a flush.
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

/// Answer a smudge request with the pointer unchanged.
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
    request: &SmudgeRequest,
    smudge_extensions: &[SmudgeExtension],
    fetch: &mut F,
) -> Result<Option<SmudgeOutcome>, FilterProcessError>
where
    W: Write,
    F: FnMut(&Pointer, &SmudgeRequest) -> Result<(), FetchError>,
{
    write_initial_status(writer)?;
    let mut outcome: Option<SmudgeOutcome> = None;
    let result = run_through_sink(writer, |sink| {
        // Git only learns success or error, so say why on stderr.
        match smudge_with_fetch(
            store,
            &mut { payload },
            sink,
            request.pathname,
            smudge_extensions,
            |p| fetch(p, request),
        ) {
            Ok(o) => {
                outcome = Some(o);
                Ok(())
            }
            Err(e) => {
                if let Some(line) = e.download_error_line() {
                    eprintln!("{line}");
                }
                Err(io::Error::other(e.to_string()))
            }
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
    // Sent after the content, this overrides the initial `status=success`.
    writer.write_text(if ok { "status=success" } else { "status=error" })?;
    writer.write_flush()
}

/// Run `f` with a pkt-line sink, then write the end-of-content flush.
///
/// The flush is written whether or not `f` fails, and `f`'s result is
/// returned for the final status.
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

    /// Fetcher that always errors, for tests that expect no miss.
    fn no_fetch(_p: &Pointer, _r: &SmudgeRequest) -> Result<(), FetchError> {
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
        // Server welcome and version, two capabilities, each with a flush.
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
        // No content, just the end-of-content flush and the error status.
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
            |p: &Pointer, _: &SmudgeRequest| {
                // Stand in for a real download by re-inserting the bytes.
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
        // Handshake is 6 tokens, and so is each clean response.
        // (status=success, flush, content, flush, status=success, flush)
        assert_eq!(toks.len(), 6 + 6 + 6, "got tokens: {toks:?}");
    }
}
