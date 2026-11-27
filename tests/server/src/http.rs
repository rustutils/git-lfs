//! Requests and responses, with the parts of Go's `net/http` behavior the
//! tests see.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use hyper::body::{Frame, SizeHint};
use hyper::header::{HeaderName, HeaderValue};
use hyper::{HeaderMap, Response, StatusCode};

/// A request with its body read.
pub(crate) struct Request {
    pub method: String,
    /// The percent-decoded path.
    pub path: String,
    /// The query string, without the `?`.
    pub query: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Request {
    /// The first value of a header, or `""`.
    pub(crate) fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    /// A query parameter, or `""`.
    pub(crate) fn query_param(&self, name: &str) -> String {
        url::form_urlencoded::parse(self.query.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    }

    /// The path and query, as Go's `URL.String()` gives them for a request.
    pub(crate) fn url_string(&self) -> String {
        if self.query.is_empty() {
            self.path.clone()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }
}

pub(crate) type Resp = Response<Body>;

/// A response body sent in one piece.
pub(crate) struct Body {
    data: Option<Bytes>,
    /// Fail after the data, so the connection is closed. That's how Go ends
    /// a response with fewer bytes than its `Content-Length`, and how the
    /// client sees that the body was cut short.
    cut_short: bool,
    /// Whether hyper has had a chance to send the data before the failure.
    /// It drops what it hasn't written when a body fails.
    flushed: bool,
}

impl Body {
    /// Data that the response stops after, though its `Content-Length`
    /// promises more.
    pub(crate) fn cut_short(data: Bytes) -> Self {
        Self {
            data: Some(data),
            cut_short: true,
            flushed: false,
        }
    }
}

impl hyper::body::Body for Body {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if let Some(data) = self.data.take().filter(|d| !d.is_empty()) {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        if self.cut_short && !std::mem::replace(&mut self.flushed, true) {
            // hyper flushes when a body isn't ready.
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if std::mem::take(&mut self.cut_short) {
            return Poll::Ready(Some(Err(std::io::Error::other("cut short"))));
        }
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        !self.cut_short && self.data.as_ref().is_none_or(Bytes::is_empty)
    }

    fn size_hint(&self) -> SizeHint {
        match &self.data {
            Some(data) if !self.cut_short => SizeHint::with_exact(data.len() as u64),
            Some(_) => SizeHint::default(),
            None => SizeHint::with_exact(0),
        }
    }
}

/// A response with `status`, the given headers, and `body`.
pub(crate) fn respond(status: u16, headers: &[(&str, &str)], body: impl Into<Bytes>) -> Resp {
    let mut resp = Response::new(Body {
        data: Some(body.into()),
        cut_short: false,
        flushed: false,
    });
    *resp.status_mut() = StatusCode::from_u16(status).expect("valid status");
    if status == 416 {
        // Go's wording, which upstream's client prints from the status
        // line and t-batch-storage-retries matches. hyper's is RFC 9110's
        // "Range Not Satisfiable".
        resp.extensions_mut()
            .insert(hyper::ext::ReasonPhrase::from_static(
                b"Requested Range Not Satisfiable",
            ));
    }
    for (name, value) in headers {
        add_header(&mut resp, name, value);
    }
    resp
}

/// A response with only a status.
pub(crate) fn status(code: u16) -> Resp {
    respond(code, &[], Bytes::new())
}

/// Add a header to `resp`.
pub(crate) fn add_header(resp: &mut Resp, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        resp.headers_mut().append(name, value);
    }
}

/// Like Go's `http.Error`: a plain text body ending in a newline.
pub(crate) fn error(code: u16, message: &str) -> Resp {
    respond(
        code,
        &[
            ("Content-Type", "text/plain; charset=utf-8"),
            ("X-Content-Type-Options", "nosniff"),
        ],
        format!("{message}\n"),
    )
}

/// Like Go's `http.NotFound`.
pub(crate) fn not_found() -> Resp {
    error(404, "404 page not found")
}

/// An LFS error body, `{"message":...}`, with the LFS content type.
pub(crate) fn lfs_error(code: u16, message: &str) -> Resp {
    respond(
        code,
        &[("Content-Type", "application/vnd.git-lfs+json")],
        serde_json::json!({ "message": message }).to_string(),
    )
}

/// JSON followed by a newline, as Go's `json.Encoder` writes it.
pub(crate) fn json_line<T: serde::Serialize>(value: &T) -> String {
    let mut s = serde_json::to_string(value).expect("serializable");
    s.push('\n');
    s
}

/// Header names as Go's `CanonicalMIMEHeaderKey` writes them, such as
/// `Content-Type`.
pub(crate) fn canonical_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = true;
    for c in name.chars() {
        out.push(if upper {
            c.to_ascii_uppercase()
        } else {
            c.to_ascii_lowercase()
        });
        upper = c == '-';
    }
    out
}

/// Decode `%XX` escapes in a URL path.
pub(crate) fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Like Go's `url.QueryEscape`.
pub(crate) fn query_escape(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_canonical() {
        assert_eq!(canonical_name("content-type"), "Content-Type");
        assert_eq!(canonical_name("x-check-OBJECT"), "X-Check-Object");
        assert_eq!(canonical_name("lfs-authenticate"), "Lfs-Authenticate");
    }

    #[test]
    fn paths_are_decoded() {
        assert_eq!(percent_decode("/a%20b/c%2"), "/a b/c%2");
        assert_eq!(percent_decode("/plain"), "/plain");
    }
}
