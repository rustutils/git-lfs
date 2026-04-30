//! Resolve the LFS server endpoint for a repo.
//!
//! Implements the priority chain documented in
//! `docs/api/server-discovery.md`, plus the SSH/git URL → HTTPS rewriting
//! upstream does so a `git@github.com:foo/bar.git` remote yields the
//! expected `https://github.com/foo/bar.git/info/lfs` endpoint.
//!
//! # Priority order
//!
//! 1. `GIT_LFS_URL` environment variable (matches upstream's escape hatch).
//! 2. `lfs.url` from git config — local → global → system → `.lfsconfig`.
//! 3. `remote.<name>.lfsurl` (same scopes as above).
//! 4. `remote.<name>.url` rewritten via [`derive_lfs_url`].
//!
//! `<name>` defaults to `origin` when the caller hasn't passed a remote.
//!
//! # SSH-style URLs
//!
//! `git lfs` itself only speaks HTTP(S); for SSH remotes the *protocol* is
//! still HTTPS, just inferred from the remote's host/path. Upstream also
//! supports the `git-lfs-authenticate` SSH command for handing back a
//! token; that's deferred (see NOTES.md).

use std::path::Path;

use crate::Error;
use crate::aliases;
use crate::config::{self, ConfigScope};

const DEFAULT_REMOTE: &str = "origin";

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error(transparent)]
    Git(#[from] Error),
    #[error("no LFS endpoint could be determined for remote {0:?}")]
    Unresolved(String),
    #[error("invalid remote URL {url:?}: {reason}")]
    InvalidUrl { url: String, reason: String },
}

/// SSH-shaped remote/endpoint URL parsed into the components `git lfs
/// env` echoes back as `  SSH=<user_and_host>:<path>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshInfo {
    /// Either `<user>@<host>` if a user was present, or just `<host>`.
    pub user_and_host: String,
    /// The path portion — `:foo/bar.git` from `git@host:foo/bar.git`,
    /// or `/foo/bar.git` from `ssh://host/foo/bar.git` (we keep
    /// upstream's exact form: leading `/` preserved for `ssh://`,
    /// stripped for bare SSH).
    pub path: String,
}

/// LFS endpoint resolution result with the optional SSH metadata
/// upstream's `git lfs env` displays alongside the HTTPS-equivalent
/// endpoint URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    pub url: String,
    pub ssh: Option<SshInfo>,
}

/// Resolve the LFS endpoint URL for `cwd` + `remote`. Pass `None` for the
/// default (`origin`, with a "single remote" fallback when origin doesn't
/// exist and exactly one other remote does).
pub fn endpoint_for_remote(cwd: &Path, remote: Option<&str>) -> Result<String, EndpointError> {
    Ok(resolve_endpoint(cwd, remote)?.url)
}

