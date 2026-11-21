use std::time::Duration;

use git_lfs_api::{ApiError, ObjectError};
use git_lfs_pointer::OidParseError;
use git_lfs_store::StoreError;

/// Why a per-object transfer failed.
///
/// Errors for which [`is_retryable`](Self::is_retryable) is true are retried
/// up to [`TransferConfig::max_attempts`](crate::TransferConfig::max_attempts)
/// times. Others fail the object at once.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// The batch response carried an error for this object.
    #[error("server error for object: {} ({})", .0.message, .0.code)]
    ServerObject(ObjectError),

    /// The batch response gave a download no download action and no error.
    #[error("server returned no download action for object")]
    NoDownloadAction,

    /// An action URL answered with a non-success status.
    ///
    /// `url` has its query string removed, so signed URL parameters don't
    /// end up in messages. It shows the URL after any `insteadOf`
    /// rewriting. `retry_after` holds the response's `Retry-After` delay.
    #[error("{}", format_action_status(*.status, .url))]
    ActionStatus {
        status: u16,
        url: String,
        retry_after: Option<Duration>,
    },

    /// HTTP transport failure, such as a reset connection or a TLS error.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// Local I/O failure, or another failure carried as a message, such as
    /// an SSH transport error.
    #[error("local io error: {0}")]
    Io(#[from] std::io::Error),

    /// The store rejected the bytes, most often because a download didn't
    /// hash to its OID.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// An OID is not valid hex.
    #[error("invalid oid from server: {0}")]
    InvalidOid(#[from] OidParseError),

    /// The batch response named a hash algorithm other than `sha256`.
    #[error("unsupported hash algorithm: {0}")]
    UnsupportedHashAlgo(String),

    /// The HTTP batch request failed.
    ///
    /// Displays with upstream's `batch response:` prefix.
    #[error("batch response: {0}")]
    BatchResponse(Box<ApiError>),

    /// An action expires within 5 seconds, so it wasn't used.
    ///
    /// Upstream requests a fresh batch and retries. This crate fails the
    /// object instead.
    #[error("action \"{rel}\" expired")]
    ActionExpired { rel: String },

    /// An upload was redirected from HTTPS to HTTP.
    #[error("refusing insecure redirect: HTTPS to HTTP")]
    InsecureRedirect,

    /// A download or verify request was redirected from HTTPS to HTTP, or
    /// too many times.
    #[error("{0}")]
    Redirect(String),

    /// Every verify attempt failed. Holds the last attempt's error.
    ///
    /// Not retried, since verify has its own budget,
    /// [`TransferConfig::max_verifies`](crate::TransferConfig::max_verifies).
    #[error("{0}")]
    VerifyExhausted(Box<TransferError>),
}

/// Format an action status error with upstream's `lfshttp.defaultError`
/// wording, which the upstream tests match.
///
/// Statuses upstream treats as fatal (5xx except 501, 507, and 509) get the
/// `Fatal error:` prefix. The rest get `LFS:`.
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
    /// Whether the failed attempt is worth repeating.
    ///
    /// True for transport errors, local I/O errors, action statuses 408,
    /// 429, and 5xx, and batch errors the [`ApiError`] calls retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            TransferError::Http(e) => {
                // reqwest::Error doesn't say enough to be precise, so treat
                // everything but decode and builder errors as transient.
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
            | TransferError::InsecureRedirect
            | TransferError::Redirect(_)
            | TransferError::VerifyExhausted(_) => false,
            TransferError::BatchResponse(e) => e.is_retryable(),
        }
    }

    /// The delay from an action response's `Retry-After` header.
    ///
    /// When set, the retry loop waits this long instead of backing off.
    /// Always `None` for [`BatchResponse`](Self::BatchResponse), whose
    /// delay is read from the [`ApiError`] directly.
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
            // Other errors keep only their message. Io is retryable, so
            // this loses the ApiError's own retry classification.
            other => TransferError::Io(std::io::Error::other(other.to_string())),
        }
    }
}

/// Outcome of a download or upload, by OID.
///
/// Together the two lists cover every object in the batch response.
#[derive(Debug, Default)]
pub struct Report {
    /// OIDs of objects that completed successfully.
    pub succeeded: Vec<String>,
    /// OIDs of objects that failed, with the reason.
    pub failed: Vec<(String, TransferError)>,
}

impl Report {
    /// Whether no object failed.
    pub fn is_complete_success(&self) -> bool {
        self.failed.is_empty()
    }
}
