//! `git lfs standalone-file`: the built-in `lfs-standalone-file` transfer
//! adapter, for remotes with a `file://` URL or a local path.
//!
//! It speaks the custom transfer protocol (`docs/custom-transfers.md`) on
//! stdin and stdout, and copies objects between this repository's store
//! and the store of the repository the remote points at. Objects are
//! hard-linked where possible. Messages follow upstream's
//! `lfshttp/standalone`.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_git::endpoint::Operation;
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    event: String,
    #[serde(default)]
    operation: String,
    #[serde(default)]
    remote: String,
    #[serde(default)]
    oid: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    path: Option<PathBuf>,
}

#[derive(Serialize)]
struct ErrorMessage {
    message: String,
}

#[derive(Serialize)]
struct Complete<'a> {
    event: &'static str,
    oid: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorMessage>,
}

/// Serve requests from stdin until `terminate` or the end of input.
pub fn run(cwd: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut handler: Option<Handler> = None;
    let mut result = Ok(());
    for line in stdin.lines() {
        let line = line.map_err(|e| format!("error reading input: {e}"))?;
        let msg: Input =
            serde_json::from_str(&line).map_err(|e| format!("error decoding JSON: {e}"))?;
        let h = match &mut handler {
            Some(h) => h,
            None => match Handler::new(cwd, &msg) {
                Ok(h) => handler.insert(h),
                Err(e) => {
                    let message = format!("error creating handler: {e}");
                    let reply = serde_json::json!({ "error": { "message": message } });
                    writeln!(stdout, "{reply}")?;
                    stdout.flush()?;
                    return Err(message.into());
                }
            },
        };
        match msg.event.as_str() {
            "init" => writeln!(stdout, "{{}}")?,
            "upload" => {
                let outcome = h.upload(&msg.oid, msg.size, msg.path.as_deref());
                respond(&mut stdout, &msg.oid, outcome)?;
            }
            "download" => {
                let outcome = h.download(&msg.oid, msg.size);
                respond(&mut stdout, &msg.oid, outcome)?;
            }
            "terminate" => break,
            other => {
                result = Err(format!("unknown event {other:?}").into());
                break;
            }
        }
        stdout.flush()?;
    }
    result
}

/// Write the `complete` message for a transfer's outcome.
fn respond(
    out: &mut impl Write,
    oid: &str,
    outcome: Result<Option<PathBuf>, String>,
) -> io::Result<()> {
    let (path, error) = match outcome {
        Ok(path) => (path, None),
        Err(message) => (None, Some(ErrorMessage { message })),
    };
    let line = serde_json::to_string(&Complete {
        event: "complete",
        oid,
        path,
        error,
    })
    .map_err(io::Error::other)?;
    writeln!(out, "{line}")
}

/// The two stores, and a directory for downloads in flight.
struct Handler {
    remote: Store,
    remote_path: PathBuf,
    /// Removed when the handler is dropped.
    tmp: tempfile::TempDir,
}

impl Handler {
    fn new(cwd: &Path, msg: &Input) -> Result<Self, String> {
        let url = file_url(cwd, &msg.remote, &msg.operation)
            .ok_or_else(|| "no valid file:// URLs found".to_owned())?;
        let remote_path = url
            .to_file_path()
            .map_err(|()| format!("invalid file URL {url}"))?;
        let git_dir = git_dir_at(&remote_path)?;
        if trace_enabled() {
            eprintln!(
                "using {:?} as remote git directory",
                git_dir.display().to_string()
            );
        }
        let tmp_root = git_lfs_git::lfs_dir(cwd)
            .map(|d| d.join("tmp"))
            .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&tmp_root).map_err(|e| e.to_string())?;
        let tmp = tempfile::Builder::new()
            .prefix("lfs-standalone-file-")
            .tempdir_in(&tmp_root)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            remote: Store::new(git_dir.join("lfs")),
            remote_path,
            tmp,
        })
    }

    /// Put the object at `path` in the remote store, unless it's there.
    fn upload(&self, oid: &str, size: u64, path: Option<&Path>) -> Result<Option<PathBuf>, String> {
        let oid = Oid::from_hex(oid).map_err(|e| e.to_string())?;
        if self.remote.contains_with_size(oid, size) {
            return Ok(None);
        }
        let src = path.ok_or_else(|| "no path given for upload".to_owned())?;
        link_or_copy(src, &self.remote.object_path(oid)).map_err(|e| e.to_string())?;
        Ok(None)
    }

    /// Copy the object from the remote store to a temporary file, and
    /// return its path.
    fn download(&self, oid: &str, size: u64) -> Result<Option<PathBuf>, String> {
        let parsed = Oid::from_hex(oid).map_err(|e| e.to_string())?;
        if !self.remote.contains_with_size(parsed, size) {
            if trace_enabled() {
                eprintln!(
                    "missing object in {:?} ({oid})",
                    self.remote_path.display().to_string()
                );
            }
            return Err(format!("remote missing object {oid}"));
        }
        let dest = self.tmp.path().join(format!("download-{oid}"));
        link_or_copy(&self.remote.object_path(parsed), &dest).map_err(|e| e.to_string())?;
        Ok(Some(dest))
    }
}

/// The `file://` URL for `remote`, which is a URL or the name of a remote
/// whose endpoint for `operation` is one.
fn file_url(cwd: &Path, remote: &str, operation: &str) -> Option<url::Url> {
    if remote.starts_with("file://")
        && let Ok(url) = url::Url::parse(remote)
    {
        return Some(url);
    }
    let operation = if operation == "upload" {
        Operation::Upload
    } else {
        Operation::Download
    };
    let info = git_lfs_git::endpoint::resolve_endpoint_for(cwd, Some(remote), operation).ok()?;
    if !info.url.starts_with("file://") {
        return None;
    }
    url::Url::parse(&info.url).ok()
}

/// The git directory of the repository at `path`, which may be a working
/// tree, its `.git` directory, or a bare repository.
///
/// `GIT_*` variables are cleared, so they can't point git elsewhere.
fn git_dir_at(path: &Path) -> Result<PathBuf, String> {
    let path = if path.file_name().is_some_and(|n| n == ".git") {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(path)
        .args(["rev-parse", "--absolute-git-dir"]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to call `git rev-parse --git-dir`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "failed to call `git rev-parse --git-dir`: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'),
    ))
}

/// Hard-link `src` to `dest`, or copy it if that fails, creating the
/// parent directories.
///
/// A copy is written next to `dest` and renamed into place, so a reader
/// never sees part of it.
fn link_or_copy(src: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    let dir = dest.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::fs::copy(src, tmp.path())?;
    tmp.persist(dest).map_err(|e| e.error)?;
    Ok(())
}

fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}
