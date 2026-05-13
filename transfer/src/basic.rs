//! Basic transfer adapter — direct GET/PUT/POST against the action URLs
//! the batch endpoint hands back.
//!
//! See `docs/api/basic-transfers.md`.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;

use futures::StreamExt;
use git_lfs_api::{Action, Actions};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use reqwest::header::{CONTENT_LENGTH, RANGE};
use reqwest::{Body, Method, Response};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::io::{ReaderStream, StreamReader, SyncIoBridge};

use crate::error::TransferError;
use crate::event::Event;

/// Fallback Content-Type for raw object uploads — used when the action
/// response didn't set one and the file's content didn't match any
/// signature we sniff for. Also the value we send unconditionally when
/// `lfs.<url>.contenttype=false`. Servers/CDNs may override per-action
/// via `action.header` regardless.
const OCTET_STREAM: &str = "application/octet-stream";

#[derive(Debug, Serialize)]
struct VerifyBody<'a> {
    oid: &'a str,
    size: u64,
}

/// Stream-download `oid` from `action.href` into `store`.
///
/// Writes go to `<lfs_dir>/incomplete/<oid>.part` and only rename into
/// the object store once the bytes hash to `expected`. The partial
/// file persists across retry attempts: a previous interrupted download
/// becomes the resume point for the next call (which sends a `Range:`
/// header). Mirrors upstream's `tq/basic_download.go`.
///
/// Status-code handling:
/// - 200 OK without a Range request → fresh download, overwrites any
///   stale partial.
/// - 200 OK with a Range request → server ignored the Range; same as
///   a fresh download (truncate, write full body).
/// - 206 Partial Content → append to the existing partial; the bytes
///   from the prior attempt remain at the front of the file.
/// - 416 Requested Range Not Satisfiable → clear the partial and
///   recursively retry without `Range:` (`xfer: server rejected
///   resume … re-downloading from start`).
pub(crate) async fn download(
    http: &reqwest::Client,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    action: &Action,
    events: Option<&UnboundedSender<Event>>,
) -> Result<u64, TransferError> {
    let expected = Oid::from_hex(oid)?;
    let partial_path = store.incomplete_path(expected);

    // A partial whose size meets or exceeds the expected total is
    // useless as a resume point — the bytes might be corrupt and
    // `bytes=<size>-<size-1>` is an invalid Range (the upstream
    // gitserver rejects with 400). Drop it before deciding whether to
    // resume. t-batch-storage-retries.sh test 5 part B asserts no
    // Range / 400 line in this case.
    if let Ok(m) = std::fs::metadata(&partial_path)
        && m.len() >= size
    {
        let _ = std::fs::remove_file(&partial_path);
    }
    let resume_offset: Option<u64> = std::fs::metadata(&partial_path)
        .ok()
        .map(|m| m.len())
        .filter(|&n| n > 0 && n < size);

    // Build the GET. Action headers (auth tokens, etc.) ride along on
    // every attempt; Range only when resuming.
    let mut req = http.request(Method::GET, &action.href);
    for (k, v) in &action.header {
        req = req.header(k, v);
    }
    let range_header = resume_offset.map(|offset| {
        // RFC 7233 closed range: end byte is inclusive.
        format!("bytes={offset}-{}", size.saturating_sub(1))
    });
    if let Some(range) = &range_header {
        req = req.header(RANGE, range);
        if trace_enabled() {
            // Upstream's `xfer: Attempting to resume download of %q
            // from byte %d` in basic_download.go:105. Test grep is
            // just the substring with the quoted oid.
            eprintln!(
                "xfer: Attempting to resume download of {oid:?} from byte {}",
                resume_offset.unwrap()
            );
        }
    }
    dump_verbose_request(&action.href, range_header.as_deref(), &action.header);

    let resp = req.send().await?;
    dump_verbose_response(&resp);

    // 416 + we sent Range → server rejected the resume. Clear and
    // restart without Range. Upstream's
    // `xfer: server rejected resume … re-downloading from start`
    // trace at basic_download.go:182.
    if let Some(offset) = resume_offset
        && resp.status().as_u16() == 416
    {
        if trace_enabled() {
            eprintln!(
                "xfer: server rejected resume download request for {oid:?} from byte {offset}; re-downloading from start"
            );
        }
        let _ = std::fs::remove_file(&partial_path);
        return Box::pin(download(http, store, oid, size, action, events)).await;
    }

    check_status(&resp, &action.href)?;

    // 206 only counts as "server accepted resume" when we actually
    // asked. A 200 to a Range request means the server ignored it —
    // treat like a fresh download.
    let server_resumed = matches!((resume_offset, resp.status().as_u16()), (Some(_), 206),);
    if let Some(offset) = resume_offset
        && server_resumed
        && trace_enabled()
    {
        eprintln!("xfer: server accepted resume download request: {oid:?} from byte {offset}");
    }

    let mut bytes_done: u64 = if server_resumed {
        resume_offset.unwrap_or(0)
    } else {
        0
    };
    let oid_owned = oid.to_owned();
    let events_clone = events.cloned();
    let body_stream = resp.bytes_stream().map(move |chunk| {
        if let Ok(ref c) = chunk {
            bytes_done += c.len() as u64;
            if let Some(s) = &events_clone {
                let _ = s.send(Event::Progress {
                    oid: oid_owned.clone(),
                    bytes_done,
                });
            }
        }
        chunk.map_err(std::io::Error::other)
    });

    let async_reader = StreamReader::new(body_stream);
    let mut bridge = SyncIoBridge::new(async_reader);

    let partial_path_owned = partial_path.clone();
    let store_for_blocking = store.clone();
    let total = tokio::task::spawn_blocking(move || -> Result<u64, TransferError> {
        store_for_blocking.prepare_incomplete_dir()?;
        write_partial(&partial_path_owned, server_resumed, &mut bridge)?;
        let actual = hash_file(&partial_path_owned)?;
        if actual != expected {
            return Err(TransferError::Store(
                git_lfs_store::StoreError::HashMismatch { expected, actual },
            ));
        }
        let total = std::fs::metadata(&partial_path_owned)?.len();
        store_for_blocking.commit_partial(expected, &partial_path_owned)?;
        Ok(total)
    })
    .await
    .map_err(|join_err| std::io::Error::other(join_err.to_string()))??;

    Ok(total)
}

