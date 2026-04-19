use reqwest::RequestBuilder;

/// Authentication to attach to API requests.
///
/// Populated by the caller — typically by `creds/` once it lands. Resolving
/// credentials (git-credential, keychain, etc.) is deliberately not this
/// crate's job.
#[derive(Debug, Clone)]
pub enum Auth {
    /// No `Authorization` header.
    None,
    /// HTTP Basic auth — sent as `Authorization: Basic <base64(user:pass)>`.
    Basic { username: String, password: String },
    /// Bearer token — sent as `Authorization: Bearer <token>`.
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
}
