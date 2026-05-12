use std::time::Duration;

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
    /// the `LFS-Authenticate` response header (only present on 401). `url`
    /// is the request URL — used by the `Display` impl to format
    /// `Authorization error: <url>` for 401/403, mirroring upstream's
    /// `lfshttp.defaultError` strings.
    #[error("{}", format_status(*status, url.as_deref(), body.as_ref()))]
    Status {
        status: u16,
        url: Option<String>,
        lfs_authenticate: Option<String>,
        body: Option<ServerError>,
        /// Parsed `Retry-After` response header (delta-seconds today;
        /// RFC 1123 deferred). `Some` when the server pinned a wait
        /// time the caller should honor instead of falling back to
        /// exponential backoff. Used by the transfer queue's batch
        /// retry loop.
        retry_after: Option<Duration>,
    },

    /// JSON body did not match the expected schema.
    #[error("malformed response body: {0}")]
    Decode(String),

    /// Failed to construct the request URL from the endpoint.
    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    /// `git credential` couldn't supply usable credentials for the
    /// endpoint. `detail` carries the underlying helper-side reason
    /// (e.g. `credential value for path contains newline: …`) when
    /// available; absent when every helper just returned "I don't know".
    /// Format mirrors upstream's `creds.FillCreds`.
    #[error("Git credentials for {url} not found{}", detail.as_deref().map(|d| format!(":\n{d}")).unwrap_or_else(|| ".".into()))]
    CredentialsNotFound { url: String, detail: Option<String> },
}

/// Render an [`ApiError::Status`] for the user. Auth-class statuses
/// (401/403) format as `Authorization error: <url>` to match upstream's
/// `lfshttp.defaultError` shape — `t-credentials` and `t-askpass` grep
/// the wording verbatim. All other statuses keep the
/// `server returned status N: <body-message>` format we use elsewhere.
fn format_status(status: u16, url: Option<&str>, body: Option<&ServerError>) -> String {
    if matches!(status, 401 | 403)
        && let Some(u) = url
    {
        return format!("Authorization error: {u}");
    }
    let suffix = body.map(|b| format!(": {}", b.message)).unwrap_or_default();
    format!("server returned status {status}{suffix}")
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
                | ApiError::Status {
                    status: 408 | 429 | 500..=599,
                    ..
                }
        )
    }

    /// Server-supplied retry delay, if any. Pulled from the
    /// `Retry-After` response header at decode time. Mirrors upstream's
    /// `errors.NewRetriableLaterError` gate; falls back to exponential
    /// backoff at the call site when `None`.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ApiError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Parse a `Retry-After` header value. Upstream's
/// `errors.NewRetriableLaterError` accepts two forms; we accept only
/// the first today:
///
/// 1. Integer seconds (delta-seconds), e.g. `Retry-After: 5`.
/// 2. RFC 1123 datetime (deferred — the test server only emits
///    integer seconds, and HTTP-date support adds a date-parsing
///    dependency we don't otherwise need).
///
/// Returns `None` for missing or unparseable values. `None` means "fall
/// back to exponential backoff" — same semantic upstream uses when the
/// helper returns nil.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    trimmed.parse::<u64>().ok().map(Duration::from_secs)
}
