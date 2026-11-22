use std::sync::Arc;
use std::time::Duration;

/// Rewrites an action URL from the batch response before it's used.
///
/// Lets the caller apply `url.<base>.insteadOf` rules when
/// `lfs.transfer.enablehrefrewrite` is set.
pub type UrlRewriter = Arc<dyn Fn(&str) -> String + Send + Sync>;

pub use git_lfs_api::HeaderSource;

/// Looks up a setting for a URL, such as
/// `lfs.transfer.<url>.httpDownloadEncoding`.
pub type UrlSetting = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

use crate::custom::CustomAdapter;

/// Settings for the transfer queue.
#[derive(Clone)]
pub struct TransferConfig {
    /// Maximum number of objects transferring at once.
    ///
    /// Default 8. Upstream Git LFS scales its default with the CPU count.
    pub concurrency: usize,
    /// Total attempts per object and per HTTP batch request, including
    /// the first.
    ///
    /// Default 9: the first attempt and 8 retries, as for upstream's
    /// `lfs.transfer.maxRetries`.
    pub max_attempts: u32,
    /// Delay before the first retry.
    ///
    /// Doubled after each retry that wasn't given a `Retry-After` delay,
    /// up to [`backoff_max`](Self::backoff_max). Default 250 ms, as in
    /// upstream.
    pub initial_backoff: Duration,
    /// Upper bound for the backoff delay. Default 10 s, as for upstream's
    /// `lfs.transfer.maxRetryDelay`.
    pub backoff_max: Duration,
    /// The longest `Retry-After` delay to wait for.
    ///
    /// A server asking for a longer wait fails the request instead.
    /// Follows `lfs.transfer.maxRetryTime`. Default 300 s.
    pub max_retry_time: Duration,
    /// Rewriter for download action URLs.
    pub url_rewriter: Option<UrlRewriter>,
    /// Rewriter for upload and verify action URLs.
    ///
    /// Typically applies `url.<base>.pushInsteadOf`. When `None`,
    /// [`url_rewriter`](Self::url_rewriter) is used instead.
    pub upload_url_rewriter: Option<UrlRewriter>,
    /// Maximum objects per batch request.
    ///
    /// Larger lists are split into chunks of this size, sent one after
    /// another. Default 100, as for upstream's `lfs.transfer.batchSize`.
    /// Zero is treated as 1.
    pub batch_size: usize,
    /// Whether HTTP uploads detect the `Content-Type` from the object.
    ///
    /// Follows `lfs.<url>.contenttype`. When `true` (the default), the
    /// first 512 bytes pick the type. When `false`, uploads send
    /// `application/octet-stream`. A `Content-Type` in the upload action's
    /// headers always wins.
    pub detect_content_type: bool,
    /// Total attempts at the verify action after an HTTP upload.
    ///
    /// Follows `lfs.transfer.maxverifies`. Default 3. The value is used as
    /// given: upstream's minimum of 3 is applied by the caller, and 0
    /// fails every verify without sending it.
    pub max_verifies: u32,
    /// Extra headers for each HTTP transfer request, looked up for its URL.
    ///
    /// The CLI passes the `http.extraHeader` settings.
    pub extra_headers: Option<HeaderSource>,
    /// The `Accept-Encoding` to ask for on each download URL, from
    /// `lfs.transfer.<url>.httpDownloadEncoding`: `gzip` or `zstd`.
    ///
    /// Unset means `gzip`. Any other value fails the download, as in
    /// upstream. A resumed download asks for no encoding.
    pub download_encoding: Option<UrlSetting>,
    /// Credentials for action URLs that answer 401. Without it, a 401
    /// fails the object.
    pub action_auth: Option<Arc<dyn crate::ActionAuth>>,
    /// Custom transfer adapters to offer the server, from
    /// `lfs.customtransfer.<name>.*`.
    pub custom_adapters: Vec<CustomAdapter>,
    /// Offer the server only `basic`, from `lfs.basictransfersonly`.
    pub basic_transfers_only: bool,
    /// Offer the server `tus` resumable uploads, from `lfs.tustransfers`.
    pub tus: bool,
    /// The remote, as a name or URL, for the custom adapters' `init`
    /// message.
    pub remote: String,
    /// A custom adapter that moves every object by itself, from
    /// `lfs.standalonetransferagent`.
    ///
    /// No batch request is sent, and the adapter gets `null` actions, in
    /// the directions it handles.
    pub standalone: Option<CustomAdapter>,
}

impl TransferConfig {
    /// The custom adapter called `name` that handles the direction.
    pub(crate) fn custom_adapter(&self, name: &str, upload: bool) -> Option<&CustomAdapter> {
        if self.basic_transfers_only {
            return None;
        }
        self.custom_adapters
            .iter()
            .find(|a| a.name == name && a.handles(upload))
    }

    /// The transfer adapters to offer in a batch request.
    pub(crate) fn transfer_names(&self, upload: bool) -> Vec<String> {
        let mut names = vec!["basic".to_owned()];
        if !self.basic_transfers_only {
            if upload && self.tus {
                names.push("tus".to_owned());
            }
            let customs: Vec<String> = self
                .custom_adapters
                .iter()
                .filter(|a| a.handles(upload) && !names.contains(&a.name))
                .map(|a| a.name.clone())
                .collect();
            names.extend(customs);
        }
        names
    }
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            concurrency: 8,
            max_attempts: 9,
            initial_backoff: Duration::from_millis(250),
            backoff_max: Duration::from_secs(10),
            max_retry_time: Duration::from_secs(300),
            url_rewriter: None,
            upload_url_rewriter: None,
            batch_size: 100,
            detect_content_type: true,
            max_verifies: 3,
            extra_headers: None,
            download_encoding: None,
            action_auth: None,
            custom_adapters: Vec::new(),
            basic_transfers_only: false,
            tus: false,
            remote: "origin".to_owned(),
            standalone: None,
        }
    }
}

impl std::fmt::Debug for TransferConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferConfig")
            .field("concurrency", &self.concurrency)
            .field("max_attempts", &self.max_attempts)
            .field("initial_backoff", &self.initial_backoff)
            .field("backoff_max", &self.backoff_max)
            .field("max_retry_time", &self.max_retry_time)
            .field("url_rewriter", &self.url_rewriter.as_ref().map(|_| "<fn>"))
            .field(
                "upload_url_rewriter",
                &self.upload_url_rewriter.as_ref().map(|_| "<fn>"),
            )
            .field("batch_size", &self.batch_size)
            .field("detect_content_type", &self.detect_content_type)
            .field("max_verifies", &self.max_verifies)
            .field(
                "extra_headers",
                &self.extra_headers.as_ref().map(|_| "<fn>"),
            )
            .field(
                "download_encoding",
                &self.download_encoding.as_ref().map(|_| "<fn>"),
            )
            .field("action_auth", &self.action_auth.as_ref().map(|_| "<auth>"))
            .field("custom_adapters", &self.custom_adapters)
            .field("basic_transfers_only", &self.basic_transfers_only)
            .field("tus", &self.tus)
            .field("remote", &self.remote)
            .field("standalone", &self.standalone)
            .finish()
    }
}
