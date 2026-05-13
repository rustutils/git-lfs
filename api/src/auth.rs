use reqwest::RequestBuilder;

/// Authentication to attach to API requests.
///
/// Populated by the caller (typically from a credential helper).
/// Resolving credentials (git-credential, keychain, etc.) is
/// deliberately not this crate's job; see [`git-lfs-creds`][creds].
///
/// [creds]: https://docs.rs/git-lfs-creds
#[derive(Debug, Clone)]
pub enum Auth {
    /// No `Authorization` header.
    None,
    /// HTTP Basic auth, sent as `Authorization: Basic <base64(user:pass)>`.
    Basic { username: String, password: String },
    /// Bearer token, sent as `Authorization: Bearer <token>`.
    Bearer(String),
}

impl Auth {
    pub(crate) fn apply(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            Auth::None => req,
            Auth::Basic { username, password } => req.basic_auth(username, Some(password)),
            Auth::Bearer(token) => req.bearer_auth(token),
        }
    }

    /// Masked rendering of the `Authorization:` header for
    /// `GIT_CURL_VERBOSE` dumps. Mirrors upstream's
    /// `lfshttp/verbose.go::traceHTTPDump`, which masks `Basic` only;
    /// `Bearer` tokens dump verbatim there but we mask them too to avoid
    /// leaking long-lived bearer credentials into shell-test logs.
    pub(crate) fn masked_header(&self) -> Option<&'static str> {
        match self {
            Auth::None => None,
            Auth::Basic { .. } => Some("Authorization: Basic * * * * *"),
            Auth::Bearer(_) => Some("Authorization: Bearer * * * * *"),
        }
    }
}
