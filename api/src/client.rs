use std::io::Write;
use std::sync::{Arc, Mutex};

use git_lfs_creds::{Credentials, FillContext, Helper, Query};
use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue,
};
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::auth::{Auth, verbose_header_line};
use crate::error::ApiError;
use crate::redirect::send_with_redirects;
use crate::ssh::{SharedSshResolver, SshAuth, SshOperation};

/// `Content-Type` and `Accept` value for LFS API requests.
///
/// The spec in `docs/api/batch.md` also allows a `charset=utf-8` parameter.
/// We send the bare media type.
pub(crate) const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

/// Maximum number of authenticated sends per request.
///
/// A request that starts without auth gets one extra, uncounted send first.
/// Matches upstream's `defaultMaxAuthAttempts` in `lfsapi/auth.go`.
const MAX_AUTH_ATTEMPTS: u32 = 3;

/// Gives the extra headers for a request URL, as `(name, value)` pairs.
pub type HeaderSource = Arc<dyn Fn(&str) -> Vec<(String, String)> + Send + Sync>;

/// HTTP client for one LFS endpoint.
///
/// Cheap to clone. Clones share the connection pool, the current auth, and
/// the filled credentials.
///
/// # Authentication
///
/// Each request is first sent with the current [`Auth`], which starts as the
/// value passed to [`Client::new`]. Without a credential helper, a 401 is
/// not retried.
///
/// With a helper attached by [`Client::with_credential_helper`], each 401
/// rejects the credentials just sent, fills new ones, and retries. That
/// happens up to three times, plus one uncounted send when the request
/// started without auth. Running out gives
/// [`ApiError::AuthAttemptsExceeded`], and a fill that finds nothing gives
/// [`ApiError::CredentialsNotFound`]. A success approves the credentials.
/// After that, each request fills from the helper again before sending.
#[derive(Clone)]
pub struct Client {
    pub(crate) endpoint: Url,
    pub(crate) http: reqwest::Client,
    pub(crate) auth: Arc<Mutex<Auth>>,
    pub(crate) credentials: Option<Arc<dyn Helper>>,
    /// The last filled credentials and the query they were filled for.
    pub(crate) filled: Arc<Mutex<Option<(Query, Credentials)>>>,
    /// Whether credential queries include the URL path
    /// (`credential.useHttpPath`).
    pub(crate) use_http_path: bool,
    /// URL for credential queries and the credentials-not-found error.
    ///
    /// `None` means the endpoint.
    pub(crate) cred_url: Option<Url>,
    /// Resolver for SSH remotes, asked once per API call.
    pub(crate) ssh_resolver: Option<SharedSshResolver>,
    /// Extra headers for each request, looked up by URL.
    pub(crate) extra_headers: Option<HeaderSource>,
    /// Whether to leave authenticate headers out of credential fills
    /// (`credential.<url>.skipwwwauth`).
    pub(crate) skip_wwwauth: bool,
    /// How many times a batch request that fails to connect is sent again.
    pub(crate) batch_retries: u32,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("endpoint", &self.endpoint)
            .field("auth", &self.auth)
            .field("has_credential_helper", &self.credentials.is_some())
            .finish()
    }
}

impl Client {
    /// Build a client for an LFS endpoint.
    ///
    /// `endpoint` is the LFS server URL, such as
    /// `https://git-server.com/foo/bar.git/info/lfs`. Request paths like
    /// `objects/batch` and `locks` are joined onto it.
    pub fn new(endpoint: Url, auth: Auth) -> Self {
        Self::with_http_client(endpoint, auth, crate::redirect::http_client())
    }

    /// Like [`new`](Self::new), but with a caller-supplied `reqwest::Client`.
    ///
    /// Use it to share a connection pool or to set timeouts, proxies, or
    /// default headers. The client must not follow redirects itself: build
    /// it with `redirect::Policy::none()`, as [`http_client`](crate::http_client)
    /// does, so this client can follow them with upstream's rules.
    pub fn with_http_client(endpoint: Url, auth: Auth, http: reqwest::Client) -> Self {
        Self {
            endpoint,
            http,
            auth: Arc::new(Mutex::new(auth)),
            credentials: None,
            filled: Arc::new(Mutex::new(None)),
            use_http_path: false,
            cred_url: None,
            ssh_resolver: None,
            extra_headers: None,
            skip_wwwauth: false,
            batch_retries: 8,
        }
    }

