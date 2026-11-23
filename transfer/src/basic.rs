//! The basic HTTP transfer adapter, which uses the action URLs from the
//! batch response directly.
//!
//! See `docs/api/basic-transfers.md`.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;

use futures::StreamExt;
use git_lfs_api::{
    Action, Actions, ApiError, is_credential_header, send_with_redirects, trace_request,
    trace_response, verbose_header_line,
};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use reqwest::header::{ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, RANGE};
use reqwest::{Body, Method, Response};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::io::{ReaderStream, StreamReader, SyncIoBridge};

use crate::action_auth::{ActionAuth, RequestAuth};
use crate::config::{HeaderSource, UrlSetting};
use crate::error::TransferError;
use crate::event::Event;
use crate::http_clients::HttpClients;

/// Upload `Content-Type` when detection is off or finds nothing.
const OCTET_STREAM: &str = "application/octet-stream";

#[derive(Debug, Serialize)]
struct VerifyBody<'a> {
    oid: &'a str,
    size: u64,
}

/// Stream-download `oid` from `action.href` into `store`.
///
/// Bytes go to the store's `incomplete/<oid>.part` file, which moves into
/// the store only once it hashes to `oid`. A partial file left by an
/// earlier attempt is resumed with a `Range` request, as in upstream's
/// `tq/basic_download.go`:
///
/// - 206 appends to the partial file.
/// - 200 means the server ignored the range, so the file is rewritten.
/// - 416 deletes the partial file and downloads again from the start.
///
/// A download from the start asks for `encoding`'s choice of gzip or zstd,
/// and the body is decompressed as it's written. A resumed one asks for
/// no encoding, since its range is of the decompressed object. A zstd body
/// reuses the decoder in `zstd_slot`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download(
    clients: &HttpClients,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    action: &Action,
    extra_headers: Option<&HeaderSource>,
    encoding: Option<&UrlSetting>,
    zstd_slot: Option<&ZstdSlot>,
    auth: Option<&Arc<dyn ActionAuth>>,
    events: Option<&UnboundedSender<Event>>,
) -> Result<u64, TransferError> {
    let expected = Oid::from_hex(oid)?;
    let partial_path = store.incomplete_path(expected);

    // A partial file at least `size` long can't be resumed: it may be
    // corrupt, and `bytes=<size>-<size-1>` is not a valid range.
    if let Ok(m) = std::fs::metadata(&partial_path)
        && m.len() >= size
    {
        let _ = std::fs::remove_file(&partial_path);
    }
    let resume_offset: Option<u64> = std::fs::metadata(&partial_path)
        .ok()
        .map(|m| m.len())
        .filter(|&n| n > 0 && n < size);

    let http = &clients.for_url(&action.href);
    let mut req = http.request(Method::GET, &action.href);
    let extra = extra_for(extra_headers, &action.href);
    for (k, v) in action
        .header
        .iter()
        .chain(extra.iter().map(|(k, v)| (k, v)))
    {
        req = req.header(k, v);
    }
    let range_header = resume_offset.map(|offset| {
        // The end byte is inclusive.
        format!("bytes={offset}-{}", size.saturating_sub(1))
    });
    // Go's client asks for gzip unless there's a range, and upstream asks
    // for zstd instead when configured.
    let accept_encoding = match (&range_header, encoding.and_then(|e| e(&action.href))) {
        (Some(_), _) => None,
        (None, None) => Some("gzip"),
        (None, Some(v)) => match v.as_str() {
            "gzip" => Some("gzip"),
            "zstd" => Some("zstd"),
            _ => return Err(TransferError::UnsupportedEncoding(v)),
        },
    };
    if let Some(e) = accept_encoding {
        req = req.header(ACCEPT_ENCODING, e);
    }
    if let Some(range) = &range_header {
        req = req.header(RANGE, range);
        if trace_enabled() {
            eprintln!(
                "xfer: Attempting to resume download of {oid:?} from byte {}",
                resume_offset.unwrap()
            );
        }
    }
    dump_verbose_request(
        &action.href,
        range_header.as_deref(),
        accept_encoding,
        &action.header,
    );

    let mut request_auth =
        RequestAuth::start(auth_for(auth, &action.header, &extra), &action.href, oid).await;
    let resp = loop {
        let mut attempt = req.try_clone().expect("a GET has no body to clone");
        if let Some(h) = request_auth.header() {
            attempt = attempt.header(AUTHORIZATION, h);
        }
        let resp = send_with_redirects(http, attempt.build()?, 0)
            .await
            .map_err(from_api)?;
        if request_auth.retry_after(resp.status().as_u16()).await {
            continue;
        }
        break resp;
    };
    let content_encoding = resp
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase());
    let gzipped = content_encoding.as_deref() == Some("gzip");
    let zstd = content_encoding.as_deref() == Some("zstd");
    if gzipped && trace_enabled() {
        eprintln!("http: decompressed gzipped response");
    }
    dump_verbose_response(&resp, gzipped);

    // The server rejected the range. Without the partial file, the
    // second call sends no range, so this recurses at most once.
    if let Some(offset) = resume_offset
        && resp.status().as_u16() == 416
    {
        if trace_enabled() {
            eprintln!(
                "xfer: server rejected resume download request for {oid:?} from byte {offset}; re-downloading from start"
            );
        }
        let _ = std::fs::remove_file(&partial_path);
        return Box::pin(download(
            clients,
            store,
            oid,
            size,
            action,
            extra_headers,
            encoding,
            zstd_slot,
            auth,
            events,
        ))
        .await;
    }

    check_status(&resp, &action.href)?;

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
    let bridge = SyncIoBridge::new(async_reader);
    if zstd && trace_enabled() {
        eprintln!("http: decompressing zstd-encoded response");
    }

    let partial_path_owned = partial_path.clone();
    let store_for_blocking = store.clone();
    let zstd_slot = zstd_slot.cloned();
    let total = tokio::task::spawn_blocking(move || -> Result<u64, TransferError> {
        store_for_blocking.prepare_incomplete_dir()?;
        if gzipped {
            let mut body = flate2::read::GzDecoder::new(bridge);
            write_partial(&partial_path_owned, server_resumed, &mut body)?;
        } else if zstd {
            let reused = zstd_slot.as_ref().and_then(|s| s.lock().unwrap().take());
            let frame = reused.unwrap_or_else(|| {
                if trace_enabled() {
                    eprintln!("http: initialized zstd decoder");
                }
                FrameDecoder::new()
            });
            let mut body = ZstdFrames::new(bridge, frame)?;
            let written = write_partial(&partial_path_owned, server_resumed, &mut body);
            if let (Some(slot), Some(frame)) = (&zstd_slot, body.into_decoder()) {
                *slot.lock().unwrap() = Some(frame);
            }
            written?;
        } else {
            let mut body = bridge;
            write_partial(&partial_path_owned, server_resumed, &mut body)?;
        }
        let actual = hash_file(&partial_path_owned)?;
        let total = std::fs::metadata(&partial_path_owned)?.len();
        if actual != expected {
            return Err(TransferError::Corrupt {
                expected,
                actual,
                written: total,
            });
        }
        store_for_blocking.commit_partial(expected, &partial_path_owned)?;
        Ok(total)
    })
    .await
    .map_err(|join_err| std::io::Error::other(join_err.to_string()))??;

    Ok(total)
}