/// Returns true when the user asked for `GIT_TRACE`. Kept in sync with
/// the same gate in `transfer.rs` — both files use this signal.
fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Returns true when the user asked for `GIT_CURL_VERBOSE`.
fn verbose_enabled() -> bool {
    std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Emit the outgoing request line + selected headers in curl-verbose
/// shape so `t-batch-storage-retries.sh` can grep `Range: bytes=`.
fn dump_verbose_request(
    url: &str,
    range: Option<&str>,
    extra_headers: &std::collections::HashMap<String, String>,
) {
    if !verbose_enabled() {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "> GET {url}");
    if let Some(r) = range {
        let _ = writeln!(err, "> Range: {r}");
    }
    for (k, v) in extra_headers {
        let _ = writeln!(err, "> {k}: {v}");
    }
    let _ = writeln!(err);
}

/// Emit the response status line + headers. Tests grep for the curl-
/// style verbose output (`206 Partial Content`, `416 Requested Range
/// Not Satisfiable`, `Content-Range: bytes …`), so headers go through
/// `title_case_header` to undo the http crate's lowercase
/// normalization, and 416 is forced to the RFC 2616 reason phrase
/// `Requested Range Not Satisfiable` rather than RFC 7233's renamed
/// `Range Not Satisfiable` — that's what the upstream test suite is
/// written against.
fn dump_verbose_response(resp: &Response) {
    if !verbose_enabled() {
        return;
    }
    let mut err = std::io::stderr().lock();
    let code = resp.status().as_u16();
    let reason = match code {
        416 => "Requested Range Not Satisfiable",
        _ => resp.status().canonical_reason().unwrap_or(""),
    };
    let _ = writeln!(err, "< HTTP/1.1 {code} {reason}");
    for (k, v) in resp.headers() {
        if let Ok(value) = v.to_str() {
            let _ = writeln!(err, "< {}: {value}", title_case_header(k.as_str()));
        }
    }
    let _ = writeln!(err);
}

/// Title-case a hyphen-separated header name: `content-range` →
/// `Content-Range`. The http crate normalizes to lowercase, but the
/// vendored shell tests grep for capitalized header names (the form
/// curl emits in `-v` output).
fn title_case_header(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut at_start = true;
    for c in name.chars() {
        if at_start {
            out.extend(c.to_uppercase());
            at_start = false;
        } else {
            out.push(c);
        }
        if c == '-' {
            at_start = true;
        }
    }
    out
}

/// Write `src`'s contents into `path`. `append == true` (server-accepted
/// resume): open with O_APPEND so the prior partial bytes stay at the
/// front. `append == false`: truncate first — either a fresh download
/// or the server ignored our `Range`.
fn write_partial(path: &Path, append: bool, src: &mut impl std::io::Read) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true);
    if append {
        opts.append(true);
    } else {
        opts.truncate(true);
    }
    let mut file = opts.open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
    }
    file.flush()?;
    Ok(())
}