/// Like [`endpoint_for_remote`], but also returns the SSH metadata
/// when the underlying URL was SSH-shaped. Used by `git lfs env` to
/// render the `  SSH=<user_and_host>:<path>` line alongside the
/// HTTPS-equivalent endpoint.
pub fn resolve_endpoint(
    cwd: &Path,
    remote: Option<&str>,
) -> Result<EndpointInfo, EndpointError> {
    let caller_specified_remote = remote.is_some();
    let mut remote = remote.unwrap_or(DEFAULT_REMOTE).to_owned();

    if let Some(v) = std::env::var_os("GIT_LFS_URL") {
        let s = v.to_string_lossy().into_owned();
        if !s.is_empty() {
            return Ok(direct_endpoint(cwd, &s)?);
        }
    }

    if let Some(v) = config::get_effective(cwd, "lfs.url")? {
        return Ok(direct_endpoint(cwd, &v)?);
    }

    // When the caller didn't pin a remote name and `origin` doesn't
    // exist, fall back to the only configured remote. Mirrors
    // upstream's `git remote` discovery in `lfsfetch` and is what
    // `t-fetch.sh::fetch with no origin remote` exercises (rename
    // origin → something, then bare `git lfs fetch`).
    if !caller_specified_remote && remote_url(cwd, &remote)?.is_none() {
        let remotes = list_remotes(cwd)?;
        if remotes.len() == 1 {
            remote = remotes.into_iter().next().expect("len==1");
        }
    }

    let remote_lfsurl_key = format!("remote.{remote}.lfsurl");
    if let Some(v) = config::get_effective(cwd, &remote_lfsurl_key)? {
        return Ok(direct_endpoint(cwd, &v)?);
    }

    if let Some(remote_url) = remote_url(cwd, &remote)? {
        // Apply insteadOf rewrite *before* deriving the LFS suffix so
        // a `gh:org/repo` style alias resolves to the real URL first
        // and `derive_lfs_url` sees a URL it can parse.
        let rewritten = aliases::rewrite(cwd, &remote_url)?;
        return Ok(EndpointInfo {
            url: derive_lfs_url(&rewritten)?,
            ssh: parse_ssh_url(&rewritten),
        });
    }

    // Last fallback: the caller may have passed a URL directly in
    // place of a remote name (e.g. `git lfs push https://host/repo`).
    // Treat anything that looks URL-shaped as the remote URL and run
    // it through the same rewriter — same outcome as if they'd added
    // a `remote.x.url = <URL>` entry first. Bare-SSH (`git@host:path`)
    // also covers the SCP-style case the rewriter understands.
    if looks_like_url(&remote) {
        let rewritten = aliases::rewrite(cwd, &remote)?;
        return Ok(EndpointInfo {
            url: derive_lfs_url(&rewritten)?,
            ssh: parse_ssh_url(&rewritten),
        });
    }

    Err(EndpointError::Unresolved(remote))
}

/// Build an `EndpointInfo` from a directly-configured LFS URL value
/// (`GIT_LFS_URL`, `lfs.url`, `remote.X.lfsurl`). These values are
/// returned to callers as-is — no `.git/info/lfs` derivation — but we
/// still parse SSH metadata so `git lfs env` can echo the original
/// SSH-shaped string back. Aliases are applied first so users can
/// store something like `lfs.url = gh:org/repo` and have it resolve.
fn direct_endpoint(cwd: &Path, value: &str) -> Result<EndpointInfo, EndpointError> {
    let rewritten = aliases::rewrite(cwd, value)?;
    let ssh = parse_ssh_url(&rewritten);
    Ok(EndpointInfo { url: rewritten, ssh })
}

/// `git remote` enumeration. Returns the configured remote names in
/// definition order. Used by [`endpoint_for_remote`] to fall back from
/// `origin` to the "only remote" when one exists.
fn list_remotes(cwd: &Path) -> Result<Vec<String>, Error> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote"])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Quick syntactic check: does `s` look like one of the URL forms
/// [`derive_lfs_url`] recognizes? Used to decide whether to treat a
/// "remote name" argument as a literal URL.
fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("ssh://")
        || s.starts_with("git+ssh://")
        || s.starts_with("ssh+git://")
        || s.starts_with("git://")
        || s.starts_with("file://")
        || s.contains("://")
        || s.contains('@')
}

/// Read `remote.<name>.url` from the standard git config scopes.
///
/// We don't currently honor `remote.<name>.pushurl` separately — that's a
/// minor accuracy issue for `git push`-driven LFS uploads, captured in
/// NOTES.md.
fn remote_url(cwd: &Path, remote: &str) -> Result<Option<String>, Error> {
    let key = format!("remote.{remote}.url");
    if let Some(v) = config::get(cwd, ConfigScope::Local, &key)? {
        return Ok(Some(v));
    }
    if let Some(v) = config::get(cwd, ConfigScope::Global, &key)? {
        return Ok(Some(v));
    }
    config::get(cwd, ConfigScope::System, &key)
}

