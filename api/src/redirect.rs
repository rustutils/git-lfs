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
///
/// A request that fails to connect is sent again, up to `retries` times, as
/// in upstream's `DoWithRedirect`. Under `GIT_TRACE`, each request prints
/// `HTTP: <method> <url>` and each response `HTTP: <status>`, like
/// upstream.
pub async fn send_with_redirects(
    client: &Client,
    request: Request,
    retries: u32,
) -> Result<Response, ApiError> {
    let mut request = request;
    let mut send_credentials = true;
    let mut redirects = 0;
    loop {
        let next = request.try_clone();
        trace_request(&request);
        let response = execute_with_retries(client, request, retries).await?;
        trace_response(&response);
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

/// Send `request`, sending it again up to `retries` times if it fails to
/// connect. A request whose body can't be copied is sent once.
async fn execute_with_retries(
    client: &Client,
    request: Request,
    retries: u32,
) -> Result<Response, reqwest::Error> {
    let mut request = request;
    let mut left = retries;
    loop {
        let again = if left > 0 { request.try_clone() } else { None };
        match (client.execute(request).await, again) {
            (Err(_), Some(again)) => {
                request = again;
                left -= 1;
            }
            (result, _) => return result,
        }
    }
}

/// Under `GIT_TRACE`, print `HTTP: <method> <url>` without the query, as
/// upstream's `traceReq` does.
pub fn trace_request(request: &Request) {
    let url = request.url().as_str();
    let url = url.split_once('?').map_or(url, |(base, _)| base);
    trace(format_args!("HTTP: {} {url}", request.method()));
}

/// Under `GIT_TRACE`, print `HTTP: <status>`, as upstream does.
pub fn trace_response(response: &Response) {
    trace(format_args!("HTTP: {}", response.status().as_u16()));
}

fn trace(args: std::fmt::Arguments) {
    let on = std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0");
    if on {
        eprintln!("{args}");
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
