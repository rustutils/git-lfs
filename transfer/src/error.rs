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
    /// Not retryable: the server has already classified the object.
    #[error("server error for object: {} ({})", .0.message, .0.code)]
    ServerObject(ObjectError),

    /// The batch response listed the object with neither `actions` nor
    /// `error` for a download — the spec forbids this, but real servers do
    /// it occasionally; we surface it instead of panicking.
    #[error("server returned no download action for object")]
    NoDownloadAction,

    /// The action URL responded with a non-success status.
    #[error("transfer action returned status {status}")]
    ActionStatus { status: u16 },

    /// HTTP transport failure (connection reset, TLS error, …).
    /// Retryable.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// Local I/O while reading the object file (uploads) or the staging
    /// tempfile (downloads).
    #[error("local io error: {0}")]
    Io(#[from] std::io::Error),

    /// The local store rejected the bytes — most importantly, hash mismatch
    /// after a download. Not retryable per attempt: the bytes the server
    /// gave us did not hash to what they promised.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// The OID returned by the server is not valid hex.
    #[error("invalid oid from server: {0}")]
    InvalidOid(#[from] OidParseError),
}

impl TransferError {
    /// Worth another attempt? Network blips and 5xx are retryable; spec
    /// violations and hash mismatches are not.
    pub fn is_retryable(&self) -> bool {
        match self {
            TransferError::Http(e) => {
                // reqwest::Error doesn't expose enough to be precise — treat
                // any non-decode transport error as retryable. Hash mismatch
                // surfaces via Store, not Http.
                !e.is_decode() && !e.is_builder()
            }
            TransferError::ActionStatus { status } => {
                matches!(status, 408 | 429 | 500..=599)
            }
            TransferError::Io(_) => true,
            TransferError::ServerObject(_)
            | TransferError::NoDownloadAction
            | TransferError::Store(_)
            | TransferError::InvalidOid(_) => false,
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
#[derive(Debug, Default)]
pub struct Report {
    /// OIDs of objects that completed successfully.
    pub succeeded: Vec<String>,
    /// OIDs and reasons for objects that ultimately failed.
    pub failed: Vec<(String, TransferError)>,
}

impl Report {
    pub fn is_complete_success(&self) -> bool {
        self.failed.is_empty()
    }
}
