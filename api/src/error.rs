use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Error body an LFS server sends with a non-2xx response.
///
/// The batch and locking APIs use the same shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerError {
    /// Human-readable description.
    pub message: String,
    /// Server-assigned request identifier, useful for debugging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// URL where the user can report the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
}

/// Errors returned by the API client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Network, TLS, or connection failure.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The server returned a non-success HTTP status.
    ///
    /// Displays the `message` from the error body if there is one. Otherwise
    /// a 401 or 403 displays as `Authorization error: <url>`, as upstream
    /// does, and any other status as `server returned status N`.
    #[error("{}", format_status(*status, url.as_deref(), body.as_ref()))]
    Status {
        /// HTTP status code.
        status: u16,
        /// URL of the request that failed.
        url: Option<String>,
        /// `LFS-Authenticate` response header, which names the auth scheme
        /// the server wants. Servers send it with 401 responses.
        lfs_authenticate: Option<String>,
        /// Parsed error body, if the response had one.
        body: Option<ServerError>,
        /// Delay from the `Retry-After` response header.
        ///
        /// When set, the caller should wait this long before retrying
        /// instead of using its own backoff.
        retry_after: Option<Duration>,
    },

    /// A response body didn't match the expected schema.
    ///
    /// Also used when a request body fails to serialize, an SSH-resolved
    /// URL doesn't parse, or a credential helper task fails to run.
    #[error("malformed response body: {0}")]
    Decode(String),

    /// The request URL couldn't be built from the endpoint.
    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    /// No credential helper could supply credentials after a 401.
    ///
    /// Displays as upstream does: `Git credentials for <url> not found.`, or
    /// with `:` and the detail on the next line.
    #[error("Git credentials for {url} not found{}", detail.as_deref().map(|d| format!(":\n{d}")).unwrap_or_else(|| ".".into()))]
    CredentialsNotFound {
        /// URL the credentials were requested for.
        url: String,
        /// Error from the helper, if one failed rather than finding nothing.
        detail: Option<String>,
    },

    /// The server kept answering 401 after every allowed auth attempt.
    #[error("too many authentication attempts")]
    AuthAttemptsExceeded,
}

/// Render an [`ApiError::Status`] for the user.
///
/// Upstream shows the server's `message` verbatim and falls back to
/// `Authorization error: <url>` for 401 and 403. Shell tests match both.
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
    /// `true` for a 401 response.
    ///
    /// With a credential helper attached the client handles 401s itself, so
    /// this only happens without one.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, ApiError::Status { status: 401, .. })
    }

    /// `true` for a 403 response.
    pub fn is_forbidden(&self) -> bool {
        matches!(self, ApiError::Status { status: 403, .. })
    }

    /// `true` for a 404 response.
    pub fn is_not_found(&self) -> bool {
        matches!(self, ApiError::Status { status: 404, .. })
    }

    /// `true` for errors worth retrying.
    ///
    /// These are transport failures and 408, 429, and 5xx responses.
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

    /// How long the server asked the caller to wait, from `Retry-After`.
    ///
    /// `None` when the error isn't a status error or the header was absent
    /// or unparseable. The caller should then use its own backoff.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ApiError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Parse a `Retry-After` header value.
///
/// Accepts only the delay in seconds, such as `5`. Returns `None` for the
/// HTTP date form and for anything else that doesn't parse.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    trimmed.parse::<u64>().ok().map(Duration::from_secs)
}
