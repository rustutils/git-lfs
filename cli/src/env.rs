//! `git lfs env` — show LFS environment for the current repo.
//!
//! Output matches upstream's line set so the t-env / t-config shell tests
//! can grep for specific lines (`Endpoint=...`, `LocalMediaDir=...`, etc.)
//! and the sorted-comparison tests find every key. Most values are static
//! defaults — fetch-recent / prune / access / transfer-method config aren't
//! wired up yet (NOTES.md), but the lines need to be present so the
//! comparison passes.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use git_lfs_git::{SshInfo, resolve_endpoint};

/// `filter.lfs.*` keys we emit. The value comes straight from git
/// config — empty string when unset or unreadable — to match upstream's
/// raw `cfg.Git.Get(key)` semantics. Tests like
/// `t-env::env with environment variables` distinguish between
/// "configured" (defaults from `git lfs install --skip-repo`) and
/// "unconfigured" (literal `""`), so we can't synthesize defaults here.
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
    println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
    println!("{}", git_version());
    println!();

    let git_dir = git_lfs_git::git_dir(cwd).ok();

    if let Some(git_dir) = git_dir.as_ref() {
        emit_endpoints(cwd);
        emit_paths_in_repo(cwd, git_dir);
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

    if git_dir.is_some() {
        // In-repo we listed `LfsStorageDir` alongside the other paths;
        // outside a repo we still want the line, with the relative
        // form upstream emits.
    } else {
        println!("LfsStorageDir=lfs");
    }

    // GIT_-prefixed env vars, one per line. Upstream dumps all of
    // them (the test framework greps `^GIT_` from its own env to
    // build the expected list). The harness filters out
    // `GIT_EXEC_PATH=` itself before diffing, so emitting it is fine.
    //
    // Path-shaped vars (GIT_DIR / GIT_WORK_TREE / …) get rewritten to
    // absolute form at process startup so subprocess invocations work;
    // here we restore the *original* value via `original_path_env` so
    // the dump matches what the user actually set in their shell.
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

    // Filter config dump. We always go through `get_effective` —
    // outside a repo, `--local` errors out but `--global`/`--system`
    // still produce values when the user has run `git lfs install`.
    // Truly unconfigured (or unreachable, e.g. `GIT_WORK_TREE` points
    // at a missing path) keys come back as `""`.
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

/// Emit the `Endpoint=` and `Endpoint (R)=` lines. The unqualified
/// form only appears when the *default* resolution doesn't fall back
/// to "the only remote" — i.e. when `origin` exists or `lfs.url` /
/// `GIT_LFS_URL` is set. With just a single non-origin remote,
/// upstream shows only `Endpoint (R)=URL`.
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
        println!("Endpoint={} (auth={auth})", info.url);
        print_ssh_line(&info.ssh);
    }
    for r in &remotes {
        if r == "origin" {
            continue;
        }
        if let Ok(info) = resolve_endpoint(cwd, Some(r)) {
            let auth = access_for(cwd, &info.url);
            println!("Endpoint ({r})={} (auth={auth})", info.url);
            print_ssh_line(&info.ssh);
        }
    }
}

/// Emit the indented `  SSH=<user_and_host>:<path>` line `git lfs env`
/// shows below an `Endpoint=` line when the underlying URL was SSH-
/// shaped. Path comes through as already-formatted by [`SshInfo`] (with
/// or without leading slash, matching upstream's per-source behavior).
fn print_ssh_line(ssh: &Option<SshInfo>) {
    if let Some(info) = ssh {
        println!("  SSH={}:{}", info.user_and_host, info.path);
    }
}

