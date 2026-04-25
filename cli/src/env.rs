//! `git lfs env` — show LFS environment for the current repo.
//!
//! Slim version of upstream's command. We emit the parts we can answer
//! truthfully today: our version, git's version, the configured LFS
//! endpoint(s), the on-disk paths LFS uses, and the three `filter.lfs.*`
//! config values that drive the clean/smudge/process filters.
//!
//! Out of repo, the repo-specific lines are skipped — the command still
//! succeeds so it can be used as a sanity check.

use std::path::Path;
use std::process::Command;

use git_lfs_git::endpoint_for_remote;

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn run(cwd: &Path) -> Result<(), EnvError> {
    println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
    println!("{}", git_version());
    println!();

    if let Ok(git_dir) = git_lfs_git::git_dir(cwd) {
        let working_dir = working_dir(cwd);
        let media_dir = git_dir.join("lfs").join("objects");
        let tmp_dir = git_dir.join("lfs").join("tmp");

        // Endpoints: default remote first, then any others. Quietly
        // skip remotes for which we can't resolve an endpoint — that's
        // not an error here, it just means LFS isn't configured for
        // that remote.
        //
        // The default lookup goes through `endpoint_for_remote(None)`,
        // which falls back to `lfs.url` even when no `origin` remote
        // exists. So this still prints `Endpoint=…` in repos that have
        // no remotes configured but do have `lfs.url`.
        if let Ok(url) = endpoint_for_remote(cwd, None) {
            println!("Endpoint={url}");
        }
        for r in remotes(cwd) {
            if r == "origin" {
                continue;
            }
            if let Ok(url) = endpoint_for_remote(cwd, Some(&r)) {
                println!("Endpoint ({r})={url}");
            }
        }

        if let Some(wd) = working_dir {
            println!("LocalWorkingDir={}", wd.display());
        }
        println!("LocalGitDir={}", git_dir.display());
        println!("LocalMediaDir={}", media_dir.display());
        println!("TempDir={}", tmp_dir.display());
    }

    println!();
    for key in ["filter.lfs.process", "filter.lfs.smudge", "filter.lfs.clean"] {
        let value = git_lfs_git::config::get_effective(cwd, key)
            .ok()
            .flatten()
            .unwrap_or_default();
        println!("git config {key} = {value:?}");
    }

    Ok(())
}

fn git_version() -> String {
    Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            o.status.success().then(|| {
                String::from_utf8_lossy(&o.stdout).trim().to_owned()
            })
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
