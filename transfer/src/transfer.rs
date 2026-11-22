//! The transfer queue: batch request, then concurrent object transfers.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use git_lfs_api::{
    BatchRequest, BatchResponse, Client as ApiClient, ObjectResult, ObjectSpec, Operation, Ref,
};
use git_lfs_store::Store;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use crate::backend::{Backend, HttpTransport, SshBackend, Transport};
use crate::basic;
use crate::config::TransferConfig;
use crate::error::{Report, TransferError};
use crate::event::Event;
use crate::sshtransfer::adapter as ssh;

/// Direction of a transfer batch.
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

/// Concurrent transfer queue for one LFS endpoint and one local store.
#[derive(Clone)]
pub struct Transfer {
    backend: Backend,
    store: Arc<Store>,
    config: TransferConfig,
}

impl Transfer {
    /// Build an HTTP transfer queue with a new `reqwest::Client` for the
    /// action URLs.
    pub fn new(api: ApiClient, store: Store, config: TransferConfig) -> Self {
        Self::with_http_client(api, store, config, git_lfs_api::http_client())
    }

    /// Build an HTTP transfer queue that uses `http` for the action URLs.
    ///
    /// Use this to share a client's TLS settings and connection pool. The
    /// client must not follow redirects itself, as with
    /// [`git_lfs_api::http_client`].
    pub fn with_http_client(
        api: ApiClient,
        store: Store,
        config: TransferConfig,
        http: reqwest::Client,
    ) -> Self {
        Self {
            backend: Backend::Http {
                api: Box::new(api),
                http,
            },
            store: Arc::new(store),
            config,
        }
    }

    /// Build a pure-SSH transfer queue. See [`SshBackend::new`].
    pub fn new_ssh(backend: SshBackend, store: Store, config: TransferConfig) -> Self {
        Self {
            backend: Backend::Ssh(Arc::new(backend)),
            store: Arc::new(store),
            config,
        }
    }

