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

/// Extra context piggybacking on a `fill` call.
///
/// Carries the per-attempt details that upstream forwards to
/// `git credential fill` via the `wwwauth[]`, `state[]`, and
/// `capability[]` input lines. Default-constructed for the first
/// attempt; populated by the API client's auth loop on subsequent
/// retries from the prior 401's `WWW-Authenticate` response headers
/// (`wwwauth`) and the prior helper response's continuation tokens
/// (`state`). `capabilities` advertises which protocol extensions
/// (`authtype`, `state`) we as a client understand.
///
/// Helpers that don't speak any of these knobs (cache, netrc,
/// askpass) ignore the context entirely — the default
/// [`Helper::fill_with_context`] impl just forwards to [`Helper::fill`].
#[derive(Debug, Clone, Default)]
pub struct FillContext {
    /// `WWW-Authenticate` response header values from the prior 401,
    /// or empty on the first attempt. Forwarded as `wwwauth[]=…` lines
    /// on the `git credential fill` input so helpers can pick the
    /// right scheme.
    pub wwwauth: Vec<String>,

    /// `state[]` values returned by the helper in the previous
    /// response, or empty on the first attempt. Forwarded back as
    /// `state[]=…` lines so multistage helpers can resume where they
    /// left off.
    pub state: Vec<String>,

    /// `capability[]` values to advertise on the input. Today we send
    /// `authtype` and `state` so multistage-capable helpers know they
    /// can use those extensions; passive helpers ignore them.
    pub capabilities: Vec<String>,
}

/// What a helper did with an `approve` / `reject` call.
///
/// The [`HelperChain`](crate::HelperChain) iterates in order and stops
/// at the first [`Handled`](HelperOutcome::Handled) result, mirroring
/// upstream's `CredentialHelpers.Approve` semantics in
/// `creds/creds.go:552`. This is how `CachingHelper` short-circuits the
/// chain on the second approve for the same query — the trace fires
/// once per push from `GitCredentialHelper`, not once per request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperOutcome {
    /// This helper handled the call; do not consult further helpers.
    Handled,
    /// This helper has nothing to add; consult the next helper.
    Continue,
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

    /// Like [`fill`](Self::fill) but accepting a [`FillContext`] from
    /// the calling auth loop. The default implementation ignores the
    /// context and delegates to `fill`; helpers that consume the
    /// extra fields (notably [`GitCredentialHelper`](crate::GitCredentialHelper))
    /// override this.
    fn fill_with_context(
        &self,
        query: &Query,
        _ctx: &FillContext,
    ) -> Result<Option<Credentials>, HelperError> {
        self.fill(query)
    }

    /// Tell the helper that `creds` worked for `query`.
    ///
    /// Helpers that can persist (git credential, OS keychain via
    /// `git credential`) should store the pair and return
    /// [`HelperOutcome::Handled`]; pure caches use this to populate
    /// themselves and typically return [`HelperOutcome::Continue`] so
    /// the chain reaches a real persistor.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError>;

    /// Tell the helper that `creds` did **not** work for `query`.
    ///
    /// Helpers should drop the credentials so we don't loop on stale
    /// entries. Return [`HelperOutcome::Handled`] when the rejection
    /// is authoritative for this helper (e.g. `git credential reject`
    /// ran); return [`HelperOutcome::Continue`] when it's a soft
    /// invalidation (e.g. evicting a cache entry).
    fn reject(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError>;
}
