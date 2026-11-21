use reqwest::RequestBuilder;
use reqwest::header::{AUTHORIZATION, HeaderValue};

/// Authentication to attach to API requests.
///
/// The value passed to [`Client::new`](crate::Client::new) is where a client
/// starts. A client with a credential helper replaces it with filled
/// credentials after a 401. Finding credentials is the job of
/// [`git_lfs_creds`].
#[derive(Debug, Clone)]
pub enum Auth {
    /// No `Authorization` header.
    None,
    /// HTTP Basic auth, sent as `Authorization: Basic <base64(user:pass)>`.
    Basic { username: String, password: String },
    /// Bearer token, sent as `Authorization: Bearer <token>`.
    Bearer(String),
    /// A scheme chosen by a credential helper that supports
    /// `capability[]=authtype`.
    ///
    /// Sent as `Authorization: <authtype> <credential>`. A value that isn't
    /// a valid header value is not sent at all.
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
                let raw = format!("{authtype} {credential}");
                match HeaderValue::try_from(raw) {
                    Ok(v) => req.header(AUTHORIZATION, v),
                    Err(_) => req,
                }
            }
        }
    }

    /// The `Authorization` line for the `GIT_CURL_VERBOSE` dump.
    ///
    /// Only Basic is masked, as in upstream's `traceHTTPDump`. Bearer and
    /// custom values print as they are, and the multistage shell tests grep
    /// for them.
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
