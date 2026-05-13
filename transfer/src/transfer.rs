//! Top-level transfer orchestrator: batch + concurrent per-object transfer.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use git_lfs_api::{
    BatchRequest, BatchResponse, Client as ApiClient, ObjectResult, ObjectSpec, Operation, Ref,
};
use git_lfs_store::Store;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use crate::basic;
use crate::config::TransferConfig;
use crate::error::{Report, TransferError};
use crate::event::Event;

/// Direction of a single transfer batch — used internally to share the
/// fan-out machinery between [`Transfer::download`] and [`Transfer::upload`].
#[derive(Debug, Clone, Copy)]
enum Dir {
    Download,
    Upload,
}

impl From<Dir> for Operation {
    fn from(d: Dir) -> Self {
        match d {
            Dir::Download => Operation::Download,
            Dir::Upload => Operation::Upload,
        }
    }
}

/// Concurrent transfer queue. One [`Transfer`] is bound to one LFS endpoint
/// (the `api` client) and one local store; create more if you need more.
#[derive(Clone)]
pub struct Transfer {
    api: ApiClient,
    store: Arc<Store>,
    http: reqwest::Client,
    config: TransferConfig,
}

impl Transfer {
    /// Build a transfer queue. The `reqwest::Client` used for the action-URL
    /// transfers is created fresh; if you need to share a connection pool,
    /// use [`with_http_client`](Self::with_http_client).
    pub fn new(api: ApiClient, store: Store, config: TransferConfig) -> Self {
        Self::with_http_client(api, store, config, reqwest::Client::new())
    }

    /// Build a transfer queue around an existing `reqwest::Client`.
    ///
    /// Use this when the caller already has an HTTP client wired up
    /// for the LFS endpoint (with custom TLS config, headers,
    /// cookies, etc.) and wants the transfer queue to reuse its
    /// connection pool rather than spawn a fresh one.
    pub fn with_http_client(
        api: ApiClient,
        store: Store,
        config: TransferConfig,
        http: reqwest::Client,
    ) -> Self {
        Self {
            api,
            store: Arc::new(store),
            http,
            config,
        }
    }

