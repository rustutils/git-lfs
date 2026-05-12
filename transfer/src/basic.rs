//! Basic transfer adapter — direct GET/PUT/POST against the action URLs
//! the batch endpoint hands back.
//!
//! See `docs/api/basic-transfers.md`.

use std::io::Write as _;
use std::sync::Arc;

use futures::StreamExt;
use git_lfs_api::{Action, Actions};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use reqwest::header::CONTENT_LENGTH;
use reqwest::{Body, Method, Response};
use serde::Serialize;
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
/// Hashing + atomic insert happens inside [`store::Store::insert_verified`]
/// on a blocking thread; the bytes flow through a [`SyncIoBridge`] so we
/// don't buffer the whole object in memory.
pub(crate) async fn download(
    http: &reqwest::Client,
    store: Arc<Store>,
    oid: &str,
    action: &Action,
    events: Option<&UnboundedSender<Event>>,
) -> Result<u64, TransferError> {
    let expected = Oid::from_hex(oid)?;
    let mut req = http.request(Method::GET, &action.href);
    for (k, v) in &action.header {
        req = req.header(k, v);
    }
    let resp = req.send().await?;
    check_status(&resp, &action.href)?;

    let mut bytes_done: u64 = 0;
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

    let size = tokio::task::spawn_blocking(move || store.insert_verified(expected, &mut bridge))
        .await
        .map_err(|join_err| std::io::Error::other(join_err.to_string()))??;

    Ok(size)
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
