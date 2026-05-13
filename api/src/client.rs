use std::io::Write;
use std::sync::{Arc, Mutex};

use git_lfs_creds::{Credentials, FillContext, Helper, Query};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::auth::Auth;
use crate::error::ApiError;
use crate::ssh::{SharedSshResolver, SshAuth, SshOperation};

/// `Content-Type` and `Accept` value mandated by the LFS API.
///
/// See `docs/api/batch.md`. The spec also allows a `; charset=utf-8`
/// parameter; we send the bare media type (servers must accept either).
pub(crate) const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

/// Maximum total auth attempts per request, matching upstream's
/// `lfsapi/auth.go::defaultMaxAuthAttempts`. The retry loop sends with
/// whatever auth is in place, and on each 401 rejects + refills via
/// the helper chain — so the second attempt typically falls through to
/// the next helper. Exhausting the budget emits
/// `api: too many authentication attempts` and surfaces
/// [`ApiError::AuthAttemptsExceeded`].
const MAX_AUTH_ATTEMPTS: u32 = 3;

/// HTTP client for the git-lfs API endpoints.
///
/// One instance per LFS endpoint URL. `Client` is cheap to clone and
/// shares an underlying connection pool, so clone freely.
///
/// # Authentication
///
/// Two complementary mechanisms:
///
/// - [`Auth`] passed at construction is the initial auth, applied to
///   every request with no retry on 401.
/// - A credential helper attached via [`Self::with_credential_helper`]
///   is queried on a 401 response: the request is retried once with
///   the filled-in credentials, and the helper is told `approve` or
///   `reject` based on the second attempt's outcome. Once a fill
///   succeeds, the client remembers the credentials and uses them
///   for subsequent requests, so the 401 dance happens at most once
///   per process.
#[derive(Clone)]
pub struct Client {
    pub(crate) endpoint: Url,
    pub(crate) http: reqwest::Client,
    pub(crate) auth: Arc<Mutex<Auth>>,
    pub(crate) credentials: Option<Arc<dyn Helper>>,
    /// Cached creds + query they were filled for. `None` means we haven't
    /// successfully filled yet (but may have an initial `Auth`).
    pub(crate) filled: Arc<Mutex<Option<(Query, Credentials)>>>,
    /// Mirrors `credential.useHttpPath` (default `false`). When set, the
    /// endpoint URL's path is included in the credential-fill query, so
    /// helpers can scope per-repo. Off by default to match git's host-only
    /// scoping.
    pub(crate) use_http_path: bool,
    /// URL used for credential-fill prompts and "Git credentials for X
    /// not found" wording. When the LFS endpoint and the git remote URL
    /// share scheme+host, upstream uses the **git** URL here so prompts
    /// read like `Username for "https://host/repo"` instead of
    /// `https://host/repo.git/info/lfs`. `None` falls back to
    /// [`Self::endpoint`].
    pub(crate) cred_url: Option<Url>,
    /// SSH-mediated auth resolver (`git-lfs-authenticate`). Called once
    /// per request; a non-empty `href` overrides the endpoint URL for
    /// that call, and headers are merged into the outgoing request.
    /// `None` means "not an SSH endpoint" — request flow is unchanged.
    pub(crate) ssh_resolver: Option<SharedSshResolver>,
    /// Snapshot of `http.<url>.extraHeader` values for `GIT_CURL_VERBOSE`
    /// logging. The headers themselves are already installed on the
    /// underlying `reqwest::Client` via `default_headers`, so they ride
    /// along on every request — we just don't have a way to read them
    /// back out for the verbose dump. Keeping a parallel copy here is
    /// cheap and lets the dump match upstream's `> Name: Value` form
    /// (which the `t-extra-header.sh` greps look for).
    pub(crate) extra_headers: Vec<(String, String)>,
    /// Mirrors `credential.<url>.skipwwwauth`. When `true`, the auth
    /// loop suppresses the `wwwauth[]` forwarding into `git credential
    /// fill`. Some helpers (e.g. `git-credential-manager` setups
    /// configured against a different realm than the LFS server
    /// advertises) trip on the extra input.
    pub(crate) skip_wwwauth: bool,
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
    /// Build a client rooted at the given LFS endpoint.
    ///
    /// `endpoint` is the LFS server URL (e.g.
    /// `https://git-server.com/foo/bar.git/info/lfs`). Subpaths
    /// (`/objects/batch`, `/locks`, …) are joined onto it per request.
    pub fn new(endpoint: Url, auth: Auth) -> Self {
        Self::with_http_client(endpoint, auth, reqwest::Client::new())
    }

