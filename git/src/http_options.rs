//! Read git's `http.<key>` and `http.<url>.<key>` settings.
//!
//! [`HttpOptions`] holds the TLS settings and `http.cookieFile`.
//! [`ExtraHeaders`] gives the `http.extraHeader` values for a URL, and
//! [`lfs_url_bool`] reads URL-scoped `lfs.<url>.<key>` booleans.
//!
//! A URL-scoped key applies when its URL equals the request URL or is a
//! prefix of it ending at a `/`, compared without case. The longest match
//! wins. This is simpler than git's matching, which also handles
//! wildcards, user names and default ports.
//!
//! Other TLS settings, such as `http.sslCertPasswordProtected`, are not
//! read.

use std::path::Path;
use std::process::Command;

use crate::Error;

/// HTTP settings that apply to one URL.
#[derive(Debug, Default, Clone)]
pub struct HttpOptions {
    /// `http.sslCAInfo`: path to a PEM bundle of trusted CAs.
    pub ssl_ca_info: Option<String>,
    /// `http.sslVerify`: whether to verify the server's certificate.
    ///
    /// `None` when unset or not a boolean, which means verify. Only `false`,
    /// `no`, `off`, `0`, and `f` turn verification off. Upstream also turns
    /// it off for a global value it doesn't recognize, such as a typo.
    pub ssl_verify: Option<bool>,
    /// The `http.sslVerify` value, when it's set but isn't a boolean.
    ///
    /// Verification stays on. The caller can warn about it.
    pub ssl_verify_invalid: Option<String>,
    /// `http.sslCert`: path to a PEM client certificate, used with
    /// [`Self::ssl_key`].
    pub ssl_cert: Option<String>,
    /// `http.sslKey`: path to the client certificate's PEM private key.
    pub ssl_key: Option<String>,
    /// `http.cookieFile`: path to a Netscape-format cookie file to send
    /// cookies from, such as a load balancer's session cookie.
    pub cookie_file: Option<String>,
}

impl HttpOptions {
    /// Read the settings for `url`.
    ///
    /// Each setting comes from the best matching `http.<url>.<key>`, or
    /// from `http.<key>` when none matches.
    pub fn for_url(cwd: &Path, url: &str) -> Result<Self, Error> {
        let scoped = scoped_keys(cwd, url)?;
        let ssl_verify_raw = scoped
            .lookup("sslverify")
            .or_else(|| get_global(cwd, "http.sslVerify").ok().flatten());
        let ssl_verify = ssl_verify_raw.as_deref().and_then(parse_ssl_verify);
        let ssl_verify_invalid = ssl_verify_raw.filter(|v| !v.is_empty() && ssl_verify.is_none());
        Ok(Self {
            ssl_ca_info: scoped
                .lookup("sslcainfo")
                .or_else(|| get_global(cwd, "http.sslcainfo").ok().flatten()),
            ssl_verify,
            ssl_verify_invalid,
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

/// URL-scoped entries that match one URL, as `(url, key, value)`, longest
/// URL first.
struct Scoped(Vec<(String, String, String)>);

impl Scoped {
    /// The value of `key` from the longest matching URL.
    fn lookup(&self, key: &str) -> Option<String> {
        let key = key.to_ascii_lowercase();
        self.0
            .iter()
            .find(|(_, k, _)| k.to_ascii_lowercase() == key)
            .map(|(_, _, v)| v.clone())
    }
}

/// Read the `http.<url>.<key>` entries that match `url`, longest URL first.
fn scoped_keys(cwd: &Path, url: &str) -> Result<Scoped, Error> {
    let mut entries: Vec<_> = read_scoped_entries(cwd)?
        .into_iter()
        .filter(|(prefix, _, _)| url_matches(prefix, url))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.len()));
    Ok(Scoped(entries))
}

/// Read every `http.<url>.<key>` entry as `(url, key, value)`.
fn read_scoped_entries(cwd: &Path) -> Result<Vec<(String, String, String)>, Error> {
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
        // Exit 1 means nothing matched. Other failures are treated the
        // same.
        return Ok(Vec::new());
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for record in raw.split('\0').filter(|s| !s.is_empty()) {
        // With `--null`, each record is the key, a newline, then the
        // value.
        let (key_full, value) = match record.split_once('\n') {
            Some((k, v)) => (k, v),
            None => (record, ""),
        };
        // `http.<url>.<subkey>`, where the URL may contain dots.
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
        entries.push((prefix.to_owned(), subkey.to_owned(), value.to_owned()));
    }
    Ok(entries)
}

/// Read `key` from git's merged config.
///
/// Returns `Ok(None)` if the key is unset or the config can't be read.
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

/// Read every value of `key` from git's merged config, in config order.
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

/// `http.extraHeader` settings, looked up per request URL.
///
/// [`ExtraHeaders::load`] reads the config once. Upstream applies these to
/// every request, including those to storage hosts in action URLs, so look
/// them up for each request's own URL.
#[derive(Debug, Clone, Default)]
pub struct ExtraHeaders {
    /// `(url, header line)` for each `http.<url>.extraHeader` value.
    scoped: Vec<(String, String)>,
    /// Each `http.extraHeader` value.
    global: Vec<String>,
}

impl ExtraHeaders {
    /// Read the `http.extraHeader` and `http.<url>.extraHeader` values.
    pub fn load(cwd: &Path) -> Self {
        let scoped = read_scoped_entries(cwd)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, subkey, _)| subkey.eq_ignore_ascii_case("extraheader"))
            .map(|(prefix, _, value)| (prefix, value))
            .collect();
        let global = get_all_global(cwd, "http.extraHeader");
        Self { scoped, global }
    }

