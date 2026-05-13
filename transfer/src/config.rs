use std::sync::Arc;
use std::time::Duration;

/// Optional URL transform applied to every action `href` returned by the
/// batch endpoint before the transfer adapter dials it.
///
/// Used to plumb `lfs.transfer.enablehrefrewrite` + `url.<base>.insteadOf`
/// from the caller (which has the git-config context) down into the queue.
pub type UrlRewriter = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Tunables for the transfer queue.
///
/// Defaults aim at "sensible for a developer laptop on a corporate
/// VPN": not too aggressive on concurrency, generous retries for
/// flaky links. Upstream Git LFS scales `concurrency` to CPU count;
/// this crate hard-codes 8 and lets callers override.
#[derive(Clone)]
pub struct TransferConfig {
    /// Max number of concurrent in-flight transfers.
    pub concurrency: usize,
    /// Total attempts per object, including the first.
    ///
    /// 9 means "try once, then up to 8 retries". Matches upstream's
    /// `defaultMaxRetries = 8` (upstream counts retries; we count
    /// attempts, hence +1).
    pub max_attempts: u32,
    /// Sleep before the first retry. Doubled before each subsequent retry,
    /// capped at [`backoff_max`](Self::backoff_max).
    pub initial_backoff: Duration,
    /// Upper bound for exponential backoff between retries.
    pub backoff_max: Duration,
    /// Optional rewriter applied to download action URLs before
    /// transfer. Carries `url.<base>.insteadOf` when
    /// `lfs.transfer.enablehrefrewrite=true`. `None` ⇒ no rewriting.
    pub url_rewriter: Option<UrlRewriter>,
    /// Optional rewriter applied to upload + verify action URLs. Carries
    /// `url.<base>.pushInsteadOf` (falling back to `insteadOf`) when
    /// `lfs.transfer.enablehrefrewrite=true`. `None` ⇒ no push-direction
    /// rewriting, in which case the upload-side falls back to
    /// `url_rewriter`.
    pub upload_url_rewriter: Option<UrlRewriter>,
    /// Max objects per `POST /objects/batch` call. The transfer queue
    /// chunks the input list into runs of this size and issues one
    /// batch API call per chunk. Default: 100 (matches upstream's
    /// `lfs.transfer.batchSize` default). Values < 1 are clamped to 1.
    pub batch_size: usize,
    /// `lfs.<url>.contenttype` — when `true` (default), the basic
    /// upload adapter sniffs the first 512 bytes of each object and
    /// sets the `Content-Type` header on the action PUT to the
    /// detected MIME type. When `false`, the adapter sends
    /// `application/octet-stream` so a misconfigured CDN can't reject
    /// the upload based on its content sniffing. Honored only when
    /// the batch response didn't already set a `Content-Type` in
    /// `action.header`.
    pub detect_content_type: bool,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            concurrency: 8,
            max_attempts: 9,
            initial_backoff: Duration::from_millis(100),
            backoff_max: Duration::from_secs(30),
            url_rewriter: None,
            upload_url_rewriter: None,
            batch_size: 100,
            detect_content_type: true,
        }
    }
}

impl std::fmt::Debug for TransferConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferConfig")
            .field("concurrency", &self.concurrency)
            .field("max_attempts", &self.max_attempts)
            .field("initial_backoff", &self.initial_backoff)
            .field("backoff_max", &self.backoff_max)
            .field("url_rewriter", &self.url_rewriter.as_ref().map(|_| "<fn>"))
            .field(
                "upload_url_rewriter",
                &self.upload_url_rewriter.as_ref().map(|_| "<fn>"),
            )
            .field("batch_size", &self.batch_size)
            .field("detect_content_type", &self.detect_content_type)
            .finish()
    }
}