type FrameDecoder = ruzstd::decoding::FrameDecoder;
type Streaming<R> = ruzstd::decoding::StreamingDecoder<std::io::BufReader<R>, FrameDecoder>;

/// A zstd decoder kept for reuse by one concurrent download slot, like
/// the one each of upstream's transfer workers keeps.
///
/// The first zstd download in a slot traces `http: initialized zstd
/// decoder`, and [`close_zstd_slots`] traces `http: closed zstd decoder`
/// for each slot that made one.
pub(crate) type ZstdSlot = Arc<std::sync::Mutex<Option<FrameDecoder>>>;

/// Trace the closing of each slot's decoder, at the end of a batch.
pub(crate) fn close_zstd_slots(slots: &[ZstdSlot]) {
    for slot in slots {
        if slot.lock().unwrap().take().is_some() && trace_enabled() {
            eprintln!("http: closed zstd decoder");
        }
    }
}

/// Decodes a zstd stream of one or more frames, as a server may send.
///
/// `StreamingDecoder` stops at the end of a frame, so the next frame is
/// started on the same input, with the same decoder, whenever input
/// remains.
struct ZstdFrames<R: std::io::Read> {
    /// `None` after a failure.
    decoder: Option<Streaming<R>>,
}

impl<R: std::io::Read> ZstdFrames<R> {
    fn new(inner: R, decoder: FrameDecoder) -> std::io::Result<Self> {
        let decoder = Streaming::new_with_decoder(std::io::BufReader::new(inner), decoder)
            .map_err(zstd_error)?;
        Ok(Self {
            decoder: Some(decoder),
        })
    }

    /// The decoder, for reuse, if decoding didn't fail.
    fn into_decoder(self) -> Option<FrameDecoder> {
        self.decoder.map(|d| d.into_frame_decoder())
    }
}

