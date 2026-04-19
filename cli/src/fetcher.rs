//! Sync→async bridge for on-demand LFS downloads from the smudge filter.
//!
//! `filter/`'s [`smudge_with_fetch`](git_lfs_filter::smudge_with_fetch) and
//! [`filter_process`](git_lfs_filter::filter_process) take a sync closure
//! that's invoked when an object is missing from the local store. This
//! module hosts the closure: it owns the tokio runtime + the
//! [`git_lfs_transfer::Transfer`] instance that actually drives the
//! download.
//!
//! Construction is infallible at the runtime level — if `lfs.url` isn't
//! configured, the [`LfsFetcher`] returns the error lazily, on the first
//! `fetch` call. That means a smudge of a *locally-present* object never
//! has to talk to git config, so a stripped-down repo can still smudge
//! its already-fetched bytes.

use std::path::Path;

use git_lfs_api::{Auth, Client as ApiClient, ObjectSpec};
use git_lfs_filter::FetchError;
use git_lfs_git::ConfigScope;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use git_lfs_transfer::{Transfer, TransferConfig};
use tokio::runtime::Runtime;

/// Owns the tokio runtime and the configured [`Transfer`]. One per CLI
/// invocation; `fetch` is cheap to call repeatedly (e.g. inside the
/// long-running filter-process loop).
pub struct LfsFetcher {
    runtime: Runtime,
    transfer: Result<Transfer, String>,
}

impl LfsFetcher {
    /// Build a fetcher rooted at the given repo. Runtime construction is
    /// the only thing that can fail here; missing LFS config is captured
    /// and surfaced on the first [`fetch`](Self::fetch) call instead.
    pub fn from_repo(cwd: &Path, store: &Store) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let transfer = build_transfer(cwd, store);
        Ok(Self { runtime, transfer })
    }

    /// Download the object identified by `pointer` into the local store.
    /// Returns once the bytes are committed (and hash-verified by
    /// [`Store::insert_verified`](git_lfs_store::Store)).
    pub fn fetch(&self, pointer: &Pointer) -> Result<(), FetchError> {
        let transfer = self
            .transfer
            .as_ref()
            .map_err(|msg| -> FetchError { msg.clone().into() })?;

        let oid = pointer.oid.to_string();
        let size = pointer.size;
        let report = self.runtime.block_on(transfer.download(
            vec![ObjectSpec { oid: oid.clone(), size }],
            None,
            None,
        ))?;

        if let Some((failed_oid, err)) = report.failed.into_iter().next() {
            return Err(format!("download failed for {failed_oid}: {err}").into());
        }
        Ok(())
    }
}

fn build_transfer(cwd: &Path, store: &Store) -> Result<Transfer, String> {
    // For v0 we only check the repo-local config. Upstream also reads the
    // `.lfsconfig` file at the repo root and falls back to deriving the
    // URL from `remote.<name>.url`; both are listed as deferred work in
    // NOTES.md.
    let endpoint = git_lfs_git::config::get(cwd, ConfigScope::Local, "lfs.url")
        .map_err(|e| format!("reading lfs.url: {e}"))?
        .ok_or_else(|| "lfs.url is not configured".to_string())?;
    let url = url::Url::parse(&endpoint).map_err(|e| format!("invalid lfs.url: {e}"))?;
    let api = ApiClient::new(url, Auth::None);
    Ok(Transfer::new(api, store.clone(), TransferConfig::default()))
}