    /// Like [`new`](Self::new) but reuses a caller-supplied `reqwest::Client`.
    ///
    /// Useful for sharing a connection pool, custom timeouts, proxies, etc.
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
            extra_headers: Vec::new(),
            skip_wwwauth: false,
        }
    }

    /// Tell the client which `http.<url>.extraHeader` values are
    /// installed on the underlying `reqwest::Client`, so we can echo
    /// them under `GIT_CURL_VERBOSE`.
    ///
    /// Doesn't change what's sent: the reqwest client's `default_headers`
    /// already carries them.
    #[must_use]
    pub fn with_extra_headers_for_verbose(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Attach an SSH auth resolver.
    ///
    /// Called once per request to resolve
    /// `git-lfs-authenticate` output; a non-empty returned `href`
    /// overrides the endpoint URL for that request and the returned
    /// headers are merged in. Pass when the LFS endpoint is reached via
    /// SSH (`ssh://...` URL or bare `git@host:repo`); leave unset for
    /// pure-HTTPS endpoints.
    #[must_use]
    pub fn with_ssh_resolver(mut self, resolver: SharedSshResolver) -> Self {
        self.ssh_resolver = Some(resolver);
        self
    }

    /// Override the URL used for credential prompts and the
    /// `Git credentials for <url> not found` wording.
    ///
    /// Pass the git remote URL when it shares scheme+host with the LFS endpoint;
    /// otherwise leave unset and credentials key on the LFS endpoint.
    #[must_use]
    pub fn with_cred_url(mut self, url: Url) -> Self {
        self.cred_url = Some(url);
        self
    }

    /// Attach a credential helper.
    ///
    /// On 401, the client will call `helper.fill`, retry once with the
    /// result, then `approve`/`reject` based on the outcome.
    #[must_use]
    pub fn with_credential_helper(mut self, helper: Arc<dyn Helper>) -> Self {
        self.credentials = Some(helper);
        self
    }

    /// Toggle `credential.useHttpPath`.
    ///
    /// When `true`, the endpoint URL's path is included in the credential-fill
    /// query (so a helper can scope per-repo); when `false` (the default,
    /// matching git), only protocol+host are sent.
    #[must_use]
    pub fn with_use_http_path(mut self, on: bool) -> Self {
        self.use_http_path = on;
        self
    }

    /// Toggle `credential.<url>.skipwwwauth`.
    ///
    /// When `true`, the auth loop will not forward
    /// `WWW-Authenticate` response headers as `wwwauth[]=…` lines on
    /// the next `git credential fill` input.
    #[must_use]
    pub fn with_skip_wwwauth(mut self, on: bool) -> Self {
        self.skip_wwwauth = on;
        self
    }

    /// Read-only access to the endpoint URL this client was built
    /// against.
    ///
    /// Used by callers that want to persist
    /// `lfs.<url>.access` after a successful authenticated request.
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// Check if this client's current auth state is basic
    /// (username/password).
    ///
    /// Used by callers to detect whether the
    /// most recent operation actually used basic auth, so they can
    /// persist `lfs.<url>.access = basic` to local git config.
    pub fn used_basic_auth(&self) -> bool {
        matches!(*self.auth.lock().unwrap(), Auth::Basic { .. })
    }

    /// Join `path` onto an explicit base URL.
    ///
    /// Used both for the configured endpoint and for SSH-resolved
    /// `href` overrides — the latter replaces the endpoint for a
    /// single request.
    pub(crate) fn join(base: &Url, path: &str) -> Result<Url, ApiError> {
        let mut base = base.clone();
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        Ok(base.join(path)?)
    }

    /// Resolve SSH auth (if a resolver is attached) for `operation`.
    ///
    /// Returns the effective base URL (`href` override or the configured
    /// endpoint) plus headers to merge into the request. With no
    /// resolver, returns `(self.endpoint.clone(), {})`.
    pub(crate) fn resolve_ssh(&self, operation: SshOperation) -> Result<(Url, SshAuth), ApiError> {
        let Some(resolver) = self.ssh_resolver.as_ref() else {
            return Ok((self.endpoint.clone(), SshAuth::default()));
        };
        let auth = resolver.resolve(operation)?;
        let base = if auth.href.is_empty() {
            self.endpoint.clone()
        } else {
            let mut u = Url::parse(&auth.href)
                .map_err(|e| ApiError::Decode(format!("ssh href {:?}: {e}", auth.href)))?;
            // Collapse consecutive slashes in the path. The reference
            // `lfs-ssh-echo` test server produces hrefs like
            // `http://host:port//repo.git/info/lfs` because the path
            // argument we pass to `git-lfs-authenticate` already starts
            // with `/`. Go's `http.ServeMux` 301-redirects double-slash
            // paths to the cleaned form, and reqwest converts POST→GET
            // on 301. Upstream Go's HTTP client preserves the method,
            // so it never trips on this; we have to normalize ourselves.
            let path = u.path().to_owned();
            let cleaned = collapse_slashes(&path);
            if cleaned != path {
                u.set_path(&cleaned);
            }
            u
        };
        Ok((base, auth))
    }

    /// Build a request with the configured auth applied, then merge
    /// `ssh.headers` on top
    ///
    /// Lets SSH-issued `Authorization` headers
    /// override what we'd otherwise apply from the credential helper.
    /// Pass `&SshAuth::default()` for non-SSH calls.
    pub(crate) fn request_with_headers(
        &self,
        method: Method,
        url: Url,
        ssh: &SshAuth,
    ) -> RequestBuilder {
        let auth = self.auth.lock().unwrap().clone();
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(LFS_MEDIA_TYPE));
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

    /// Default credential query for this client, derived from
    /// [`Self::cred_url`] when set (the git remote URL), otherwise from
    /// [`Self::endpoint`]. Path is cleared unless `use_http_path` is
    /// set (matches `git credential`'s host-only default and the
    /// `credential.useHttpPath` knob).
    fn cred_query(&self) -> Query {
        let url = self.cred_url.as_ref().unwrap_or(&self.endpoint);
        let q = Query::from_url(url);
        if self.use_http_path {
            q
        } else {
            q.without_path()
        }
    }

    /// Render the credential URL as a string.
    ///
    /// Used when constructing upstream-compatible error messages like
    /// `Git credentials for <url> not found`.
    fn cred_url_string(&self) -> String {
        self.cred_url.as_ref().unwrap_or(&self.endpoint).to_string()
    }

    /// POST a JSON body and decode a JSON response, with LFS error handling
    /// and the auth-retry loop.
    ///
    /// `op` selects the `git-lfs-authenticate`
    /// operation when an SSH resolver is attached.
    pub(crate) async fn post_json<B, R>(
        &self,
        path: &str,
        body: &B,
        op: SshOperation,
    ) -> Result<R, ApiError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let (base, ssh) = self.resolve_ssh(op)?;
        let url = Self::join(&base, path)?;
        let body_bytes = serde_json::to_vec(body)
            .map_err(|e| ApiError::Decode(format!("serializing request body: {e}")))?;
        // GIT_CURL_VERBOSE mimics upstream's libcurl-backed dump: shell
        // tests grep request bodies (e.g. t-batch-transfer test 2 verifies
        // descending-size object order in the upload batch). reqwest
        // doesn't emit this on its own, so write the body to stderr
        // ourselves when the env is set.
        if std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0") {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "> POST {url}");
            let _ = writeln!(err, "> Content-Type: {LFS_MEDIA_TYPE}");
            // Mirror upstream's curl-style dump of `http.extraHeader`
            // values — `t-extra-header.sh` greps for `> X-Foo: bar`
            // and similar. Reqwest's `default_headers` carries these
            // bytes on the wire; the parallel snapshot here exists
            // purely so we can name them in the dump.
            for (name, value) in &self.extra_headers {
                let _ = writeln!(err, "> {name}: {value}");
            }
            // Note: the `Authorization` header is dumped per attempt
            // from inside the auth retry loop (see
            // `send_with_auth_retry_response`), not here. Multistage
            // tests grep one line per attempt and the auth changes
            // between attempts, so a one-time dump up here wouldn't
            // capture the handshake.
            let _ = writeln!(err);
            let _ = err.write_all(&body_bytes);
            let _ = writeln!(err);
        }
        self.send_with_auth_retry(|| {
            self.request_with_headers(Method::POST, url.clone(), &ssh)
                .header(CONTENT_TYPE, LFS_MEDIA_TYPE)
                .body(body_bytes.clone())
        })
        .await
    }

    /// GET a JSON response, with LFS error handling and the auth-retry loop.
    ///
    /// `query` is appended as URL query parameters. `op` selects the
    /// `git-lfs-authenticate` operation when an SSH resolver is attached.
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
        // serde_urlencoded is what reqwest uses internally; serializing
        // to a String once means the closure can rebuild the request
        // cheaply on retry without re-running the serializer.
        let qs = serde_urlencoded::to_string(query)
            .map_err(|e| ApiError::Decode(format!("serializing query: {e}")))?;
        self.send_with_auth_retry(|| {
            let mut u = url.clone();
            if !qs.is_empty() {
                u.set_query(Some(&qs));
            }
            self.request_with_headers(Method::GET, u, &ssh)
        })
        .await
    }

    /// Drive a single request through the credential-helper retry loop
    /// and return the raw `Response`. Caller is on the hook for
    /// decoding it — used by endpoints with bespoke status handling
    /// (`create_lock`'s 409 → Conflict path, mostly).
    ///
    /// `build` produces a fresh `RequestBuilder` each call. The loop
    /// invokes it up to [`MAX_AUTH_ATTEMPTS`] times; between attempts
    /// it rejects the current credentials (so helpers like netrc set
    /// their skip flag and the next fill falls through the chain),
    /// refills via the helper chain, and re-sends with the new auth.
    ///
    /// Mirrors upstream's `lfsapi/auth.go::DoWithAuth`, including the
    /// `api: too many authentication attempts` trace + error on
    /// overrun.
    pub(crate) async fn send_with_auth_retry_response<F>(
        &self,
        build: F,
    ) -> Result<Response, ApiError>
    where
        F: Fn() -> RequestBuilder,
    {
        // Preemptive fill: once we've successfully resolved credentials
        // for this endpoint, re-walk the helper chain on every
        // subsequent request. The chain returns the same creds from
        // cache (no extra cost), but helpers that trace their fill
        // (notably netrc) get to log a line — matching upstream's
        // `lfshttp/auth.go::setRequestAuth` behavior, which fires
        // helper.Fill every time an endpoint is in access=basic
        // mode. `t-credentials.sh`'s netrc tests count these traces
        // (2 fill + 2 approve per push); without this, we'd log 1
        // fill + 2 approves and miss the count.
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

        // The first send goes out with whatever auth is currently set
        // (anonymous for fresh requests, cached Basic for repeats).
        // When we start anonymous, the initial 401 isn't counted
        // toward the budget — upstream's `DoWithAuth` bumps
        // `maxAuthAttempts++` for `NoneAccess`. Subsequent sends all
        // carry auth from the prior fill and count toward the limit.
        let starts_anonymous = matches!(*self.auth.lock().unwrap(), Auth::None);
        let max = if starts_anonymous {
            MAX_AUTH_ATTEMPTS + 1
        } else {
            MAX_AUTH_ATTEMPTS
        };

        // `wwwauth[]` accumulates across attempts: each 401 may carry
        // one or more `WWW-Authenticate` headers we forward to the
        // next `git credential fill` so helpers can pick a scheme.
        // Suppressed when `credential.<url>.skipwwwauth` is set.
        let mut wwwauth: Vec<String> = Vec::new();
        // `state[]` carries the helper's continuation tokens across a
        // multistage handshake; populated from the previous fill's
        // response.
        let mut state: Vec<String> = Vec::new();

        let verbose =
            std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0");

        for attempt in 0..max {
            if verbose && let Some(masked) = self.auth.lock().unwrap().masked_header() {
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "> {masked}");
            }
            let resp = build().send().await?;
            let status = resp.status();

            if status.is_success() {
                self.approve_filled().await;
                return Ok(resp);
            }
            if status.as_u16() != 401 {
                return Ok(resp);
            }

            // 401. We can only refill if a helper is configured.
            let Some(helper) = self.credentials.clone() else {
                return Ok(resp);
            };

            // Capture authenticate-challenge values from this response
            // before we drop it. Each 401 may carry multiple
            // challenges; we forward every Basic/Bearer/etc. line
            // verbatim — git credential's helpers pick the one they
            // support. Upstream looks at both `Lfs-Authenticate` (LFS
            // server's preferred name) and `Www-Authenticate` (HTTP
            // standard), in that order, matching
            // `lfsapi/auth.go::authenticateHeaders`.
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

            // Multistage flag + state from the creds we just sent.
            // Upstream's `DoWithAuth` suppresses `helper.Reject` for
            // multistage creds (`!multistage` gate at `lfsapi/auth.go:107`)
            // because they aren't "wrong" — they're mid-handshake.
            // State carries to the next fill so the helper can resume.
            let (was_multistage, prev_state) = {
                let filled = self.filled.lock().unwrap();
                filled.as_ref().map_or((false, Vec::new()), |(_, c)| {
                    (c.multistage, c.state.clone())
                })
            };
            if !prev_state.is_empty() {
                state = prev_state;
            }

            // Out of attempts. Mirror upstream's trace + error
            // wording. Reject the last filled set unless it was a
            // multistage cred (those aren't "wrong"; the handshake
            // just didn't complete in time).
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

            // Reject the creds we just sent — unless they were
            // multistage. Reject clears the cache entry and sets
            // netrc's skip flag so the next walk falls through to the
            // next helper. For multistage we just clear the filled
            // record (no helper notification) so the next fill
            // reaches the helper that drove the handshake.
            if was_multistage {
                *self.filled.lock().unwrap() = None;
                *self.auth.lock().unwrap() = Auth::None;
            } else {
                self.reject_filled().await;
            }

            // Refill for the next attempt. Carry wwwauth[] so the
            // helper can pick the right scheme, and state[] so a
            // multistage handshake can resume. Advertise the
            // capabilities the API client can act on.
            let query = self.cred_query();
            let cred_url_str = self.cred_url_string();
            let ctx = FillContext {
                wwwauth: wwwauth.clone(),
                state: state.clone(),
                capabilities: vec!["authtype".to_owned(), "state".to_owned()],
            };
            let creds =
                match fill_for_endpoint(helper.clone(), query.clone(), &cred_url_str, ctx).await? {
                    Some(c) => c,
                    // No helper had anything for this URL. Surface upstream's
                    // "Git credentials for X not found" wording so callers
                    // (and batch-error formatters) can distinguish "auth
                    // missing" from a generic 401.
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
            // Loop continues for the next attempt with the new auth.
        }

        unreachable!("the loop returns or errors before exhausting max attempts")
    }

    /// Like [`send_with_auth_retry_response`] but decodes a JSON body.
    /// Used by `post_json` / `get_json`.
    async fn send_with_auth_retry<F, R>(&self, build: F) -> Result<R, ApiError>
    where
        F: Fn() -> RequestBuilder,
        R: DeserializeOwned,
    {
        let resp = self.send_with_auth_retry_response(build).await?;
        decode::<R>(resp).await
    }

    async fn approve_filled(&self) {
        let snapshot = self.filled.lock().unwrap().clone();
        if let (Some(helper), Some((q, c))) = (self.credentials.clone(), snapshot) {
            // Approve is best-effort — a failure to write to the keystore
            // shouldn't fail the user's API call.
            let _ = approve_blocking(helper, q, c).await;
        }
    }

    async fn reject_filled(&self) {
        let snapshot = self.filled.lock().unwrap().take();
        if let (Some(helper), Some((q, c))) = (self.credentials.clone(), snapshot) {
            let _ = reject_blocking(helper, q, c).await;
            *self.auth.lock().unwrap() = Auth::None;
        }
    }
}

