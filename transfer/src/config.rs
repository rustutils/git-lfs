use std::time::Duration;

/// Tunables for the transfer queue.
///
/// Defaults aim at "sensible for a developer laptop on a corporate VPN" —
/// not too aggressive on concurrency, generous retries for flaky links.
/// Upstream Git LFS scales `concurrency` to CPU count (commit `aa08c37f`);
/// we hard-code 8 for v0 and let callers override.
#[derive(Debug, Clone)]
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
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            concurrency: 8,
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            backoff_max: Duration::from_secs(30),
        }
    }
}
