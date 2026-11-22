//! Rust port of upstream's `t/cmd/lfstest-customadapter.go`.
//!
//! A custom transfer adapter for the shell tests. It speaks the protocol
//! in `docs/custom-transfers.md` and moves each object by sending the
//! action's HTTP request itself, so it's a proxy for the basic adapter.
//! Everything it does is logged to stderr, which Git LFS traces.
//!
//! Upstream sends the requests with Git LFS's own API client, which adds
//! credentials. This port doesn't, since the test server's storage doesn't
//! ask for them in the tests that use it.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    event: String,
    #[serde(default)]
    operation: String,
    #[serde(default)]
    oid: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    path: String,
    #[serde(default)]
    action: Option<Action>,
}

#[derive(Deserialize)]
struct Action {
    href: String,
    #[serde(default)]
    header: HashMap<String, String>,
}

#[derive(Serialize)]
struct TransferError {
    code: u16,
    message: String,
}

#[derive(Serialize)]
struct Complete<'a> {
    event: &'static str,
    oid: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TransferError>,
}

#[derive(Serialize)]
struct Progress<'a> {
    event: &'static str,
    oid: &'a str,
    #[serde(rename = "bytesSoFar")]
    bytes_so_far: u64,
    #[serde(rename = "bytesSinceLast")]
    bytes_since_last: u64,
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let client = reqwest::Client::new();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => {
                log(&format!("Unable to parse request: {line}"));
                continue;
            }
        };
        match req.event.as_str() {
            "init" => {
                log(&format!(
                    "Initialised test custom adapter for {}",
                    req.operation
                ));
                send(&serde_json::json!({}));
            }
            "download" => {
                log(&format!("Received download request for {}", req.oid));
                runtime.block_on(download(&client, &req));
            }
            "upload" => {
                log(&format!("Received upload request for {}", req.oid));
                runtime.block_on(upload(&client, &req));
            }
            "terminate" => {
                log("Terminating test custom adapter gracefully.");
                break;
            }
            _ => {}
        }
    }
}

async fn download(client: &reqwest::Client, req: &Request) {
    let Some(action) = &req.action else {
        return fail(&req.oid, 2, "no download action".into());
    };
    let mut builder = client.get(&action.href);
    for (k, v) in &action.header {
        builder = builder.header(k, v);
    }
    let mut res = match builder.send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status().as_u16();
            return fail(&req.oid, status, format!("HTTP {status}"));
        }
        Err(e) => return fail(&req.oid, 6, e.to_string()),
    };
    let file = match tempfile::Builder::new()
        .prefix("lfscustomdl")
        .tempfile()
        .and_then(|f| f.keep().map_err(|e| e.error))
    {
        Ok(f) => f,
        Err(e) => return fail(&req.oid, 3, e.to_string()),
    };
    let (mut file, path) = file;
    let mut so_far = 0u64;
    loop {
        match res.chunk().await {
            Ok(Some(chunk)) => {
                if let Err(e) = file.write_all(&chunk) {
                    let _ = std::fs::remove_file(&path);
                    return fail(
                        &req.oid,
                        4,
                        format!("cannot write data to tempfile {path:?}: {e}"),
                    );
                }
                so_far += chunk.len() as u64;
                progress(&req.oid, so_far, chunk.len() as u64);
            }
            Ok(None) => break,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return fail(&req.oid, 4, e.to_string());
            }
        }
    }
    drop(file);
    send(&Complete {
        event: "complete",
        oid: &req.oid,
        path: Some(path.to_string_lossy().into_owned()),
        error: None,
    });
}

async fn upload(client: &reqwest::Client, req: &Request) {
    let Some(action) = &req.action else {
        return fail(&req.oid, 2, "no upload action".into());
    };
    let data = match std::fs::read(&req.path) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                &req.oid,
                3,
                format!("Cannot read data from {:?}: {e}", req.path),
            );
        }
    };
    let mut builder = client.put(&action.href);
    let mut has_content_type = false;
    for (k, v) in &action.header {
        has_content_type |= k.eq_ignore_ascii_case("content-type");
        builder = builder.header(k, v);
    }
    if !has_content_type {
        builder = builder.header("Content-Type", "application/octet-stream");
    }
    let size = data.len() as u64;
    let res = match builder
        .header("Content-Length", req.size.to_string())
        .body(data)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return fail(
                &req.oid,
                6,
                format!("Error uploading data for {}: {e}", req.oid),
            );
        }
    };
    progress(&req.oid, size, size);
    let status = res.status().as_u16();
    if status > 299 {
        let url = action.href.split('?').next().unwrap_or_default();
        return fail(
            &req.oid,
            status,
            format!("Invalid status for PUT {url}: {status}"),
        );
    }
    send(&Complete {
        event: "complete",
        oid: &req.oid,
        path: None,
        error: None,
    });
}

fn fail(oid: &str, code: u16, message: String) {
    send(&Complete {
        event: "complete",
        oid,
        path: None,
        error: Some(TransferError { code, message }),
    });
}

fn progress(oid: &str, bytes_so_far: u64, bytes_since_last: u64) {
    send(&Progress {
        event: "progress",
        oid,
        bytes_so_far,
        bytes_since_last,
    });
}

/// Write one line of JSON to stdout, and log it.
fn send(message: &impl Serialize) {
    let line = serde_json::to_string(message).expect("serializable");
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    log(&format!("Sent message {line}"));
}

fn log(message: &str) {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{}", message.trim_end_matches('\n'));
    let _ = err.flush();
}
