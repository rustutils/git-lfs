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
use std::sync::Arc;

use git_lfs_api::{Auth, Client as ApiClient, ObjectSpec, Ref};
use git_lfs_creds::{CachingHelper, GitCredentialHelper, Helper, HelperChain};
use git_lfs_filter::FetchError;
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use git_lfs_transfer::{Report, Transfer, TransferConfig};
use tokio::runtime::Runtime;

/// Owns the tokio runtime and the configured [`Transfer`]. One per CLI
/// invocation; `fetch` is cheap to call repeatedly (e.g. inside the
/// long-running filter-process loop).
pub struct LfsFetcher {
    runtime: Runtime,
    transfer: Result<Transfer, String>,
    /// Refspec sent on every batch request. Resolved once at
    /// construction (current branch's tracked upstream, falling back
    /// to the current branch's full ref). Required by servers that
    /// scope LFS operations per-branch — without it, branch-required
    /// repos return `403 Expected ref ..., got ""` to push/pull.
    refspec: Option<Ref>,
}

impl LfsFetcher {
    /// Build a fetcher rooted at the given repo, defaulting the remote
    /// name to `origin`. Runtime construction is the only thing that can
    /// fail here; an unresolvable LFS endpoint is captured and surfaced
    /// on the first [`fetch`](Self::fetch) call instead.
    pub fn from_repo(cwd: &Path, store: &Store) -> std::io::Result<Self> {
        Self::from_repo_with_remote(cwd, store, None)
    }

    /// Like [`from_repo`](Self::from_repo) but lets the caller specify
    /// which remote to resolve against. Used by `push` / `pre-push` so
    /// the LFS endpoint matches the remote being pushed to (otherwise a
    /// `git push upstream` would still upload via `origin`'s LFS config).
    pub fn from_repo_with_remote(
        cwd: &Path,
        store: &Store,
        remote: Option<&str>,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let transfer = build_transfer(cwd, store, remote);
        let refspec = git_lfs_git::refs::current_refspec(cwd).map(Ref::new);
        Ok(Self {
            runtime,
            transfer,
            refspec,
        })
    }

    /// Override the auto-resolved refspec. Used by `pre-push`, where
    /// the relevant ref is the *remote* ref being pushed (parsed from
    /// the hook's stdin), not the local branch the user happens to be
    /// on.
    #[must_use]
    pub fn with_refspec(mut self, refspec: Option<String>) -> Self {
        self.refspec = refspec.map(Ref::new);
        self
    }

    /// Download the object identified by `pointer` into the local store.
    /// Returns once the bytes are committed (and hash-verified by
    /// [`Store::insert_verified`](git_lfs_store::Store)).
    ///
    /// Single-object convenience over [`download_many`](Self::download_many).
    /// Used by `smudge` / `filter-process`.
    pub fn fetch(&self, pointer: &Pointer) -> Result<(), FetchError> {
        let report = self.download_many(vec![ObjectSpec {
            oid: pointer.oid.to_string(),
            size: pointer.size,
        }])?;
        if let Some((failed_oid, err)) = report.failed.into_iter().next() {
            return Err(format!("download failed for {failed_oid}: {err}").into());
        }
        Ok(())
    }

    /// Download many objects in one batch. Errors eagerly if `lfs.url`
    /// isn't configured (vs. [`fetch`](Self::fetch) which only complains
    /// when actually invoked). Caller inspects the returned [`Report`] for
    /// per-object outcomes.
    pub fn download_many(&self, specs: Vec<ObjectSpec>) -> Result<Report, FetchError> {
        let transfer = self.transfer()?;
        let report = self
            .runtime
            .block_on(transfer.download(specs, self.refspec.clone(), None))?;
        Ok(report)
    }

    /// Upload many objects to the configured LFS endpoint. Server-side
    /// dedup happens at the batch layer: objects the server already has
    /// come back with no `actions`, the transfer queue treats those as
    /// success without any byte transfer.
    pub fn upload_many(&self, specs: Vec<ObjectSpec>) -> Result<Report, FetchError> {
        let transfer = self.transfer()?;
        let report = self
            .runtime
            .block_on(transfer.upload(specs, self.refspec.clone(), None))?;
        Ok(report)
    }

    fn transfer(&self) -> Result<&Transfer, FetchError> {
        self.transfer
            .as_ref()
            .map_err(|msg| -> FetchError { msg.clone().into() })
    }
}

fn build_transfer(
    cwd: &Path,
    store: &Store,
    remote: Option<&str>,
) -> Result<Transfer, String> {
    let api = build_api_client(cwd, remote)?;
    Ok(Transfer::new(api, store.clone(), TransferConfig::default()))
}

/// Build an [`ApiClient`] for `cwd`+`remote` with our standard credential
/// helper chain attached. Used both by [`LfsFetcher`] (for transfers) and
/// by the locking commands (no transfers, just JSON requests).
pub fn build_api_client(cwd: &Path, remote: Option<&str>) -> Result<ApiClient, String> {
    let endpoint = git_lfs_git::endpoint_for_remote(cwd, remote)
        .map_err(|e| format!("resolving LFS endpoint: {e}"))?;
    let url = url::Url::parse(&endpoint).map_err(|e| format!("invalid LFS endpoint: {e}"))?;
    Ok(ApiClient::new(url, Auth::None).with_credential_helper(default_helper_chain()))
}

/// Default credential resolution chain: in-process cache → `git credential`.
/// Cache is consulted first so a single CLI invocation only shells out to
/// `git credential fill` once per host.
fn default_helper_chain() -> Arc<dyn Helper> {
    let helpers: Vec<Box<dyn Helper>> = vec![
        Box::new(CachingHelper::new()),
        Box::new(GitCredentialHelper::new()),
    ];
    Arc::new(HelperChain::new(helpers))
}
