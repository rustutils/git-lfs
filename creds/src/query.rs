//! Per-request input to the credential helpers.

use url::Url;

/// The fields `git credential` expects on stdin (and that `Helper`
/// implementations key on).
///
/// This intentionally mirrors the subset of `git-credential` input that
/// upstream LFS sends — `protocol`, `host`, `path`. We do **not** populate
/// `username` from the URL up-front; helpers may fill it in themselves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    pub protocol: String,
    pub host: String,
    /// Path portion. Empty string means "no path", which omits the
    /// `path=` line on stdin to git-credential. Helpers that key on path
    /// (e.g. `credential.<url>.useHttpPath`) only see this when the
    /// caller decides to pass it.
    pub path: String,
}

impl Query {
    /// Build a query from a URL. `path` is included by default — callers
    /// that want host-only matching (the upstream default) should clear
    /// it via [`Self::without_path`].
    pub fn from_url(url: &Url) -> Self {
        Self {
            protocol: url.scheme().to_owned(),
            host: host_with_port(url),
            path: url.path().trim_start_matches('/').to_owned(),
        }
    }

    /// Variant with the path cleared. Matches the default `git credential`
    /// behavior, which scopes by host only unless `useHttpPath` is set.
    pub fn without_path(mut self) -> Self {
        self.path.clear();
        self
    }
}

fn host_with_port(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        // url::Url enforces a host on http/https, so this branch only fires
        // for unusual schemes. Empty string keeps the type non-optional.
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_url_extracts_protocol_host_path() {
        let q =
            Query::from_url(&Url::parse("https://git.example.com/foo/bar.git/info/lfs").unwrap());
        assert_eq!(q.protocol, "https");
        assert_eq!(q.host, "git.example.com");
        assert_eq!(q.path, "foo/bar.git/info/lfs");
    }

    #[test]
    fn from_url_includes_port() {
        let q = Query::from_url(&Url::parse("http://localhost:8080/lfs").unwrap());
        assert_eq!(q.host, "localhost:8080");
    }

    #[test]
    fn without_path_clears_path() {
        let q = Query::from_url(&Url::parse("https://h.example/a/b").unwrap()).without_path();
        assert!(q.path.is_empty());
    }
}
