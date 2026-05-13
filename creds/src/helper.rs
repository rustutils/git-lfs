//! The `Helper` trait and shared types.

use crate::query::Query;

/// A username/password pair returned by a credential helper.
///
/// Username may be empty: some servers accept a token-as-password with no
/// username set (e.g. GitHub personal access tokens). Password is
/// required; helpers that can't supply one should return `Ok(None)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// Username string. May be empty for token-as-password setups.
    pub username: String,
    /// Password (or token) string.
    pub password: String,
}

impl Credentials {
    /// Build a credentials pair from any pair of string-like values.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}

/// Things that can go wrong while invoking a credential helper.
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    /// Failed to spawn or talk to the helper subprocess.
    #[error("io error invoking credential helper: {0}")]
    Io(#[from] std::io::Error),
    /// The helper ran but reported a failure (non-zero exit, malformed
    /// stdout, refused input, etc).
    #[error("credential helper failed: {0}")]
    Failed(String),
}

/// Resolve credentials for a given query, and report success/failure back
/// so the helper can persist or invalidate its state.
///
/// `Send + Sync` so a single helper can be shared across the async
/// transfer queue. Implementations that aren't naturally thread-safe
/// (e.g. wrap a `Cell`) should layer their own synchronization.
pub trait Helper: Send + Sync {
    /// Try to fetch credentials for `query`.
    ///
    /// `Ok(None)` means "I don't know"; the chain should consult the
    /// next helper. `Err` is a hard failure (e.g. helper subprocess
    /// crashed) and aborts the chain.
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError>;

    /// Tell the helper that `creds` worked for `query`.
    ///
    /// Helpers that can persist (git credential, OS keychain via
    /// `git credential`) should store the pair; pure caches use this
    /// to populate themselves.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError>;

    /// Tell the helper that `creds` did **not** work for `query`.
    ///
    /// Helpers should drop the credentials so we don't loop on stale
    /// entries.
    fn reject(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError>;
}