    /// The headers for a request to `url`, as `(name, value)` pairs.
    ///
    /// Values from matching `http.<url>.extraHeader` keys come first,
    /// longest URL first, then those from `http.extraHeader`, as in
    /// upstream's `extraHeaders` in `lfshttp/client.go`. Values without a `:`
    /// are skipped. Header names are returned as written.
    pub fn for_url(&self, url: &str) -> Vec<(String, String)> {
        let mut scoped: Vec<&(String, String)> = self
            .scoped
            .iter()
            .filter(|(prefix, _)| url_matches(prefix, url))
            .collect();
        scoped.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
        scoped
            .into_iter()
            .map(|(_, line)| line)
            .chain(&self.global)
            .filter_map(|line| parse_header_line(line))
            .collect()
    }
}

/// Split `Name: Value` at the first `:` and trim both halves.
///
/// Returns `None` if there is no `:` or the name is empty. The value may
/// be empty.
fn parse_header_line(s: &str) -> Option<(String, String)> {
    let (name, value) = s.split_once(':')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some((name.to_owned(), value.trim().to_owned()))
}

/// Read the boolean `lfs.<url>.<subkey>` that best matches `url`.
///
/// Falls back to `lfs.<subkey>`, then to `default` when neither is set.
/// URLs match as in [`HttpOptions::for_url`]. A set value other than
/// `true`, `1`, `yes` or `on` reads as false.
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

/// Read the string `lfs.<url>.<subkey>` that best matches `url`.
///
/// Falls back to `lfs.<subkey>`. URLs match as in [`HttpOptions::for_url`].
pub fn lfs_url_value(cwd: &Path, url: &str, subkey: &str) -> Option<String> {
    url_value(cwd, "lfs", url, subkey)
}

/// Read the string `<section>.<url>.<subkey>` that best matches `url`,
/// such as `lfs.transfer.<url>.httpDownloadEncoding`.
///
/// Falls back to `<section>.<subkey>`. URLs match as in
/// [`HttpOptions::for_url`].
pub fn url_value(cwd: &Path, section: &str, url: &str, subkey: &str) -> Option<String> {
    let scoped = scoped_keys_in(cwd, section, url).unwrap_or(Scoped(Vec::new()));
    if let Some(v) = scoped.lookup(subkey) {
        return Some(v);
    }
    get_global(cwd, &format!("{section}.{subkey}"))
        .ok()
        .flatten()
}

/// Read the `lfs.<url>.<key>` entries that match `url`, like
/// [`scoped_keys`] does for `http.*`.
fn lfs_scoped_keys(cwd: &Path, url: &str) -> Result<Scoped, Error> {
    scoped_keys_in(cwd, "lfs", url)
}

/// Read the `<section>.<url>.<key>` entries that match `url`. `section`
/// may itself have a dot, as `lfs.transfer` does.
fn scoped_keys_in(cwd: &Path, section: &str, url: &str) -> Result<Scoped, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "config",
            "--includes",
            "--null",
            "--get-regexp",
            &format!(r"^{}\..+\..+$", section.replace('.', r"\.")),
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
        let Some(rest) = key_full
            .strip_prefix(section)
            .and_then(|r| r.strip_prefix('.'))
        else {
            continue;
        };
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

/// Whether the URL from a scoped key applies to the request `url`.
///
/// Trailing slashes are ignored and the whole URL is compared without
/// case, so unlike git, paths are case-insensitive too.
fn url_matches(prefix: &str, url: &str) -> bool {
    let p = prefix.trim_end_matches('/').to_ascii_lowercase();
    let u = url.trim_end_matches('/').to_ascii_lowercase();
    u == p || u.starts_with(&format!("{p}/"))
}

/// Parse an `http.sslVerify` value.
///
/// Returns `None` for anything that isn't one of Git's boolean spellings, so
/// a typo can't turn verification off.
fn parse_ssl_verify(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "t" => Some(true),
        "false" | "no" | "off" | "0" | "f" => Some(false),
        _ => None,
    }
}

/// Whether `s` is `true`, `1`, `yes` or `on`, in any case.
fn parse_bool(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A new repository with `git config <key> <value>` set.
    fn repo_with(key: &str, value: &str) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "--quiet"]);
        git(&["config", key, value]);
        tmp
    }

    #[test]
    fn invalid_ssl_verify_keeps_verification_on() {
        for key in ["http.sslVerify", "http.https://lfs.test.sslVerify"] {
            let repo = repo_with(key, "ture");
            let opts = HttpOptions::for_url(repo.path(), "https://lfs.test/x").unwrap();
            assert_ne!(opts.ssl_verify, Some(false), "{key}");
        }
    }

    #[test]
    fn ssl_verify_false_values_turn_verification_off() {
        for value in ["false", "no", "off", "0", "f", "FALSE"] {
            let repo = repo_with("http.sslVerify", value);
            let opts = HttpOptions::for_url(repo.path(), "https://lfs.test/x").unwrap();
            assert_eq!(opts.ssl_verify, Some(false), "{value}");
        }
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn extra_headers_follow_the_request_url() {
        let headers = ExtraHeaders {
            scoped: vec![
                ("https://lfs.test".into(), "X-Host: lfs".into()),
                ("https://lfs.test/repo".into(), "X-Repo: repo".into()),
                ("https://other.test".into(), "X-Other: other".into()),
            ],
            global: vec!["X-All: all".into(), "not a header".into()],
        };
        assert_eq!(
            headers.for_url("https://lfs.test/repo/info/lfs/objects/batch"),
            pairs(&[("X-Repo", "repo"), ("X-Host", "lfs"), ("X-All", "all")])
        );
        assert_eq!(
            headers.for_url("https://storage.test/object"),
            pairs(&[("X-All", "all")])
        );
    }
}