/// Convert a clone URL into the matching LFS endpoint URL.
///
/// Rules (mirroring upstream's `NewEndpointFromCloneURL`):
/// - `https://host/path` → `https://host/path.git/info/lfs`
/// - `https://host/path.git` → `https://host/path.git/info/lfs`
/// - `ssh://[user@]host[:port]/path` → `https://host/path.git/info/lfs`
///   (port is dropped — LFS is HTTPS-only at the wire layer)
/// - `git@host:path` → `https://host/path.git/info/lfs`
/// - `git://host/path` → `https://host/path.git/info/lfs`
/// - `file://path` → returned unchanged (used by upstream test infra)
pub fn derive_lfs_url(remote_url: &str) -> Result<String, EndpointError> {
    let trimmed = remote_url.trim();
    if trimmed.is_empty() {
        return Err(EndpointError::InvalidUrl {
            url: remote_url.to_owned(),
            reason: "empty URL".into(),
        });
    }

    if let Some(rest) = trimmed.strip_prefix("file://") {
        // file:// URLs are local — pass through. The transfer/ basic
        // adapter doesn't speak file:// today, but rewriting it would be
        // worse than letting it fall over visibly.
        return Ok(format!("file://{rest}"));
    }

    // URL schemes we handle by parsing.
    if let Some(rest) = trimmed.strip_prefix("https://") {
        return Ok(append_lfs_path(&format!("https://{rest}")));
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        return Ok(append_lfs_path(&format!("http://{rest}")));
    }
    if let Some(rest) = trimmed.strip_prefix("ssh://") {
        return ssh_to_https(rest, "ssh://");
    }
    if let Some(rest) = trimmed.strip_prefix("git+ssh://") {
        return ssh_to_https(rest, "git+ssh://");
    }
    if let Some(rest) = trimmed.strip_prefix("ssh+git://") {
        return ssh_to_https(rest, "ssh+git://");
    }
    if let Some(rest) = trimmed.strip_prefix("git://") {
        // `git://` is the bare git protocol — LFS rides on top via HTTPS.
        return Ok(append_lfs_path(&format!("https://{rest}")));
    }

    // Bare-SSH form: `[user@]host:path`. Distinguish from a Windows path
    // (`C:\…`) by requiring the part before `:` to contain a `@` or be a
    // hostname-shaped token (no backslash, no drive-letter pattern).
    if let Some((host_part, path)) = bare_ssh_split(trimmed) {
        let host = host_part.split('@').next_back().unwrap_or(host_part);
        return Ok(append_lfs_path(&format!(
            "https://{host}/{}",
            path.trim_start_matches('/'),
        )));
    }

    Err(EndpointError::InvalidUrl {
        url: remote_url.to_owned(),
        reason: "unrecognized URL form".into(),
    })
}

/// Extract the SSH metadata from a remote URL — the `<user_and_host>`
/// and `<path>` pieces `git lfs env` echoes back as
/// `  SSH=<user_and_host>:<path>`. Returns `None` for URLs that don't
/// look SSH-shaped (HTTP(S), git://, file://, plain paths).
///
/// Mirrors upstream's `EndpointFromSshUrl` / `EndpointFromBareSshUrl`
/// for the metadata fields specifically; the URL itself is rewritten
/// elsewhere (see [`derive_lfs_url`]).
pub fn parse_ssh_url(rawurl: &str) -> Option<SshInfo> {
    let trimmed = rawurl.trim();
    // Schemes upstream classifies as SSH: `ssh://`, `git+ssh://`,
    // `ssh+git://`. Plain HTTP(S) and `git://` are not SSH; `file://`
    // and bare paths aren't either.
    let ssh_rest = trimmed
        .strip_prefix("ssh://")
        .or_else(|| trimmed.strip_prefix("git+ssh://"))
        .or_else(|| trimmed.strip_prefix("ssh+git://"));
    if let Some(rest) = ssh_rest {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.is_empty() {
            return None;
        }
        // Drop the port component for the user_and_host string —
        // upstream's `EndpointFromSshUrl` keeps user@host but stores
        // the port separately.
        let user_and_host = authority
            .rsplit_once(':')
            .map(|(host, _port)| host)
            .unwrap_or(authority);
        return Some(SshInfo {
            user_and_host: user_and_host.to_owned(),
            // Leading `/` preserved for ssh:// to match upstream.
            path: format!("/{}", path.trim_start_matches('/')),
        });
    }
    // HTTP/HTTPS/git/file aren't SSH.
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("git://")
        || trimmed.starts_with("file://")
    {
        return None;
    }
    // Bare-SSH form: `[user@]host:path`. Strip leading `/` from path
    // (upstream's `EndpointFromBareSshUrl` does this explicitly).
    let (host, path) = bare_ssh_split(trimmed)?;
    Some(SshInfo {
        user_and_host: host.to_owned(),
        path: path.trim_start_matches('/').to_owned(),
    })
}

