//! Basic transfer adapter — direct GET/PUT/POST against the action URLs
//! the batch endpoint hands back.
//!
//! See `docs/api/basic-transfers.md`.

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

/// Default Content-Type for raw object uploads. Servers/CDNs may override
/// it via `action.header`.
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
    check_status(&resp)?;

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
pub(crate) async fn upload(
    http: &reqwest::Client,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    actions: &Actions,
    events: Option<&UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    let upload_action = actions
        .upload
        .as_ref()
        .ok_or_else(|| TransferError::Io(std::io::Error::other("missing upload action")))?;

    let oid_parsed = Oid::from_hex(oid)?;
    let path = store.object_path(oid_parsed);
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
    let mut saw_content_type = false;
    for (k, v) in &upload_action.header {
        if k.eq_ignore_ascii_case("content-type") {
            saw_content_type = true;
        }
        req = req.header(k, v);
    }
    if !saw_content_type {
        req = req.header(reqwest::header::CONTENT_TYPE, OCTET_STREAM);
    }

    let resp = req.body(body).send().await?;
    check_status(&resp)?;

    if let Some(verify_action) = &actions.verify {
        verify(http, oid, size, verify_action).await?;
    }
    Ok(())
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
    check_status(&resp)?;
    Ok(())
}

fn check_status(resp: &Response) -> Result<(), TransferError> {
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(TransferError::ActionStatus {
            status: resp.status().as_u16(),
        })
    }
}
