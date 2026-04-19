use serde::{Deserialize, Serialize};

/// The standard error body returned by the LFS server for non-2xx responses.
///
/// Defined in `docs/api/batch.md` § "Response Errors". The same shape is
/// reused by the locking endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
}

/// Errors returned by the API client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Network / TLS / connection-level failure.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Server returned a non-success HTTP status. `body` is `Some` if the
    /// response had a parseable LFS error body. `lfs_authenticate` mirrors
    /// the `LFS-Authenticate` response header (only present on 401).
    #[error("server returned status {status}{}", body.as_ref().map(|b| format!(": {}", b.message)).unwrap_or_default())]
    Status {
        status: u16,
        lfs_authenticate: Option<String>,
        body: Option<ServerError>,
    },

    /// JSON body did not match the expected schema.
    #[error("malformed response body: {0}")]
    Decode(String),

    /// Failed to construct the request URL from the endpoint.
    #[error("url error: {0}")]
    Url(#[from] url::ParseError),
}

impl ApiError {
    /// True for 401 responses — caller should resolve credentials and retry.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, ApiError::Status { status: 401, .. })
    }

    /// True for 403 responses — caller lacks permission for this operation.
    pub fn is_forbidden(&self) -> bool {
        matches!(self, ApiError::Status { status: 403, .. })
    }

    /// True for 404 responses.
    pub fn is_not_found(&self) -> bool {
        matches!(self, ApiError::Status { status: 404, .. })
    }

    /// True for 5xx and 408/429 — transient errors a caller may want to retry.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ApiError::Transport(_)
                | ApiError::Status { status: 408 | 429 | 500..=599, .. }
        )
    }
}
