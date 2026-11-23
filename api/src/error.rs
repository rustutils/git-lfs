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

/// `error`'s message followed by those of its sources, joined by `: `.
///
/// reqwest's own message is only the outermost one, such as "error sending
/// request for url (...)", without the cause, such as an untrusted
/// certificate. A source whose message is already in the text so far is
/// skipped.
pub fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let message = cause.to_string();
        if !message.is_empty() && !text.contains(&message) {
            text.push_str(": ");
            text.push_str(&message);
        }
        source = cause.source();
    }
    text
}

/// Errors returned by the API client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Network, TLS, or connection failure.
    #[error("transport error: {}", with_causes(.0))]
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

    /// A successful response's body wasn't the expected JSON.
    ///
    /// Displays with upstream's wording, which the shell tests match.
    #[error("Unable to parse HTTP response for {method} {url}: {message}")]
    ResponseParse {
        /// Request method, such as `POST`.
        method: String,
        /// URL the response came from, after redirects.
        url: String,
        /// The JSON parser's error.
        message: String,
    },

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
    /// The `href` from `git-lfs-authenticate` contains a control character,
    /// such as a newline.
    ///
    /// The `url` crate would strip it, so it's rejected first, with
    /// upstream's message.
    #[error("parse {href:?}: net/url: invalid control character in URL")]
    ControlCharacterInHref { href: String },
    /// A redirect wasn't followed: from HTTPS to HTTP, or one too many.
    #[error("{0}")]
    Redirect(String),
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

    /// `true` for errors worth retrying with backoff.
    ///
    /// These are 408, 429, and 5xx responses. Upstream only retries a 429,
    /// but a 5xx from a proxy is often gone in seconds. A request that
    /// failed to connect isn't included: the client already sent it again.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ApiError::Status {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An error with a message and an optional source.
    #[derive(Debug)]
    struct Chain(&'static str, Option<Box<Chain>>);

    impl std::fmt::Display for Chain {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Chain {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|e| e as _)
        }
    }

    #[test]
    fn causes_are_appended_once() {
        let error = Chain(
            "error sending request",
            Some(Box::new(Chain(
                "client error (Connect)",
                Some(Box::new(Chain(
                    "client error (Connect)",
                    Some(Box::new(Chain("invalid peer certificate", None))),
                ))),
            ))),
        );
        assert_eq!(
            with_causes(&error),
            "error sending request: client error (Connect): invalid peer certificate"
        );
    }
}