    /// Add headers to each request, looked up for the request's URL.
    ///
    /// The CLI passes the `http.extraHeader` settings. The headers also
    /// appear in the `GIT_CURL_VERBOSE` dump.
    #[must_use]
    pub fn with_extra_headers(mut self, source: HeaderSource) -> Self {
        self.extra_headers = Some(source);
        self
    }

    /// The extra headers for a request to `url`.
    pub(crate) fn extra_headers_for(&self, url: &str) -> Vec<(String, String)> {
        self.extra_headers
            .as_ref()
            .map(|source| source(url))
            .unwrap_or_default()
    }

    /// Attach an SSH auth resolver.
    ///
    /// Use it when the endpoint is reached over SSH. The resolver runs once
    /// per API call. A non-empty `href` replaces the endpoint for that call,
    /// and the returned headers are added to the request.
    #[must_use]
    pub fn with_ssh_resolver(mut self, resolver: SharedSshResolver) -> Self {
        self.ssh_resolver = Some(resolver);
        self
    }

    /// Set the URL for credential queries and the
    /// `Git credentials for <url> not found` error.
    ///
    /// Pass the git remote URL when it has the same scheme and host as the
    /// endpoint, so prompts name the repository. Otherwise the endpoint is
    /// used.
    #[must_use]
    pub fn with_cred_url(mut self, url: Url) -> Self {
        self.cred_url = Some(url);
        self
    }

    /// Attach a credential helper.
    ///
    /// On a 401, the client fills credentials from it and retries. See
    /// [`Client`] for the details.
    #[must_use]
    pub fn with_credential_helper(mut self, helper: Arc<dyn Helper>) -> Self {
        self.credentials = Some(helper);
        self
    }

    /// Set `credential.useHttpPath`.
    ///
    /// When `true`, credential queries include the URL path, so a helper can
    /// tell repositories apart. The default `false` sends only protocol and
    /// host, like git.
    #[must_use]
    pub fn with_use_http_path(mut self, on: bool) -> Self {
        self.use_http_path = on;
        self
    }

    /// Set `credential.<url>.skipwwwauth`.
    ///
    /// When `true`, the `LFS-Authenticate` and `WWW-Authenticate` headers of
    /// a 401 are not passed to the next fill as `wwwauth[]`.
    #[must_use]
    pub fn with_skip_wwwauth(mut self, on: bool) -> Self {
        self.skip_wwwauth = on;
        self
    }

    /// Set how many times a batch request that fails to connect is sent
    /// again, as for upstream's `lfs.transfer.maxRetries`. Default 8.
    ///
    /// Other requests aren't sent again, also as in upstream.
    #[must_use]
    pub fn with_batch_retries(mut self, retries: u32) -> Self {
        self.batch_retries = retries;
        self
    }

    /// The endpoint URL this client was built with.
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// Whether the current auth is HTTP Basic.
    ///
    /// Callers check this after a request to decide whether to persist
    /// `lfs.<url>.access = basic`.
    pub fn used_basic_auth(&self) -> bool {
        matches!(*self.auth.lock().unwrap(), Auth::Basic { .. })
    }

    /// Join `path` onto `base`, treating `base` as a directory.
    pub(crate) fn join(base: &Url, path: &str) -> Result<Url, ApiError> {
        let mut base = base.clone();
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        Ok(base.join(path)?)
    }

    /// Resolve SSH auth for `operation`.
    ///
    /// Returns the base URL for the call (the `href` override or the
    /// endpoint) and the headers to add. Without a resolver, returns the
    /// endpoint and no headers.
    pub(crate) fn resolve_ssh(&self, operation: SshOperation) -> Result<(Url, SshAuth), ApiError> {
        let Some(resolver) = self.ssh_resolver.as_ref() else {
            return Ok((self.endpoint.clone(), SshAuth::default()));
        };
        let auth = resolver.resolve(operation)?;
        let base = if auth.href.is_empty() {
            self.endpoint.clone()
        } else {
            if auth.href.bytes().any(|b| b < 0x20 || b == 0x7f) {
                return Err(ApiError::ControlCharacterInHref { href: auth.href });
            }
            let mut u = Url::parse(&auth.href)
                .map_err(|e| ApiError::Decode(format!("ssh href {:?}: {e}", auth.href)))?;
            // Collapse repeated slashes in the path. The `lfs-ssh-echo`
            // test server returns hrefs like
            // `http://host:port//repo.git/info/lfs`, because the path we
            // pass to `git-lfs-authenticate` starts with a slash. Go's
            // `http.ServeMux` answers those with a 301 to the cleaned
            // path. Cleaning it first saves that round trip.
            let path = u.path().to_owned();
            let cleaned = collapse_slashes(&path);
            if cleaned != path {
                u.set_path(&cleaned);
            }
            u
        };
        Ok((base, auth))
    }

