//! Per-request input to the credential helpers.

use url::Url;

/// The fields `git credential` expects on stdin, and that [`Helper`]
/// implementations key on.
///
/// Mirrors the subset of git-credential input upstream LFS sends:
/// `protocol`, `host`, `path`. `username` is intentionally not
/// pre-populated from the URL; helpers may fill it in themselves.
///
/// [`Helper`]: crate::Helper
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    /// URL scheme (`https`, `http`, `ssh`, …).
    pub protocol: String,
    /// Host portion, with `:port` suffix when the URL specifies one.
    pub host: String,
    /// Path portion. Empty string means "no path", which omits the
    /// `path=` line on stdin to git-credential. Helpers that key on path
    /// (e.g. `credential.<url>.useHttpPath`) only see this when the
    /// caller decides to pass it.
    pub path: String,
}

impl Query {
    /// Build a query from a URL.
    ///
    /// `path` is included by default; callers that want host-only
    /// matching (the upstream default) should clear it via
    /// [`Self::without_path`]. The path is **percent-decoded** to
    /// match what upstream LFS sends to `git credential` (Go's
    /// `url.URL.Path` is the decoded form), which lets
    /// `git credential`'s `protectProtocol` check see real
    /// newlines / NULs / CRs in hostile URLs rather than their `%0a` /
    /// `%00` / `%0d` forms.
    pub fn from_url(url: &Url) -> Self {
        let raw_path = url.path().trim_start_matches('/');
        Self {
            protocol: url.scheme().to_owned(),
            host: host_with_port(url),
            path: percent_decode(raw_path),
        }
    }

    /// Variant with the path cleared.
    ///
    /// Matches the default `git credential` behavior, which scopes by
    /// host only unless `useHttpPath` is set.
    pub fn without_path(mut self) -> Self {
        self.path.clear();
        self
    }
}

/// Decode `%xx` byte triples in `s`, leaving everything else verbatim.
/// Lossy on invalid UTF-8 (replaces with `U+FFFD`) — matches Go's
/// `url.URL.Path`, which always returns a string. Real-world LFS paths
/// are ASCII, so the lossy edge case is academic.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

    #[test]
    fn from_url_decodes_percent_escapes_in_path() {
        // `%0a` (newline) in the URL must reach the credential helper as a
        // literal `\n` so git's `protectProtocol` can reject it. Mirrors
        // upstream's `url.URL.Path` behavior in Go.
        let q =
            Query::from_url(&Url::parse("https://h.example/test%0aprotect-linefeed.git").unwrap());
        assert_eq!(q.path, "test\nprotect-linefeed.git");
    }

    #[test]
    fn from_url_preserves_invalid_percent_sequences() {
        // A bare `%` with non-hex following stays as-is rather than
        // crashing or eating bytes.
        let q = Query::from_url(&Url::parse("https://h.example/100%25done").unwrap());
        assert_eq!(q.path, "100%done");
    }
}