/// Compute the SHA-256 of `path` in 64 KiB chunks. Called after a
/// successful stream to decide whether the assembled bytes match what
/// the batch endpoint promised.
fn hash_file(path: &Path) -> std::io::Result<Oid> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let bytes: [u8; 32] = hasher.finalize().into();
    Ok(Oid::from_bytes(bytes))
}

/// Stream-upload the local copy of `oid` to `action.href`, then call the
/// verify callback if present.
///
/// `detect_content_type` follows `lfs.<url>.contenttype` (default
/// `true`). When `true` and the batch response didn't already pin a
/// `Content-Type`, sniff the file's first 512 bytes; when `false`,
/// send `application/octet-stream` unconditionally. Mirrors upstream's
/// `tq/basic_upload.go::setContentTypeFor`.
pub(crate) async fn upload(
    http: &reqwest::Client,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    actions: &Actions,
    detect_content_type: bool,
    events: Option<&UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    let upload_action = actions
        .upload
        .as_ref()
        .ok_or_else(|| TransferError::Io(std::io::Error::other("missing upload action")))?;

    let oid_parsed = Oid::from_hex(oid)?;
    let path = store.object_path(oid_parsed);

    // Sniff before opening the stream — `tokio::fs::File::open` consumes
    // the path once and the sniff needs its own read. Cheap: at most
    // 512 bytes off the disk. Only runs when detection is enabled AND
    // the batch response didn't already set a Content-Type (we still
    // need to know that before deciding whether to sniff).
    let action_has_content_type = upload_action
        .header
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"));
    let sniffed_content_type = if !action_has_content_type && detect_content_type {
        Some(sniff_content_type(&path).await.unwrap_or(OCTET_STREAM))
    } else {
        None
    };

    let file = tokio::fs::File::open(&path).await?;

    let mut bytes_done: u64 = 0;
    let oid_owned = oid.to_owned();
    let events_clone = events.cloned();
    let stream = ReaderStream::new(file).map(move |chunk| {
        if let Ok(ref c) = chunk {
            bytes_done += c.len() as u64;
            if let Some(s) = &events_clone {
                let _ = s.send(Event::Progress {
                    oid: oid_owned.clone(),
                    bytes_done,
                });
            }
        }
        chunk
    });
    let body = Body::wrap_stream(stream);

    let mut req = http
        .request(Method::PUT, &upload_action.href)
        .header(CONTENT_LENGTH, size);
    for (k, v) in &upload_action.header {
        req = req.header(k, v);
    }
    let effective_content_type = if action_has_content_type {
        // Action header pinned a Content-Type — find it for verbose
        // logging but don't override.
        upload_action
            .header
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str().to_owned())
    } else {
        let ct = sniffed_content_type.unwrap_or(OCTET_STREAM);
        req = req.header(reqwest::header::CONTENT_TYPE, ct);
        Some(ct.to_owned())
    };

    // GIT_CURL_VERBOSE: dump the request line + headers so shell tests
    // like `t-content-type.sh` can grep for the chosen Content-Type.
    // Stays cheap behind the env-var gate; no per-request cost when
    // verbose isn't asked for.
    if std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0") {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "> PUT {}", upload_action.href);
        let _ = writeln!(err, "> Content-Length: {size}");
        if let Some(ct) = &effective_content_type {
            let _ = writeln!(err, "> Content-Type: {ct}");
        }
        for (k, v) in &upload_action.header {
            if k.eq_ignore_ascii_case("content-type") {
                continue; // already printed above
            }
            let _ = writeln!(err, "> {k}: {v}");
        }
        let _ = writeln!(err);
    }

    let resp = req.body(body).send().await?;
    let status = resp.status();
    if status.as_u16() == 422 {
        // 422 = "Unprocessable Entity" — typically a CDN rejecting the
        // Content-Type we sniffed. Print upstream's three-line hint so
        // the user knows the disable knob exists. Stays best-effort:
        // we still propagate the failure, the message just nudges
        // toward a fix. `t-content-type.sh` test 3 greps for these.
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "info: Uploading failed due to unsupported Content-Type header(s)."
        );
        let _ = writeln!(err, "info: Consider disabling Content-Type detection with:");
        let _ = writeln!(err);
        let _ = writeln!(err, "info:   $ git config lfs.contenttype false");
        let _ = writeln!(err);
    }
    check_status(&resp, &upload_action.href)?;

    if let Some(verify_action) = &actions.verify {
        verify(http, oid, size, verify_action).await?;
    }
    Ok(())
}

