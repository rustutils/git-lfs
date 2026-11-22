//! `git lfs env`: print the Git LFS settings for the current repository.
//!
//! It prints the same keys as upstream, since `t-env.sh` compares the whole
//! output. Some values are fixed rather than read from config: the
//! `FetchRecent*` and `Prune*` lines always show the defaults, and
//! `AccessDownload` and `AccessUpload` always show `none`. The
//! `FetchInclude`, `FetchExclude`, and `Extension[N]` lines upstream adds
//! when those are set are never printed.
//!
//! Endpoint URLs are printed as resolved, but without any password.
//!
//! Like upstream, it warns about `lfs.*` keys set more than once with
//! different values.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use git_lfs_git::endpoint::{SshInfo, resolve_endpoint};

/// The filter settings printed at the end, as their raw config values.
///
/// An unset key prints as `""`, which tests tell apart from the values
/// `git lfs install` writes.
const FILTER_KEYS: &[&str] = &[
    "filter.lfs.process",
    "filter.lfs.smudge",
    "filter.lfs.clean",
];

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn run(cwd: &Path) -> Result<(), EnvError> {
    warn_clashes(cwd);
    println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
    println!("{}", git_version());
    println!();

    let git_dir = git_lfs_git::git_dir(cwd).ok();
    let common_dir = git_lfs_git::git_common_dir(cwd).ok();

    if let Some(git_dir) = git_dir.as_ref() {
        emit_endpoints(cwd);
        // The two only differ in linked worktrees.
        let common = common_dir.as_deref().unwrap_or(git_dir);
        emit_paths_in_repo(cwd, git_dir, common);
    } else {
        emit_paths_outside_repo();
    }

    emit_static_defaults(cwd);
    println!("AccessDownload=none");
    println!("AccessUpload=none");
    let tus = bool_config(cwd, "lfs.tustransfers");
    let customs = custom_transfer_methods(cwd);
    println!("DownloadTransfers={}", transfer_list(&customs, false));
    println!("UploadTransfers={}", transfer_list(&customs, tus));

    // In a repository this line was printed with the other paths.
    if git_dir.is_none() {
        println!("LfsStorageDir=lfs");
    }

    // Every `GIT_` environment variable, like upstream. Path variables
    // like `GIT_DIR` were made absolute at startup, so print the values
    // the user set instead.
    println!();
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("GIT_"))
        .map(|(k, v)| {
            let restored = crate::original_path_env(&k)
                .map(|os| os.to_string_lossy().into_owned())
                .unwrap_or(v);
            (k, restored)
        })
        .collect();
    vars.sort();
    for (k, v) in vars {
        println!("{k}={v}");
    }

    // Outside a repository this still finds global and system values. A
    // config that can't be read prints `""`.
    println!();
    for key in FILTER_KEYS {
        let value = git_lfs_git::config::get_effective(cwd, key)
            .ok()
            .flatten()
            .unwrap_or_default();
        println!("git config {key} = {value:?}");
    }

    Ok(())
}

/// Warn about `lfs.*` keys set more than once with different values, in
/// upstream's format, which t-config 13 matches.
fn warn_clashes(cwd: &Path) {
    let Ok(clashes) = git_lfs_git::config::clashes(cwd) else {
        return;
    };
    for clash in clashes {
        let earlier: Vec<String> = clash.earlier.iter().map(|v| go_quote(v)).collect();
        eprintln!("warning: These `git config` values clash:");
        eprintln!(
            "  git config {} = [{}]",
            go_quote(&clash.key),
            earlier.join(" ")
        );
        eprintln!(
            "  git config {} = {}",
            go_quote(&clash.key),
            go_quote(&clash.value)
        );
    }
}