    /// Build a request with the `Accept` header and the current auth, then
    /// add `ssh.headers`.
    ///
    /// The SSH headers are appended, so an SSH `Authorization` header goes
    /// out next to the one from the current auth. Pass `&SshAuth::default()`
    /// for non-SSH calls.
    pub(crate) fn request_with_headers(
        &self,
        method: Method,
        url: Url,
        ssh: &SshAuth,
    ) -> RequestBuilder {
        let auth = self.auth.lock().unwrap().clone();
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(LFS_MEDIA_TYPE));
        // Like Go's HTTP client, which upstream uses. A gzipped response
        // is decompressed in `body_bytes`.
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        for (k, v) in self.extra_headers_for(url.as_str()) {
            if let (Ok(name), Ok(value)) = (
                HeaderName::try_from(k.as_str()),
                HeaderValue::try_from(v.as_str()),
            ) {
                headers.append(name, value);
            }
        }
        let req = self.http.request(method, url).headers(headers);
        let mut req = auth.apply(req);
        for (k, v) in &ssh.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::try_from(k.as_str()),
                HeaderValue::try_from(v.as_str()),
            ) {
                req = req.header(name, value);
            }
        }
        req
    }

    /// The credential query for this client.
    ///
    /// Built from the credential URL, without the path unless
    /// `use_http_path` is set.
    fn cred_query(&self) -> Query {
        let url = self.cred_url.as_ref().unwrap_or(&self.endpoint);
        let q = Query::from_url(url);
        if self.use_http_path {
            q
        } else {
            q.without_path()
        }
    }

    /// The credential URL as shown in errors, without any password.
    fn cred_url_string(&self) -> String {
        let mut url = self.cred_url.as_ref().unwrap_or(&self.endpoint).clone();
        let _ = url.set_password(None);
        url.to_string()
    }

    /// POST a JSON body and decode the JSON response, with the auth retry
    /// loop.
    ///
    /// `op` is the `git-lfs-authenticate` operation for SSH remotes. A
    /// request that fails to connect is sent again, up to `retries` times.
    pub(crate) async fn post_json<B, R>(
        &self,
        path: &str,
        body: &B,
        op: SshOperation,
        retries: u32,
    ) -> Result<R, ApiError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let (base, ssh) = self.resolve_ssh(op)?;
        let url = Self::join(&base, path)?;
        let body_bytes = serde_json::to_vec(body)
            .map_err(|e| ApiError::Decode(format!("serializing request body: {e}")))?;
        // Upstream's GIT_CURL_VERBOSE dump includes request bodies, and
        // shell tests grep them (t-batch-transfer test 2 checks the object
        // order in an upload batch). reqwest prints nothing, so write the
        // dump ourselves.
        if std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0") {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "> POST {url}");
            let _ = writeln!(err, "> Content-Type: {LFS_MEDIA_TYPE}");
            let _ = writeln!(err, "> Accept-Encoding: gzip");
            // `t-extra-header.sh` greps for lines like `> X-Foo: bar`.
            for (name, value) in self.extra_headers_for(url.as_str()) {
                let _ = writeln!(err, "> {}", verbose_header_line(&name, &value));
            }
            // The retry loop prints the `Authorization` line, once per
            // attempt.
            let _ = writeln!(err);
            let _ = err.write_all(&body_bytes);
            let _ = writeln!(err);
        }
        self.send_with_auth_retry(
            Method::POST,
            || {
                self.request_with_headers(Method::POST, url.clone(), &ssh)
                    .header(CONTENT_TYPE, LFS_MEDIA_TYPE)
                    .body(body_bytes.clone())
            },
            retries,
        )
        .await
    }

    /// GET and decode a JSON response, with the auth retry loop.
    ///
    /// `query` becomes the URL query string. `op` is the
    /// `git-lfs-authenticate` operation for SSH remotes.
    pub(crate) async fn get_json<Q, R>(
        &self,
        path: &str,
        query: &Q,
        op: SshOperation,
    ) -> Result<R, ApiError>
    where
        Q: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let (base, ssh) = self.resolve_ssh(op)?;
        let url = Self::join(&base, path)?;
        // Serialize once, so retries don't run the serializer again.
        let qs = serde_urlencoded::to_string(query)
            .map_err(|e| ApiError::Decode(format!("serializing query: {e}")))?;
        self.send_with_auth_retry(
            Method::GET,
            || {
                let mut u = url.clone();
                if !qs.is_empty() {
                    u.set_query(Some(&qs));
                }
                self.request_with_headers(Method::GET, u, &ssh)
            },
            0,
        )
        .await
    }

    /// Send a request through the auth retry loop and return the raw
    /// response.
    ///
    /// For callers with their own status handling, such as the 409 from
    /// lock creation. `build` makes a fresh request for each attempt.
    ///
    /// Returns the response on success, on any status other than 401, and
    /// on a 401 when no helper is attached. Matches upstream's `DoWithAuth`
    /// in `lfsapi/auth.go`, including the
    /// `api: too many authentication attempts` trace under `GIT_TRACE`.
    pub(crate) async fn send_with_auth_retry_response<F>(
        &self,
        build: F,
    ) -> Result<Response, ApiError>
    where
        F: Fn() -> RequestBuilder,
    {
        self.send_with_retries(build, 0).await
    }

    /// Like [`send_with_auth_retry_response`](Self::send_with_auth_retry_response),
    /// but a request that fails to connect is sent again, up to `retries`
    /// times.
    async fn send_with_retries<F>(&self, build: F, retries: u32) -> Result<Response, ApiError>
    where
        F: Fn() -> RequestBuilder,
    {
        // After a successful fill, ask the helpers again before each
        // request, as upstream's `setRequestAuth` does for an endpoint with
        // basic access. The caching helper answers without running
        // `git credential`, but netrc traces each fill, and the netrc tests
        // in `t-credentials.sh` count those traces.
        let filled_already = self.filled.lock().unwrap().is_some();
        if filled_already && let Some(helper) = self.credentials.clone() {
            let query = self.cred_query();
            if let Ok(Some(c)) = tokio::task::spawn_blocking(move || helper.fill(&query))
                .await
                .unwrap_or(Ok(None))
            {
                *self.auth.lock().unwrap() = auth_from_creds(&c);
                *self.filled.lock().unwrap() = Some((self.cred_query(), c));
            }
        }

        // A request that starts without auth gets one extra send, as
        // upstream's `DoWithAuth` does for `NoneAccess`.
        let starts_anonymous = matches!(*self.auth.lock().unwrap(), Auth::None);
        let max = if starts_anonymous {
            MAX_AUTH_ATTEMPTS + 1
        } else {
            MAX_AUTH_ATTEMPTS
        };

        // Authenticate headers from the latest 401 that had any, passed to
        // the next fill as `wwwauth[]`.
        let mut wwwauth: Vec<String> = Vec::new();
        // `state[]` from the last fill, passed back so a multistage helper
        // can continue its handshake.
        let mut state: Vec<String> = Vec::new();

        let verbose =
            std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0");

        for attempt in 0..max {
            if verbose && let Some(masked) = self.auth.lock().unwrap().masked_header() {
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "> {masked}");
            }
            let resp = send_with_redirects(&self.http, build().build()?, retries).await?;
            let status = resp.status();

            if status.is_success() {
                self.approve_filled().await;
                return Ok(resp);
            }
            if status.as_u16() != 401 {
                return Ok(resp);
            }

            let Some(helper) = self.credentials.clone() else {
                return Ok(resp);
            };

            // Pass on every challenge and let the helper pick a scheme it
            // supports. `LFS-Authenticate` comes before `WWW-Authenticate`,
            // as in upstream's `authenticateHeaders`.
            if !self.skip_wwwauth {
                let mut new_wwwauth: Vec<String> = Vec::new();
                for hname in ["lfs-authenticate", "www-authenticate"] {
                    for v in resp.headers().get_all(hname) {
                        if let Ok(s) = v.to_str() {
                            new_wwwauth.push(s.to_owned());
                        }
                    }
                }
                if !new_wwwauth.is_empty() {
                    wwwauth = new_wwwauth;
                }
            }
            drop(resp);

            // Multistage credentials are mid-handshake, not wrong, so
            // upstream's `DoWithAuth` doesn't reject them. Their state goes
            // to the next fill.
            let (was_multistage, prev_state) = {
                let filled = self.filled.lock().unwrap();
                filled.as_ref().map_or((false, Vec::new()), |(_, c)| {
                    (c.multistage, c.state.clone())
                })
            };
            if !prev_state.is_empty() {
                state = prev_state;
            }

            // Out of attempts.
            if attempt + 1 >= max {
                if was_multistage {
                    *self.filled.lock().unwrap() = None;
                    *self.auth.lock().unwrap() = Auth::None;
                } else {
                    self.reject_filled().await;
                }
                if trace_enabled() {
                    let mut e = std::io::stderr().lock();
                    let _ = writeln!(e, "api: too many authentication attempts");
                }
                return Err(ApiError::AuthAttemptsExceeded);
            }

            // Rejecting clears the cached entry and makes netrc skip its
            // entry, so the next fill moves on to the next helper.
            // Multistage credentials are dropped without telling the
            // helpers.
            if was_multistage {
                *self.filled.lock().unwrap() = None;
                *self.auth.lock().unwrap() = Auth::None;
            } else {
                self.reject_filled().await;
            }

            // Advertise the capabilities this client can act on.
            let query = self.cred_query();
            let cred_url_str = self.cred_url_string();
            let ctx = FillContext {
                wwwauth: wwwauth.clone(),
                state: state.clone(),
                capabilities: vec!["authtype".to_owned(), "state".to_owned()],
                ..FillContext::default()
            };
            let creds =
                match fill_for_endpoint(helper.clone(), query.clone(), &cred_url_str, ctx).await? {
                    Some(c) => c,
                    // Upstream's not-found error lets callers tell missing
                    // credentials from a plain 401.
                    None => {
                        return Err(ApiError::CredentialsNotFound {
                            url: cred_url_str,
                            detail: None,
                        });
                    }
                };
            {
                let mut auth = self.auth.lock().unwrap();
                *auth = auth_from_creds(&creds);
            }
            {
                let mut filled = self.filled.lock().unwrap();
                *filled = Some((query.clone(), creds));
            }
        }

        unreachable!("the loop returns or errors before exhausting max attempts")
    }

    /// Like [`send_with_auth_retry_response`], but decodes the JSON body.
    ///
    /// [`send_with_auth_retry_response`]: Self::send_with_auth_retry_response
    async fn send_with_auth_retry<F, R>(
        &self,
        method: Method,
        build: F,
        retries: u32,
    ) -> Result<R, ApiError>
    where
        F: Fn() -> RequestBuilder,
        R: DeserializeOwned,
    {
        let resp = self.send_with_retries(build, retries).await?;
        decode::<R>(&method, resp).await
    }

    /// Approve the filled credentials, if any.
    async fn approve_filled(&self) {
        let snapshot = self.filled.lock().unwrap().clone();
        if let (Some(helper), Some((q, c))) = (self.credentials.clone(), snapshot) {
            // Failing to store the credentials shouldn't fail the API call.
            let _ = approve_blocking(helper, q, c).await;
        }
    }

    /// Reject the filled credentials, if any, and fall back to no auth.
    async fn reject_filled(&self) {
        let snapshot = self.filled.lock().unwrap().take();
        if let (Some(helper), Some((q, c))) = (self.credentials.clone(), snapshot) {
            let _ = reject_blocking(helper, q, c).await;
            *self.auth.lock().unwrap() = Auth::None;
        }
    }
}

