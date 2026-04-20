//! The `Helper` trait and shared types.

use crate::query::Query;

/// A username/password pair returned by a credential helper.
///
/// Username is optional because some servers accept token-as-password with
/// an empty username (e.g. GitHub personal access tokens). Password is
/// required — if a helper can't supply one, it should return `Ok(None)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Credentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("io error invoking credential helper: {0}")]
    Io(#[from] std::io::Error),
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
    /// Try to fetch credentials for `query`. `Ok(None)` means "I don't
    /// know" — the chain should consult the next helper. `Err` is a hard
    /// failure (e.g. helper subprocess crashed) and aborts the chain.
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError>;

    /// Tell the helper that `creds` worked for `query`. Helpers that can
    /// persist (git credential, OS keychain via `git credential`) should
    /// store the pair. Pure caches use this to populate themselves.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError>;

    /// Tell the helper that `creds` did **not** work for `query`. Helpers
    /// should drop the credentials so we don't loop on stale entries.
    fn reject(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError>;
}
