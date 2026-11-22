//! Find the LFS server URL for a remote.
//!
//! [`resolve_endpoint`] follows the [server discovery] rules. The first of
//! these that is set wins:
//!
//! 1. The `GIT_LFS_URL` environment variable.
//! 2. `lfs.url`.
//! 3. `remote.<name>.lfsurl`.
//! 4. `remote.<name>.url`, turned into an LFS URL by [`derive_lfs_url`].
//!
//! A local path in any of these, or as the remote, becomes a `file://` URL
//! with [`local_path_url`].
//!
//! `lfs.url` and `remote.<name>.lfsurl` can also come from `.lfsconfig`, at
//! the lowest priority. `url.<base>.insteadOf` aliases apply to every URL.
//!
//! Without a remote name, `<name>` is `origin`, or the only remote if there
//! is no `origin`. When nothing above is set, a remote name that looks like
//! a URL is used as the remote URL. Without a remote name, the URL of the
//! last `git fetch` in `FETCH_HEAD` is the last resort.
//!
//! Uploads first check `lfs.pushurl`, `remote.<name>.lfspushurl`, and
//! `remote.<name>.pushurl`, each ahead of its fetch counterpart, and rewrite
//! URLs with `url.<base>.pushInsteadOf` before `insteadOf`.
//!
//! An SSH remote such as `git@github.com:foo/bar.git` gets the HTTPS
//! endpoint `https://github.com/foo/bar.git/info/lfs`. [`parse_ssh_url`]
//! keeps the SSH details, which callers need to run `git-lfs-authenticate`
//! or `git-lfs-transfer` on the server.
//!
//! [server discovery]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/server-discovery.md

use std::path::Path;

use crate::Error;
use crate::aliases;
use crate::config;

const DEFAULT_REMOTE: &str = "origin";

/// An error from resolving an endpoint.
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    /// Reading config or `FETCH_HEAD` failed.
    #[error(transparent)]
    Git(#[from] Error),
    /// Nothing configured an endpoint for this remote.
    #[error("no LFS endpoint could be determined for remote {0:?}")]
    Unresolved(String),
    /// The remote URL has a form [`derive_lfs_url`] doesn't recognize.
    #[error("invalid remote URL {url:?}: {reason}")]
    InvalidUrl { url: String, reason: String },
}

/// The parts of an SSH remote URL needed to run commands on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshInfo {
    /// `<user>@<host>`, or just `<host>` if the URL has no user.
    pub user_and_host: String,
    /// The repository path in upstream's form.
    ///
    /// `ssh://host/foo/bar.git` gives `/foo/bar.git`, with the leading
    /// slash. `git@host:foo/bar.git` gives `foo/bar.git`, without one.
    pub path: String,
    /// The port of an `ssh://host:22/path` URL.
    ///
    /// `None` for bare `git@host:path` URLs and for `ssh://` URLs without a
    /// port.
    pub port: Option<String>,
}

/// A resolved LFS endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    /// The LFS server URL.
    ///
    /// URLs derived from `remote.<name>.url` are HTTP(S) or `file://`.
    /// Configured LFS URLs such as `lfs.url` are returned as they are.
    pub url: String,
    /// The SSH details, if the URL the endpoint came from was an SSH URL.
    pub ssh: Option<SshInfo>,
}

/// Whether an endpoint is for downloading or uploading.
///
/// Uploads can have their own URLs, such as `lfs.pushurl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Download,
    Upload,
}

/// Resolve the LFS endpoint URL for `remote` in the repository at `cwd`.
///
/// Pass `None` for the default remote. The [module docs](crate::endpoint)
/// give the order of lookups.
pub fn endpoint_for_remote(cwd: &Path, remote: Option<&str>) -> Result<String, EndpointError> {
    Ok(resolve_endpoint(cwd, remote)?.url)
}

/// Like [`endpoint_for_remote`], but also returns the SSH details.
pub fn resolve_endpoint(cwd: &Path, remote: Option<&str>) -> Result<EndpointInfo, EndpointError> {
    resolve_endpoint_for(cwd, remote, Operation::Download)
}