/// Split `<host>:<path>` if `rawurl` looks like a bare SSH URL. Returns
/// `None` if it doesn't (e.g. a plain filesystem path like `/foo/bar` or
/// a Windows drive letter `C:\foo`).
fn bare_ssh_split(rawurl: &str) -> Option<(&str, &str)> {
    // Reject things that look like local paths.
    if rawurl.starts_with('/') || rawurl.starts_with('.') {
        return None;
    }
    if rawurl.contains('\\') {
        return None;
    }

    let (host, path) = rawurl.split_once(':')?;
    if host.is_empty() || path.is_empty() {
        return None;
    }
    // A single ASCII letter before `:` is almost certainly a Windows
    // drive letter, not a hostname. `git@C:/foo` would be malformed
    // anyway.
    if host.len() == 1 && host.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((host, path))
}

/// Convert the post-scheme portion of an `ssh://` URL into the matching
/// HTTPS endpoint.
fn ssh_to_https(rest: &str, scheme_for_error: &str) -> Result<String, EndpointError> {
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() {
        return Err(EndpointError::InvalidUrl {
            url: format!("{scheme_for_error}{rest}"),
            reason: "missing host".into(),
        });
    }
    // Strip off any `user@` prefix.
    let host_with_port = authority.split('@').next_back().unwrap_or(authority);
    // Drop the port: `ssh://host:22/foo` → host portion is just `host`.
    let host = host_with_port.split(':').next().unwrap_or(host_with_port);
    Ok(append_lfs_path(&format!(
        "https://{host}/{}",
        path.trim_start_matches('/'),
    )))
}

