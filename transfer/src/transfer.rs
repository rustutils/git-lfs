//! The transfer queue: batch request, then concurrent object transfers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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
use crate::custom::Workers;
use crate::error::{Report, TransferError};
use crate::event::Event;
use crate::http_clients::HttpClients;
use crate::sshtransfer::adapter as ssh;
use crate::tus;

/// Direction of a transfer batch.
#[derive(Debug, Clone, Copy)]
enum Dir {
    Download,
    Upload,
}

impl Dir {
    fn is_upload(self) -> bool {
        matches!(self, Dir::Upload)
    }
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
        Self::with_http_clients(api, store, config, HttpClients::single(http))
    }

    /// Build an HTTP transfer queue that picks a client for each action
    /// URL, so each host can have its own TLS settings.
    pub fn with_http_clients(
        api: ApiClient,
        store: Store,
        config: TransferConfig,
        http: HttpClients,
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

        let mut session = Session::new(self.config.concurrency.max(1));
        let result = self
            .run_rounds(dir, objects, r#ref, events, &mut session)
            .await;
        if let Some(workers) = session.workers {
            workers.shutdown().await;
        }
        basic::close_zstd_slots(&session.zstd_slots);
        result
    }

    /// Transfer `objects`, retrying failures in rounds.
    ///
    /// As in upstream, each round sends a new batch request with the
    /// objects whose retry delay has passed, so a retry gets fresh actions
    /// instead of reusing an expired one.
    async fn run_rounds(
        &self,
        dir: Dir,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
        events: Option<UnboundedSender<Event>>,
        session: &mut Session,
    ) -> Result<Report, TransferError> {
        let mut report = Report::default();
        let mut retries: HashMap<String, RetryState> = HashMap::new();
        let mut waiting: Vec<(Instant, ObjectSpec)> = Vec::new();
        let mut next = objects;
        loop {
            let results = self
                .run_batch(dir, next, r#ref.clone(), events.clone(), session)
                .await?;
            for (spec, result) in results {
                match result {
                    Ok(()) => {
                        if let Some(s) = &events {
                            let _ = s.send(Event::Completed {
                                oid: spec.oid.clone(),
                            });
                        }
                        report.succeeded.push(spec.oid);
                    }
                    Err(err) => match self.retry_delay(&spec, &err, &mut retries) {
                        Some(delay) => waiting.push((Instant::now() + delay, spec)),
                        None => {
                            if let Some(s) = &events {
                                let _ = s.send(Event::Failed {
                                    oid: spec.oid.clone(),
                                    error: err.to_string(),
                                });
                            }
                            report.failed.push((spec.oid, err));
                        }
                    },
                }
            }
            let Some(earliest) = waiting.iter().map(|(at, _)| *at).min() else {
                return Ok(report);
            };
            tokio::time::sleep_until(earliest.into()).await;
            let now = Instant::now();
            let (ready, later) = waiting.into_iter().partition(|(at, _)| *at <= now);
            waiting = later;
            next = ready.into_iter().map(|(_, spec)| spec).collect();
        }
    }

    /// Decide whether a failed object is retried, and after how long.
    ///
    /// Waits for the error's `Retry-After` delay when it has one, and
    /// otherwise for the object's exponential backoff. Returns `None` when
    /// the error isn't retryable, the object has used up `max_attempts`,
    /// or a `Retry-After` delay is longer than `max_retry_time`.
    fn retry_delay(
        &self,
        spec: &ObjectSpec,
        err: &TransferError,
        retries: &mut HashMap<String, RetryState>,
    ) -> Option<Duration> {
        let config = &self.config;
        let state = retries.entry(spec.oid.clone()).or_insert(RetryState {
            attempts: 0,
            backoff: config.initial_backoff,
        });
        state.attempts += 1;
        if !err.is_retryable() || state.attempts >= config.max_attempts {
            return None;
        }
        let server_delay = err.retry_after();
        if let Some(wait) = server_delay
            && wait > config.max_retry_time
        {
            trace_retry_refused(&spec.oid, wait, config.max_retry_time);
            return None;
        }
        let delay = server_delay.unwrap_or(state.backoff);
        // A Retry-After delay doesn't advance the backoff.
        if server_delay.is_none() {
            state.backoff = (state.backoff * 2).min(config.backoff_max);
        }
        emit_retry_trace(&spec.oid, spec.size, state.attempts, delay, err);
        Some(delay)
    }

    /// Send one batch request for `objects` and make one attempt at each
    /// object in the response.
    ///
    /// Returns each object's outcome, including objects the response
    /// didn't ask for, which fail with
    /// [`UnknownOid`](TransferError::UnknownOid). Emits `Started` the first
    /// time an object starts. The caller emits `Completed` or `Failed`.
    async fn run_batch(
        &self,
        dir: Dir,
        objects: Vec<ObjectSpec>,
        r#ref: Option<Ref>,
        events: Option<UnboundedSender<Event>>,
        session: &mut Session,
    ) -> Result<Vec<(ObjectSpec, Result<(), TransferError>)>, TransferError> {
        // Some servers, including the upstream test server, omit `size`
        // from the response. Fill it back in from the request.
        let req_sizes: HashMap<String, u64> =
            objects.iter().map(|o| (o.oid.clone(), o.size)).collect();

        // Largest objects first, so they start early and small ones fill
        // in around them. Upstream sends the batch in the same order, and
        // its tests check it.
        let mut objects = objects;
        objects.sort_by_key(|o| std::cmp::Reverse(o.size));

        let mut req = BatchRequest::new(dir.into(), objects)
            .with_transfers(self.config.transfer_names(dir.is_upload()));
        if let Some(r) = r#ref {
            req = req.with_ref(r);
        }
        // The adapter name, for upstream's `tq: starting transfer adapter`
        // trace.
        let mut adapter = "basic";
        let standalone = self
            .config
            .standalone
            .as_ref()
            .filter(|a| a.handles(dir.is_upload()));
        let resp: BatchResponse = match &self.backend {
            // A standalone adapter needs nothing from the server.
            _ if standalone.is_some() => BatchResponse {
                transfer: None,
                objects: req
                    .objects
                    .iter()
                    .map(|o| ObjectResult {
                        oid: o.oid.clone(),
                        size: o.size,
                        authenticated: None,
                        actions: None,
                        error: None,
                    })
                    .collect(),
                hash_algo: None,
            },
            Backend::Http { api, .. } => self.batch_with_retry(api, &req).await?,
            // SSH batch requests aren't retried: a failure there means
            // the SSH session itself is broken.
            Backend::Ssh(ssh_backend) => match ssh_backend.dispatch(dir.into()) {
                Transport::Pool(pool) => {
                    adapter = "ssh";
                    ssh::batch(pool, &req).await?
                }
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

        // The server picks a custom adapter by name. Any other choice,
        // or none, means `basic`, as in upstream.
        let custom = match adapter {
            _ if standalone.is_some() => standalone,
            "basic" => resp
                .transfer
                .as_deref()
                .and_then(|t| self.config.custom_adapter(t, dir.is_upload())),
            _ => None,
        };
        if let Some(c) = custom {
            adapter = &c.name;
        }
        // The server may pick tus for uploads, when it's enabled.
        let tus = adapter == "basic"
            && custom.is_none()
            && dir.is_upload()
            && self.config.tus
            && !self.config.basic_transfers_only
            && resp.transfer.as_deref() == Some("tus");
        if tus {
            adapter = "tus";
        }

        let limit = Arc::new(Semaphore::new(self.config.concurrency.max(1)));
        let free_slots = Arc::new(std::sync::Mutex::new(
            session
                .zstd_slots
                .iter()
                .cloned()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let mut join: JoinSet<(String, Result<(), TransferError>)> = JoinSet::new();

        let mut results = Vec::new();
        for mut obj in resp.objects {
            // Only act on objects this batch asked for, as upstream does.
            // An upload of any other OID would send whatever the store has
            // under it.
            if !req_sizes.contains_key(&obj.oid) {
                let spec = ObjectSpec {
                    oid: obj.oid,
                    size: obj.size,
                };
                results.push((spec, Err(TransferError::UnknownOid)));
                continue;
            }
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
            // Like upstream, start the adapter only once an object needs
            // transferring.
            if (obj.actions.is_some() || standalone.is_some()) && !session.adapter_started {
                session.adapter_started = true;
                if trace_enabled() {
                    eprintln!("tq: starting transfer adapter {adapter:?}");
                }
                session.workers = custom.map(|c| {
                    Arc::new(Workers::new(
                        c.clone(),
                        dir.is_upload(),
                        self.config.remote.clone(),
                        self.config.concurrency,
                    ))
                });
            }
            // The adapter started in the first round keeps running for
            // the retries.
            let custom_workers = custom.and(session.workers.clone());
            let announce = session.started.insert(obj.oid.clone());
            let permit_src = limit.clone();
            let free_slots = free_slots.clone();
            let backend = self.backend.clone();
            let store = self.store.clone();
            let config = self.config.clone();
            let events = events.clone();
            join.spawn(async move {
                let _permit = permit_src.acquire_owned().await.expect("semaphore live");
                // There's a slot for each permit.
                let slot = free_slots.lock().unwrap().pop_front();
                let oid = obj.oid.clone();
                let result = process_object(
                    dir,
                    &backend,
                    custom_workers.as_deref(),
                    tus,
                    slot.as_ref(),
                    store,
                    &config,
                    obj,
                    announce,
                    events.as_ref(),
                )
                .await;
                if let Some(slot) = slot {
                    free_slots.lock().unwrap().push_back(slot);
                }
                (oid, result)
            });
        }

        while let Some(joined) = join.join_next().await {
            let (oid, result) =
                joined.map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
            let size = req_sizes.get(&oid).copied().unwrap_or(0);
            results.push((ObjectSpec { oid, size }, result));
        }
        Ok(results)
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
                    if matches!(e, git_lfs_api::ApiError::ControlCharacterInHref { .. }) {
                        return Err(TransferError::BatchRequest(Box::new(e)));
                    }
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

/// State shared by the rounds of one transfer.
struct Session {
    /// The custom adapter's processes, started with the first transfer.
    workers: Option<Arc<Workers>>,
    /// Whether the `starting transfer adapter` trace was printed.
    adapter_started: bool,
    /// One zstd decoder slot per concurrent download, handed out in turn,
    /// so concurrent downloads get different slots.
    zstd_slots: Vec<basic::ZstdSlot>,
    /// OIDs that have had their `Started` event.
    started: HashSet<String>,
}

impl Session {
    fn new(concurrency: usize) -> Self {
        Self {
            workers: None,
            adapter_started: false,
            zstd_slots: (0..concurrency)
                .map(|_| basic::ZstdSlot::default())
                .collect(),
            started: HashSet::new(),
        }
    }
}

/// An object's attempts so far and its next backoff delay.
struct RetryState {
    attempts: u32,
    backoff: Duration,
}

/// Whether `GIT_TRACE` is set to something other than empty or `0`.
fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Make one attempt at transferring an object from the batch response.
///
/// Emits `Started` when `announce` is set. The caller emits `Completed`
/// or `Failed`, or retries the object in a new batch.
#[allow(clippy::too_many_arguments)]
async fn process_object(
    dir: Dir,
    backend: &Backend,
    custom: Option<&Workers>,
    tus: bool,
    zstd_slot: Option<&basic::ZstdSlot>,
    store: Arc<Store>,
    config: &TransferConfig,
    obj: ObjectResult,
    announce: bool,
    events: Option<&UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    if let Some(err) = obj.error {
        return Err(TransferError::ServerObject(err));
    }

    if announce && let Some(s) = events {
        let _ = s.send(Event::Started {
            oid: obj.oid.clone(),
            size: obj.size,
        });
    }

    let transport = resolve_transport(backend, dir);
    let action_auth = action_auth(config, &obj);

    if let Some(workers) = custom {
        let standalone = config
            .standalone
            .as_ref()
            .is_some_and(|a| a.handles(dir.is_upload()));
        return process_custom(
            dir, workers, standalone, &transport, store, config, &obj, events,
        )
        .await;
    }

    match (dir, &obj.actions) {
        (Dir::Download, Some(actions)) => {
            let action = actions
                .download
                .as_ref()
                .ok_or(TransferError::NoDownloadAction)?;
            check_not_expired("download", action)?;
            async {
                match &transport {
                    Transport::Http(http) => basic::download(
                        &http.http,
                        store.clone(),
                        &obj.oid,
                        obj.size,
                        action,
                        config.extra_headers.as_ref(),
                        config.download_encoding.as_ref(),
                        zstd_slot,
                        action_auth,
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
            }
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
            async {
                match &transport {
                    Transport::Http(http) if tus => {
                        tus::upload(
                            &http.http,
                            store.clone(),
                            &obj.oid,
                            obj.size,
                            actions,
                            config.max_verifies,
                            config.extra_headers.as_ref(),
                            action_auth,
                            events,
                        )
                        .await
                    }
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
                            action_auth,
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
            }
            .await
        }
        // No actions: the server already has the object.
        (Dir::Upload, None) => Ok(()),
    }
}

/// Transfer one object through a custom adapter.
///
/// After an upload, the verify action is sent over HTTP, as in upstream.
/// A `standalone` adapter gets no actions, and uploads every object.
#[allow(clippy::too_many_arguments)]
async fn process_custom(
    dir: Dir,
    workers: &Workers,
    standalone: bool,
    transport: &Transport,
    store: Arc<Store>,
    config: &TransferConfig,
    obj: &ObjectResult,
    events: Option<&UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    let actions = obj.actions.as_ref();
    match dir {
        Dir::Download => {
            let action = actions.and_then(|a| a.download.as_ref());
            if !standalone {
                let action = action.ok_or(TransferError::NoDownloadAction)?;
                check_not_expired("download", action)?;
            }
            workers
                .download(store.clone(), &obj.oid, obj.size, action, events)
                .await
        }
        Dir::Upload => {
            let action = actions.and_then(|a| a.upload.as_ref());
            if !standalone {
                // No upload action: the server already has the object.
                let Some(action) = action else {
                    return Ok(());
                };
                check_not_expired("upload", action)?;
            }
            let verify = actions.and_then(|a| a.verify.as_ref());
            if let Some(v) = verify {
                check_not_expired("verify", v)?;
            }
            workers
                .upload(&store, &obj.oid, obj.size, action, events)
                .await?;
            match (verify, transport) {
                (Some(v), Transport::Http(http)) => {
                    basic::verify(
                        &http.http,
                        &obj.oid,
                        obj.size,
                        v,
                        config.max_verifies,
                        config.extra_headers.as_ref(),
                        action_auth(config, obj),
                    )
                    .await
                }
                _ => Ok(()),
            }
        }
    }
}

/// The credentials source for `obj`'s actions, unless the server said
/// they're already authenticated.
fn action_auth<'a>(
    config: &'a TransferConfig,
    obj: &ObjectResult,
) -> Option<&'a Arc<dyn crate::ActionAuth>> {
    if obj.authenticated == Some(true) {
        return None;
    }
    config.action_auth.as_ref()
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
    let now = SystemTime::now();
    if let Some(at) = action.expiration(now)
        && at < now + ACTION_EXPIRATION_BUFFER
    {
        return Err(TransferError::ActionExpired {
            rel: rel.to_owned(),
            at,
        });
    }
    Ok(())
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
