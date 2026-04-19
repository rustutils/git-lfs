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
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) endpoint: Url,
    pub(crate) http: reqwest::Client,
    pub(crate) auth: Auth,
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
        Self { endpoint, http, auth }
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

    pub(crate) fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(LFS_MEDIA_TYPE));
        let req = self.http.request(method, url).headers(headers);
        self.auth.apply(req)
    }

    /// POST a JSON body and decode a JSON response, with LFS error handling.
    pub(crate) async fn post_json<B, R>(&self, path: &str, body: &B) -> Result<R, ApiError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = self.url(path)?;
        let resp = self
            .request(Method::POST, url)
            .header(CONTENT_TYPE, LFS_MEDIA_TYPE)
            .json(body)
            .send()
            .await?;
        decode(resp).await
    }

    /// GET a JSON response, with LFS error handling. `query` is appended as
    /// URL query parameters.
    pub(crate) async fn get_json<Q, R>(&self, path: &str, query: &Q) -> Result<R, ApiError>
    where
        Q: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = self.url(path)?;
        let resp = self
            .request(Method::GET, url)
            .query(query)
            .send()
            .await?;
        decode(resp).await
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
    let bytes = resp.bytes().await.unwrap_or_default();

    Err(ApiError::Status {
        status: status.as_u16(),
        lfs_authenticate,
        body: serde_json::from_slice(&bytes).ok(),
    })
}