/// Append the LFS protocol suffix to an HTTPS URL — `.git/info/lfs` if
/// the URL doesn't already end in `.git`, just `/info/lfs` if it does.
/// Trailing slash on the input URL is collapsed first.
fn append_lfs_path(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    if trimmed.ends_with(".git") {
        format!("{trimmed}/info/lfs")
    } else {
        format!("{trimmed}.git/info/lfs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- derive_lfs_url ---------------------------------------------------

    #[test]
    fn https_url_without_dotgit_gets_dotgit_info_lfs() {
        assert_eq!(
            derive_lfs_url("https://git-server.com/foo/bar").unwrap(),
            "https://git-server.com/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn https_url_with_dotgit_gets_just_info_lfs() {
        assert_eq!(
            derive_lfs_url("https://git-server.com/foo/bar.git").unwrap(),
            "https://git-server.com/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn http_url_is_preserved_as_http() {
        assert_eq!(
            derive_lfs_url("http://localhost:8080/foo/bar").unwrap(),
            "http://localhost:8080/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn trailing_slash_is_collapsed() {
        assert_eq!(
            derive_lfs_url("https://git-server.com/foo/bar/").unwrap(),
            "https://git-server.com/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn ssh_url_becomes_https() {
        assert_eq!(
            derive_lfs_url("ssh://git-server.com/foo/bar.git").unwrap(),
            "https://git-server.com/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn ssh_url_strips_user_and_port() {
        assert_eq!(
            derive_lfs_url("ssh://git@git-server.com:22/foo/bar.git").unwrap(),
            "https://git-server.com/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn bare_ssh_url_becomes_https() {
        assert_eq!(
            derive_lfs_url("git@github.com:user/repo.git").unwrap(),
            "https://github.com/user/repo.git/info/lfs",
        );
    }

    #[test]
    fn bare_ssh_without_user_becomes_https() {
        // `host:path/to/repo.git` is a valid bare SSH form.
        assert_eq!(
            derive_lfs_url("git-server.com:foo/bar.git").unwrap(),
            "https://git-server.com/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn git_protocol_url_becomes_https() {
        assert_eq!(
            derive_lfs_url("git://git-server.com/foo/bar.git").unwrap(),
            "https://git-server.com/foo/bar.git/info/lfs",
        );
    }

    #[test]
    fn ssh_git_variants_are_recognized() {
        for prefix in ["git+ssh", "ssh+git"] {
            let url = format!("{prefix}://git@git-server.com/foo/bar.git");
            assert_eq!(
                derive_lfs_url(&url).unwrap(),
                "https://git-server.com/foo/bar.git/info/lfs",
            );
        }
    }

    #[test]
    fn file_url_is_passed_through_unchanged() {
        assert_eq!(
            derive_lfs_url("file:///srv/repos/foo.git").unwrap(),
            "file:///srv/repos/foo.git",
        );
    }

    #[test]
    fn empty_url_errors() {
        assert!(matches!(
            derive_lfs_url(""),
            Err(EndpointError::InvalidUrl { .. }),
        ));
    }

    #[test]
    fn windows_path_is_not_misread_as_ssh() {
        // `C:\repos\foo` would otherwise look like `host:path`, but a
        // single drive letter is not a valid hostname.
        assert!(derive_lfs_url("C:\\repos\\foo").is_err());
    }

    #[test]
    fn relative_path_is_rejected_not_treated_as_ssh() {
        assert!(derive_lfs_url("./relative/path").is_err());
        assert!(derive_lfs_url("/abs/path").is_err());
    }

    // ---- parse_ssh_url ----------------------------------------------------

    #[test]
    fn ssh_metadata_for_bare_user_at_host() {
        let info = parse_ssh_url("git@github.com:user/repo.git").unwrap();
        assert_eq!(info.user_and_host, "git@github.com");
        assert_eq!(info.path, "user/repo.git");
    }

    #[test]
    fn ssh_metadata_for_bare_host_only() {
        let info = parse_ssh_url("badalias:rest").unwrap();
        assert_eq!(info.user_and_host, "badalias");
        assert_eq!(info.path, "rest");
    }

    #[test]
    fn ssh_metadata_for_ssh_scheme_keeps_leading_slash() {
        let info = parse_ssh_url("ssh://git@host.example/path/to/repo.git").unwrap();
        assert_eq!(info.user_and_host, "git@host.example");
        assert_eq!(info.path, "/path/to/repo.git");
    }

    #[test]
    fn ssh_metadata_for_ssh_scheme_drops_port_from_host() {
        let info = parse_ssh_url("ssh://git@host.example:2222/path").unwrap();
        assert_eq!(info.user_and_host, "git@host.example");
        assert_eq!(info.path, "/path");
    }

    #[test]
    fn ssh_metadata_for_https_returns_none() {
        assert!(parse_ssh_url("https://host.example/path").is_none());
        assert!(parse_ssh_url("http://host.example/path").is_none());
    }

    #[test]
    fn ssh_metadata_for_git_protocol_returns_none() {
        assert!(parse_ssh_url("git://host.example/path").is_none());
    }

    #[test]
    fn ssh_metadata_for_file_url_returns_none() {
        assert!(parse_ssh_url("file:///srv/repos/foo.git").is_none());
    }

    #[test]
    fn ssh_metadata_for_local_path_returns_none() {
        assert!(parse_ssh_url("/abs/path").is_none());
        assert!(parse_ssh_url("./relative").is_none());
    }

    // ---- endpoint_for_remote ---------------------------------------------
    //
    // Every test in this section reads `GIT_LFS_URL` indirectly via
    // `endpoint_for_remote`. cargo runs tests in parallel by default, so we
    // serialize them through a single mutex to keep one test's env-var
    // mutation from leaking into another's expectations.

    use std::sync::{Mutex, MutexGuard};
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        // PoisonError can happen if a previous test panicked while holding
        // the lock — that's a test bug, but recovering keeps the rest of
        // the suite useful instead of cascading-failing.
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fresh_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let s = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(tmp.path())
            .status()
            .unwrap();
        assert!(s.success());
        tmp
    }

    fn git_in(repo: &Path, args: &[&str]) {
        let s = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(s.success(), "git {args:?} failed");
    }

    #[test]
    fn endpoint_prefers_explicit_lfs_url() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        git_in(
            repo.path(),
            &["config", "--local", "lfs.url", "https://example.com/lfs"],
        );
        git_in(
            repo.path(),
            &[
                "config",
                "--local",
                "remote.origin.url",
                "git@github.com:x/y.git",
            ],
        );
        let url = endpoint_for_remote(repo.path(), None).unwrap();
        assert_eq!(url, "https://example.com/lfs");
    }

    #[test]
    fn endpoint_uses_remote_lfsurl_when_no_lfs_url() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        git_in(
            repo.path(),
            &[
                "config",
                "--local",
                "remote.origin.lfsurl",
                "https://lfs.dev/repo",
            ],
        );
        git_in(
            repo.path(),
            &[
                "config",
                "--local",
                "remote.origin.url",
                "git@github.com:x/y.git",
            ],
        );
        let url = endpoint_for_remote(repo.path(), None).unwrap();
        assert_eq!(url, "https://lfs.dev/repo");
    }

    #[test]
    fn endpoint_derives_from_remote_url() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        git_in(
            repo.path(),
            &[
                "config",
                "--local",
                "remote.origin.url",
                "git@github.com:x/y.git",
            ],
        );
        let url = endpoint_for_remote(repo.path(), None).unwrap();
        assert_eq!(url, "https://github.com/x/y.git/info/lfs");
    }

    #[test]
    fn endpoint_uses_named_remote_over_origin() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        git_in(
            repo.path(),
            &[
                "config",
                "--local",
                "remote.upstream.url",
                "https://other.example.com/foo",
            ],
        );
        let url = endpoint_for_remote(repo.path(), Some("upstream")).unwrap();
        assert_eq!(url, "https://other.example.com/foo.git/info/lfs");
    }

    #[test]
    fn endpoint_reads_lfsconfig_at_repo_root() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        // Write a .lfsconfig file (it's just a git config file).
        std::fs::write(
            repo.path().join(".lfsconfig"),
            "[lfs]\n\turl = https://from-lfsconfig.example/\n",
        )
        .unwrap();
        let url = endpoint_for_remote(repo.path(), None).unwrap();
        assert_eq!(url, "https://from-lfsconfig.example/");
    }

    #[test]
    fn endpoint_local_config_overrides_lfsconfig() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        std::fs::write(
            repo.path().join(".lfsconfig"),
            "[lfs]\n\turl = https://from-lfsconfig.example/\n",
        )
        .unwrap();
        git_in(
            repo.path(),
            &[
                "config",
                "--local",
                "lfs.url",
                "https://from-localconfig.example/",
            ],
        );
        let url = endpoint_for_remote(repo.path(), None).unwrap();
        assert_eq!(url, "https://from-localconfig.example/");
    }

    #[test]
    fn endpoint_unresolved_when_nothing_configured() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        let err = endpoint_for_remote(repo.path(), None).unwrap_err();
        assert!(matches!(err, EndpointError::Unresolved(_)));
    }

    #[test]
    fn endpoint_env_var_wins_over_everything() {
        let _g = lock_env();
        let repo = fresh_repo();
        git_in(
            repo.path(),
            &["config", "--local", "lfs.url", "https://lo.cal/lfs"],
        );

        let prev = std::env::var_os("GIT_LFS_URL");
        unsafe { std::env::set_var("GIT_LFS_URL", "https://from-env.example/") };
        let url = endpoint_for_remote(repo.path(), None).unwrap();
        assert_eq!(url, "https://from-env.example/");
        unsafe {
            match prev {
                Some(v) => std::env::set_var("GIT_LFS_URL", v),
                None => std::env::remove_var("GIT_LFS_URL"),
            }
        }
    }
}