/// Collapse each run of slashes in a URL path to one slash.
fn collapse_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut last_was_slash = false;
    for c in path.chars() {
        if c == '/' {
            if !last_was_slash {
                out.push('/');
            }
            last_was_slash = true;
        } else {
            out.push(c);
            last_was_slash = false;
        }
    }
    out
}

/// Convert an HTTP response into either a typed body or an [`ApiError`].
///
/// `method` is the request's method, for the parse error message.
pub(crate) async fn decode<R: DeserializeOwned>(
    method: &Method,
    resp: Response,
) -> Result<R, ApiError> {
    let status = resp.status();
    if status.is_success() {
        let url = resp.url().to_string();
        let bytes = body_bytes(resp)
            .await
            .map_err(|e| ApiError::ResponseParse {
                method: method.to_string(),
                url: url.clone(),
                message: e.to_string(),
            })?;
        return serde_json::from_slice(&bytes).map_err(|e| ApiError::ResponseParse {
            method: method.to_string(),
            url,
            message: e.to_string(),
        });
    }

    let lfs_authenticate = resp
        .headers()
        .get("LFS-Authenticate")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(crate::error::parse_retry_after);
    let request_url = resp.url().to_string();
    let bytes = body_bytes(resp).await.unwrap_or_default();

    Err(ApiError::Status {
        status: status.as_u16(),
        url: Some(request_url),
        lfs_authenticate,
        body: serde_json::from_slice(&bytes).ok(),
        retry_after,
    })
}

