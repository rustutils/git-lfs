//! The HTTP client for each action URL.

use std::fmt;
use std::sync::Arc;

/// Picks the `reqwest::Client` for each action URL.
///
/// Upstream looks up the `http.<url>.*` TLS settings for each request's
/// own URL, so a storage host can need different settings from the LFS
/// endpoint. [`per_url`](Self::per_url) lets the caller pick a client for
/// each URL, and [`single`](Self::single) uses one client for all of them.
///
/// The clients must not follow redirects themselves, as with
/// [`git_lfs_api::http_client`].
#[derive(Clone)]
pub struct HttpClients(Source);

#[derive(Clone)]
enum Source {
    Single(reqwest::Client),
    PerUrl(Arc<dyn Fn(&str) -> reqwest::Client + Send + Sync>),
}

impl HttpClients {
    /// Use `client` for every URL.
    pub fn single(client: reqwest::Client) -> Self {
        Self(Source::Single(client))
    }

    /// Call `pick` with each request's URL to get its client.
    ///
    /// It's called for every request, so it should be cheap, for example
    /// by caching clients.
    pub fn per_url(pick: impl Fn(&str) -> reqwest::Client + Send + Sync + 'static) -> Self {
        Self(Source::PerUrl(Arc::new(pick)))
    }

    /// The client for a request to `url`.
    pub fn for_url(&self, url: &str) -> reqwest::Client {
        match &self.0 {
            Source::Single(client) => client.clone(),
            Source::PerUrl(pick) => pick(url),
        }
    }
}

impl From<reqwest::Client> for HttpClients {
    fn from(client: reqwest::Client) -> Self {
        Self::single(client)
    }
}

impl fmt::Debug for HttpClients {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Source::Single(_) => f.write_str("HttpClients::Single"),
            Source::PerUrl(_) => f.write_str("HttpClients::PerUrl"),
        }
    }
}
