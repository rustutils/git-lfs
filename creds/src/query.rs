//! The key credential helpers look credentials up by.

use url::Url;

/// What a [`Helper`] looks credentials up by.
///
/// These are the `protocol`, `host`, and `path` fields of `git credential`
/// input. A username in the URL is not included.
///
/// [`Helper`]: crate::Helper
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    /// URL scheme, such as `https`.
    pub protocol: String,
    /// Host, with `:port` when the URL has a port other than the scheme's
    /// default.
    pub host: String,
    /// Path without the leading slash, or empty for none.
    ///
    /// An empty path isn't sent to `git credential`, so helpers match on
    /// host alone.
    pub path: String,
}

impl Query {
    /// Build a query from a URL, including its path.
    ///
    /// Call [`without_path`](Self::without_path) for host-only matching,
    /// which is what `git credential` does unless `useHttpPath` is set.
    ///
    /// The path is percent-decoded, as upstream sends it. That way a
    /// newline, NUL, or carriage return hidden as `%0a`, `%00`, or `%0d`
    /// reaches the check that rejects it.
    pub fn from_url(url: &Url) -> Self {
        let raw_path = url.path().trim_start_matches('/');
        Self {
            protocol: url.scheme().to_owned(),
            host: host_with_port(url),
            path: percent_decode(raw_path),
        }
    }

    /// The same query with the path cleared.
    pub fn without_path(mut self) -> Self {
        self.path.clear();
        self
    }
}

/// Decode `%xx` escapes in `s`, leaving everything else as is.
///
/// Bytes that aren't valid UTF-8 after decoding become `U+FFFD`.
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
        // Only reachable for schemes without a host, since `url` requires
        // one for http and https.
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
        // `%25` decodes to a literal `%`, which isn't decoded again.
        let q = Query::from_url(&Url::parse("https://h.example/100%25done").unwrap());
        assert_eq!(q.path, "100%done");
    }
}
