use std::sync::Arc;
use std::time::Duration;

/// Optional URL transform applied to every action `href` returned by the
/// batch endpoint before the transfer adapter dials it. Used to plumb
/// `lfs.transfer.enablehrefrewrite` + `url.<base>.insteadOf` from the
/// caller (which has the git-config context) down into the queue.
pub type UrlRewriter = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Tunables for the transfer queue.
///
/// Defaults aim at "sensible for a developer laptop on a corporate VPN" —
/// not too aggressive on concurrency, generous retries for flaky links.
/// Upstream Git LFS scales `concurrency` to CPU count (commit `aa08c37f`);
/// we hard-code 8 for v0 and let callers override.
#[derive(Clone)]
pub struct TransferConfig {
    /// Max number of concurrent in-flight transfers.
    pub concurrency: usize,
    /// Total attempts per object — including the first. So 3 means "try
    /// once, then up to 2 retries".
    pub max_attempts: u32,
    /// Sleep before the first retry. Doubled before each subsequent retry,
    /// capped at [`backoff_max`](Self::backoff_max).
    pub initial_backoff: Duration,
    /// Upper bound for exponential backoff between retries.
    pub backoff_max: Duration,
    /// Optional rewriter applied to every action URL before transfer.
    /// `None` ⇒ use action URLs verbatim.
    pub url_rewriter: Option<UrlRewriter>,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            concurrency: 8,
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            backoff_max: Duration::from_secs(30),
            url_rewriter: None,
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
            .finish()
    }
}