/// Quote `s` like Go's `%q`, which upstream prints config values with.
fn go_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_control() => {
                let n = u32::from(c);
                if n < 0x80 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else {
                    out.push_str(&format!("\\u{n:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `url` with any password removed, for display.
///
/// Other URLs are returned unchanged, so their formatting is kept exactly.
fn without_password(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) if parsed.password().is_some() => {
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        _ => url.to_owned(),
    }
}

/// Print the `Endpoint=` line and an `Endpoint (<remote>)=` line for each
/// remote other than `origin`.
///
/// `Endpoint=` is only printed when `origin` exists or `lfs.url` or
/// `GIT_LFS_URL` is set. With a single remote of another name, upstream
/// prints only that remote's line.
fn emit_endpoints(cwd: &Path) {
    let remotes = remotes(cwd);
    let has_origin = remotes.iter().any(|r| r == "origin");
    let has_default_url = std::env::var_os("GIT_LFS_URL").is_some()
        || git_lfs_git::config::get_effective(cwd, "lfs.url")
            .ok()
            .flatten()
            .is_some();
    if (has_origin || has_default_url)
        && let Ok(info) = resolve_endpoint(cwd, None)
    {
        let auth = access_for(cwd, &info.url);
        println!("Endpoint={} (auth={auth})", without_password(&info.url));
        print_ssh_line(&info.ssh);
    }
    for r in &remotes {
        if r == "origin" {
            continue;
        }
        if let Ok(info) = resolve_endpoint(cwd, Some(r)) {
            let auth = access_for(cwd, &info.url);
            println!(
                "Endpoint ({r})={} (auth={auth})",
                without_password(&info.url)
            );
            print_ssh_line(&info.ssh);
        }
    }
}

/// Print the `  SSH=<user@host>:<path>` line that follows an endpoint
/// resolved from an SSH URL.
fn print_ssh_line(ssh: &Option<SshInfo>) {
    if let Some(info) = ssh {
        println!("  SSH={}:{}", info.user_and_host, info.path);
    }
}

fn emit_paths_in_repo(cwd: &Path, git_dir: &Path, common_dir: &Path) {
    let lfs_dir = common_dir.join("lfs");
    let media_dir = lfs_dir.join("objects");
    let tmp_dir = lfs_dir.join("tmp");
    let working_dir = working_dir(cwd);

    if let Some(wd) = working_dir {
        println!("LocalWorkingDir={}", wd.display());
    } else {
        // A bare repository.
        println!("LocalWorkingDir=");
    }
    println!("LocalGitDir={}", git_dir.display());
    // In a linked worktree, the Git directory is
    // `.git/worktrees/<name>` and the storage directory is the shared
    // `.git`.
    println!("LocalGitStorageDir={}", common_dir.display());
    println!("LocalMediaDir={}", media_dir.display());
    println!("LocalReferenceDirs=");
    println!("TempDir={}", tmp_dir.display());
    println!("LfsStorageDir={}", lfs_dir.display());
}

fn emit_paths_outside_repo() {
    println!("LocalWorkingDir=");
    println!("LocalGitDir=");
    println!("LocalGitStorageDir=");
    // Upstream prints these relative paths outside a repository.
    println!(
        "LocalMediaDir={}",
        PathBuf::from("lfs").join("objects").display()
    );
    println!("LocalReferenceDirs=");
    println!("TempDir={}", PathBuf::from("lfs").join("tmp").display());
}

/// Print the transfer settings and the fixed fetch and prune defaults.
fn emit_static_defaults(cwd: &Path) {
    println!("ConcurrentTransfers={}", concurrent_transfers(cwd));
    println!("TusTransfers={}", bool_config(cwd, "lfs.tustransfers"));
    println!(
        "BasicTransfersOnly={}",
        bool_config(cwd, "lfs.basictransfersonly")
    );
    println!("SkipDownloadErrors={}", skip_download_errors(cwd));
    println!("FetchRecentAlways=false");
    println!("FetchRecentRefsDays=7");
    println!("FetchRecentCommitsDays=0");
    println!("FetchRecentRefsIncludeRemotes=true");
    println!("PruneOffsetDays=3");
    println!("PruneVerifyRemoteAlways=false");
    println!("PruneVerifyUnreachableAlways=false");
    println!("PruneRemoteName=origin");
}

/// Whether `lfs.skipdownloaderrors` or `GIT_LFS_SKIP_DOWNLOAD_ERRORS` is
/// true.
///
/// When it is, `git lfs smudge` writes the pointer instead of failing
/// when a download fails.
pub(crate) fn skip_download_errors(cwd: &Path) -> bool {
    bool_config(cwd, "lfs.skipdownloaderrors") || bool_env("GIT_LFS_SKIP_DOWNLOAD_ERRORS")
}

/// Whether config `key` is exactly `true`, `1`, `yes`, or `on`.
fn bool_config(cwd: &Path, key: &str) -> bool {
    matches!(
        git_lfs_git::config::get_effective(cwd, key)
            .ok()
            .flatten()
            .as_deref(),
        Some("true" | "1" | "yes" | "on")
    )
}

fn bool_env(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("true" | "1" | "yes" | "on")
    )
}

/// The names of the custom transfer adapters, sorted.
fn custom_transfer_methods(cwd: &Path) -> Vec<String> {
    let mut names: Vec<String> = crate::fetcher::configured_adapters(cwd)
        .into_iter()
        .map(|a| a.name)
        .collect();
    names.sort();
    names
}

/// The value of `DownloadTransfers` or `UploadTransfers`.
///
/// The built-in adapters come first, then the custom ones, then `tus` if
/// `with_tus` is set.
fn transfer_list(customs: &[String], with_tus: bool) -> String {
    let mut parts: Vec<&str> = vec!["basic", "lfs-standalone-file", "ssh"];
    for c in customs {
        parts.push(c.as_str());
    }
    if with_tus {
        parts.push("tus");
    }
    parts.join(",")
}

/// `lfs.concurrenttransfers` if set, or else the default of 8, which is
/// also what transfers use.
fn concurrent_transfers(cwd: &Path) -> usize {
    if let Some(v) = git_lfs_git::config::get_effective(cwd, "lfs.concurrenttransfers")
        .ok()
        .flatten()
        && let Ok(n) = v.parse::<usize>()
        && n > 0
    {
        return n;
    }
    8
}

/// The `lfs.<url>.access` value for `url`, or `none`.
fn access_for(cwd: &Path, url: &str) -> String {
    let key = format!("lfs.{url}.access");
    git_lfs_git::config::get_effective(cwd, &key)
        .ok()
        .flatten()
        .unwrap_or_else(|| "none".to_owned())
}

fn git_version() -> String {
    Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            o.status
                .success()
                .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "git version unknown".to_owned())
}

fn working_dir(cwd: &Path) -> Option<std::path::PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(s))
    }
}

fn remotes(cwd: &Path) -> Vec<String> {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("remote")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_passwords_are_removed() {
        assert_eq!(
            without_password("https://alice:s3cret@host/repo.git/info/lfs"),
            "https://alice@host/repo.git/info/lfs"
        );
        // Without a password, the URL is printed as it was.
        assert_eq!(without_password("http://foo/bar"), "http://foo/bar");
        assert_eq!(without_password("git@host:repo.git"), "git@host:repo.git");
    }

    #[test]
    fn go_quote_escapes_like_go() {
        assert_eq!(go_quote("http://a"), r#""http://a""#);
        assert_eq!(go_quote("a\"b\\c\nd\u{7}\u{1}é"), r#""a\"b\\c\nd\a\x01é""#);
    }
}
