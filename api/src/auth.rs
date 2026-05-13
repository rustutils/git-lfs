use reqwest::RequestBuilder;
use reqwest::header::{AUTHORIZATION, HeaderValue};

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
    /// Arbitrary auth scheme returned by a credential helper that
    /// advertised `capability[]=authtype` on its input. Sent as a
    /// literal `Authorization: <authtype> <credential>` header.
    /// Used for Bearer-on-non-bearer schemes (NTLM, Negotiate,
    /// Multistage) where the helper picks the scheme and value.
    Custom {
        authtype: String,
        credential: String,
    },
}

impl Auth {
    pub(crate) fn apply(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            Auth::None => req,
            Auth::Basic { username, password } => req.basic_auth(username, Some(password)),
            Auth::Bearer(token) => req.bearer_auth(token),
            Auth::Custom {
                authtype,
                credential,
            } => {
                // Format as `<scheme> <credential>`. Invalid header
                // bytes (newlines, NULs) would have been rejected by
                // the credential helper validator upstream; we still
                // defer to reqwest's HeaderValue::try_from to skip
                // anything reqwest itself can't serialize.
                let raw = format!("{authtype} {credential}");
                match HeaderValue::try_from(raw) {
                    Ok(v) => req.header(AUTHORIZATION, v),
                    Err(_) => req,
                }
            }
        }
    }

    /// Rendering of the `Authorization:` header for `GIT_CURL_VERBOSE`
    /// dumps. Mirrors upstream's `lfshttp/verbose.go::traceHTTPDump`:
    /// `Basic` is masked (the base64 user:pass is a credential), but
    /// `Bearer` and custom schemes emit the literal value. Multistage
    /// shell tests grep the literal `Authorization: Multistage <cred>`
    /// line, so masking those would break them.
    pub(crate) fn masked_header(&self) -> Option<String> {
        match self {
            Auth::None => None,
            Auth::Basic { .. } => Some("Authorization: Basic * * * * *".to_owned()),
            Auth::Bearer(token) => Some(format!("Authorization: Bearer {token}")),
            Auth::Custom {
                authtype,
                credential,
            } => Some(format!("Authorization: {authtype} {credential}")),
        }
    }
}