/// Like [`resolve_endpoint`], for `operation`.
pub fn resolve_endpoint_for(
    cwd: &Path,
    remote: Option<&str>,
    operation: Operation,
) -> Result<EndpointInfo, EndpointError> {
    let upload = operation == Operation::Upload;
    let rewrite = |url: &str| -> Result<String, Error> {
        if upload {
            aliases::rewrite_for_push(cwd, url)
        } else {
            aliases::rewrite(cwd, url)
        }
    };
    let direct = |url: &str| -> Result<EndpointInfo, EndpointError> {
        let rewritten = rewrite(url)?;
        if let Some(file_url) = local_path_url(cwd, &rewritten) {
            return Ok(EndpointInfo {
                url: file_url,
                ssh: None,
            });
        }
        let ssh = parse_ssh_url(&rewritten);
        Ok(EndpointInfo {
            url: rewritten,
            ssh,
        })
    };
    let from_clone_url = |url: &str| -> Result<EndpointInfo, EndpointError> {
        // Rewrite aliases first, so `derive_lfs_url` sees a real URL
        // instead of something like `gh:org/repo`.
        let rewritten = rewrite(url)?;
        if let Some(file_url) = local_path_url(cwd, &rewritten) {
            return Ok(EndpointInfo {
                url: file_url,
                ssh: None,
            });
        }
        Ok(EndpointInfo {
            url: derive_lfs_url(&rewritten)?,
            ssh: parse_ssh_url(&rewritten),
        })
    };

    let caller_specified_remote = remote.is_some();
    let mut remote = remote.unwrap_or(DEFAULT_REMOTE).to_owned();

    if let Some(v) = std::env::var_os("GIT_LFS_URL") {
        let s = v.to_string_lossy().into_owned();
        if !s.is_empty() {
            return direct(&s);
        }
    }

    if upload && let Some(v) = config::get_effective(cwd, "lfs.pushurl")? {
        return direct(&v);
    }
    if let Some(v) = config::get_effective(cwd, "lfs.url")? {
        return direct(&v);
    }

    // Without a remote name and without `origin`, use the only remote,
    // as upstream does (`t-fetch.sh`, "fetch with no origin remote").
    if !caller_specified_remote && remote_url(cwd, &remote)?.is_none() {
        let remotes = list_remotes(cwd)?;
        if remotes.len() == 1 {
            remote = remotes.into_iter().next().expect("len==1");
        }
    }

    if upload && let Some(v) = config::get_effective(cwd, &format!("remote.{remote}.lfspushurl"))? {
        return direct(&v);
    }
    if let Some(v) = config::get_effective(cwd, &format!("remote.{remote}.lfsurl"))? {
        return direct(&v);
    }

    if upload && let Some(url) = config::get_effective(cwd, &format!("remote.{remote}.pushurl"))? {
        return from_clone_url(&url);
    }
    if let Some(url) = remote_url(cwd, &remote)? {
        return from_clone_url(&url);
    }

    // The caller may have passed a URL in place of a remote name, as in
    // `git lfs push https://host/repo`.
    if looks_like_url(&remote) {
        return from_clone_url(&remote);
    }
    // Or a local path, as in `git push ../other`.
    if let Some(file_url) = local_path_url(cwd, &remote) {
        return Ok(EndpointInfo {
            url: file_url,
            ssh: None,
        });
    }

    // Use the URL of the last `git fetch`. This lets the smudge filter
    // find objects in a repository filled by `git fetch <url> <ref>`
    // without a remote (`t-no-remote.sh`). A named remote means the
    // caller wants that remote, so this doesn't apply.
    if !caller_specified_remote && let Some(url) = read_fetch_head_url(cwd)? {
        return from_clone_url(&url);
    }

    Err(EndpointError::Unresolved(remote))
}

/// Read the source URL of the last `git fetch` from `FETCH_HEAD`.
///
/// Takes the URL after `of` on the first line that has one, as in
/// `<oid>\t\tbranch 'main' of <url>`. Like upstream, this finds no URL
/// when only `HEAD` was fetched, since git then writes `<oid>\t\t<url>`.
/// Returns `None` if the file is missing or has no such line.
fn read_fetch_head_url(cwd: &Path) -> Result<Option<String>, EndpointError> {
    let git_dir = match crate::path::git_dir(cwd) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let path = git_dir.join("FETCH_HEAD");
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(EndpointError::Git(Error::Io(e))),
    };
    for line in content.lines() {
        if let Some(idx) = line.rfind(" of ") {
            let url = line[idx + 4..].trim();
            if !url.is_empty() {
                return Ok(Some(url.to_owned()));
            }
        }
    }
    Ok(None)
}

