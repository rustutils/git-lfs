//! Top-level transfer orchestrator: batch + concurrent per-object transfer.

use std::sync::Arc;

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
        // GIT_TRACE breadcrumb mirroring upstream's `tq:` line in
        // `tq/transfer_queue.go`. t-alternates greps for it to verify
        // the queue *did* (or didn't) reach the server when the local
        // alternates store should have satisfied the lookup.
        if std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0") {
            eprintln!("tq: sending batch of size {}", req.objects.len());
        }
        let resp: BatchResponse = self.api.batch(&req).await?;

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
            if let Some(rewriter) = &self.config.url_rewriter
                && let Some(actions) = obj.actions.as_mut()
            {
                for action in [
                    actions.download.as_mut(),
                    actions.upload.as_mut(),
                    actions.verify.as_mut(),
                ]
                .into_iter()
                .flatten()
                {
                    action.href = rewriter(&action.href);
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
            with_retry(config, || async {
                basic::download(http, store.clone(), &obj.oid, action, events)
                    .await
                    .map(|_| ())
            })
            .await
        }
        (Dir::Download, None) => Err(TransferError::NoDownloadAction),
        (Dir::Upload, Some(actions)) => {
            with_retry(config, || async {
                basic::upload(http, store.clone(), &obj.oid, obj.size, actions, events).await
            })
            .await
        }
        (Dir::Upload, None) => {
            // Server already has it — no actions means no-op, treated as success.
            Ok(())
        }
    }
}

/// Run `op` with exponential-backoff retry. Stops on non-retryable errors
/// or when `max_attempts` is reached.
async fn with_retry<F, Fut>(config: &TransferConfig, mut op: F) -> Result<(), TransferError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), TransferError>>,
{
    let mut backoff = config.initial_backoff;
    let mut last_err: Option<TransferError> = None;
    for attempt in 0..config.max_attempts {
        match op().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let retry = e.is_retryable() && attempt + 1 < config.max_attempts;
                last_err = Some(e);
                if !retry {
                    break;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(config.backoff_max);
            }
        }
    }
    Err(last_err.expect("loop ran at least once"))
}