impl<R: std::io::Read> std::io::Read for ZstdFrames<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let decoder = self
                .decoder
                .as_mut()
                .ok_or_else(|| zstd_error("decoder failed earlier"))?;
            let n = decoder.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            // The frame is done. Start the next, if there is one.
            if std::io::BufRead::fill_buf(decoder.get_mut())?.is_empty() {
                return Ok(0);
            }
            let (source, frame) = self.decoder.take().expect("checked above").into_parts();
            self.decoder = Some(Streaming::new_with_decoder(source, frame).map_err(zstd_error)?);
        }
    }
}

fn zstd_error(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("zstd: {e}"))
}

/// Whether `GIT_TRACE` is set to something other than empty or `0`.
pub(crate) fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Whether `GIT_CURL_VERBOSE` is set to something other than empty or `0`.
pub(crate) fn verbose_enabled() -> bool {
    std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Log a download request in curl's verbose format.
///
/// Action headers are printed as they are, credentials included.
fn dump_verbose_request(
    url: &str,
    range: Option<&str>,
    accept_encoding: Option<&str>,
    extra_headers: &std::collections::HashMap<String, String>,
) {
    if !verbose_enabled() {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "> GET {url}");
    if let Some(e) = accept_encoding {
        let _ = writeln!(err, "> Accept-Encoding: {e}");
    }
    if let Some(r) = range {
        let _ = writeln!(err, "> Range: {r}");
    }
    for (k, v) in extra_headers {
        let _ = writeln!(err, "> {}", verbose_header_line(k, v));
    }
    let _ = writeln!(err);
}

/// Log a download response in curl's verbose format.
///
/// Header names are title-cased and 416 uses its older reason phrase,
/// `Requested Range Not Satisfiable`, because the upstream tests match
/// curl's output. For a `gzipped` response, `Content-Encoding` and
/// `Content-Length` are left out, as Go's client drops them when it
/// decompresses.
pub(crate) fn dump_verbose_response(resp: &Response, gzipped: bool) {
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
        if gzipped && (k == CONTENT_ENCODING || k == CONTENT_LENGTH) {
            continue;
        }
        if let Ok(value) = v.to_str() {
            let _ = writeln!(err, "< {}: {value}", title_case_header(k.as_str()));
        }
    }
    let _ = writeln!(err);
}

/// Title-case a header name, such as `content-range` to `Content-Range`.
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

/// Copy `src` into `path`, appending or replacing the existing content.
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

/// Compute the SHA-256 of the file at `path`.
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

