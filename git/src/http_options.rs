//! Read git's `http.<key>` and `http.<url>.<key>` settings.
//!
//! Mirrors the small subset upstream consults when wiring TLS into its
//! HTTP client. URL-specific overrides (`http.<url>.<key>`) are
//! resolved by longest prefix match against the request URL, the same
//! way git itself routes them.
//!
//! Only a few keys are surfaced for now (CA bundle, verify toggle, and
//! the client-cert pair for mTLS); the rest of the surface
//! (`sslCertPasswordProtected`, `cookieFile`, …) is on the deferred
//! list.

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
