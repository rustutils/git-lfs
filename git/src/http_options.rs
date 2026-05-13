//! Read git's `http.<key>` and `http.<url>.<key>` settings.
//!
//! Mirrors the small subset upstream consults when wiring TLS into its
//! HTTP client. URL-specific overrides (`http.<url>.<key>`) are
//! resolved by longest prefix match against the request URL, the same
//! way git itself routes them.
//!
//! TLS surface is intentionally narrow (CA bundle, verify toggle, the
//! client-cert pair for mTLS). The same URL-prefix machinery is reused
//! for `http.extraHeader` (multi-value, applied to every request), the
//! Netscape-format `http.cookieFile`, and the LFS-namespace boolean
//! `lfs.<url>.contenttype`. Remaining surface (e.g.
//! `sslCertPasswordProtected`) is deferred.

use std::path::Path;
use std::process::Command;

use crate::Error;

#[derive(Debug, Default, Clone)]
pub struct HttpOptions {
    /// `http.sslcainfo` — path to a PEM bundle of trusted CAs.
    pub ssl_ca_info: Option<String>,
    /// `http.sslVerify` — false flips reqwest into accept-any-cert mode.
    /// Default is true; this only stores the explicit value.
    pub ssl_verify: Option<bool>,
    /// `http.sslCert` — path to a PEM-encoded client certificate (for
    /// mTLS). Pairs with `ssl_key`.
    pub ssl_cert: Option<String>,
    /// `http.sslKey` — path to the matching PEM-encoded private key.
    pub ssl_key: Option<String>,
    /// `http.cookieFile` — path to a Netscape-format cookie jar that
    /// should be sent with every HTTP request. Used to pass through
    /// load-balancer / proxy session cookies in front of the LFS
    /// server.
    pub cookie_file: Option<String>,
}

impl HttpOptions {
    /// Resolve options for the given `url`. Reads URL-specific
    /// `http.<url>.<key>` (longest matching prefix wins), falling back
    /// to global `http.<key>` when no override is present.
    pub fn for_url(cwd: &Path, url: &str) -> Result<Self, Error> {
        let scoped = scoped_keys(cwd, url)?;
        Ok(Self {
            ssl_ca_info: scoped
                .lookup("sslcainfo")
                .or_else(|| get_global(cwd, "http.sslcainfo").ok().flatten()),
            ssl_verify: scoped
                .lookup("sslverify")
                .or_else(|| get_global(cwd, "http.sslVerify").ok().flatten())
                .map(|v| parse_bool(&v)),
            ssl_cert: scoped
                .lookup("sslcert")
                .or_else(|| get_global(cwd, "http.sslCert").ok().flatten()),
            ssl_key: scoped
                .lookup("sslkey")
                .or_else(|| get_global(cwd, "http.sslKey").ok().flatten()),
            cookie_file: scoped
                .lookup("cookiefile")
                .or_else(|| get_global(cwd, "http.cookieFile").ok().flatten()),
        })
    }
}

/// Per-URL overrides matched by prefix. Stored as a flat list of
/// `(prefix, key, value)` so a single `git config --get-regexp` call
/// covers the whole `http.*` namespace.
struct Scoped(Vec<(String, String, String)>);

impl Scoped {
    fn lookup(&self, key: &str) -> Option<String> {
        let key = key.to_ascii_lowercase();
        // Longest prefix wins. Entries are already sorted by prefix
        // length descending in `scoped_keys`.
        self.0
            .iter()
            .find(|(_, k, _)| k.to_ascii_lowercase() == key)
            .map(|(_, _, v)| v.clone())
    }
}

fn scoped_keys(cwd: &Path, url: &str) -> Result<Scoped, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "config",
            "--includes",
            "--null",
            "--get-regexp",
            r"^http\..+\..+$",
        ])
        .output()?;
    if !out.status.success() {
        // exit 1 = no matches; treat as empty.
        return Ok(Scoped(Vec::new()));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for record in raw.split('\0').filter(|s| !s.is_empty()) {
        // `--null` separates records by NUL and key-from-value by LF.
        let (key_full, value) = match record.split_once('\n') {
            Some((k, v)) => (k, v),
            None => (record, ""),
        };
        // key_full looks like `http.<url>.<subkey>`. The middle is
        // case-sensitive (it's a URL); subkey is canonical-lowercase.
        let parts: Vec<&str> = key_full.splitn(2, '.').collect();
        if parts.len() != 2 || parts[0] != "http" {
            continue;
        }
        let rest = parts[1];
        let last_dot = rest.rfind('.').unwrap_or(rest.len());
        if last_dot == rest.len() {
            continue;
        }
        let prefix = &rest[..last_dot];
        let subkey = &rest[last_dot + 1..];
        if url_matches(prefix, url) {
            entries.push((prefix.to_owned(), subkey.to_owned(), value.to_owned()));
        }
    }
    // Longest prefix wins.
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.len()));
    Ok(Scoped(entries))
}