/// Stream the stored copy of `oid` to the upload action, then call the
/// verify action if there is one.
///
/// [`TransferConfig::detect_content_type`](crate::TransferConfig::detect_content_type)
/// says how the `Content-Type` is chosen.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload(
    clients: &HttpClients,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    actions: &Actions,
    detect_content_type: bool,
    max_verifies: u32,
    extra_headers: Option<&HeaderSource>,
    auth: Option<&Arc<dyn ActionAuth>>,
    events: Option<&UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    let upload_action = actions
        .upload
        .as_ref()
        .ok_or_else(|| TransferError::Io(std::io::Error::other("missing upload action")))?;

    let oid_parsed = Oid::from_hex(oid)?;
    let path = store.object_path(oid_parsed);

    let action_has_content_type = upload_action
        .header
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"));
    let sniffed_content_type = if !action_has_content_type && detect_content_type {
        Some(sniff_content_type(&path).await.unwrap_or(OCTET_STREAM))
    } else {
        None
    };

    let effective_content_type = if action_has_content_type {
        upload_action
            .header
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str().to_owned())
    } else {
        Some(sniffed_content_type.unwrap_or(OCTET_STREAM).to_owned())
    };

    // An action can ask for a chunked upload, which has no `Content-Length`.
    // hyper drops it anyway, but the verbose trace should match.
    let chunked = upload_action
        .header
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v == "chunked");

    // reqwest can't follow redirects for a streamed body, so follow them
    // here, reopening the file for each request. All action headers go
    // to the new URL, even on another host.
    let mut href = upload_action.href.clone();
    let mut verbose_logged = false;
    let mut redirects = 0u32;
    // Cleared once a redirect leaves the original origin.
    let mut send_credentials = true;
    // Looked up for the action URL and kept across redirects, like upstream.
    let extra = extra_for(extra_headers, &upload_action.href);
    let mut request_auth = RequestAuth::start(
        auth_for(auth, &upload_action.header, &extra),
        &upload_action.href,
        oid,
    )
    .await;
    let resp = loop {
        let file = tokio::fs::File::open(&path).await?;
        let oid_owned = oid.to_owned();
        let events_clone = events.cloned();
        let mut bytes_done: u64 = 0;
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

        // A redirect to another host uses that host's settings.
        let http = clients.for_url(&href);
        let mut req = http.request(Method::PUT, &href);
        if !chunked {
            req = req.header(CONTENT_LENGTH, size);
        }
        for (k, v) in upload_action
            .header
            .iter()
            .chain(extra.iter().map(|(k, v)| (k, v)))
        {
            if !send_credentials && is_credential_header(k) {
                continue;
            }
            req = req.header(k, v);
        }
        if !action_has_content_type {
            req = req.header(
                reqwest::header::CONTENT_TYPE,
                sniffed_content_type.unwrap_or(OCTET_STREAM),
            );
        }
        if send_credentials && let Some(h) = request_auth.header() {
            req = req.header(AUTHORIZATION, h);
        }

        // Log only the first request, not the redirected ones.
        if !verbose_logged && verbose_enabled() {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "> PUT {href}");
            if !chunked {
                let _ = writeln!(err, "> Content-Length: {size}");
            }
            if let Some(ct) = &effective_content_type {
                let _ = writeln!(err, "> Content-Type: {ct}");
            }
            for (k, v) in &upload_action.header {
                if k.eq_ignore_ascii_case("content-type") {
                    continue;
                }
                let _ = writeln!(err, "> {}", verbose_header_line(k, v));
            }
            let _ = writeln!(err);
            verbose_logged = true;
        }

        let request = req.body(body).build()?;
        trace_request(&request);
        let resp = http.execute(request).await?;
        trace_response(&resp);
        if redirects < 10
            && let Some(new_href) = follow_redirect(&resp, &href)
        {
            if !redirect_keeps_credentials(&href, &new_href)? {
                send_credentials = false;
            }
            redirects += 1;
            href = new_href;
            continue;
        }
        if send_credentials && request_auth.retry_after(resp.status().as_u16()).await {
            continue;
        }
        break resp;
    };

    let status = resp.status();
    if status.as_u16() == 422 {
        // Usually a server rejecting the detected Content-Type. Print
        // upstream's hint on how to turn detection off.
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
        verify(
            clients,
            oid,
            size,
            verify_action,
            max_verifies,
            extra_headers,
            auth,
        )
        .await?;
    }
    Ok(())
}

/// The URL `resp` redirects to, if it's a redirect with a valid
/// `Location`.
///
/// Logs `api: redirect PUT <old> to <new>` under `GIT_TRACE`.
fn follow_redirect(resp: &Response, current: &str) -> Option<String> {
    if !resp.status().is_redirection() {
        return None;
    }
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    let base = reqwest::Url::parse(current).ok()?;
    let new = base.join(location).ok()?;
    let new = new.to_string();
    if trace_enabled() {
        let old_no_q = current.split_once('?').map_or(current, |(p, _)| p);
        let new_no_q = new.split_once('?').map_or(new.as_str(), |(p, _)| p);
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "api: redirect PUT {old_no_q} to {new_no_q}");
    }
    Some(new)
}

/// Check a redirect, like upstream's `newRequestForRetry`.
///
/// Fails on a redirect from HTTPS to HTTP. Otherwise returns whether
/// credentials may still be sent, which is only when the scheme, host, and
/// port stay the same.
fn redirect_keeps_credentials(from: &str, to: &str) -> Result<bool, TransferError> {
    let (Ok(from), Ok(to)) = (reqwest::Url::parse(from), reqwest::Url::parse(to)) else {
        return Ok(false);
    };
    git_lfs_api::redirect_keeps_credentials(&from, &to).map_err(|_| TransferError::InsecureRedirect)
}

/// Convert an error from [`send_with_redirects`].
pub(crate) fn from_api(err: ApiError) -> TransferError {
    match err {
        ApiError::Transport(e) => TransferError::Http(e),
        ApiError::Redirect(message) => TransferError::Redirect(message),
        other => TransferError::Io(std::io::Error::other(other.to_string())),
    }
}

/// The extra headers for a request to `url`.
pub(crate) fn extra_for(source: Option<&HeaderSource>, url: &str) -> Vec<(String, String)> {
    source.map(|source| source(url)).unwrap_or_default()
}

/// Detect the content type from the first 512 bytes of `path`.
///
/// Returns `None` if the file can't be read.
async fn sniff_content_type(path: &std::path::Path) -> Option<&'static str> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut buf = [0u8; 512];
    let n = file.read(&mut buf).await.ok()?;
    Some(detect_content_type_bytes(&buf[..n]))
}

