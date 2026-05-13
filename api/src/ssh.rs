//! SSH-based endpoint resolution hook.
//!
//! When the LFS endpoint is reached via SSH (e.g. `lfs.url =
//! ssh://...`, or a `git@host:repo` remote without a separate
//! `lfs.url`), upstream Git LFS shells out to `git-lfs-authenticate` to
//! obtain a replacement HTTPS URL plus auth headers. This crate is
//! transport-agnostic, so it expresses the hook as a [`SshResolver`]
//! trait — the actual `ssh` invocation lives in `git-lfs-creds`.
//!
//! The [`Client`](crate::Client) consults the resolver once per
//! request: a non-empty [`SshAuth::href`] overrides the endpoint URL
//! prefix for that call, and [`SshAuth::headers`] are merged into the
//! outgoing request. Caching is the resolver's responsibility — see
//! `git_lfs_creds::SshAuthClient` for the production implementation.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::ApiError;

/// `git-lfs-authenticate <path> <op>` operation argument. Wire form is
/// lowercase `upload` or `download`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SshOperation {
    /// Auth scope needed to push objects to the server.
    Upload,
    /// Auth scope needed to fetch objects from the server.
    Download,
}

impl SshOperation {
    /// Default mirrors upstream's `endpointOperation`: GET/HEAD →
    /// download, anything else → upload. Used as the fallback when a
    /// caller doesn't pass an explicit operation.
    pub fn from_http_method(method: &reqwest::Method) -> Self {
        if matches!(*method, reqwest::Method::GET | reqwest::Method::HEAD) {
            Self::Download
        } else {
            Self::Upload
        }
    }
}

/// Resolved auth from a `git-lfs-authenticate` call.
#[derive(Debug, Clone, Default)]
pub struct SshAuth {
    /// Replacement endpoint URL prefix.
    ///
    /// Empty (`""`) when the server expects the original URL to be used as-is.
    pub href: String,
    /// Headers to merge into the outgoing request.
    ///
    /// Typically a single `Authorization` entry, but the schema lets servers
    /// set arbitrary keys (e.g. `X-RemoteAuth-Provider` for vendor extensions).
    pub headers: HashMap<String, String>,
}

/// Hook called once per LFS API request to resolve SSH-mediated auth.
///
/// Implementations are typically backed by a `git-lfs-authenticate`
/// invocation with a small in-memory cache keyed on `(host, path,
/// operation)` so the SSH command runs at most once per cache TTL.
pub trait SshResolver: Send + Sync {
    /// Return the auth response for `operation`. `Ok(default)` (empty
    /// `href`, empty `headers`) means "no SSH override — use the
    /// configured endpoint with whatever auth is already on the
    /// request".
    fn resolve(&self, operation: SshOperation) -> Result<SshAuth, ApiError>;
}

/// Type alias for the boxed-resolver field on [`Client`](crate::Client).
pub type SharedSshResolver = Arc<dyn SshResolver>;
