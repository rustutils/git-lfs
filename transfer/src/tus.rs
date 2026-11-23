//! Resumable uploads with the [tus.io] protocol, as upstream's `tus`
//! adapter does them.
//!
//! A `HEAD` request to the upload action asks how much the server has,
//! from its `Upload-Offset`. A `PATCH` then sends the rest from there. A
//! failed upload is retried like any other, so the next attempt resumes
//! where the server stopped. Only the core protocol is used: the batch
//! response already gives the upload URL, so the creation extension isn't
//! needed.
//!
//! [tus.io]: https://tus.io/protocols/resumable-upload

use std::io::Write as _;
use std::sync::Arc;

use futures::StreamExt;
use git_lfs_api::{Actions, HeaderSource};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{Body, Method, RequestBuilder, Response};
use tokio::io::AsyncSeekExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::io::ReaderStream;

use crate::action_auth::{ActionAuth, RequestAuth};
use crate::basic::{
    auth_for, check_status, dump_verbose_response, extra_for, from_api, trace_enabled,
    verbose_enabled, verify,
};
use crate::error::TransferError;
use crate::event::Event;
use crate::http_clients::HttpClients;

/// The protocol version sent in `Tus-Resumable`.
const TUS_VERSION: &str = "1.0.0";

/// Upload `oid` from `store` to the upload action, resuming from what the
/// server already has, then call the verify action if there is one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload(
    clients: &HttpClients,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    actions: &Actions,
    max_verifies: u32,
    extra_headers: Option<&HeaderSource>,
    auth: Option<&Arc<dyn ActionAuth>>,
    events: Option<&UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    let action = actions.upload.as_ref().ok_or_else(|| {
        TransferError::Io(std::io::Error::other(format!(
            "No upload action for object: {oid}"
        )))
    })?;
    let href = &action.href;
    let http = &clients.for_url(href);
    let extra = extra_for(extra_headers, href);
    let auth = auth_for(auth, &action.header, &extra);
    let base = |method: Method| {
        let mut req = http.request(method, href);
        for (k, v) in action
            .header
            .iter()
            .chain(extra.iter().map(|(k, v)| (k, v)))
        {
            req = req.header(k, v);
        }
        req.header("Tus-Resumable", TUS_VERSION)
    };

    // 1. Ask the server how much it has.
    trace(format_args!(
        "xfer: sending tus.io HEAD request for {oid:?}"
    ));
    dump_request("HEAD", href, &[("Tus-Resumable", TUS_VERSION.to_owned())]);
    let resp = send(http, base(Method::HEAD), auth, href, oid).await?;
    check_status(&resp, href)?;
    let header = resp
        .headers()
        .get("Upload-Offset")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| {
            TransferError::Io(std::io::Error::other(format!(
                "missing Upload-Offset header from tus.io HEAD response at {href:?}, contact server admin"
            )))
        })?;
    let offset: u64 = header.trim().parse().map_err(|_| {
        TransferError::Io(std::io::Error::other(format!(
            "invalid Upload-Offset value {header:?} in response from tus.io HEAD at {href:?}, contact server admin"
        )))
    })?;
    if offset >= size {
        trace(format_args!(
            "xfer: tus.io HEAD offset {offset} indicates {oid:?} is already fully uploaded, skipping"
        ));
        progress(events, oid, size);
        return Ok(());
    }
    if offset == 0 {
        trace(format_args!("xfer: tus.io uploading {oid:?} from start"));
    } else {
        trace(format_args!(
            "xfer: tus.io resuming upload {oid:?} from {offset}"
        ));
        progress(events, oid, offset);
    }

    // 2. Send the rest, from that offset.
    let path = store.object_path(Oid::from_hex(oid)?);
    let remaining = size - offset;
    trace(format_args!(
        "xfer: sending tus.io PATCH request for {oid:?}"
    ));
    dump_request(
        "PATCH",
        href,
        &[
            ("Tus-Resumable", TUS_VERSION.to_owned()),
            ("Upload-Offset", offset.to_string()),
            ("Content-Type", "application/offset+octet-stream".to_owned()),
            ("Content-Length", remaining.to_string()),
        ],
    );
    let patch = || {
        base(Method::PATCH)
            .header("Upload-Offset", offset)
            .header(CONTENT_TYPE, "application/offset+octet-stream")
            .header(CONTENT_LENGTH, remaining)
    };
    let resp = send_patch(http, patch, &path, offset, auth, href, oid, events).await?;
    check_status(&resp, href)?;

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

/// Send a `PATCH` with the object's bytes at `path` from `offset`, with
/// credentials as for any action. `build` makes the request without its
/// body, so a credential retry can send it again.
#[allow(clippy::too_many_arguments)]
async fn send_patch(
    http: &reqwest::Client,
    build: impl Fn() -> RequestBuilder,
    path: &std::path::Path,
    offset: u64,
    auth: Option<&Arc<dyn ActionAuth>>,
    href: &str,
    oid: &str,
    events: Option<&UnboundedSender<Event>>,
) -> Result<Response, TransferError> {
    let mut request_auth = RequestAuth::start(auth, href, oid).await;
    loop {
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let (oid_owned, events_owned) = (oid.to_owned(), events.cloned());
        let mut done = offset;
        let stream = ReaderStream::new(file).map(move |chunk| {
            if let Ok(c) = &chunk {
                done += c.len() as u64;
                progress(events_owned.as_ref(), &oid_owned, done);
            }
            chunk
        });
        let mut req = build();
        if let Some(h) = request_auth.header() {
            req = req.header(AUTHORIZATION, h);
        }
        let request = req.body(Body::wrap_stream(stream)).build()?;
        // A streamed body can't be sent again, so a redirect isn't
        // followed.
        let resp = git_lfs_api::send_with_redirects(http, request, 0)
            .await
            .map_err(from_api)?;
        dump_verbose_response(&resp, false);
        if request_auth.retry_after(resp.status().as_u16()).await {
            continue;
        }
        return Ok(resp);
    }
}

/// Send a request without a body, with credentials as for any action.
async fn send(
    http: &reqwest::Client,
    req: RequestBuilder,
    auth: Option<&Arc<dyn ActionAuth>>,
    href: &str,
    oid: &str,
) -> Result<Response, TransferError> {
    let mut request_auth = RequestAuth::start(auth, href, oid).await;
    loop {
        let mut attempt = req.try_clone().expect("no body to clone");
        if let Some(h) = request_auth.header() {
            attempt = attempt.header(AUTHORIZATION, h);
        }
        let resp = git_lfs_api::send_with_redirects(http, attempt.build()?, 0)
            .await
            .map_err(from_api)?;
        dump_verbose_response(&resp, false);
        if request_auth.retry_after(resp.status().as_u16()).await {
            continue;
        }
        return Ok(resp);
    }
}

fn progress(events: Option<&UnboundedSender<Event>>, oid: &str, bytes_done: u64) {
    if let Some(s) = events {
        let _ = s.send(Event::Progress {
            oid: oid.to_owned(),
            bytes_done,
        });
    }
}

/// Log a request in curl's verbose format, like upstream.
fn dump_request(method: &str, url: &str, headers: &[(&str, String)]) {
    if !verbose_enabled() {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "> {method} {url}");
    for (k, v) in headers {
        let _ = writeln!(err, "> {k}: {v}");
    }
    let _ = writeln!(err);
}

fn trace(args: std::fmt::Arguments) {
    if trace_enabled() {
        eprintln!("{args}");
    }
}
