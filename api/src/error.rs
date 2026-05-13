use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The standard error body returned by the LFS server for non-2xx responses.
///
/// Defined by the batch spec § "Response Errors". The same shape is
/// reused by the locking endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerError {
    /// Human-readable error description.
    pub message: String,
    /// Server-assigned request identifier, useful for support tickets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// URL pointing at server-side documentation for the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
}

/// Errors returned by the API client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Network, TLS or connection-level failure.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Server returned a non-success HTTP status.
    ///
    /// The `Display` impl renders 401 and 403 as `Authorization
    /// error: <url>` to match upstream's `lfshttp.defaultError`;
    /// other statuses fall through to a plain server-side message
    /// when a parseable error body is present, otherwise to a
    /// generic `server returned status N` line.
    #[error("{}", format_status(*status, url.as_deref(), body.as_ref()))]
    Status {
        /// HTTP status code from the server.
        status: u16,
        /// Request URL the server responded to. Embedded in the
        /// `Display` impl so users can see *which* endpoint failed.
        url: Option<String>,
        /// `LFS-Authenticate` response header, mirrored verbatim.
        /// Only present on 401 responses; signals which auth scheme
        /// the server wants.
        lfs_authenticate: Option<String>,
        /// Parsed LFS error body when the response carried one.
        body: Option<ServerError>,
        /// Parsed `Retry-After` response header.
        ///
        /// `Some` when the server pinned a wait time the caller
        /// should honor instead of falling back to exponential
        /// backoff. Used by the transfer queue's batch retry loop.
        retry_after: Option<Duration>,
    },

    /// JSON body did not match the expected schema.
    #[error("malformed response body: {0}")]
    Decode(String),

    /// Failed to construct the request URL from the endpoint.
    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    /// `git credential` couldn't supply usable credentials for the
    /// endpoint.
    ///
    /// `detail` carries the underlying helper-side reason
    /// (e.g. `credential value for path contains newline: …`) when
    /// available; absent when every helper just returned "I don't know".
    /// Format mirrors upstream's `creds.FillCreds`.
    #[error("Git credentials for {url} not found{}", detail.as_deref().map(|d| format!(":\n{d}")).unwrap_or_else(|| ".".into()))]
    CredentialsNotFound { url: String, detail: Option<String> },

    /// The auth retry loop tried `MAX_AUTH_ATTEMPTS` times and still
    /// kept getting 401s. Surfaces upstream's `too many authentication
    /// attempts` wording so `t-credentials.sh` tests 12/13 can grep
    /// for it.
    #[error("too many authentication attempts")]
    AuthAttemptsExceeded,
}

/// Render an [`ApiError::Status`] for the user.
///
/// When the response carried a parseable error body, surface its
/// `message` verbatim — that's what upstream's `lfshttp.ClientError.Error()`
/// does, and what tests like `t-pre-push` / `t-fetch-refspec` "with
/// bad ref" grep for ("`Expected ref \"refs/heads/other\", got …`").
///
/// Falling back: 401/403 format as `Authorization error: <url>` to
/// match upstream's `lfshttp.defaultError`, which `t-credentials` and
/// `t-askpass` grep for verbatim. Everything else gets a plain
/// `server returned status N` line.
fn format_status(status: u16, url: Option<&str>, body: Option<&ServerError>) -> String {
    if let Some(b) = body
        && !b.message.is_empty()
    {
        return b.message.clone();
    }
    if matches!(status, 401 | 403)
        && let Some(u) = url
    {
        return format!("Authorization error: {u}");
    }
    format!("server returned status {status}")
}

impl ApiError {
    /// `true` for 401 responses; caller should resolve credentials and retry.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, ApiError::Status { status: 401, .. })
    }

    /// `true` for 403 responses; caller lacks permission for this operation.
    pub fn is_forbidden(&self) -> bool {
        matches!(self, ApiError::Status { status: 403, .. })
    }

    /// `true` for 404 responses.
    pub fn is_not_found(&self) -> bool {
        matches!(self, ApiError::Status { status: 404, .. })
    }

    /// `true` for 5xx and 408/429 responses (transient errors a
    /// caller may want to retry).
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

    /// Server-supplied retry delay, if any.
    ///
    /// Pulled from the `Retry-After` response header at decode
    /// time. Mirrors upstream's `errors.NewRetriableLaterError`
    /// gate; falls back to exponential backoff at the call site
    /// when `None`.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ApiError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Parse a `Retry-After` header value.
///
/// Accepts the integer-seconds form (`Retry-After: 5`). The
/// alternate RFC 1123 datetime form isn't supported; callers
/// requiring it should parse the header themselves.
///
/// Returns `None` for missing or unparseable values, signaling
/// "fall back to exponential backoff" (the same semantic upstream
/// uses when its helper returns nil).
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    trimmed.parse::<u64>().ok().map(Duration::from_secs)
}
