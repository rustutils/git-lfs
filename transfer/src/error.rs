use std::time::{Duration, SystemTime};

use git_lfs_api::{ApiError, ObjectError};
use git_lfs_pointer::{Oid, OidParseError};
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
    #[error("http error: {}", git_lfs_api::with_causes(.0))]
    Http(#[from] reqwest::Error),

    /// Local I/O failure, or another failure carried as a message, such as
    /// an SSH transport error.
    #[error("local io error: {0}")]
    Io(#[from] std::io::Error),

    /// The local store failed.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// A download didn't hash to its OID. Nothing is stored.
    ///
    /// Displays with upstream's wording, which the shell tests match.
    #[error("expected OID {expected}, got {actual} after {written} bytes written")]
    Corrupt {
        expected: Oid,
        actual: Oid,
        /// Size of the downloaded content.
        written: u64,
    },

    /// An OID is not valid hex.
    #[error("invalid oid from server: {0}")]
    InvalidOid(#[from] OidParseError),

    /// The batch response has an object that wasn't requested.
    ///
    /// It's skipped, so a server can't make us upload an object from the
    /// store that it didn't ask for in this batch.
    #[error("The server returned an unknown OID.")]
    UnknownOid,

    /// The batch response named a hash algorithm other than `sha256`.
    #[error("unsupported hash algorithm: {0}")]
    UnsupportedHashAlgo(String),

    /// The HTTP batch request failed.
    ///
    /// Displays with upstream's `batch response:` prefix.
    #[error("batch response: {0}")]
    BatchResponse(Box<ApiError>),

    /// The batch request couldn't be built, such as for an invalid `href`
    /// from `git-lfs-authenticate`.
    #[error("batch request: {0}")]
    BatchRequest(Box<ApiError>),

    /// An action expires within 5 seconds, so it wasn't used.
    ///
    /// Retried with a fresh batch request, which gives new actions.
    /// Displays with upstream's wording, which the shell tests match, but
    /// in UTC.
    #[error("LFS: action \"{rel}\" expires at {}", format_rfc822(*.at))]
    ActionExpired { rel: String, at: SystemTime },

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

    /// A custom transfer adapter failed, or couldn't be talked to.
    ///
    /// Not retried, as in upstream.
    #[error("{0}")]
    Custom(String),

    /// `lfs.transfer.httpDownloadEncoding` has a value other than `gzip`
    /// or `zstd`. Displays with upstream's wording.
    #[error(
        "unsupported lfs.transfer.httpDownloadEncoding value {0:?}: must be \"gzip\" or \"zstd\""
    )]
    UnsupportedEncoding(String),
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

/// Format `time` in UTC like Go's `time.RFC822`, such as
/// `02 Jan 06 15:04 UTC`.
fn format_rfc822(time: SystemTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{day:02} {} {:02} {:02}:{:02} UTC",
        MONTHS[(month - 1) as usize],
        year.rem_euclid(100),
        rem / 3600,
        rem % 3600 / 60
    )
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
            TransferError::Io(_) | TransferError::ActionExpired { .. } => true,
            TransferError::ServerObject(_)
            | TransferError::NoDownloadAction
            | TransferError::Store(_)
            | TransferError::Corrupt { .. }
            | TransferError::InvalidOid(_)
            | TransferError::UnknownOid
            | TransferError::UnsupportedHashAlgo(_)
            | TransferError::InsecureRedirect
            | TransferError::Redirect(_)
            | TransferError::VerifyExhausted(_)
            | TransferError::Custom(_)
            | TransferError::UnsupportedEncoding(_)
            | TransferError::BatchRequest(_) => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc822_formats_utc() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_795_523_460);
        assert_eq!(format_rfc822(at), "24 Nov 26 12:31 UTC");
        assert_eq!(format_rfc822(SystemTime::UNIX_EPOCH), "01 Jan 70 00:00 UTC");
    }
}
