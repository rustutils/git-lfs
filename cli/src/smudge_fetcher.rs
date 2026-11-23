//! Downloads for the smudge filter, from the right remote.
//!
//! By default objects come from the default remote. Two settings change
//! that, as in upstream:
//!
//! - `lfs.remote.autodetect`: on the first smudge request, use the first
//!   remote with a branch containing the commit being checked out, from
//!   `git branch -r --contains <treeish>`. Without a treeish, as in a
//!   sparse checkout, `HEAD` is used. Upstream does this only in
//!   filter-process.
//! - `lfs.remote.searchall`: when a download fails, try every remote in
//!   turn. The first one that works is used for the rest of the session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git_lfs_filter::FetchError;
use git_lfs_git::endpoint::Operation;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;

use crate::fetcher::LfsFetcher;

pub struct SmudgeFetcher {
    cwd: PathBuf,
    store: Store,
    autodetect: bool,
    search_all: bool,
    /// Whether the remote has been picked from the first request's
    /// treeish yet.
    detected: bool,
    /// The remote in use. `None` is the default remote.
    remote: Option<String>,
    fetchers: HashMap<Option<String>, LfsFetcher>,
}

impl SmudgeFetcher {
    pub fn new(cwd: &Path, store: &Store) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            store: store.clone(),
            autodetect: config_bool(cwd, "lfs.remote.autodetect"),
            search_all: config_bool(cwd, "lfs.remote.searchall"),
            detected: false,
            remote: None,
            fetchers: HashMap::new(),
        }
    }

    /// Download `pointer`, with the treeish of the request that needs it,
    /// if Git sent one.
    pub fn fetch(&mut self, pointer: &Pointer, treeish: Option<&str>) -> Result<(), FetchError> {
        if self.autodetect && !self.detected {
            self.detected = true;
            if let Some(remote) = first_remote_for_treeish(&self.cwd, treeish.unwrap_or("HEAD")) {
                self.remote = Some(remote);
            }
        }
        let remote = self.remote.clone();
        let err = match self.fetcher(remote)?.fetch(pointer) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if !self.search_all {
            return Err(err);
        }
        trace("git: smudge: default remote failed. searching alternate remotes");
        let remotes = git_lfs_git::endpoint::list_remotes(&self.cwd).unwrap_or_default();
        let mut last = err;
        for remote in remotes {
            match self.fetcher(Some(remote.clone()))?.fetch(pointer) {
                Ok(()) => {
                    // Keep using it, so later objects don't search again.
                    self.remote = Some(remote);
                    return Ok(());
                }
                Err(e) => {
                    trace(&format!("git: download: remote failed {remote} {e}"));
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// Download `pointer` with no request metadata, as `git lfs smudge`
    /// and the other callers do.
    pub fn fetch_plain(&mut self, pointer: &Pointer) -> Result<(), FetchError> {
        self.fetch(pointer, None)
    }

    /// Write `lfs.<url>.access` for each remote used, like
    /// [`LfsFetcher::persist_access_mode`].
    pub fn persist_access_mode(&self) {
        for fetcher in self.fetchers.values() {
            fetcher.persist_access_mode();
        }
    }

    fn fetcher(&mut self, remote: Option<String>) -> Result<&LfsFetcher, FetchError> {
        if !self.fetchers.contains_key(&remote) {
            let fetcher = LfsFetcher::from_repo_with_remote(
                &self.cwd,
                &self.store,
                remote.as_deref(),
                Operation::Download,
            )
            .map_err(|e| -> FetchError { e.to_string().into() })?;
            self.fetchers.insert(remote.clone(), fetcher);
        }
        Ok(&self.fetchers[&remote])
    }
}

/// The remote of the first remote-tracking branch containing `treeish`,
/// like upstream's `FirstRemoteForTreeish`.
///
/// The remote is the part of the branch name before the first slash, so
/// nothing is returned when a remote's name has a slash.
fn first_remote_for_treeish(cwd: &Path, treeish: &str) -> Option<String> {
    trace(&format!("git: treeish: {treeish:?}"));
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["branch", "-r", "--contains", treeish])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    let branch = listing.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (remote, _) = branch.split_once('/')?;
    let remotes = git_lfs_git::endpoint::list_remotes(cwd).unwrap_or_default();
    if remotes.iter().any(|r| r.contains('/')) {
        trace(&format!(
            "git: ref remote: cannot determine remote for ref {branch} since a remote name contains a slash"
        ));
        return None;
    }
    trace(&format!("git: working remote {remote}"));
    Some(remote.to_owned())
}

fn config_bool(cwd: &Path, key: &str) -> bool {
    git_lfs_git::config::get_bool(cwd, key, false)
}

fn trace(message: &str) {
    if std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("{message}");
    }
}
