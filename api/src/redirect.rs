//! Follow HTTP redirects with upstream's rules.
//!
//! reqwest's own redirect handling keeps credentials when only the scheme
//! changes, and follows a redirect from HTTPS to HTTP. Upstream's
//! `newRequestForRetry` does neither, so clients built for Git LFS should
//! disable reqwest's redirects and send requests with [`send_with_redirects`].

use reqwest::header::{AUTHORIZATION, COOKIE, LOCATION};
use reqwest::{Client, Request, Response, StatusCode, Url};

use crate::error::ApiError;

/// How many redirects one request may follow, as in upstream's
/// `DoWithRedirect`.
const MAX_REDIRECTS: usize = 2;

/// A `reqwest::Client` that leaves redirects to [`send_with_redirects`].
pub fn http_client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("TLS backend should initialize")
}

/// Send `request`, following redirects.
///
/// The method, headers, and body are kept, except that `Authorization` and
/// `Cookie` are dropped once a redirect leaves the original origin. A
/// redirect from HTTPS to HTTP is refused. A request whose body can't be
/// copied, such as a stream, isn't redirected: the redirect response is
/// returned.
pub async fn send_with_redirects(client: &Client, request: Request) -> Result<Response, ApiError> {
    let mut request = request;
    let mut send_credentials = true;
    let mut redirects = 0;
    loop {
        let next = request.try_clone();
        let response = client.execute(request).await?;
        let Some(mut next) = next else {
            return Ok(response);
        };
        let Some(location) = redirect_target(&response) else {
            return Ok(response);
        };
        if redirects == MAX_REDIRECTS {
            return Err(ApiError::Redirect("too many redirects".into()));
        }
        if !redirect_keeps_credentials(next.url(), &location)? {
            send_credentials = false;
        }
        if !send_credentials {
            next.headers_mut().remove(AUTHORIZATION);
            next.headers_mut().remove(COOKIE);
        }
        *next.url_mut() = location;
        request = next;
        redirects += 1;
    }
}

/// The URL `response` redirects to, if it's a redirect that should be
/// followed.
fn redirect_target(response: &Response) -> Option<Url> {
    let followed = [
        StatusCode::MOVED_PERMANENTLY,
        StatusCode::FOUND,
        StatusCode::SEE_OTHER,
        StatusCode::TEMPORARY_REDIRECT,
        StatusCode::PERMANENT_REDIRECT,
    ];
    if !followed.contains(&response.status()) {
        return None;
    }
    let location = response.headers().get(LOCATION)?.to_str().ok()?;
    response.url().join(location).ok()
}

/// Check a redirect from `from` to `to`, like upstream's `newRequestForRetry`.
///
/// Fails on a redirect from HTTPS to HTTP. Otherwise returns whether
/// credentials may still be sent, which is only when the scheme, host, and
/// port stay the same.
pub fn redirect_keeps_credentials(from: &Url, to: &Url) -> Result<bool, ApiError> {
    if from.scheme() == "https" && to.scheme() == "http" {
        return Err(ApiError::Redirect(
            "refusing insecure redirect: HTTPS to HTTP".into(),
        ));
    }
    Ok(from.origin() == to.origin())
}

/// Whether a header carries credentials that mustn't follow a redirect to
/// another origin.
pub fn is_credential_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("cookie")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keeps(from: &str, to: &str) -> Result<bool, ApiError> {
        redirect_keeps_credentials(&Url::parse(from).unwrap(), &Url::parse(to).unwrap())
    }

    #[test]
    fn same_origin_keeps_credentials() {
        assert!(keeps("https://a.test/x", "https://a.test/y?z").unwrap());
    }

    #[test]
    fn another_origin_drops_credentials() {
        for (from, to) in [
            ("https://a.test/x", "https://b.test/x"),
            ("https://a.test/x", "https://a.test:8443/x"),
            // Same host and port, but a new scheme.
            ("http://a.test:8080/x", "https://a.test:8080/x"),
        ] {
            assert!(!keeps(from, to).unwrap(), "{from} to {to}");
        }
    }

    #[test]
    fn https_to_http_is_refused() {
        assert!(matches!(
            keeps("https://a.test/x", "http://a.test/x"),
            Err(ApiError::Redirect(_))
        ));
    }
}
