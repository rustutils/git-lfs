use std::sync::Arc;
use std::time::Duration;

/// Rewrites an action URL from the batch response before it's used.
///
/// Lets the caller apply `url.<base>.insteadOf` rules when
/// `lfs.transfer.enablehrefrewrite` is set.
pub type UrlRewriter = Arc<dyn Fn(&str) -> String + Send + Sync>;

pub use git_lfs_api::HeaderSource;

/// Settings for the transfer queue.
#[derive(Clone)]
pub struct TransferConfig {
    /// Maximum number of objects transferring at once.
    ///
    /// Default 8. Upstream Git LFS scales its default with the CPU count.
    pub concurrency: usize,
    /// Total attempts per object and per HTTP batch request, including
    /// the first.
    ///
    /// Default 9, which is upstream's 8 retries plus the first attempt.
    pub max_attempts: u32,
    /// Delay before the first retry.
    ///
    /// Doubled after each retry that wasn't given a `Retry-After` delay,
    /// up to [`backoff_max`](Self::backoff_max). Default 100 ms.
    pub initial_backoff: Duration,
    /// Upper bound for the backoff delay. Default 30 s.
    pub backoff_max: Duration,
    /// Rewriter for download action URLs.
    pub url_rewriter: Option<UrlRewriter>,
    /// Rewriter for upload and verify action URLs.
    ///
    /// Typically applies `url.<base>.pushInsteadOf`. When `None`,
    /// [`url_rewriter`](Self::url_rewriter) is used instead.
    pub upload_url_rewriter: Option<UrlRewriter>,
    /// Maximum objects per batch request.
    ///
    /// Larger lists are split into chunks of this size, sent one after
    /// another. Default 100, as for upstream's `lfs.transfer.batchSize`.
    /// Zero is treated as 1.
    pub batch_size: usize,
    /// Whether HTTP uploads detect the `Content-Type` from the object.
    ///
    /// Follows `lfs.<url>.contenttype`. When `true` (the default), the
    /// first 512 bytes pick the type. When `false`, uploads send
    /// `application/octet-stream`. A `Content-Type` in the upload action's
    /// headers always wins.
    pub detect_content_type: bool,
    /// Total attempts at the verify action after an HTTP upload.
    ///
    /// Follows `lfs.transfer.maxverifies`. Default 3. The value is used as
    /// given: upstream's minimum of 3 is applied by the caller, and 0
    /// fails every verify without sending it.
    pub max_verifies: u32,
    /// Extra headers for each HTTP transfer request, looked up for its URL.
    ///
    /// The CLI passes the `http.extraHeader` settings.
    pub extra_headers: Option<HeaderSource>,
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
            max_verifies: 3,
            extra_headers: None,
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
            .field("max_verifies", &self.max_verifies)
            .field(
                "extra_headers",
                &self.extra_headers.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}