/// Read the body of `resp`, decompressing it if it's gzipped.
///
/// Under `GIT_TRACE`, a decompressed body is traced as upstream does.
pub(crate) async fn body_bytes(resp: Response) -> std::io::Result<Vec<u8>> {
    let gzipped = resp
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip"));
    let bytes = resp.bytes().await.map_err(std::io::Error::other)?;
    if !gzipped {
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut out)?;
    if std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("http: decompressed gzipped response");
    }
    Ok(out)
}

/// Fill credentials on a blocking thread, since a helper may run
/// `git credential`.
///
/// A helper error becomes [`ApiError::CredentialsNotFound`] for `endpoint`
/// with the error as its detail, as in upstream's `FillCreds`.
async fn fill_for_endpoint(
    helper: Arc<dyn Helper>,
    query: Query,
    endpoint: &str,
    ctx: FillContext,
) -> Result<Option<Credentials>, ApiError> {
    let endpoint_str = endpoint.to_owned();
    tokio::task::spawn_blocking(move || helper.fill_with_context(&query, &ctx))
        .await
        .map_err(|e| ApiError::Decode(format!("credential helper join: {e}")))?
        .map_err(|e| ApiError::CredentialsNotFound {
            url: endpoint_str,
            detail: Some(e.to_string()),
        })
}