/// Collapse consecutive `/` runs in a URL path to a single `/`.
/// Preserves a single leading slash if the input was rooted.
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
pub(crate) async fn decode<R: DeserializeOwned>(resp: Response) -> Result<R, ApiError> {
    let status = resp.status();
    if status.is_success() {
        let bytes = resp.bytes().await?;
        return serde_json::from_slice(&bytes).map_err(|e| ApiError::Decode(e.to_string()));
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
    let bytes = resp.bytes().await.unwrap_or_default();

    Err(ApiError::Status {
        status: status.as_u16(),
        url: Some(request_url),
        lfs_authenticate,
        body: serde_json::from_slice(&bytes).ok(),
        retry_after,
    })
}

/// `Helper` is a sync trait — wrap each call in `spawn_blocking` so we don't
/// stall the executor while git-credential's subprocess runs.
///
/// On a helper-side error (e.g. `protectProtocol` rejected a malformed
/// URL), surface it as [`ApiError::CredentialsNotFound`] keyed on
/// `endpoint`. Matches upstream's `FillCreds` wrapping so the underlying
/// "credential value for path contains newline" message reaches the user
/// alongside the "Git credentials for X not found" header.
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

/// Translate a credential-helper response into the [`Auth`] variant
/// we'll send on the wire. Authtype-style creds (`authtype` +
/// `credential` advertised via `capability[]=authtype`) become
/// [`Auth::Custom`] and drive a literal `Authorization: <scheme>
/// <credential>` header; the classic `username` + `password` shape
/// becomes [`Auth::Basic`].
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

/// Mirrors git's `GIT_TRACE` semantics: any value other than `""`,
/// `0`, `false`, `no`, `off` enables tracing.
fn trace_enabled() -> bool {
    match std::env::var_os("GIT_TRACE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy().trim().to_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
}
