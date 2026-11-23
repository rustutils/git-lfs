//! Hook for LFS endpoints reached over SSH.
//!
//! For an SSH remote, Git LFS runs `git-lfs-authenticate` over SSH to get an
//! HTTPS URL and headers for the API. This crate doesn't run SSH itself.
//! Instead, the [`Client`](crate::Client) asks an [`SshResolver`] once per API
//! call. A non-empty [`SshAuth::href`] replaces the endpoint for that call,
//! and [`SshAuth::headers`] are added to the request.
//!
//! Caching is up to the resolver. [`git_lfs_creds::SshAuthClient`] runs the
//! command and caches the responses.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::ApiError;

/// The operation passed to `git-lfs-authenticate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SshOperation {
    /// Write access: uploads and lock changes.
    Upload,
    /// Read access: downloads and lock listing.
    Download,
}

impl SshOperation {
    /// The operation for an HTTP method.
    ///
    /// GET and HEAD map to download, anything else to upload, like
    /// upstream's `endpointOperation`.
    pub fn from_http_method(method: &reqwest::Method) -> Self {
        if matches!(*method, reqwest::Method::GET | reqwest::Method::HEAD) {
            Self::Download
        } else {
            Self::Upload
        }
    }
}

/// Response from `git-lfs-authenticate`.
#[derive(Debug, Clone, Default)]
pub struct SshAuth {
    /// Endpoint URL to use instead of the configured one.
    ///
    /// Empty means keep the configured endpoint.
    pub href: String,
    /// Headers to add to the request.
    ///
    /// Usually a single `Authorization` header.
    pub headers: HashMap<String, String>,
}

impl SshAuth {
    /// Whether the headers include `Authorization`.
    ///
    /// Such a request carries its own credentials, so the credential
    /// helpers aren't asked for it, as in upstream.
    pub fn has_authorization(&self) -> bool {
        self.headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("authorization"))
    }
}

/// Supplies SSH auth for each API call.
///
/// Implementations usually cache responses, so the SSH command doesn't run
/// on every call.
pub trait SshResolver: Send + Sync {
    /// Resolve auth for `operation`.
    ///
    /// `SshAuth::default()` means no override: the client uses the
    /// configured endpoint and its own auth.
    fn resolve(&self, operation: SshOperation) -> Result<SshAuth, ApiError>;
}

/// A shared [`SshResolver`], as taken by
/// [`Client::with_ssh_resolver`](crate::Client::with_ssh_resolver).
pub type SharedSshResolver = Arc<dyn SshResolver>;
