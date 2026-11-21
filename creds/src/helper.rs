//! The [`Helper`] trait and the types it uses.

use crate::query::Query;

/// Credentials returned by a credential helper.
///
/// Usually a username and password. The username may be empty, as with
/// tokens used as passwords. Instead, a helper can return an auth scheme and
/// credential (such as a Bearer token). Git credential helpers do that only
/// when the input advertised `capability[]=authtype`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    /// Username, possibly empty.
    pub username: String,
    /// Password or token.
    pub password: String,
    /// Auth scheme from `authtype=` in the helper response.
    ///
    /// With [`credential`](Self::credential), sent as a literal
    /// `Authorization: <authtype> <credential>` header instead of Basic
    /// auth.
    pub authtype: Option<String>,
    /// Credential from `credential=` in the helper response, used with
    /// [`authtype`](Self::authtype).
    pub credential: Option<String>,
    /// `state[]` values from the helper response.
    ///
    /// The API client passes them to the next fill after a 401, so a
    /// multistage helper can resume its handshake.
    pub state: Vec<String>,
    /// Set by `continue=1` or `continue=true` in the helper response.
    ///
    /// Marks an intermediate stage of a multistage handshake. The API
    /// client doesn't reject these credentials on a 401, since they aren't
    /// wrong, just incomplete.
    pub multistage: bool,
}

impl Credentials {
    /// Build username and password credentials.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            authtype: None,
            credential: None,
            state: Vec::new(),
            multistage: false,
        }
    }

    /// Build credentials from an auth scheme and credential, such as a
    /// Bearer token.
    pub fn from_authtype(authtype: impl Into<String>, credential: impl Into<String>) -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            authtype: Some(authtype.into()),
            credential: Some(credential.into()),
            state: Vec::new(),
            multistage: false,
        }
    }
}

/// An error from a credential helper.
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    /// Failed to start or talk to the helper program.
    #[error("io error invoking credential helper: {0}")]
    Io(#[from] std::io::Error),
    /// The helper ran but failed, or the input couldn't be sent safely.
    #[error("credential helper failed: {0}")]
    Failed(String),
}

/// Extra input for a [`Helper::fill_with_context`] call.
///
/// The API client's auth loop starts with the default and fills it in on
/// retries after a 401. [`GitCredentialHelper`](crate::GitCredentialHelper)
/// sends it to `git credential fill`. The other bundled helpers ignore it.
#[derive(Debug, Clone, Default)]
pub struct FillContext {
    /// `WWW-Authenticate` header values from the previous 401.
    ///
    /// Sent as `wwwauth[]` lines so the helper can pick a scheme.
    pub wwwauth: Vec<String>,

    /// `state[]` values from the previous helper response.
    ///
    /// Sent back as `state[]` lines so a multistage helper can resume.
    pub state: Vec<String>,

    /// Protocol extensions to advertise as `capability[]` lines, such as
    /// `authtype` and `state`.
    pub capabilities: Vec<String>,
}

/// What a helper did with an approve or reject call.
///
/// [`HelperChain`](crate::HelperChain) stops at the first
/// [`Handled`](HelperOutcome::Handled), like upstream's
/// `CredentialHelpers.Approve` in `creds/creds.go`. This is how
/// [`CachingHelper`](crate::CachingHelper) keeps `git credential approve`
/// from running again for every request with the same credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperOutcome {
    /// This helper handled the call. Don't consult further helpers.
    Handled,
    /// This helper has nothing to add. Consult the next helper.
    Continue,
}

/// A source of credentials.
///
/// Helpers are `Send + Sync` so one can be shared across concurrent
/// transfers.
pub trait Helper: Send + Sync {
    /// Look up credentials for `query`.
    ///
    /// Returns `Ok(None)` when this helper has none.
    /// [`HelperChain`](crate::HelperChain) moves on to the next helper on
    /// `Ok(None)` and also on `Err`.
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError>;

    /// Like [`fill`](Self::fill), with extra input from the auth loop.
    ///
    /// The default ignores `ctx` and calls `fill`.
    fn fill_with_context(
        &self,
        query: &Query,
        _ctx: &FillContext,
    ) -> Result<Option<Credentials>, HelperError> {
        self.fill(query)
    }

    /// Report that `creds` worked for `query`.
    ///
    /// A helper that stores credentials, such as `git credential`, stores
    /// them and returns [`HelperOutcome::Handled`]. Return
    /// [`HelperOutcome::Continue`] to let the next helper see the call.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError>;

    /// Report that `creds` did not work for `query`.
    ///
    /// The helper should forget them so the next fill doesn't return them
    /// again. Return [`HelperOutcome::Continue`] to let the next helper see
    /// the call too, as a cache does so that `git credential reject` still
    /// runs.
    fn reject(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError>;
}