async fn approve_blocking(
    helper: Arc<dyn Helper>,
    query: Query,
    creds: Credentials,
) -> Result<(), ApiError> {
    tokio::task::spawn_blocking(move || helper.approve(&query, &creds))
        .await
        .map_err(|e| ApiError::Decode(format!("credential helper join: {e}")))?
        .map(|_| ())
        .map_err(|e| ApiError::Decode(format!("credential helper approve: {e}")))
}

async fn reject_blocking(
    helper: Arc<dyn Helper>,
    query: Query,
    creds: Credentials,
) -> Result<(), ApiError> {
    tokio::task::spawn_blocking(move || helper.reject(&query, &creds))
        .await
        .map_err(|e| ApiError::Decode(format!("credential helper join: {e}")))?
        .map(|_| ())
        .map_err(|e| ApiError::Decode(format!("credential helper reject: {e}")))
}

/// Turn filled credentials into an [`Auth`].
///
/// An `authtype` with a `credential` gives [`Auth::Custom`]. Anything else
/// gives [`Auth::Basic`].
fn auth_from_creds(c: &Credentials) -> Auth {
    if let (Some(at), Some(cred)) = (&c.authtype, &c.credential) {
        Auth::Custom {
            authtype: at.clone(),
            credential: cred.clone(),
        }
    } else {
        Auth::Basic {
            username: c.username.clone(),
            password: c.password.clone(),
        }
    }
}

/// Whether `GIT_TRACE` is on.
///
/// Any value other than empty, `0`, `false`, `no`, or `off` (in any case)
/// turns it on.
fn trace_enabled() -> bool {
    match std::env::var_os("GIT_TRACE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy().trim().to_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
}