    /// Download the given objects into the local store.
    ///
    /// Each object is hashed before it's committed. One that doesn't match
    /// its OID is listed in [`Report::failed`].
    pub async fn download(
        &self,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
        events: Option<UnboundedSender<Event>>,
    ) -> Result<Report, TransferError> {
        self.run(Dir::Download, objects, r#ref, events).await
    }

    /// Upload the given objects from the local store.
    ///
    /// Objects the server already has are listed in [`Report::succeeded`]
    /// without being sent.
    pub async fn upload(
        &self,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
        events: Option<UnboundedSender<Event>>,
    ) -> Result<Report, TransferError> {
        self.run(Dir::Upload, objects, r#ref, events).await
    }

    /// Send only the batch request and return the server's response.
    ///
    /// Uses the same backend as [`download`](Self::download) and
    /// [`upload`](Self::upload), so callers can see what the server wants
    /// without transferring anything. HTTP batch requests are retried.
    /// Neither the request nor the response is split or rewritten
    /// according to the config.
    pub async fn batch(
        &self,
        op: Operation,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
    ) -> Result<BatchResponse, TransferError> {
        let mut req = BatchRequest::new(op, objects);
        if let Some(r) = r#ref {
            req = req.with_ref(r);
        }
        match &self.backend {
            Backend::Http { api, .. } => self.batch_with_retry(api, &req).await,
            Backend::Ssh(ssh_backend) => match ssh_backend.dispatch(op) {
                Transport::Pool(pool) => ssh::batch(pool, &req).await,
                Transport::Http(http) => self.batch_with_retry(&http.api, &req).await,
                Transport::NoFallback(e) => {
                    Err(TransferError::Io(std::io::Error::other(e.to_string())))
                }
            },
        }
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

        // Some servers, including the upstream test server, omit `size`
        // from the response. Fill it back in from the request.
        let req_sizes: std::collections::HashMap<String, u64> =
            objects.iter().map(|o| (o.oid.clone(), o.size)).collect();

        // Largest objects first, so they start early and small ones fill
        // in around them. Upstream sends the batch in the same order, and
        // its tests check it.
        let mut objects = objects;
        objects.sort_by_key(|o| std::cmp::Reverse(o.size));

        let mut req = BatchRequest::new(dir.into(), objects);
        if let Some(r) = r#ref {
            req = req.with_ref(r);
        }
        let resp: BatchResponse = match &self.backend {
            Backend::Http { api, .. } => self.batch_with_retry(api, &req).await?,
            // SSH batch requests aren't retried: a failure there means
            // the SSH session itself is broken.
            Backend::Ssh(ssh_backend) => match ssh_backend.dispatch(dir.into()) {
                Transport::Pool(pool) => ssh::batch(pool, &req).await?,
                Transport::Http(http) => self.batch_with_retry(&http.api, &req).await?,
                Transport::NoFallback(e) => {
                    return Err(TransferError::Io(std::io::Error::other(e.to_string())));
                }
            },
        };

        // An absent or empty `hash_algo` means sha256, the only one
        // supported.
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
            let backend = self.backend.clone();
            let store = self.store.clone();
            let config = self.config.clone();
            let events = events.clone();
            join.spawn(async move {
                let _permit = permit_src.acquire_owned().await.expect("semaphore live");
                let oid = obj.oid.clone();
                let result =
                    process_object(dir, &backend, store, &config, obj, events.as_ref()).await;
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

    /// Send a batch request, retrying like an object transfer.
    ///
    /// Under `GIT_TRACE`, logs `tq: sending batch of size N` on every
    /// attempt, and on each retry one `tq: enqueue retry` line per object,
    /// as upstream does.
    async fn batch_with_retry(
        &self,
        api: &ApiClient,
        req: &BatchRequest,
    ) -> Result<BatchResponse, TransferError> {
        let mut backoff = self.config.initial_backoff;
        let mut retry_count: u32 = 0;
        let mut last_err: Option<git_lfs_api::ApiError> = None;
        for attempt in 0..self.config.max_attempts {
            if trace_enabled() {
                eprintln!("tq: sending batch of size {}", req.objects.len());
            }
            match api.batch(req).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    let retry = e.is_retryable() && attempt + 1 < self.config.max_attempts;
                    if !retry {
                        return Err(TransferError::BatchResponse(Box::new(e)));
                    }
                    let server_delay = e.retry_after();
                    if let Some(wait) = server_delay
                        && wait > self.config.max_retry_time
                    {
                        for obj in &req.objects {
                            trace_retry_refused(&obj.oid, wait, self.config.max_retry_time);
                        }
                        return Err(TransferError::BatchResponse(Box::new(e)));
                    }
                    let delay = server_delay.unwrap_or(backoff);
                    retry_count += 1;
                    if trace_enabled() {
                        let secs = delay.as_secs_f64();
                        for obj in &req.objects {
                            // For a hex OID, {:?} quotes like Go's %q.
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

/// Whether `GIT_TRACE` is set to something other than empty or `0`.
fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Transfer one object from the batch response, with retries.
///
/// Emits `Started`. The caller emits `Completed` or `Failed`.
async fn process_object(
    dir: Dir,
    backend: &Backend,
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

    let transport = resolve_transport(backend, dir);

    match (dir, &obj.actions) {
        (Dir::Download, Some(actions)) => {
            let action = actions
                .download
                .as_ref()
                .ok_or(TransferError::NoDownloadAction)?;
            check_not_expired("download", action)?;
            with_retry(config, &obj.oid, obj.size, || async {
                match &transport {
                    Transport::Http(http) => basic::download(
                        &http.http,
                        store.clone(),
                        &obj.oid,
                        obj.size,
                        action,
                        config.extra_headers.as_ref(),
                        events,
                    )
                    .await
                    .map(|_| ()),
                    Transport::Pool(pool) => {
                        ssh::download(
                            pool.clone(),
                            store.clone(),
                            &obj.oid,
                            obj.size,
                            action,
                            events.cloned(),
                        )
                        .await
                    }
                    Transport::NoFallback(e) => {
                        Err(TransferError::Io(std::io::Error::other(e.to_string())))
                    }
                }
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
                match &transport {
                    Transport::Http(http) => {
                        basic::upload(
                            &http.http,
                            store.clone(),
                            &obj.oid,
                            obj.size,
                            actions,
                            config.detect_content_type,
                            config.max_verifies,
                            config.extra_headers.as_ref(),
                            events,
                        )
                        .await
                    }
                    Transport::Pool(pool) => {
                        ssh::upload(
                            pool.clone(),
                            store.clone(),
                            &obj.oid,
                            obj.size,
                            actions,
                            events.cloned(),
                        )
                        .await
                    }
                    Transport::NoFallback(e) => {
                        Err(TransferError::Io(std::io::Error::other(e.to_string())))
                    }
                }
            })
            .await
        }
        // No actions: the server already has the object.
        (Dir::Upload, None) => Ok(()),
    }
}

/// Pick the transport for `dir`.
fn resolve_transport(backend: &Backend, dir: Dir) -> Transport {
    match backend {
        Backend::Http { api, http } => Transport::Http(HttpTransport {
            api: api.clone(),
            http: http.clone(),
        }),
        Backend::Ssh(ssh_backend) => ssh_backend.dispatch(dir.into()),
    }
}

/// How long an action must stay valid to be used, as upstream's
/// `objectExpirationToTransfer`.
const ACTION_EXPIRATION_BUFFER: Duration = Duration::from_secs(5);

fn check_not_expired(rel: &str, action: &git_lfs_api::Action) -> Result<(), TransferError> {
    if action.is_expired_within(SystemTime::now(), ACTION_EXPIRATION_BUFFER) {
        return Err(TransferError::ActionExpired {
            rel: rel.to_owned(),
        });
    }
    Ok(())
}

/// Run `op` until it succeeds, fails with an error that isn't retryable,
/// or uses up `max_attempts`.
///
/// Waits for the error's `Retry-After` delay when it has one, and
/// otherwise for the exponential backoff. A `Retry-After` delay longer than
/// `max_retry_time` ends the retries.
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
                if let Some(wait) = e.retry_after()
                    && wait > config.max_retry_time
                {
                    trace_retry_refused(oid, wait, config.max_retry_time);
                    last_err = Some(e);
                    break;
                }
                let delay = e.retry_after().unwrap_or(backoff);
                retry_count += 1;
                emit_retry_trace(oid, size, retry_count, delay, &e);
                last_err = Some(e);
                tokio::time::sleep(delay).await;
                // A Retry-After delay doesn't advance the backoff.
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

/// Log under `GIT_TRACE` that a `Retry-After` delay was too long, in
/// upstream's format.
fn trace_retry_refused(oid: &str, wait: Duration, max: Duration) {
    if trace_enabled() {
        eprintln!(
            "tq: refusing to retry {oid:?}, retry after {:.0}s exceeds maximum {}s",
            wait.as_secs_f64(),
            max.as_secs()
        );
    }
}

/// Log a retry under `GIT_TRACE` in upstream's format.
///
/// Upstream logs `tq: retrying object` and `tq: enqueue retry` from
/// separate steps of its queue, and its tests match both, so each retry
/// prints both lines.
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
    // For a hex OID, {:?} quotes like Go's %q.
    eprintln!("tq: enqueue retry #{count} after {secs:.2}s for {oid:?} (size: {size}): {err}");
}