/// The configured remote names, or none if `git remote` fails.
pub fn list_remotes(cwd: &Path) -> Result<Vec<String>, Error> {
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

/// Whether a remote name argument should be treated as a URL.
///
/// True if `s` contains `://` or `@`. A bare `host:path` without a user
/// is not treated as a URL.
pub fn looks_like_url(s: &str) -> bool {
    s.contains("://") || s.contains('@')
}

/// The remote that pushes go to, if one is configured.
///
/// Like upstream's `PushRemote`, this is the first of
/// `branch.<current>.pushRemote`, `remote.lfspushdefault`, and
/// `remote.pushDefault` that is set. `None` means the default remote.
pub fn push_remote(cwd: &Path) -> Result<Option<String>, Error> {
    if let Some(branch) = crate::refs::current_branch(cwd)
        && let Some(remote) = config::get_effective(cwd, &format!("branch.{branch}.pushRemote"))?
    {
        return Ok(Some(remote));
    }
    for key in ["remote.lfspushdefault", "remote.pushDefault"] {
        if let Some(remote) = config::get_effective(cwd, key)? {
            return Ok(Some(remote));
        }
    }
    Ok(None)
}

/// The `file://` URL for `s` if it's a path to a local repository, like
/// upstream's `RewriteLocalPathAsURL`.
///
/// An absolute path, or a relative one that exists from `cwd`, counts.
/// The URL points at the `.git` directory when there is one, so a
/// working tree and its `.git` give the same URL. A path that doesn't
/// exist, or anything that could be a URL or `host:path`, gives `None`.
pub fn local_path_url(cwd: &Path, s: &str) -> Option<String> {
    if s.is_empty() || s.contains("://") {
        return None;
    }
    let absolute = Path::new(s).is_absolute();
    if !absolute && (bare_ssh_split(s).is_some() || !cwd.join(s).exists()) {
        return None;
    }
    let mut path = clean_path(&cwd.join(s));
    let git_path = if path.file_name().is_some_and(|n| n == ".git") {
        let git_path = path.clone();
        path.pop();
        git_path
    } else {
        path.join(".git")
    };
    if git_path.exists() {
        path = git_path;
    } else if !path.exists() {
        return None;
    }
    let slashed = path.to_string_lossy().replace('\\', "/");
    // A Windows path like `C:/x` needs a slash before it.
    let lead = if slashed.starts_with('/') { "" } else { "/" };
    Some(format!("file://{lead}{slashed}"))
}

/// Resolve `.` and `..` in `path` without touching the file system, like
/// Go's `filepath.Clean`.
fn clean_path(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Read `remote.<name>.url` from git config.
pub fn remote_url(cwd: &Path, remote: &str) -> Result<Option<String>, Error> {
    config::get_effective(cwd, &format!("remote.{remote}.url"))
}

/// Convert a clone URL into the matching LFS endpoint URL.
///
/// Follows upstream's `NewEndpointFromCloneURL`:
///
/// - `https://host/path` and `https://host/path.git` become
///   `https://host/path.git/info/lfs`. `http://` stays `http://`.
/// - `ssh://[user@]host[:port]/path` becomes
///   `https://host/path.git/info/lfs`, dropping the user and port. The
///   `git+ssh://` and `ssh+git://` schemes work the same way.
/// - `[user@]host:path` becomes `https://host/path.git/info/lfs`.
/// - `git://host/path` becomes `https://host/path.git/info/lfs`.
/// - `file://` URLs are returned without a trailing slash.
///
/// Anything else is an error. [`local_path_url`] handles local paths.
pub fn derive_lfs_url(remote_url: &str) -> Result<String, EndpointError> {
    let trimmed = remote_url.trim();
    if trimmed.is_empty() {
        return Err(EndpointError::InvalidUrl {
            url: remote_url.to_owned(),
            reason: "empty URL".into(),
        });
    }

    if let Some(rest) = trimmed.strip_prefix("file://") {
        // Objects go through the `lfs-standalone-file` adapter.
        return Ok(format!("file://{}", rest.strip_suffix('/').unwrap_or(rest)));
    }

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
        return Ok(append_lfs_path(&format!("https://{rest}")));
    }

    // Bare SSH form: `[user@]host:path`.
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

/// Extract the SSH details from a remote URL.
///
/// Recognizes `ssh://`, `git+ssh://`, `ssh+git://`, and bare
/// `[user@]host:path` URLs, like upstream's `EndpointFromSshUrl` and
/// `EndpointFromBareSshUrl`. Returns `None` for anything else, including
/// HTTP(S), `git://`, `file://`, and local paths.
pub fn parse_ssh_url(rawurl: &str) -> Option<SshInfo> {
    let trimmed = rawurl.trim();
    let ssh_rest = trimmed
        .strip_prefix("ssh://")
        .or_else(|| trimmed.strip_prefix("git+ssh://"))
        .or_else(|| trimmed.strip_prefix("ssh+git://"));
    if let Some(rest) = ssh_rest {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.is_empty() {
            return None;
        }
        let (user_and_host, port) = match authority.rsplit_once(':') {
            Some((host, p)) => (host.to_owned(), Some(p.to_owned())),
            None => (authority.to_owned(), None),
        };
        // As a URL, the path is percent-decoded, as in upstream.
        return Some(SshInfo {
            user_and_host,
            path: format!("/{}", percent_decode(path.trim_start_matches('/'))),
            port,
        });
    }
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("git://")
        || trimmed.starts_with("file://")
    {
        return None;
    }
    let (host, path) = bare_ssh_split(trimmed)?;
    Some(SshInfo {
        user_and_host: host.to_owned(),
        path: path.trim_start_matches('/').to_owned(),
        port: None,
    })
}

/// Decode `%XX` escapes in `s`. Invalid escapes are kept as they are.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split a bare SSH URL `[user@]host:path` at the first colon.
///
/// Returns `None` for local paths such as `/foo/bar`, `./foo`, or
/// `C:\foo`.
fn bare_ssh_split(rawurl: &str) -> Option<(&str, &str)> {
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
    // A single letter before the colon is a Windows drive letter.
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
    let host_with_port = authority.split('@').next_back().unwrap_or(authority);
    let host = host_with_port.split(':').next().unwrap_or(host_with_port);
    Ok(append_lfs_path(&format!(
        "https://{host}/{}",
        path.trim_start_matches('/'),
    )))
}

/// The HTTPS URL of an SSH LFS URL such as `ssh://git@host:22/repo`.
///
/// This is the host and path, as in upstream's `EndpointFromSshUrl`, without
/// the `.git/info/lfs` that [`derive_lfs_url`] adds to a clone URL. A client
/// uses it when the server has no `git-lfs-authenticate`. `None` if `url`
/// isn't an SSH URL.
pub fn guessed_https_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let (authority, path) = match ["ssh://", "git+ssh://", "ssh+git://"]
        .iter()
        .find_map(|scheme| trimmed.strip_prefix(scheme))
    {
        Some(rest) => rest.split_once('/').unwrap_or((rest, "")),
        None if trimmed.contains("://") => return None,
        None => bare_ssh_split(trimmed)?,
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    Some(format!("https://{host}/{}", path.trim_start_matches('/')))
}

/// Append `/info/lfs` to a URL, adding `.git` first if it isn't there.
///
/// Trailing slashes are removed first.
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

    #[test]
    fn ssh_url_path_is_percent_decoded() {
        let info = parse_ssh_url("ssh://git@host/my%20repo%0a%3bx%zz").unwrap();
        assert_eq!(info.path, "/my repo\n;x%zz");
        // A bare URL isn't a URL, so its path is kept.
        let info = parse_ssh_url("git@host:my%20repo").unwrap();
        assert_eq!(info.path, "my%20repo");
    }

    #[test]
    fn guessed_https_url_keeps_the_path() {
        let cases = [
            ("ssh://git@host:2222/repo", Some("https://host/repo")),
            ("ssh://host/a/b%0Ac", Some("https://host/a/b%0Ac")),
            ("git@host:org/repo.git", Some("https://host/org/repo.git")),
            ("https://host/repo", None),
        ];
        for (url, want) in cases {
            assert_eq!(guessed_https_url(url).as_deref(), want, "{url}");
        }
    }

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
        assert_eq!(
            derive_lfs_url("file:///srv/repos/foo.git/").unwrap(),
            "file:///srv/repos/foo.git",
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_paths_become_file_urls() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join(".git")).unwrap();
        let bare = root.join("bare.git");
        std::fs::create_dir(&bare).unwrap();
        let url = |p: &Path| format!("file://{}", p.display());

        // A working tree, or its `.git`, gives the `.git` directory.
        let expected = url(&work.join(".git"));
        let work_str = work.to_str().unwrap();
        assert_eq!(local_path_url(&root, work_str), Some(expected.clone()));
        assert_eq!(
            local_path_url(&root, &format!("{work_str}/.git")),
            Some(expected.clone())
        );
        assert_eq!(local_path_url(&root, "work/"), Some(expected.clone()));
        assert_eq!(local_path_url(&work, "../work"), Some(expected));
        assert_eq!(local_path_url(&root, "bare.git"), Some(url(&bare)));

        // Missing paths, URLs, and `host:path` aren't local paths.
        assert_eq!(local_path_url(&root, "missing"), None);
        assert_eq!(local_path_url(&root, "/no/such/path"), None);
        assert_eq!(local_path_url(&root, "https://host/repo"), None);
        assert_eq!(local_path_url(&root, "host:repo"), None);
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
        let err = derive_lfs_url("C:\\repos\\foo").unwrap_err();
        assert!(
            matches!(err, EndpointError::InvalidUrl { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn relative_path_is_rejected_not_treated_as_ssh() {
        for input in ["./relative/path", "/abs/path"] {
            let err = derive_lfs_url(input).unwrap_err();
            assert!(
                matches!(err, EndpointError::InvalidUrl { .. }),
                "input {input:?} got {err:?}"
            );
        }
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
        // the lock. That's a test bug, but recovering keeps the rest of
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

    /// The download and upload endpoints of the default remote.
    fn both_endpoints(repo: &Path) -> (String, String) {
        let download = resolve_endpoint_for(repo, None, Operation::Download).unwrap();
        let upload = resolve_endpoint_for(repo, None, Operation::Upload).unwrap();
        (download.url, upload.url)
    }

    #[test]
    fn uploads_prefer_push_urls() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        let set = |k: &str, v: &str| git_in(repo.path(), &["config", "--local", k, v]);
        set("remote.origin.url", "https://fetch.test/r.git");
        set("remote.origin.pushurl", "https://push.test/r.git");
        assert_eq!(
            both_endpoints(repo.path()),
            (
                "https://fetch.test/r.git/info/lfs".into(),
                "https://push.test/r.git/info/lfs".into()
            )
        );

        set("remote.origin.lfsurl", "https://fetch.test/lfs");
        set("remote.origin.lfspushurl", "https://push.test/lfs");
        assert_eq!(
            both_endpoints(repo.path()),
            (
                "https://fetch.test/lfs".into(),
                "https://push.test/lfs".into()
            )
        );

        set("lfs.url", "https://lfs.test/fetch");
        set("lfs.pushurl", "https://lfs.test/push");
        assert_eq!(
            both_endpoints(repo.path()),
            (
                "https://lfs.test/fetch".into(),
                "https://lfs.test/push".into()
            )
        );
    }

    #[test]
    fn uploads_apply_push_insteadof_first() {
        let _g = lock_env();
        unsafe { std::env::remove_var("GIT_LFS_URL") };
        let repo = fresh_repo();
        let set = |k: &str, v: &str| git_in(repo.path(), &["config", "--local", k, v]);
        set("remote.origin.url", "gh:r");
        set("url.https://fetch.test/.insteadOf", "gh:");
        set("url.https://push.test/.pushInsteadOf", "gh:");
        assert_eq!(
            both_endpoints(repo.path()),
            (
                "https://fetch.test/r.git/info/lfs".into(),
                "https://push.test/r.git/info/lfs".into()
            )
        );
    }

    #[test]
    fn push_remote_follows_upstream_order() {
        let _g = lock_env();
        let repo = fresh_repo();
        let set = |k: &str, v: &str| git_in(repo.path(), &["config", "--local", k, v]);
        assert_eq!(push_remote(repo.path()).unwrap(), None);
        set("remote.pushDefault", "a");
        assert_eq!(push_remote(repo.path()).unwrap().as_deref(), Some("a"));
        set("remote.lfspushdefault", "b");
        assert_eq!(push_remote(repo.path()).unwrap().as_deref(), Some("b"));
        let branch = crate::refs::current_branch(repo.path()).unwrap();
        set(&format!("branch.{branch}.pushRemote"), "c");
        assert_eq!(push_remote(repo.path()).unwrap().as_deref(), Some("c"));
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
