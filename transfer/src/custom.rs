//! Custom transfer adapters: external processes that move objects.
//!
//! The protocol is in [`docs/custom-transfers.md`][spec]: line-delimited
//! JSON over the process's stdin and stdout. Each process handles one
//! transfer at a time, so a [`Workers`] pool starts up to
//! `lfs.concurrenttransfers` of them, or one when the adapter isn't
//! `concurrent`. Processes start when an object first needs one, and get
//! a `terminate` message when the batch is done.
//!
//! Each line the process writes to stderr is traced as
//! `xfer[<name>]: <line>`, like upstream.
//!
//! [spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/custom-transfers.md

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use git_lfs_api::Action;
use git_lfs_pointer::Oid;
use git_lfs_store::{Store, StoreError};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;

use crate::error::TransferError;
use crate::event::Event;

/// A custom transfer adapter, from the `lfs.customtransfer.<name>.*`
/// settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAdapter {
    /// The name offered to the server and matched against its choice.
    pub name: String,
    /// The program to run. It's quoted for the shell, then run with
    /// [`args`](Self::args) through `sh -c`.
    pub path: String,
    /// Arguments, expanded by the shell.
    pub args: String,
    /// Whether to run several processes at once. Default true.
    pub concurrent: bool,
    /// Whether the adapter downloads.
    pub download: bool,
    /// Whether the adapter uploads.
    pub upload: bool,
}

impl CustomAdapter {
    /// Whether the adapter handles `upload`s or downloads.
    pub fn handles(&self, upload: bool) -> bool {
        if upload { self.upload } else { self.download }
    }
}

/// How long a process gets to exit after `terminate`, as in upstream.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// The processes of one adapter, for one batch in one direction.
pub(crate) struct Workers {
    adapter: CustomAdapter,
    upload: bool,
    /// The `remote` sent in the `init` message.
    remote: String,
    /// `lfs.concurrenttransfers`, sent in the `init` message.
    concurrency: usize,
    /// Limits the number of processes.
    slots: Semaphore,
    idle: Mutex<Vec<Worker>>,
    next_id: AtomicUsize,
}

impl Workers {
    pub(crate) fn new(
        adapter: CustomAdapter,
        upload: bool,
        remote: String,
        concurrency: usize,
    ) -> Self {
        let workers = if adapter.concurrent {
            concurrency.max(1)
        } else {
            1
        };
        transfer_trace(format_args!(
            "xfer: adapter {:?} Begin() with {workers} workers",
            adapter.name
        ));
        Self {
            adapter,
            upload,
            remote,
            concurrency,
            slots: Semaphore::new(workers),
            idle: Mutex::new(Vec::new()),
            next_id: AtomicUsize::new(0),
        }
    }