/// Detect the content type of `bytes`, using Go's
/// `http.DetectContentType` names.
///
/// Only gzip is recognized. Everything else is `application/octet-stream`.
fn detect_content_type_bytes(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        return "application/x-gzip";
    }
    OCTET_STREAM
}

pub(crate) async fn verify(
    clients: &HttpClients,
    oid: &str,
    size: u64,
    action: &Action,
    max_attempts: u32,
    extra_headers: Option<&HeaderSource>,
    auth: Option<&Arc<dyn ActionAuth>>,
) -> Result<(), TransferError> {
    let short = oid.get(..7).unwrap_or(oid);
    let http = &clients.for_url(&action.href);
    let extra = extra_for(extra_headers, &action.href);
    let mut request_auth =
        RequestAuth::start(auth_for(auth, &action.header, &extra), &action.href, oid).await;
    let mut last_err: Option<TransferError> = None;
    for attempt in 1..=max_attempts {
        if trace_enabled() {
            eprintln!("tq: verify {short} attempt #{attempt} (max: {max_attempts})");
        }
        let sent = loop {
            let sent = send_verify(http, oid, size, action, &extra, request_auth.header()).await;
            if let Ok(resp) = &sent
                && request_auth.retry_after(resp.status().as_u16()).await
            {
                continue;
            }
            break sent;
        };
        match sent {
            Ok(resp) => match check_status(&resp, &action.href) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if trace_enabled() {
                        eprintln!("tq: verify err: {e}");
                    }
                    last_err = Some(e);
                }
            },
            Err(err) => {
                if trace_enabled() {
                    eprintln!("tq: verify err: {err}");
                }
                last_err = Some(err);
            }
        }
    }
    let last = last_err
        .unwrap_or_else(|| TransferError::Io(std::io::Error::other("verify exhausted retries")));
    Err(TransferError::VerifyExhausted(Box::new(last)))
}

/// Send one verify request.
async fn send_verify(
    http: &reqwest::Client,
    oid: &str,
    size: u64,
    action: &Action,
    extra: &[(String, String)],
    authorization: Option<&str>,
) -> Result<Response, TransferError> {
    {
        let mut req = http
            .request(Method::POST, &action.href)
            .header(reqwest::header::ACCEPT, "application/vnd.git-lfs+json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/vnd.git-lfs+json",
            );
        for (k, v) in action
            .header
            .iter()
            .chain(extra.iter().map(|(k, v)| (k, v)))
        {
            req = req.header(k, v);
        }
        if let Some(h) = authorization {
            req = req.header(AUTHORIZATION, h);
        }
        match req.json(&VerifyBody { oid, size }).build() {
            Ok(request) => send_with_redirects(http, request, 0)
                .await
                .map_err(from_api),
            Err(e) => Err(TransferError::from(e)),
        }
    }
}

/// `auth`, unless the action or `http.extraHeader` already sets an
/// `Authorization` header, as upstream's `requestHasAuth` checks.
pub(crate) fn auth_for<'a>(
    auth: Option<&'a Arc<dyn ActionAuth>>,
    action_headers: &std::collections::HashMap<String, String>,
    extra: &[(String, String)],
) -> Option<&'a Arc<dyn ActionAuth>> {
    let has_own = action_headers
        .keys()
        .chain(extra.iter().map(|(k, _)| k))
        .any(|k| k.eq_ignore_ascii_case("authorization"));
    if has_own { None } else { auth }
}

pub(crate) fn check_status(resp: &Response, url: &str) -> Result<(), TransferError> {
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

/// Strip the query string from `url`, so signed URL parameters stay out
/// of error messages.
fn strip_query(url: &str) -> &str {
    url.split_once('?').map_or(url, |(base, _)| base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_on_the_same_origin_keeps_credentials() {
        let keep = redirect_keeps_credentials("https://a.test/x", "https://a.test/y?z");
        assert!(keep.unwrap());
    }

    #[test]
    fn redirect_to_another_origin_drops_credentials() {
        for (from, to) in [
            ("https://a.test/x", "https://b.test/x"),
            ("https://a.test/x", "https://a.test:8443/x"),
            // Upgrading to HTTPS still changes the origin, as upstream treats it.
            ("http://a.test/x", "https://a.test/x"),
        ] {
            let keep = redirect_keeps_credentials(from, to).unwrap();
            assert!(!keep, "{from} to {to}");
        }
    }

    #[test]
    fn redirect_from_https_to_http_is_refused() {
        let err = redirect_keeps_credentials("https://a.test/x", "http://a.test/x");
        assert!(matches!(err, Err(TransferError::InsecureRedirect)));
    }
}
