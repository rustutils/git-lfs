use std::io::Write;
use std::sync::{Arc, Mutex};

use git_lfs_creds::{Credentials, Helper, Query};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::auth::Auth;
use crate::error::ApiError;

/// `Content-Type` and `Accept` value mandated by the LFS API.
///
/// See `docs/api/batch.md`. The spec also allows a `; charset=utf-8`
/// parameter; we send the bare media type (servers must accept either).
pub(crate) const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

/// HTTP client for the git-lfs API endpoints.
///
/// One instance per LFS endpoint URL. `Client` is cheap to clone and shares
/// an underlying connection pool — clone freely.
///
/// # Authentication
///
/// Two complementary mechanisms:
///
/// - [`Auth`] passed at construction is the initial auth — applied to every
///   request, no retries on 401.
/// - A credential helper attached via [`Self::with_credential_helper`] is
///   queried on a 401 response: the request is retried once with the
///   filled-in credentials, and the helper is told `approve`/`reject`
///   based on the second attempt's outcome. Once a fill succeeds, the
///   client remembers the credentials and uses them for subsequent
///   requests, so the 401 dance only happens at most once per process.
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
        }
    }

    /// Override the URL used for credential prompts and the
    /// `Git credentials for <url> not found` wording. Pass the git
    /// remote URL when it shares scheme+host with the LFS endpoint;
    /// otherwise leave unset and credentials key on the LFS endpoint.
    #[must_use]
    pub fn with_cred_url(mut self, url: Url) -> Self {
        self.cred_url = Some(url);
        self
    }

    /// Attach a credential helper. On 401, the client will call
    /// `helper.fill`, retry once with the result, then `approve`/`reject`
    /// based on the outcome.
    #[must_use]
    pub fn with_credential_helper(mut self, helper: Arc<dyn Helper>) -> Self {
        self.credentials = Some(helper);
        self
    }

    /// Toggle `credential.useHttpPath`. When `true`, the endpoint URL's
    /// path is included in the credential-fill query (so a helper can
    /// scope per-repo); when `false` (the default, matching git), only
    /// protocol+host are sent.
    #[must_use]
    pub fn with_use_http_path(mut self, on: bool) -> Self {
        self.use_http_path = on;
        self
    }

    /// Read-only access to the endpoint URL this client was built
    /// against. Used by callers that want to persist
    /// `lfs.<url>.access` after a successful authenticated request.
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// `true` if this client's current auth state is basic
    /// (username/password). Used by callers to detect whether the
    /// most recent operation actually used basic auth, so they can
    /// persist `lfs.<url>.access = basic` to local git config.
    pub fn used_basic_auth(&self) -> bool {
        matches!(*self.auth.lock().unwrap(), Auth::Basic { .. })
    }

    /// Build a URL by joining `path` onto the endpoint.
    ///
    /// `path` should be a relative path like `objects/batch` or `locks`.
    /// A trailing slash on the endpoint is added if missing so the join
    /// preserves the endpoint's full path.
    pub(crate) fn url(&self, path: &str) -> Result<Url, ApiError> {
        let mut base = self.endpoint.clone();
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        Ok(base.join(path)?)
    }

    /// Build a request, applying the current auth.
    pub(crate) fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let auth = self.auth.lock().unwrap().clone();
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(LFS_MEDIA_TYPE));
        let req = self.http.request(method, url).headers(headers);
        auth.apply(req)
    }

    /// Default credential query for this client — derived from
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

    /// Render the credential URL as a string. Used when constructing
    /// upstream-compatible error messages like
    /// `Git credentials for <url> not found`.
    fn cred_url_string(&self) -> String {
        self.cred_url.as_ref().unwrap_or(&self.endpoint).to_string()
    }

    /// POST a JSON body and decode a JSON response, with LFS error handling
    /// and the auth-retry loop.
    pub(crate) async fn post_json<B, R>(&self, path: &str, body: &B) -> Result<R, ApiError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = self.url(path)?;
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
            let _ = writeln!(err);
            let _ = err.write_all(&body_bytes);
            let _ = writeln!(err);
        }
        self.send_with_auth_retry(|| {
            self.request(Method::POST, url.clone())
                .header(CONTENT_TYPE, LFS_MEDIA_TYPE)
                .body(body_bytes.clone())
        })
        .await
    }

    /// GET a JSON response, with LFS error handling and the auth-retry loop.
    /// `query` is appended as URL query parameters.
    pub(crate) async fn get_json<Q, R>(&self, path: &str, query: &Q) -> Result<R, ApiError>
    where
        Q: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = self.url(path)?;
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
            self.request(Method::GET, u)
        })
        .await
    }

    /// Drive a single request through the credential-helper retry loop
    /// and return the (possibly second) raw `Response`. Caller is on the
    /// hook for decoding it — used by endpoints with bespoke status
    /// handling (`create_lock`'s 409 → Conflict path, mostly).
    ///
    /// `build` produces a fresh `RequestBuilder` each call — it's
    /// invoked at most twice (once with whatever auth is in place, once
    /// after a 401 → fill).
    ///
    /// Approve / reject semantics (intentionally narrow):
    /// - 2xx response: approve cached creds (in case they were freshly
    ///   filled this call, or stayed valid from a prior call).
    /// - 401 response: reject + clear cached creds. After fill+retry, a
    ///   second 401 rejects the freshly-filled creds too.
    /// - Anything else (4xx not-401, 5xx): leave the credential helper
    ///   alone; we can't tell whether auth was the problem.
    pub(crate) async fn send_with_auth_retry_response<F>(
        &self,
        build: F,
    ) -> Result<Response, ApiError>
    where
        F: Fn() -> RequestBuilder,
    {
        let resp = build().send().await?;
        if resp.status().is_success() {
            self.approve_filled().await;
            return Ok(resp);
        }
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        // 401 — try the fill+retry dance.
        let Some(helper) = self.credentials.clone() else {
            return Ok(resp);
        };
        let query = self.cred_query();
        self.reject_filled().await;
        let cred_url_str = self.cred_url_string();
        let creds = match fill_for_endpoint(helper.clone(), query.clone(), &cred_url_str).await? {
            Some(c) => c,
            // No helper had anything for this URL. Surface the upstream
            // "Git credentials for X not found" wording so callers (and
            // batch-error formatters) can distinguish "auth missing" from
            // a generic 401 the server returned for non-auth reasons.
            None => {
                return Err(ApiError::CredentialsNotFound {
                    url: cred_url_str,
                    detail: None,
                });
            }
        };
        {
            let mut auth = self.auth.lock().unwrap();
            *auth = Auth::Basic {
                username: creds.username.clone(),
                password: creds.password.clone(),
            };
        }
        {
            let mut filled = self.filled.lock().unwrap();
            *filled = Some((query.clone(), creds.clone()));
        }
        let resp2 = build().send().await?;
        if resp2.status().is_success() {
            approve_blocking(helper, query, creds).await?;
        } else if matches!(resp2.status().as_u16(), 401 | 403) {
            // Both 401 (unauthorized) and 403 (forbidden after auth)
            // mean the just-filled creds are wrong. Drop them so the
            // *next* request triggers another 401 → fill → retry
            // dance — without this reset, every subsequent request
            // would silently reuse the bad credentials and skip the
            // helper. Matches upstream's per-request `getCreds` flow.
            reject_blocking(helper, query, creds).await?;
            *self.filled.lock().unwrap() = None;
            *self.auth.lock().unwrap() = Auth::None;
        }
        Ok(resp2)
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
    let request_url = resp.url().to_string();
    let bytes = resp.bytes().await.unwrap_or_default();

    Err(ApiError::Status {
        status: status.as_u16(),
        url: Some(request_url),
        lfs_authenticate,
        body: serde_json::from_slice(&bytes).ok(),
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
) -> Result<Option<Credentials>, ApiError> {
    let endpoint_str = endpoint.to_owned();
    tokio::task::spawn_blocking(move || helper.fill(&query))
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
        .map_err(|e| ApiError::Decode(format!("credential helper reject: {e}")))
}