    /// Download the given objects into the local store. Each object is
    /// hash-verified by the store before being committed; corrupt downloads
    /// are surfaced in [`Report::failed`].
    pub async fn download(
        &self,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
        events: Option<UnboundedSender<Event>>,
    ) -> Result<Report, TransferError> {
        self.run(Dir::Download, objects, r#ref, events).await
    }

    /// Upload the given objects from the local store. Objects the server
    /// already has are reported in [`Report::succeeded`] without any byte
    /// transfer.
    pub async fn upload(
        &self,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
        events: Option<UnboundedSender<Event>>,
    ) -> Result<Report, TransferError> {
        self.run(Dir::Upload, objects, r#ref, events).await
    }

    async fn run(
        &self,
        dir: Dir,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
        events: Option<UnboundedSender<Event>>,
    ) -> Result<Report, TransferError> {
        if objects.is_empty() {
            return Ok(Report::default());
        }
        // Chunk the input into `batch_size`-sized runs so an
        // `lfs.transfer.batchSize` of 1 produces one batch API call
        // per object, etc. Each chunk goes through the existing
        // batch + concurrent-transfer machinery; reports are merged.
        let batch_size = self.config.batch_size.max(1);
        if objects.len() > batch_size {
            let mut report = Report::default();
            for chunk in objects.chunks(batch_size) {
                let chunk_report =
                    Box::pin(self.run(dir, chunk.to_vec(), r#ref.clone(), events.clone())).await?;
                report.succeeded.extend(chunk_report.succeeded);
                report.failed.extend(chunk_report.failed);
            }
            return Ok(report);
        }

        // Index the request's sizes by oid so we can fill them back in
        // for servers that omit `size` from the response (the upstream
        // test fixture, plus at least one production server, drop it).
        let req_sizes: std::collections::HashMap<String, u64> =
            objects.iter().map(|o| (o.oid.clone(), o.size)).collect();

        // Sort the batch by descending object size. Larger blobs get
        // their action URLs first, which gives the bigger uploads /
        // downloads a head start on the parallel-transfer semaphore
        // — short small transfers can complete in the tail while the
        // big one is still streaming. Matches upstream's `tq` queue
        // ordering (t-batch-transfer test 2 asserts the JSON order).
        let mut objects = objects;
        objects.sort_by_key(|o| std::cmp::Reverse(o.size));

        let mut req = BatchRequest::new(dir.into(), objects);
        if let Some(r) = r#ref {
            req = req.with_ref(r);
        }
        let resp: BatchResponse = self.batch_with_retry(&req).await?;

        // The spec requires `sha256` and treats an absent/empty
        // `hash_algo` as that default. Anything else means the server
        // would expect us to recompute every OID under a different
        // digest before its action URLs could be trusted — bail
        // before any per-object work runs.
        if let Some(h) = resp.hash_algo.as_deref()
            && !h.is_empty()
            && !h.eq_ignore_ascii_case("sha256")
        {
            return Err(TransferError::UnsupportedHashAlgo(h.to_owned()));
        }

        let limit = Arc::new(Semaphore::new(self.config.concurrency.max(1)));
        let mut join: JoinSet<(String, Result<(), TransferError>)> = JoinSet::new();

        for mut obj in resp.objects {
            if obj.size == 0
                && let Some(s) = req_sizes.get(&obj.oid)
            {
                obj.size = *s;
            }
            if let Some(actions) = obj.actions.as_mut() {
                if let Some(rewriter) = &self.config.url_rewriter
                    && let Some(d) = actions.download.as_mut()
                {
                    d.href = rewriter(&d.href);
                }
                // Uploads and the verify step that follows go through
                // `pushInsteadOf` (with `insteadOf` as fallback) when
                // `lfs.transfer.enablehrefrewrite=true`. When no
                // push-direction rewriter is configured, fall back to
                // the download rewriter so the existing `insteadOf`
                // behavior keeps working for upload paths.
                let up_rewriter = self
                    .config
                    .upload_url_rewriter
                    .as_ref()
                    .or(self.config.url_rewriter.as_ref());
                if let Some(rewriter) = up_rewriter {
                    if let Some(u) = actions.upload.as_mut() {
                        u.href = rewriter(&u.href);
                    }
                    if let Some(v) = actions.verify.as_mut() {
                        v.href = rewriter(&v.href);
                    }
                }
            }
            let permit_src = limit.clone();
            let http = self.http.clone();
            let store = self.store.clone();
            let config = self.config.clone();
            let events = events.clone();
            join.spawn(async move {
                let _permit = permit_src.acquire_owned().await.expect("semaphore live");
                let oid = obj.oid.clone();
                let result = process_object(dir, &http, store, &config, obj, events.as_ref()).await;
                (oid, result)
            });
        }

        let mut report = Report::default();
        while let Some(joined) = join.join_next().await {
            let (oid, result) =
                joined.map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
            match result {
                Ok(()) => {
                    if let Some(s) = &events {
                        let _ = s.send(Event::Completed { oid: oid.clone() });
                    }
                    report.succeeded.push(oid);
                }
                Err(err) => {
                    if let Some(s) = &events {
                        let _ = s.send(Event::Failed {
                            oid: oid.clone(),
                            error: err.to_string(),
                        });
                    }
                    report.failed.push((oid, err));
                }
            }
        }
        Ok(report)
    }

    /// Call the batch endpoint with retry. Wraps `api.batch()` in the
    /// same retry shape as per-object transfers: honor `Retry-After`
    /// when the server pinned a delay, exponential backoff otherwise.
    /// Emits `tq: sending batch of size N` on every attempt (so
    /// t-alternates can still grep it) and, on each retry, one
    /// `tq: enqueue retry #N after <secs>s for "<oid>" (size: N): <err>`
    /// per object in the batch — that's the per-object format
    /// `t-batch-retries-ratelimit.sh` greps for, since upstream's
    /// transfer queue routes each object through `enqueueRetry` even
    /// though the failure is at the batch layer.
    async fn batch_with_retry(&self, req: &BatchRequest) -> Result<BatchResponse, TransferError> {
        let mut backoff = self.config.initial_backoff;
        let mut retry_count: u32 = 0;
        let mut last_err: Option<git_lfs_api::ApiError> = None;
        for attempt in 0..self.config.max_attempts {
            if trace_enabled() {
                eprintln!("tq: sending batch of size {}", req.objects.len());
            }
            match self.api.batch(req).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    let retry = e.is_retryable() && attempt + 1 < self.config.max_attempts;
                    if !retry {
                        return Err(TransferError::BatchResponse(Box::new(e)));
                    }
                    let server_delay = e.retry_after();
                    let delay = server_delay.unwrap_or(backoff);
                    retry_count += 1;
                    if trace_enabled() {
                        let secs = delay.as_secs_f64();
                        for obj in &req.objects {
                            // Upstream's `%q` Go-quotes the oid (adds
                            // quotes + Go escapes); a pure hex OID
                            // round-trips through Rust's {:?} the same
                            // way.
                            eprintln!(
                                "tq: enqueue retry #{retry_count} after {secs:.2}s for {:?} (size: {}): {e}",
                                obj.oid, obj.size
                            );
                        }
                    }
                    last_err = Some(e);
                    tokio::time::sleep(delay).await;
                    if server_delay.is_none() {
                        backoff = (backoff * 2).min(self.config.backoff_max);
                    }
                }
            }
        }
        Err(TransferError::BatchResponse(Box::new(
            last_err.expect("loop ran at least once"),
        )))
    }
}

/// Returns true when the user asked for `GIT_TRACE`. Cheap gate around
/// the per-retry `eprintln!` calls.
fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Handle one [`ObjectResult`]: emit Started, run with retry, return final
/// outcome. Completed/Failed events are emitted by the caller so we can
/// move the error into the Report without cloning.
async fn process_object(
    dir: Dir,
    http: &reqwest::Client,
    store: Arc<Store>,
    config: &TransferConfig,
    obj: ObjectResult,
    events: Option<&UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    if let Some(err) = obj.error {
        return Err(TransferError::ServerObject(err));
    }

    if let Some(s) = events {
        let _ = s.send(Event::Started {
            oid: obj.oid.clone(),
            size: obj.size,
        });
    }

    match (dir, &obj.actions) {
        (Dir::Download, Some(actions)) => {
            let action = actions
                .download
                .as_ref()
                .ok_or(TransferError::NoDownloadAction)?;
            check_not_expired("download", action)?;
            with_retry(config, &obj.oid, obj.size, || async {
                basic::download(http, store.clone(), &obj.oid, obj.size, action, events)
                    .await
                    .map(|_| ())
            })
            .await
        }
        (Dir::Download, None) => Err(TransferError::NoDownloadAction),
        (Dir::Upload, Some(actions)) => {
            if let Some(upload) = actions.upload.as_ref() {
                check_not_expired("upload", upload)?;
            }
            if let Some(verify) = actions.verify.as_ref() {
                check_not_expired("verify", verify)?;
            }
            with_retry(config, &obj.oid, obj.size, || async {
                basic::upload(
                    http,
                    store.clone(),
                    &obj.oid,
                    obj.size,
                    actions,
                    config.detect_content_type,
                    config.max_verifies,
                    events,
                )
                .await
            })
            .await
        }
        (Dir::Upload, None) => {
            // Server already has it — no actions means no-op, treated as success.
            Ok(())
        }
    }
}

/// Match upstream's `objectExpirationToTransfer = 5 * time.Second`
/// safety buffer.
const ACTION_EXPIRATION_BUFFER: Duration = Duration::from_secs(5);

fn check_not_expired(rel: &str, action: &git_lfs_api::Action) -> Result<(), TransferError> {
    if action.is_expired_within(SystemTime::now(), ACTION_EXPIRATION_BUFFER) {
        return Err(TransferError::ActionExpired {
            rel: rel.to_owned(),
        });
    }
    Ok(())
}

/// Run `op` with retry. Two paths: when the server sent a `Retry-After`
/// header, sleep for that long (the "delayed re-queue" path in
/// upstream); otherwise fall back to exponential backoff with the
/// configured initial / cap. Trace breadcrumbs match upstream's
/// `tq/transfer_queue.go` formats so `t-batch-storage-retries*` greps
/// keep matching:
///
/// - `tq: retrying object <oid> after <secs>s` (Retry-After path), or
/// - `tq: retrying object <oid>: <err>` (exponential path); plus
/// - `tq: enqueue retry #<n> after <secs>s for "<oid>" (size: <n>): <err>`
///
/// Stops on non-retryable errors or when `max_attempts` is reached.
async fn with_retry<F, Fut>(
    config: &TransferConfig,
    oid: &str,
    size: u64,
    mut op: F,
) -> Result<(), TransferError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), TransferError>>,
{
    let mut backoff = config.initial_backoff;
    let mut retry_count: u32 = 0;
    let mut last_err: Option<TransferError> = None;
    for attempt in 0..config.max_attempts {
        match op().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let retry = e.is_retryable() && attempt + 1 < config.max_attempts;
                if !retry {
                    last_err = Some(e);
                    break;
                }
                let delay = e.retry_after().unwrap_or(backoff);
                retry_count += 1;
                emit_retry_trace(oid, size, retry_count, delay, &e);
                last_err = Some(e);
                tokio::time::sleep(delay).await;
                // Only grow the exponential window when we're falling back
                // to it. A server-supplied Retry-After resets the clock —
                // upstream uses an independent timer per retry batch.
                if last_err
                    .as_ref()
                    .and_then(TransferError::retry_after)
                    .is_none()
                {
                    backoff = (backoff * 2).min(config.backoff_max);
                }
            }
        }
    }
    Err(last_err.expect("loop ran at least once"))
}

/// Emit upstream-format trace lines for a single retry. Two lines per
/// retry, both gated on `GIT_TRACE` so we don't pay the format cost
/// when nobody's watching:
///
/// - `tq: retrying object …` — what handleTransferResult logs in
///   `tq/transfer_queue.go:819` / `:835`.
/// - `tq: enqueue retry #N …` — what enqueueRetry logs at `:564`.
///
/// We emit both per retry because the upstream tests treat them as
/// independent grep targets even though they fire on the same retry
/// in our (simpler, inline) retry model.
fn emit_retry_trace(oid: &str, size: u64, count: u32, delay: Duration, err: &TransferError) {
    if !trace_enabled() {
        return;
    }
    let secs = delay.as_secs_f64();
    if err.retry_after().is_some() {
        eprintln!("tq: retrying object {oid} after {secs:.2}s");
    } else {
        eprintln!("tq: retrying object {oid}: {err}");
    }
    // Upstream uses Go's `%q` for the oid (adds quotes + Go escapes); a
    // pure hex OID round-trips through Rust's {:?} the same way.
    eprintln!("tq: enqueue retry #{count} after {secs:.2}s for {oid:?} (size: {size}): {err}");
}