fn get_global(cwd: &Path, key: &str) -> Result<Option<String>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", "--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) | Some(128) | Some(129) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Read every value of `key` from the merged config view, preserving
/// config-file order. Used for multi-value keys like `http.extraHeader`
/// where `git config --add` accumulates values.
fn get_all_global(cwd: &Path, key: &str) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", "--get-all", key])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Every `http.<...>.extraHeader` and global `http.extraHeader` value
/// whose URL part matches `url`, parsed into `(Name, Value)` pairs.
///
/// Ordering is "longest URL prefix first, then global" — same
/// semantics as upstream's `lfshttp/client.go::extraHeaders()`. Lines
/// without a `:` separator are silently dropped (we can't form a
/// header from them).
///
/// Both the URL portion of the config key and the header name are
/// case-insensitive; reqwest's `HeaderName::try_from` canonicalizes
/// the name when the value is set, so `AUTHORIZATION:` and
/// `Authorization:` end up as the same header.
pub fn extra_headers_for(cwd: &Path, url: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Ok(scoped) = scoped_keys(cwd, url) {
        for (_prefix, subkey, value) in &scoped.0 {
            if !subkey.eq_ignore_ascii_case("extraheader") {
                continue;
            }
            if let Some(pair) = parse_header_line(value) {
                out.push(pair);
            }
        }
    }
    for value in get_all_global(cwd, "http.extraHeader") {
        if let Some(pair) = parse_header_line(&value) {
            out.push(pair);
        }
    }
    out
}

/// Split `Name: Value` on the first `:`. Both halves are trimmed. The
/// name must be non-empty; the value may be empty (some servers expect
/// a bare `X-Foo:` to clear a previously set header). Returns `None`
/// for unparseable lines.
fn parse_header_line(s: &str) -> Option<(String, String)> {
    let (name, value) = s.split_once(':')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some((name.to_owned(), value.trim().to_owned()))
}

/// Resolve `lfs.<url>.<subkey>` with longest URL-prefix match,
/// falling back to global `lfs.<subkey>`. Same machinery as
/// [`HttpOptions::for_url`] but for the `lfs` namespace. `default` is
/// returned when neither scope has the key set.
///
/// Only used today for `lfs.<url>.contenttype` — keep this scoped to
/// boolean config keys until a non-bool URL-scoped lfs key shows up.
pub fn lfs_url_bool(cwd: &Path, url: &str, subkey: &str, default: bool) -> bool {
    let scoped = lfs_scoped_keys(cwd, url).unwrap_or(Scoped(Vec::new()));
    if let Some(v) = scoped.lookup(subkey) {
        return parse_bool(&v);
    }
    let global_key = format!("lfs.{subkey}");
    match get_global(cwd, &global_key) {
        Ok(Some(v)) => parse_bool(&v),
        _ => default,
    }
}

/// Twin of [`scoped_keys`] that walks the `lfs.*` namespace instead
/// of `http.*`. Same URL-prefix matching + longest-prefix-first sort.
fn lfs_scoped_keys(cwd: &Path, url: &str) -> Result<Scoped, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "config",
            "--includes",
            "--null",
            "--get-regexp",
            r"^lfs\..+\..+$",
        ])
        .output()?;
    if !out.status.success() {
        return Ok(Scoped(Vec::new()));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for record in raw.split('\0').filter(|s| !s.is_empty()) {
        let (key_full, value) = match record.split_once('\n') {
            Some((k, v)) => (k, v),
            None => (record, ""),
        };
        let parts: Vec<&str> = key_full.splitn(2, '.').collect();
        if parts.len() != 2 || parts[0] != "lfs" {
            continue;
        }
        let rest = parts[1];
        let Some(last_dot) = rest.rfind('.') else {
            continue;
        };
        let prefix = &rest[..last_dot];
        let subkey = &rest[last_dot + 1..];
        if url_matches(prefix, url) {
            entries.push((prefix.to_owned(), subkey.to_owned(), value.to_owned()));
        }
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.len()));
    Ok(Scoped(entries))
}

/// Whether `prefix` (the URL fragment in `http.<url>.*`) matches the
/// request `url`. Git's rule: the prefix must be a prefix of the URL
/// up to and including a path/host boundary. We approximate with a
/// straight prefix check on the lowercased scheme+host, which is
/// enough for the test fixtures and for typical real-world configs.
fn url_matches(prefix: &str, url: &str) -> bool {
    let p = prefix.trim_end_matches('/').to_ascii_lowercase();
    let u = url.trim_end_matches('/').to_ascii_lowercase();
    u == p || u.starts_with(&format!("{p}/"))
}

fn parse_bool(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}