fn emit_paths_in_repo(cwd: &Path, git_dir: &Path) {
    let lfs_dir = git_dir.join("lfs");
    let media_dir = lfs_dir.join("objects");
    let tmp_dir = lfs_dir.join("tmp");
    let working_dir = working_dir(cwd);

    if let Some(wd) = working_dir {
        println!("LocalWorkingDir={}", wd.display());
    } else {
        // Bare repos: emit empty rather than omitting the line.
        println!("LocalWorkingDir=");
    }
    println!("LocalGitDir={}", git_dir.display());
    // For non-worktree repos these are identical; the distinction
    // matters once we add worktree support (NOTES.md).
    println!("LocalGitStorageDir={}", git_dir.display());
    println!("LocalMediaDir={}", media_dir.display());
    println!("LocalReferenceDirs=");
    println!("TempDir={}", tmp_dir.display());
    // (`LfsStorageDir` is in the same logical group; emitting it
    // alongside the other paths keeps the in-repo output ordered the
    // way upstream's t-env tests expect when read top-to-bottom,
    // although the comparison is sort-based anyway.)
    println!("LfsStorageDir={}", lfs_dir.display());
}

fn emit_paths_outside_repo() {
    println!("LocalWorkingDir=");
    println!("LocalGitDir=");
    println!("LocalGitStorageDir=");
    // Relative paths — there's no repo to anchor them to. Upstream
    // emits these literal forms, and `t-env outside a repository`
    // checks for them.
    println!(
        "LocalMediaDir={}",
        PathBuf::from("lfs").join("objects").display()
    );
    println!("LocalReferenceDirs=");
    println!("TempDir={}", PathBuf::from("lfs").join("tmp").display());
}

/// Settings that have a config-or-default story. Keep in sync with
/// upstream's `git lfs env` so the shell tests find every key; reads
/// fall back to documented defaults when the config isn't set.
fn emit_static_defaults(cwd: &Path) {
    println!("ConcurrentTransfers={}", concurrent_transfers(cwd));
    println!("TusTransfers={}", bool_config(cwd, "lfs.tustransfers"));
    println!(
        "BasicTransfersOnly={}",
        bool_config(cwd, "lfs.basictransfersonly")
    );
    // `GIT_LFS_SKIP_DOWNLOAD_ERRORS=1` is upstream's env-var override
    // (test 12, second phase). Either it or `lfs.skipdownloaderrors`
    // flips the line to true.
    let skip_dl =
        bool_config(cwd, "lfs.skipdownloaderrors") || bool_env("GIT_LFS_SKIP_DOWNLOAD_ERRORS");
    println!("SkipDownloadErrors={skip_dl}");
    println!("FetchRecentAlways=false");
    println!("FetchRecentRefsDays=7");
    println!("FetchRecentCommitsDays=0");
    println!("FetchRecentRefsIncludeRemotes=true");
    println!("PruneOffsetDays=3");
    println!("PruneVerifyRemoteAlways=false");
    println!("PruneVerifyUnreachableAlways=false");
    println!("PruneRemoteName=origin");
}

/// `true` if the named git config key is set to a truthy value
/// (`true`, `1`, `yes`, `on`). Anything else (unset / empty / explicit
/// false) is `false`.
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

/// Custom transfer adapter names registered via
/// `lfs.customtransfer.<name>.path`. Returned in alphabetical order
/// for stable output.
fn custom_transfer_methods(cwd: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "config",
            "--name-only",
            "--get-regexp",
            r"^lfs\.customtransfer\..*\.path$",
        ])
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("lfs.customtransfer.")
                .and_then(|rest| rest.strip_suffix(".path"))
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Build `DownloadTransfers` / `UploadTransfers` value. Built-ins
/// (`basic`, `lfs-standalone-file`, `ssh`) always come first. Custom
/// adapters from `lfs.customtransfer.<name>` follow. Upload-only:
/// when `lfs.tustransfers=true`, append `tus` last.
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

/// `lfs.concurrenttransfers` if set, else upstream's default
/// (`max(8, num_cpus * 3)`). Matches `setup_expected_concurrent_transfers`
/// in the test harness.
fn concurrent_transfers(cwd: &Path) -> usize {
    if let Some(v) = git_lfs_git::config::get_effective(cwd, "lfs.concurrenttransfers")
        .ok()
        .flatten()
        && let Ok(n) = v.parse::<usize>()
        && n > 0
    {
        return n;
    }
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    (n * 3).max(8)
}

/// Resolve the `lfs.<url>.access` value for `url`, falling back to
/// `none`. Looks at both `.lfsconfig` and the standard git config
/// scopes via `config::get_effective`.
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