    /// Download `oid` into `store`.
    pub(crate) async fn download(
        &self,
        store: Arc<Store>,
        oid: &str,
        size: u64,
        action: Option<&Action>,
        events: Option<&UnboundedSender<Event>>,
    ) -> Result<(), TransferError> {
        let request = TransferRequest {
            event: "download",
            oid,
            size,
            path: None,
            action,
        };
        let path = self.transfer(&request, events).await?;
        let path = path.ok_or_else(|| {
            custom_error(format!(
                "custom adapter {:?} sent no path for {oid}",
                self.adapter.name
            ))
        })?;
        let expected = Oid::from_hex(oid)?;
        // The store hashes the file, so a bad download isn't kept.
        let result = tokio::task::spawn_blocking(move || {
            let inserted = match std::fs::File::open(&path) {
                Ok(mut file) => match store.insert_verified(expected, &mut file) {
                    Ok(_) => Ok(()),
                    // Upstream's wording, which the shell tests match.
                    Err(StoreError::HashMismatch { expected, actual }) => Err(format!(
                        "file {path:?} has an invalid hash {actual}, expected {expected}"
                    )),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            };
            let _ = std::fs::remove_file(&path);
            inserted
        })
        .await
        .map_err(|e| TransferError::Io(std::io::Error::other(e)))?;
        result.map_err(|e| custom_error(format!("downloaded file failed checks: {e}")))
    }

    /// Upload `oid` from `store`.
    pub(crate) async fn upload(
        &self,
        store: &Store,
        oid: &str,
        size: u64,
        action: Option<&Action>,
        events: Option<&UnboundedSender<Event>>,
    ) -> Result<(), TransferError> {
        let path = store.object_path(Oid::from_hex(oid)?);
        let request = TransferRequest {
            event: "upload",
            oid,
            size,
            path: Some(&path),
            action,
        };
        self.transfer(&request, events).await.map(|_| ())
    }

    /// Send `request` to an idle process and read its replies up to
    /// `complete`, returning the `path` it gave.
    ///
    /// A process that can't be talked to is killed, and the next transfer
    /// starts a new one.
    async fn transfer(
        &self,
        request: &TransferRequest<'_>,
        events: Option<&UnboundedSender<Event>>,
    ) -> Result<Option<PathBuf>, TransferError> {
        let _slot = self.slots.acquire().await.expect("semaphore live");
        let idle = self.idle.lock().await.pop();
        let mut worker = match idle {
            Some(w) => w,
            None => self.start().await?,
        };
        match worker.transfer(&self.adapter.name, request, events).await {
            Ok(result) => {
                self.idle.lock().await.push(worker);
                result
            }
            Err(e) => {
                worker.abort().await;
                Err(into_custom(e))
            }
        }
    }

    /// Start a process and send it `init`.
    async fn start(&self) -> Result<Worker, TransferError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        transfer_trace(format_args!(
            "xfer: starting up custom transfer process {:?} for worker {id}",
            self.adapter.name
        ));
        let mut worker = Worker::spawn(&self.adapter, id)?;
        let init = InitRequest {
            event: "init",
            operation: if self.upload { "upload" } else { "download" },
            remote: &self.remote,
            concurrent: self.adapter.concurrent,
            concurrenttransfers: self.concurrency,
        };
        let reply = match worker.exchange(&init).await {
            Ok(reply) => reply,
            Err(e) => {
                worker.abort().await;
                return Err(into_custom(e));
            }
        };
        if let Some(err) = reply.error {
            worker.abort().await;
            return Err(custom_error(format!(
                "error initializing custom adapter {:?} worker {id}: {err}",
                self.adapter.name
            )));
        }
        transfer_trace(format_args!(
            "xfer: started custom adapter process {:?} for worker {id} OK",
            self.adapter.path
        ));
        Ok(worker)
    }

    /// Send `terminate` to every process and wait for them to exit.
    pub(crate) async fn shutdown(&self) {
        let workers = std::mem::take(&mut *self.idle.lock().await);
        for worker in workers {
            worker.shutdown().await;
        }
    }
}

/// One running adapter process.
struct Worker {
    id: usize,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Traces the process's stderr until it closes.
    stderr: Option<JoinHandle<()>>,
}

