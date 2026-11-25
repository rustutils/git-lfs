//! `/storage/<oid>`: object uploads and downloads, including tus.

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use sha2::{Digest, Sha256};

use crate::http::{Body, Request, Resp, add_header, lfs_error, respond, status};
use crate::lfs::{missing_required_creds, record_header_redirect};
use crate::state::{State, hex, resource_key};

pub(crate) fn handle(state: &State, req: &Request) -> Resp {
    let repo = req.query_param("r");
    if let Some(resp) = record_header_redirect(state, req, &repo, "object-target") {
        return resp;
    }
    let oid = req.path.rsplit('/').next().unwrap_or("").to_owned();
    if let Some(resp) = missing_required_creds(req, &repo) {
        return resp;
    }
    match req.method.as_str() {
        "PUT" => put(state, req, &repo, &oid),
        "GET" => get(state, req, &repo, &oid),
        "HEAD" => tus_head(state, req, &repo, &oid),
        "PATCH" => tus_patch(state, req, &repo, &oid),
        _ => status(405),
    }
}

fn put(state: &State, req: &Request, repo: &str, oid: &str) -> Resp {
    match state.handler(oid) {
        "status-storage-403" => return status(403),
        "status-storage-404" => return status(404),
        "status-storage-410" => return status(410),
        "status-storage-422" => return status(422),
        "status-storage-500" => return status(500),
        "status-storage-503" => return lfs_error(503, "LFS is temporarily unavailable"),
        "object-authenticated" => {
            // The object isn't stored either way.
            if !req.header("Authorization").is_empty() {
                return respond(400, &[], "Should not send authentication");
            }
            return status(200);
        }
        "storage-upload-retry"
            if state
                .increment_retries("storage", "upload", repo, oid)
                .is_some_and(|n| n < 3) =>
        {
            return respond(500, &[], "malformed content");
        }
        "storage-upload-retry-later" => {
            if let Some(wait) =
                state.check_rate_limit(&resource_key("storage", "upload", repo, oid))
            {
                let wait = wait.to_string();
                return respond(429, &[("Retry-After", wait.as_str())], "rate limit reached");
            }
        }
        "storage-upload-retry-later-no-header"
            if state
                .check_rate_limit(&resource_key("storage", "upload", repo, oid))
                .is_some() =>
        {
            return respond(429, &[], "rate limit reached");
        }
        _ => {}
    }

    if repo == "batch-storage-upload-encoding-chunked"
        && !req
            .header("Transfer-Encoding")
            .split(',')
            .any(|e| e.trim().eq_ignore_ascii_case("chunked"))
    {
        return respond(400, &[], "Chunked transfer encoding required");
    }

    let actual = hex(&Sha256::digest(&req.body));
    if !req.path.ends_with(&format!("/{actual}")) {
        return status(403);
    }
    state
        .objects
        .lock()
        .unwrap()
        .set(repo, &actual, req.body.to_vec());
    status(200)
}

fn get(state: &State, req: &Request, repo: &str, oid: &str) -> Resp {
    let Some(mut data) = state.objects.lock().unwrap().get(repo, oid).cloned() else {
        return status(404);
    };
    let handler = state.handler(oid);
    let mut code = 200;
    let mut headers: Vec<(&str, String)> = Vec::new();
    // Send only this many bytes while claiming the full length, to
    // simulate an interrupted response.
    let mut byte_limit = 0;
    let mut gzip = false;
    let mut zstd = false;
    let accepts_zstd = accepts_zstd(req.header("Accept-Encoding"));

    match handler {
        "storage-download-corrupt" => {
            // The contents are lowercase, so this changes every letter.
            data = data.to_ascii_uppercase();
        }
        "storage-download-retry-later" => {
            if let Some(wait) =
                state.check_rate_limit(&resource_key("storage", "download", repo, oid))
            {
                code = 429;
                headers.push(("Retry-After", wait.to_string()));
                data = b"rate limit reached".to_vec();
            }
        }
        "storage-download-retry-later-no-header" => {
            if state
                .check_rate_limit(&resource_key("storage", "download", repo, oid))
                .is_some()
            {
                code = 429;
                data = b"rate limit reached".to_vec();
            }
        }
        "storage-download-retry" => {
            if state
                .increment_retries("storage", "download", repo, oid)
                .is_some_and(|n| n < 3)
            {
                code = 500;
                data = b"malformed content".to_vec();
            }
        }
        "storage-download-retry-range" => {
            if let Some(resp) = range_response(req, &data) {
                return resp;
            }
            byte_limit = handler.len() / 2;
        }
        "storage-download-retry-range-rejected" => {
            // Reject any range, so the client has to start over.
            if !req.header("Range").is_empty() {
                return status(416);
            }
            let mut attempted = state.range_rejected_attempted.lock().unwrap();
            if !*attempted {
                byte_limit = handler.len() / 2;
                *attempted = true;
            }
        }
        "storage-download-retry-no-invalid-range" => {
            if let Some(resp) = range_response(req, &data) {
                return resp;
            }
        }
        "storage-download-encoding-gzip" => {
            if req.header("Accept-Encoding") == "gzip" {
                gzip = true;
            } else {
                code = 500;
                data = b"not encoded".to_vec();
            }
        }
        "storage-download-encoding-zstd"
        | "storage-download-encoding-zstd-1"
        | "storage-download-encoding-zstd-2"
        | "storage-download-encoding-zstd-3" => {
            if accepts_zstd {
                zstd = true;
            } else {
                code = 500;
                data = b"not encoded".to_vec();
            }
        }
        "storage-download-encoding-zstd-retry-range" => {
            if let Some(resp) = range_response(req, &data) {
                return resp;
            }
            if accepts_zstd {
                zstd = true;
                // An offset into the second frame, so the client decodes
                // the whole first frame before the response stops.
                byte_limit = 6;
            } else {
                code = 500;
                data = b"not encoded".to_vec();
            }
        }
        _ => {}
    }

    if gzip {
        headers.push(("Content-Encoding", "gzip".to_owned()));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&data).expect("writing to a Vec");
        data = encoder.finish().expect("writing to a Vec");
    } else if zstd {
        headers.push(("Content-Encoding", "zstd".to_owned()));
        // Two frames, so an interrupted response can end after a whole
        // first frame.
        let mid = data.len() / 2;
        let first = compress_to_vec(&data[..mid], CompressionLevel::Fastest);
        let second = compress_to_vec(&data[mid..], CompressionLevel::Fastest);
        if byte_limit > 0 {
            byte_limit += first.len();
        }
        data = [first, second].concat();
    }

    let full_length = data.len().to_string();
    let cut_short = byte_limit > 0 && byte_limit < data.len();
    let mut resp = if cut_short {
        data.truncate(byte_limit);
        let mut resp = respond(code, &[("Content-Length", full_length.as_str())], "");
        *resp.body_mut() = Body::cut_short(data.into());
        resp
    } else {
        respond(code, &[], data)
    };
    for (name, value) in &headers {
        add_header(&mut resp, name, value);
    }
    resp
}

