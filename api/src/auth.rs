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

    /// The value of the `Authorization` header this sends, if any.
    pub fn header_value(&self) -> Option<String> {
        use base64::Engine as _;
        match self {
            Auth::None => None,
            Auth::Basic { username, password } => Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
            )),
            Auth::Bearer(token) => Some(format!("Bearer {token}")),
            Auth::Custom {
                authtype,
                credential,
            } => Some(format!("{authtype} {credential}")),
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

/// Format a header for the `GIT_CURL_VERBOSE` dump.
///
/// Basic credentials are masked, as in upstream's `traceHTTPDump`, unless
/// `LFS_DEBUG_HTTP` is set. Other schemes, such as Bearer, are shown, as
/// upstream does.
pub fn verbose_header_line(name: &str, value: &str) -> String {
    let debug = std::env::var("LFS_DEBUG_HTTP")
        .is_ok_and(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false"));
    header_line(name, value, debug)
}

fn header_line(name: &str, value: &str, unmasked: bool) -> String {
    let basic = name.eq_ignore_ascii_case("authorization")
        && value.trim_start().to_ascii_lowercase().starts_with("basic");
    if basic && !unmasked {
        "Authorization: Basic * * * * *".to_owned()
    } else {
        format!("{name}: {value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_credentials_are_masked() {
        assert_eq!(
            header_line("authorization", "Basic YWxpY2U6czNjcmV0", false),
            "Authorization: Basic * * * * *"
        );
        assert_eq!(
            header_line("Authorization", "Basic YWxpY2U6czNjcmV0", true),
            "Authorization: Basic YWxpY2U6czNjcmV0"
        );
    }

    #[test]
    fn other_headers_are_shown() {
        assert_eq!(
            header_line("Authorization", "Bearer token", false),
            "Authorization: Bearer token"
        );
        assert_eq!(header_line("X-Foo", "bar", false), "X-Foo: bar");
    }
}