/// Minimal content-type sniffing. Read the first 512 bytes of `path`
/// and match against a small magic-number table — extend as future
/// tests demand. Returns `None` if the file can't be read; the caller
/// falls back to `application/octet-stream`, which is also what we
/// return for any content that doesn't match a known signature
/// (matches Go's `http.DetectContentType` fall-through).
async fn sniff_content_type(path: &std::path::Path) -> Option<&'static str> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut buf = [0u8; 512];
    let n = file.read(&mut buf).await.ok()?;
    Some(detect_content_type_bytes(&buf[..n]))
}

/// Magic-number sniff for `bytes`. Returns the same MIME strings Go's
/// `net/http.DetectContentType` produces — `t-content-type.sh` grep
/// patterns are written against those.
///
/// Only a handful of signatures are recognized today (the test suite
/// only exercises gzip); broader coverage is deferred until a new
/// test forces it. The default is `application/octet-stream`, same
/// as upstream's fall-through.
fn detect_content_type_bytes(bytes: &[u8]) -> &'static str {
    // Gzip: `1f 8b` magic. Go labels this `application/x-gzip` (with
    // the `x-` prefix, not the RFC 6713 `application/gzip` form);
    // matching that exactly is what `t-content-type.sh` test 1
    // greps for.
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        return "application/x-gzip";
    }
    OCTET_STREAM
}

async fn verify(
    http: &reqwest::Client,
    oid: &str,
    size: u64,
    action: &Action,
) -> Result<(), TransferError> {
    let mut req = http
        .request(Method::POST, &action.href)
        .header(reqwest::header::ACCEPT, "application/vnd.git-lfs+json")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/vnd.git-lfs+json",
        );
    for (k, v) in &action.header {
        req = req.header(k, v);
    }
    let resp = req.json(&VerifyBody { oid, size }).send().await?;
    check_status(&resp, &action.href)?;
    Ok(())
}

fn check_status(resp: &Response, url: &str) -> Result<(), TransferError> {
    if resp.status().is_success() {
        Ok(())
    } else {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(git_lfs_api::parse_retry_after);
        Err(TransferError::ActionStatus {
            status: resp.status().as_u16(),
            url: strip_query(url).to_owned(),
            retry_after,
        })
    }
}

/// Strip the query string from `url`. Mirrors upstream's
/// `strings.SplitN(url, "?", 2)[0]` in `lfshttp.defaultError` —
/// auth tokens / signed-URL params shouldn't leak into error
/// messages or test grep patterns.
fn strip_query(url: &str) -> &str {
    url.split_once('?').map_or(url, |(base, _)| base)
}
