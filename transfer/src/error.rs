use std::time::Duration;

use git_lfs_api::{ApiError, ObjectError};
use git_lfs_pointer::OidParseError;
use git_lfs_store::StoreError;

/// Why a per-object transfer failed.
///
/// Errors with `is_retryable() == true` are retried by the queue up to
/// [`TransferConfig::max_attempts`](crate::TransferConfig::max_attempts);
/// everything else fails fast.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// The batch endpoint returned a per-object error (404, 410, 422, …).
    ///
    /// Not retryable: the server has already classified the object.
    #[error("server error for object: {} ({})", .0.message, .0.code)]
    ServerObject(ObjectError),

    /// The batch response listed the object with neither `actions` nor
    /// `error` for a download.
    ///
    /// The spec forbids this, but real servers do it occasionally;
    /// surfaced here instead of panicking.
    #[error("server returned no download action for object")]
    NoDownloadAction,

    /// The action URL responded with a non-success status.
    ///
    /// The URL is embedded in the [`Display`](std::fmt::Display)
    /// impl so users can see *which* endpoint failed (in
    /// particular, what `insteadOf` rewriting did to the original
    /// batch URL).
    ///
    /// `retry_after` carries the parsed `Retry-After` response
    /// header when present; see [`retry_after`](Self::retry_after).
    #[error("{}", format_action_status(*.status, .url))]
    ActionStatus {
        status: u16,
        url: String,
        retry_after: Option<Duration>,
    },

    /// HTTP transport failure (connection reset, TLS error, …). Retryable.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// Local I/O while reading the object file (uploads) or the staging
    /// tempfile (downloads).
    #[error("local io error: {0}")]
    Io(#[from] std::io::Error),

    /// The local store rejected the bytes (most importantly, a hash
    /// mismatch after a download).
    ///
    /// Not retryable per attempt: the bytes the server gave us did
    /// not hash to what they promised.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// The OID returned by the server is not valid hex.
    #[error("invalid oid from server: {0}")]
    InvalidOid(#[from] OidParseError),

    /// The batch response advertised a `hash_algo` we don't implement.
    ///
    /// Per the spec the only required value is `sha256`; anything
    /// else would mean recomputing every OID under a different
    /// digest before trusting the server's actions.
    #[error("unsupported hash algorithm: {0}")]
    UnsupportedHashAlgo(String),

    /// The batch endpoint itself failed (network, auth, or decode).
    ///
    /// Wraps the underlying [`ApiError`] with upstream's `batch
    /// response:` prefix so a `Display` of this error matches what
    /// users see in `GIT_TRACE` logs and shell-test grep patterns.
    #[error("batch response: {0}")]
    BatchResponse(Box<ApiError>),

    /// The action URL the server returned is already expired (or
    /// expires within the safety buffer).
    ///
    /// Surfacing this before the upload/download avoids hitting an
    /// action that's guaranteed to fail. Upstream re-requests a
    /// fresh batch and retries; this crate fails fast for now.
    #[error("action \"{rel}\" expired")]
    ActionExpired { rel: String },

    /// The verify action failed after exhausting `lfs.transfer.maxverifies`
    /// attempts. Not retryable at the outer queue level because verify
    /// owns its own retry budget. Carries the last underlying error so
    /// the user still sees *why* verify failed.
    #[error("{0}")]
    VerifyExhausted(Box<TransferError>),
}

/// Format the action-URL error message to match upstream's
/// `lfshttp.defaultError` strings — the test suite greps these
/// verbatim (e.g. t-pull's `pull with invalid insteadof`).
///
/// Statuses that upstream wraps with `NewFatalError` (5xx except 501,
/// 507, 509) format with the `Fatal error:` prefix that
/// `t-batch-storage-retries.sh` greps for. Everything else uses the
/// `LFS:` prefix that upstream's wrap-with-empty-message default emits.
fn format_action_status(status: u16, url: &str) -> String {
    let (fatal, prefix) = match status {
        400 => (false, "Client error:"),
        401 | 403 => (false, "Authorization error:"),
        404 => (false, "Repository or object not found:"),
        422 => (false, "Unprocessable entity:"),
        429 => (false, "Rate limit exceeded:"),
        500 => (true, "Server error:"),
        501 => (false, "Not Implemented:"),
        503 => (true, "LFS is temporarily unavailable:"),
        507 => (false, "Insufficient server storage:"),
        509 => (false, "Bandwidth limit exceeded:"),
        _ if status < 500 => return format!("LFS: Client error {url} from HTTP {status}"),
        _ => return format!("Fatal error: Server error {url} from HTTP {status}"),
    };
    if fatal {
        format!("Fatal error: {prefix} {url}")
    } else {
        format!("LFS: {prefix} {url}")
    }
}

impl TransferError {
    /// Worth another attempt?
    ///
    /// Network blips and 5xx are retryable; spec violations and
    /// hash mismatches are not.
    pub fn is_retryable(&self) -> bool {
        match self {
            TransferError::Http(e) => {
                // reqwest::Error doesn't expose enough to be precise — treat
                // any non-decode transport error as retryable. Hash mismatch
                // surfaces via Store, not Http.
                !e.is_decode() && !e.is_builder()
            }
            TransferError::ActionStatus { status, .. } => {
                matches!(status, 408 | 429 | 500..=599)
            }
            TransferError::Io(_) => true,
            TransferError::ServerObject(_)
            | TransferError::NoDownloadAction
            | TransferError::Store(_)
            | TransferError::InvalidOid(_)
            | TransferError::UnsupportedHashAlgo(_)
            | TransferError::ActionExpired { .. }
            | TransferError::VerifyExhausted(_) => false,
            // Defer to the wrapped ApiError. A 5xx batch response is
            // worth retrying; a credential-not-found is not.
            TransferError::BatchResponse(e) => e.is_retryable(),
        }
    }

    /// Server-supplied retry delay, if any.
    ///
    /// Pulled from the `Retry-After` response header at
    /// error-construction time. The retry loop uses this in place
    /// of exponential backoff when `Some`. Mirrors upstream's
    /// `errors.IsRetriableLaterError` gate. Batch-level
    /// Retry-After isn't surfaced through `BatchResponse` yet.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            TransferError::ActionStatus { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl From<ApiError> for TransferError {
    fn from(value: ApiError) -> Self {
        match value {
            ApiError::Transport(e) => TransferError::Http(e),
            other => {
                // Typed Status / Decode / Url. Wrap as Io with the original
                // message — this only fires on the batch call, which is
                // upstream of per-object retry, so we never inspect this.
                TransferError::Io(std::io::Error::other(other.to_string()))
            }
        }
    }
}

/// Aggregate outcome of a transfer batch.
///
/// Successful OIDs land in `succeeded`; each failing OID gets a
/// typed [`TransferError`] paired with it in `failed`. The two
/// vectors together cover every object the caller asked for.
#[derive(Debug, Default)]
pub struct Report {
    /// OIDs of objects that completed successfully.
    pub succeeded: Vec<String>,
    /// OIDs and reasons for objects that ultimately failed.
    pub failed: Vec<(String, TransferError)>,
}

impl Report {
    /// `true` when every object in the batch transferred without error.
    pub fn is_complete_success(&self) -> bool {
        self.failed.is_empty()
    }
}