/// Whether an `Accept-Encoding` value lists `zstd`, in any case.
fn accepts_zstd(value: &str) -> bool {
    value
        .split(',')
        .any(|e| e.trim().eq_ignore_ascii_case("zstd"))
}

/// Answer a `Range: bytes=<start>-[<end>]` request with the data from
/// `start`, ignoring any end. `None` without a `Range` header.
///
/// The client never sends `Range` with `Accept-Encoding`, so that's
/// rejected, as is a malformed range.
fn range_response(req: &Request, data: &[u8]) -> Option<Resp> {
    let range = req.header("Range");
    if range.is_empty() {
        return None;
    }
    if !req.header("Accept-Encoding").is_empty() {
        return Some(status(400));
    }
    let Some((start, end)) = range
        .split_once("bytes=")
        .and_then(|(_, spec)| spec.split_once('-'))
    else {
        return Some(status(400));
    };
    let Ok(start) = start.parse::<usize>() else {
        return Some(status(400));
    };
    if !end.is_empty() && end.parse::<usize>().map_or(true, |end| end < start) {
        return Some(status(400));
    }
    if start > data.len() {
        return Some(status(416));
    }
    let content_range = format!(
        "bytes {start}-{}/{}",
        data.len().saturating_sub(1),
        data.len()
    );
    Some(respond(
        206,
        &[("Content-Range", content_range.as_str())],
        data[start..].to_vec(),
    ))
}

/// tus: report how much of an upload the server has.
fn tus_head(state: &State, req: &Request, repo: &str, oid: &str) -> Resp {
    if req.header("Tus-Resumable").is_empty() {
        return status(400);
    }
    let offset = state
        .objects
        .lock()
        .unwrap()
        .get_incomplete(repo, oid)
        .map_or(0, Vec::len)
        .to_string();
    respond(200, &[("Upload-Offset", offset.as_str())], "")
}

/// tus: receive an upload, or the rest of one.
///
/// With `Lfs-Tus-Interrupt: true`, an upload from offset 0 keeps only the
/// first third of the body and fails, so the client has to resume.
fn tus_patch(state: &State, req: &Request, repo: &str, oid: &str) -> Resp {
    if req.header("Tus-Resumable").is_empty() {
        return status(400);
    }
    let Ok(offset) = req.header("Upload-Offset").parse::<usize>() else {
        return status(400);
    };
    let mut objects = state.objects.lock().unwrap();
    let mut data = Vec::new();
    if let Some(partial) = objects.get_incomplete(repo, oid) {
        if offset != partial.len() {
            return status(400);
        }
        data.extend_from_slice(partial);
        objects.delete_incomplete(repo, oid);
    }

    if req.header("Lfs-Tus-Interrupt") == "true" && offset == 0 {
        let Ok(length) = req.header("Content-Length").parse::<usize>() else {
            return status(400);
        };
        let keep = (length / 3).min(req.body.len());
        data.extend_from_slice(&req.body[..keep]);
        if !data.is_empty() {
            objects.set_incomplete(repo, oid, data);
        }
        return status(500);
    }

    data.extend_from_slice(&req.body);
    if hex(&Sha256::digest(&data)) != oid {
        return status(403);
    }
    let length = data.len().to_string();
    objects.set(repo, oid, data);
    respond(204, &[("Upload-Offset", length.as_str())], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_is_found_in_a_list() {
        assert!(accepts_zstd("gzip, ZSTD"));
        assert!(accepts_zstd("zstd"));
        assert!(!accepts_zstd("gzip"));
        assert!(!accepts_zstd("zstdx"));
    }
}
