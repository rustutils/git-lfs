//! Rust port of upstream's `t/cmd/lfstest-standalonecustomadapter.go`.
//!
//! A standalone custom transfer adapter for the shell tests: uploads copy
//! each object into `$TEST_STANDALONE_BACKUP_PATH`, and downloads copy it
//! back out. It logs its arguments and every message to stderr, which Git
//! LFS traces.

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

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
    path: String,
}

#[derive(Serialize)]
struct TransferError {
    code: u32,
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
    let Some(backup_dir) = std::env::var_os("TEST_STANDALONE_BACKUP_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    else {
        log("TEST_STANDALONE_BACKUP_PATH backup dir not set");
        std::process::exit(1);
    };
    for arg in std::env::args() {
        log(&format!("Saw argument {arg:?}"));
    }
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
                download(&backup_dir, &req.oid);
            }
            "upload" => {
                log(&format!("Received upload request for {}", req.oid));
                upload(&backup_dir, &req.oid, Path::new(&req.path));
            }
            "terminate" => {
                log("Terminating test custom adapter gracefully.");
                break;
            }
            _ => {}
        }
    }
}

fn download(backup_dir: &Path, oid: &str) {
    let dest = match tempfile::Builder::new()
        .prefix("lfscustomdl")
        .tempfile()
        .and_then(|f| f.keep().map_err(|e| e.error))
    {
        Ok((_, path)) => path,
        Err(e) => return fail(oid, 1, e.to_string()),
    };
    if copy(oid, &backup_dir.join(oid), &dest).is_ok() {
        send(&Complete {
            event: "complete",
            oid,
            path: Some(dest.to_string_lossy().into_owned()),
            error: None,
        });
    }
}

fn upload(backup_dir: &Path, oid: &str, from: &Path) {
    if copy(oid, from, &backup_dir.join(oid)).is_ok() {
        send(&Complete {
            event: "complete",
            oid,
            path: None,
            error: None,
        });
    }
}

/// Copy `src` to `dst`, sending progress, or an error and `Err`.
fn copy(oid: &str, src: &Path, dst: &Path) -> Result<(), ()> {
    log(&format!("Copying {} to {}", src.display(), dst.display()));
    let mut input = std::fs::File::open(src).map_err(|e| fail(oid, 10, e.to_string()))?;
    let mut output = std::fs::File::create(dst).map_err(|e| fail(oid, 11, e.to_string()))?;
    let mut buf = vec![0u8; 32 * 1024];
    let mut so_far = 0u64;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(dst);
                fail(oid, 4, format!("cannot write data to dst {dst:?}: {e}"));
                return Err(());
            }
        };
        if let Err(e) = output.write_all(&buf[..n]) {
            let _ = std::fs::remove_file(dst);
            fail(oid, 4, format!("cannot write data to dst {dst:?}: {e}"));
            return Err(());
        }
        so_far += n as u64;
        send(&Progress {
            event: "progress",
            oid,
            bytes_so_far: so_far,
            bytes_since_last: n as u64,
        });
    }
    Ok(())
}

fn fail(oid: &str, code: u32, message: String) {
    send(&Complete {
        event: "complete",
        oid,
        path: None,
        error: Some(TransferError { code, message }),
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