impl Worker {
    fn spawn(adapter: &CustomAdapter, id: usize) -> Result<Self, TransferError> {
        let script = format!("{} {}", shell_quote(&adapter.path), adapter.args);
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                custom_error(format!(
                    "failed to start custom transfer command {:?} remote: {e}",
                    adapter.path
                ))
            })?;
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let stderr = child.stderr.take().expect("piped");
        let name = Path::new(&adapter.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| adapter.path.clone());
        let stderr = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                trace(format_args!("xfer[{name}]: {}", line.trim()));
            }
        });
        Ok(Self {
            id,
            child,
            stdin,
            stdout,
            stderr: Some(stderr),
        })
    }

    async fn send(&mut self, message: &impl Serialize) -> Result<(), TransferError> {
        let mut line = serde_json::to_string(message).map_err(std::io::Error::other)?;
        transfer_trace(format_args!(
            "xfer: Custom adapter worker {} sending message: {line}",
            self.id
        ));
        line.push('\n');
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| custom_error("custom adapter stdin is closed".into()))?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Response, TransferError> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line).await? == 0 {
            return Err(TransferError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        transfer_trace(format_args!(
            "xfer: Custom adapter worker {} received response: {}",
            self.id,
            line.trim()
        ));
        serde_json::from_str(&line).map_err(|e| TransferError::Io(std::io::Error::other(e)))
    }

    async fn exchange(&mut self, message: &impl Serialize) -> Result<Response, TransferError> {
        self.send(message).await?;
        self.receive().await
    }

    /// Run one transfer. The outer error means the process can't be used
    /// any more. The inner result is the transfer's own outcome.
    async fn transfer(
        &mut self,
        name: &str,
        request: &TransferRequest<'_>,
        events: Option<&UnboundedSender<Event>>,
    ) -> Result<Result<Option<PathBuf>, TransferError>, TransferError> {
        self.send(request).await?;
        loop {
            let reply = self.receive().await?;
            if reply.oid != request.oid && matches!(reply.event.as_str(), "progress" | "complete") {
                return Err(custom_error(format!(
                    "unexpected OID {:?} in response, expecting {:?}",
                    reply.oid, request.oid
                )));
            }
            match reply.event.as_str() {
                "progress" => {
                    if let Some(s) = events {
                        let _ = s.send(Event::Progress {
                            oid: request.oid.to_owned(),
                            bytes_done: reply.bytes_so_far,
                        });
                    }
                }
                "complete" => {
                    return Ok(match reply.error {
                        Some(err) => Err(custom_error(format!(
                            "error transferring {:?}: {err}",
                            request.oid
                        ))),
                        None => Ok(reply.path.filter(|p| !p.as_os_str().is_empty())),
                    });
                }
                other => {
                    return Err(custom_error(format!(
                        "invalid message {other:?} from custom adapter {name:?}"
                    )));
                }
            }
        }
    }

    /// Send `terminate`, close the pipes, and wait for the process to exit,
    /// killing it after [`SHUTDOWN_TIMEOUT`].
    async fn shutdown(mut self) {
        transfer_trace(format_args!(
            "xfer: Shutting down adapter worker {}",
            self.id
        ));
        let _ = self.send(&TerminateRequest { event: "terminate" }).await;
        self.stdin = None;
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(_) => {
                if let Some(stderr) = self.stderr.take() {
                    let _ = stderr.await;
                }
            }
            Err(_) => {
                trace(format_args!(
                    "xfer: error finishing up custom transfer process worker {}, aborting",
                    self.id
                ));
                self.abort().await;
            }
        }
    }

    /// Kill the process.
    async fn abort(&mut self) {
        transfer_trace(format_args!("xfer: Aborting worker process: {}", self.id));
        self.stdin = None;
        let _ = self.child.kill().await;
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.await;
        }
    }
}

#[derive(Serialize)]
struct InitRequest<'a> {
    event: &'static str,
    operation: &'static str,
    remote: &'a str,
    concurrent: bool,
    concurrenttransfers: usize,
}

#[derive(Serialize)]
struct TransferRequest<'a> {
    event: &'static str,
    oid: &'a str,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a Path>,
    /// `null` for a standalone adapter.
    action: Option<&'a Action>,
}

#[derive(Serialize)]
struct TerminateRequest {
    event: &'static str,
}

/// Any message from the process.
#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    event: String,
    #[serde(default)]
    oid: String,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default, rename = "bytesSoFar")]
    bytes_so_far: u64,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Deserialize)]
struct ResponseError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

impl std::fmt::Display for ResponseError {
    /// Upstream's `ObjectError` format.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

fn custom_error(message: String) -> TransferError {
    TransferError::Custom(message)
}

/// Make a failure to talk to the process a [`TransferError::Custom`], so
/// it isn't retried.
fn into_custom(e: TransferError) -> TransferError {
    match e {
        TransferError::Custom(_) => e,
        other => TransferError::Custom(other.to_string()),
    }
}

/// Quote `s` for `sh` unless it's a plain word, like upstream's
/// `ShellQuoteSingle`.
fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@/.-".contains(c));
    if plain {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Whether `GIT_TRACE` is set to something other than empty or `0`.
fn trace_enabled() -> bool {
    env_true("GIT_TRACE")
}

fn env_true(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0" && v != "false")
}

fn trace(args: std::fmt::Arguments) {
    if trace_enabled() {
        eprintln!("{args}");
    }
}

/// Trace when `GIT_TRANSFER_TRACE` or `GIT_CURL_VERBOSE` is also set, like
/// upstream's adapter debugging.
fn transfer_trace(args: std::fmt::Arguments) {
    if env_true("GIT_TRANSFER_TRACE") || env_true("GIT_CURL_VERBOSE") {
        trace(args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_leaves_plain_words() {
        assert_eq!(
            shell_quote("lfstest-customadapter"),
            "lfstest-customadapter"
        );
        assert_eq!(shell_quote("/usr/bin/x.y"), "/usr/bin/x.y");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
