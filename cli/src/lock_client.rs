//! Backend-agnostic lock client used by `git lfs lock` / `unlock` /
//! `locks` and the pre-push lock-verify pre-flight.
//!
//! Three variants today, all behind one enum so callers don't branch
//! on transport:
//!
//! - [`LockClient::Http`] — classic HTTP path. Wraps
//!   [`ApiClient`]'s `create_lock` / `delete_lock` / `list_locks` /
//!   `verify_locks` plus a small runtime for the sync→async bridge.
//! - [`LockClient::Ssh`] — pure-SSH path. Holds an [`Arc<Pool>`] of
//!   `git-lfs-transfer` connections (typically size 1, since lock
//!   commands serialize on the wire anyway) and drives them via
//!   [`crate::sshtransfer::lock`].
//! - [`LockClient::Negotiate`] — try pure-SSH first, fall back to
//!   HTTP on pool spawn failure. Mirrors
//!   [`SshBackend`](git_lfs_transfer::backend::SshBackend)'s lazy
//!   per-direction dispatch, but with a single state since locking
//!   doesn't split by upload/download.
//!
//! For SSH variants the pool master spawns lazily on first command,
//! and is intentionally **kept alive** for the lifetime of the
//! client — that matches upstream's quirky-but-load-bearing
//! "control socket SSH connection [for locking] [is] never shut
//! down cleanly" behavior that `t-batch-transfer.sh`'s
//! `assert_ssh_transfer_sessions` counts on the upload path.

use std::path::Path;
use std::sync::{Arc, Mutex};

use git_lfs_api::{
    ApiError, Client as ApiClient, CreateLockError, CreateLockRequest, DeleteLockRequest,
    ListLocksFilter, Lock, LockList, Owner, Ref, VerifyLocksRequest, VerifyLocksResponse,
};
use git_lfs_git::endpoint::SshInfo;
use git_lfs_transfer::sshtransfer::connection::{
    ConnectionError, Metadata as PoolMetadata, Operation as SshOperation, Variant as PoolVariant,
};
use git_lfs_transfer::sshtransfer::lock as ssh_lock;
use git_lfs_transfer::sshtransfer::pool::{Pool, PoolConfig};
use tokio::runtime::Runtime;

/// Closure that brings up an SSH lock-pool master on demand. Takes
/// the transfer-side [`SshOperation`] the pool should announce
/// (locking uses `upload` for create/unlock per spec; `download`
/// works for list-only flows).
pub type SshLockPoolSpawner =
    Arc<dyn Fn(SshOperation) -> Result<Arc<Pool>, ConnectionError> + Send + Sync>;

/// Error envelope for [`LockClient`] calls.
///
/// Decoded into [`CreateLockError`] / [`ApiError`] equivalents by
/// callers that need to distinguish "lock already held" from
/// "transport failed" etc. The SSH variants are folded onto the
/// HTTP shapes (e.g. SSH 409 → `Conflict`) so the call sites in
/// `cli::lock` don't have to fork their error handling per backend.
///
/// `Api` is boxed because `ApiError::Status` carries a ~144-byte
/// body — without the box clippy's `result_large_err` would fire on
/// every `Result<Lock, _>` return.
#[derive(Debug, thiserror::Error)]
pub enum LockClientError {
    /// Path-already-locked. Mirrors [`CreateLockError::Conflict`].
    #[error("lock conflict: {message}")]
    Conflict {
        existing: Option<Lock>,
        message: String,
    },
    /// HTTP-side `ApiError`.
    #[error(transparent)]
    Api(Box<ApiError>),
    /// SSH-side `ConnectionError` (pool spawn or wire-protocol failure).
    #[error("ssh: {0}")]
    Ssh(String),
    /// SSH server returned a non-success status. Carries the parsed
    /// status code and the server-supplied message.
    #[error("ssh: status {status}: {message}")]
    SshStatus { status: u16, message: String },
}

impl From<ApiError> for LockClientError {
    fn from(e: ApiError) -> Self {
        LockClientError::Api(Box::new(e))
    }
}

impl From<CreateLockError> for LockClientError {
    fn from(e: CreateLockError) -> Self {
        match e {
            CreateLockError::Conflict { existing, message } => {
                LockClientError::Conflict { existing, message }
            }
            CreateLockError::Api(a) => LockClientError::Api(Box::new(a)),
        }
    }
}

impl From<ConnectionError> for LockClientError {
    fn from(e: ConnectionError) -> Self {
        LockClientError::Ssh(e.to_string())
    }
}

/// One page of locks plus the optional next-page cursor. Mirrors
/// [`LockList`] but lives here so the SSH side can produce it
/// without reaching into the HTTP API types' wire-shaped helpers.
#[derive(Debug, Clone, Default)]
pub struct LockPage {
    /// Locks on this page.
    pub locks: Vec<Lock>,
    /// Pagination continuation cursor; absent on the final page.
    pub next_cursor: Option<String>,
}

/// One page of a verify-locks response. Same shape as
/// [`VerifyLocksResponse`] but lives in this module so the SSH side
/// can produce it (SSH derives ours/theirs from `owner` lines on
/// upload-mode `list-lock` rather than a separate endpoint).
#[derive(Debug, Clone, Default)]
pub struct VerifyPage {
    pub ours: Vec<Lock>,
    pub theirs: Vec<Lock>,
    pub next_cursor: Option<String>,
}

/// Backend-agnostic lock client.
pub enum LockClient {
    /// HTTP transport. See module-level docs.
    Http(HttpLockClient),
    /// Pure-SSH transport. See module-level docs.
    Ssh(SshLockClient),
    /// Try SSH first, fall back to HTTP on pool spawn failure.
    Negotiate(NegotiateLockClient),
}

impl LockClient {
    /// Build a pure-HTTP client over `api`. `runtime` is used for the
    /// sync→async bridge (the underlying [`ApiClient`] methods are
    /// async).
    pub fn http(api: ApiClient, runtime: Arc<Runtime>) -> Self {
        Self::Http(HttpLockClient { api, runtime })
    }

    /// Build a pure-SSH client over a lazy pool spawner. The pool
    /// doesn't materialize until the first command runs.
    pub fn ssh(spawner: SshLockPoolSpawner) -> Self {
        Self::Ssh(SshLockClient::new(spawner))
    }

    /// Build a negotiate client that prefers SSH and falls back to
    /// HTTP per call when the pool can't spawn.
    pub fn negotiate(
        spawner: SshLockPoolSpawner,
        http_api: ApiClient,
        runtime: Arc<Runtime>,
    ) -> Self {
        Self::Negotiate(NegotiateLockClient {
            spawner,
            http: HttpLockClient {
                api: http_api,
                runtime,
            },
            state: Mutex::new(NegotiateState::Untried),
        })
    }

    /// Create a lock for `path` scoped to `refspec` (if set).
    pub fn create_lock(&self, path: &str, refspec: Option<&str>) -> Result<Lock, LockClientError> {
        match self {
            LockClient::Http(c) => c.create_lock(path, refspec),
            LockClient::Ssh(c) => c.create_lock(path, refspec),
            LockClient::Negotiate(c) => match c.resolve(SshOperation::Upload) {
                ResolvedLockBackend::Ssh(s) => s.create_lock(path, refspec),
                ResolvedLockBackend::Http => c.http.create_lock(path, refspec),
            },
        }
    }

    /// Delete the lock with the given id; pass `force` to override
    /// owner checks (server still enforces auth).
    pub fn delete_lock(
        &self,
        id: &str,
        refspec: Option<&str>,
        force: bool,
    ) -> Result<Lock, LockClientError> {
        match self {
            LockClient::Http(c) => c.delete_lock(id, refspec, force),
            LockClient::Ssh(c) => c.delete_lock(id, refspec, force),
            LockClient::Negotiate(c) => match c.resolve(SshOperation::Upload) {
                ResolvedLockBackend::Ssh(s) => s.delete_lock(id, refspec, force),
                ResolvedLockBackend::Http => c.http.delete_lock(id, refspec, force),
            },
        }
    }

    /// List locks one page at a time. Pagination is the caller's
    /// responsibility (loop on `next_cursor`).
    pub fn list_locks(&self, filter: &ListLocksFilter) -> Result<LockPage, LockClientError> {
        match self {
            LockClient::Http(c) => c.list_locks(filter),
            LockClient::Ssh(c) => c.list_locks(filter),
            LockClient::Negotiate(c) => match c.resolve(SshOperation::Download) {
                ResolvedLockBackend::Ssh(s) => s.list_locks(filter),
                ResolvedLockBackend::Http => c.http.list_locks(filter),
            },
        }
    }

    /// Run `/locks/verify` (or its SSH equivalent), partitioning
    /// locks into ours and theirs.
    pub fn verify_locks(&self, req: &VerifyLocksRequest) -> Result<VerifyPage, LockClientError> {
        match self {
            LockClient::Http(c) => c.verify_locks(req),
            LockClient::Ssh(c) => c.verify_locks(req),
            LockClient::Negotiate(c) => match c.resolve(SshOperation::Upload) {
                ResolvedLockBackend::Ssh(s) => s.verify_locks(req),
                ResolvedLockBackend::Http => c.http.verify_locks(req),
            },
        }
    }
}

/// HTTP locking backend. Thin wrapper over `ApiClient` that
/// block_ons the async methods on a shared runtime.
pub struct HttpLockClient {
    api: ApiClient,
    runtime: Arc<Runtime>,
}

impl HttpLockClient {
    fn create_lock(&self, path: &str, refspec: Option<&str>) -> Result<Lock, LockClientError> {
        let mut req = CreateLockRequest::new(path.to_owned());
        if let Some(r) = refspec {
            req = req.with_ref(Ref::new(r.to_owned()));
        }
        self.runtime
            .block_on(self.api.create_lock(&req))
            .map_err(Into::into)
    }

    fn delete_lock(
        &self,
        id: &str,
        refspec: Option<&str>,
        force: bool,
    ) -> Result<Lock, LockClientError> {
        let req = DeleteLockRequest {
            force,
            r#ref: refspec.map(|r| Ref::new(r.to_owned())),
        };
        self.runtime
            .block_on(self.api.delete_lock(id, &req))
            .map_err(Into::into)
    }

    fn list_locks(&self, filter: &ListLocksFilter) -> Result<LockPage, LockClientError> {
        let page: LockList = self.runtime.block_on(self.api.list_locks(filter))?;
        Ok(LockPage {
            locks: page.locks,
            next_cursor: page.next_cursor,
        })
    }

    fn verify_locks(&self, req: &VerifyLocksRequest) -> Result<VerifyPage, LockClientError> {
        let resp: VerifyLocksResponse = self.runtime.block_on(self.api.verify_locks(req))?;
        Ok(VerifyPage {
            ours: resp.ours,
            theirs: resp.theirs,
            next_cursor: resp.next_cursor,
        })
    }
}

/// SSH locking backend. Lazily spawns a single-slot pool on first
/// call (a master `-oControlMaster=yes` SSH connection), then
/// dispatches `lock` / `unlock` / `list-lock` commands through it.
///
/// One pool per SSH lock client; we don't tear it down — see
/// module-level docs.
pub struct SshLockClient {
    spawner: SshLockPoolSpawner,
    state: Mutex<SshState>,
}

/// Per-client lazy-spawn state. First call transitions
/// `Untried` → `Pool(...)` on success; spawn failures bubble up
/// without caching (so a transient SSH failure can be retried).
enum SshState {
    Untried,
    Pool(Arc<Pool>),
}

impl SshLockClient {
    fn new(spawner: SshLockPoolSpawner) -> Self {
        Self {
            spawner,
            state: Mutex::new(SshState::Untried),
        }
    }

    /// Return the cached pool, spawning it via the spawner closure
    /// on the first call. The operation passed to the spawner
    /// matches what the upcoming command needs (`upload` for
    /// create/delete/verify; `download` for plain list).
    fn pool(&self, op: SshOperation) -> Result<Arc<Pool>, LockClientError> {
        let mut state = self.state.lock().unwrap();
        if let SshState::Pool(p) = &*state {
            return Ok(p.clone());
        }
        let pool = (self.spawner)(op)?;
        *state = SshState::Pool(pool.clone());
        Ok(pool)
    }

    fn create_lock(&self, path: &str, refspec: Option<&str>) -> Result<Lock, LockClientError> {
        let pool = self.pool(SshOperation::Upload)?;
        let opts = ssh_lock::LockOptions {
            refname: refspec.map(str::to_owned),
        };
        let mut guard = pool.acquire().map_err(LockClientError::from)?;
        let resp = ssh_lock::lock(guard.connection(), path, &opts)?;
        translate_lock_response(resp)
    }

    fn delete_lock(
        &self,
        id: &str,
        refspec: Option<&str>,
        _force: bool,
    ) -> Result<Lock, LockClientError> {
        // `force` doesn't go on the wire — upstream's sshLockClient.Unlock
        // omits it and scutiger ignores it. Server-side auth is
        // expected to gate the override.
        let pool = self.pool(SshOperation::Upload)?;
        let opts = ssh_lock::UnlockOptions {
            refname: refspec.map(str::to_owned),
        };
        let mut guard = pool.acquire().map_err(LockClientError::from)?;
        let resp = ssh_lock::unlock(guard.connection(), id, &opts)?;
        translate_lock_response(resp)
    }

    fn list_locks(&self, filter: &ListLocksFilter) -> Result<LockPage, LockClientError> {
        let pool = self.pool(SshOperation::Download)?;
        let f = ssh_lock::ListFilter {
            path: filter.path.clone(),
            id: filter.id.clone(),
            cursor: filter.cursor.clone(),
            limit: filter.limit,
            refname: filter.refspec.clone(),
        };
        let mut guard = pool.acquire().map_err(LockClientError::from)?;
        let resp = ssh_lock::list(guard.connection(), &f)?;
        translate_list_response(resp, filter.path.is_some())
    }

    fn verify_locks(&self, req: &VerifyLocksRequest) -> Result<VerifyPage, LockClientError> {
        // SSH has no separate verify endpoint — it's `list-lock` on
        // an upload-mode connection, which causes the server to add
        // `owner <id> ours|theirs` lines that we partition on.
        let pool = self.pool(SshOperation::Upload)?;
        let f = ssh_lock::ListFilter {
            path: None,
            id: None,
            cursor: req.cursor.clone(),
            limit: req.limit,
            refname: req.r#ref.as_ref().map(|r| r.name.clone()),
        };
        let mut guard = pool.acquire().map_err(LockClientError::from)?;
        let resp = ssh_lock::list(guard.connection(), &f)?;
        if !(200..300).contains(&resp.status) {
            return Err(LockClientError::SshStatus {
                status: resp.status,
                message: resp.message.unwrap_or_default(),
            });
        }
        let mut ours = Vec::new();
        let mut theirs = Vec::new();
        for (spec, owner) in resp.locks {
            let lock = lock_from_spec(spec);
            match owner {
                ssh_lock::Owner::Ours => ours.push(lock),
                ssh_lock::Owner::Theirs => theirs.push(lock),
                ssh_lock::Owner::Unknown => {}
            }
        }
        Ok(VerifyPage {
            ours,
            theirs,
            next_cursor: resp.next_cursor,
        })
    }
}

/// Negotiate client: try SSH first, then HTTP if the SSH pool fails
/// to spawn. The choice is cached for the lifetime of the client.
pub struct NegotiateLockClient {
    spawner: SshLockPoolSpawner,
    http: HttpLockClient,
    state: Mutex<NegotiateState>,
}

/// Per-client negotiate state. Mirrors
/// [`SshBackend`](git_lfs_transfer::backend::SshBackend)'s pattern:
/// first call decides, subsequent calls reuse.
enum NegotiateState {
    Untried,
    Ssh(Arc<SshLockClient>),
    Fallback,
}

/// One concrete backend choice made by
/// [`NegotiateLockClient::resolve`].
enum ResolvedLockBackend {
    Ssh(Arc<SshLockClient>),
    Http,
}

impl NegotiateLockClient {
    fn resolve(&self, op: SshOperation) -> ResolvedLockBackend {
        let mut state = self.state.lock().unwrap();
        match &*state {
            NegotiateState::Ssh(s) => return ResolvedLockBackend::Ssh(s.clone()),
            NegotiateState::Fallback => return ResolvedLockBackend::Http,
            NegotiateState::Untried => {}
        }
        // First call — try spawning the SSH pool.
        let ssh = SshLockClient::new(self.spawner.clone());
        match ssh.pool(op) {
            Ok(_) => {
                let arc = Arc::new(ssh);
                *state = NegotiateState::Ssh(arc.clone());
                ResolvedLockBackend::Ssh(arc)
            }
            Err(_) => {
                *state = NegotiateState::Fallback;
                ResolvedLockBackend::Http
            }
        }
    }
}

/// Turn an SSH lock/unlock response into a `Result<Lock, LockClientError>`,
/// folding conflict and error statuses onto the HTTP-shaped error
/// variants so callers don't have to branch on transport.
fn translate_lock_response(resp: ssh_lock::LockResponse) -> Result<Lock, LockClientError> {
    let status = resp.status;
    let is_success = (200..300).contains(&status);
    if is_success {
        if let Some(spec) = resp.lock {
            return Ok(lock_from_spec(spec));
        }
        return Err(LockClientError::Ssh(format!(
            "ssh lock response missing lock attributes (status {status})"
        )));
    }
    if status == 409 {
        let existing = resp.lock.map(lock_from_spec);
        return Err(LockClientError::Conflict {
            existing,
            message: resp.message.unwrap_or_else(|| "conflict".into()),
        });
    }
    Err(LockClientError::SshStatus {
        status,
        message: resp.message.unwrap_or_default(),
    })
}

/// Turn an SSH list-lock response into a `LockPage`. A 404 with a
/// path filter means "no lock matches" (scutiger's "not found" for
/// `list-lock path=<p>`) — surface as empty page so callers don't
/// have to special-case it.
fn translate_list_response(
    resp: ssh_lock::ListResponse,
    had_path_filter: bool,
) -> Result<LockPage, LockClientError> {
    if (200..300).contains(&resp.status) {
        let locks = resp
            .locks
            .into_iter()
            .map(|(spec, _owner)| lock_from_spec(spec))
            .collect();
        return Ok(LockPage {
            locks,
            next_cursor: resp.next_cursor,
        });
    }
    if resp.status == 404 && had_path_filter {
        return Ok(LockPage::default());
    }
    Err(LockClientError::SshStatus {
        status: resp.status,
        message: resp.message.unwrap_or_default(),
    })
}

/// Coerce an SSH-side `LockSpec` into the API crate's `Lock`. The
/// SSH wire format reuses the same four fields, so the conversion
/// is straight assignment.
fn lock_from_spec(spec: ssh_lock::LockSpec) -> Lock {
    Lock {
        id: spec.id,
        path: spec.path,
        owner: spec.ownername.map(|name| Owner { name }),
        locked_at: spec.locked_at,
    }
}

/// Build a [`LockClient`] for `(cwd, remote)`. Resolves the LFS
/// endpoint, reads `lfs.<url>.sshtransfer`, and picks the variant:
///
/// - HTTP-shaped endpoint, or `sshtransfer=never`: [`LockClient::Http`].
/// - SSH-shaped endpoint with `sshtransfer=always`: [`LockClient::Ssh`].
/// - SSH-shaped endpoint, default or `sshtransfer=negotiate`:
///   [`LockClient::Negotiate`].
///
/// The `runtime` argument is only used by the HTTP-side block-on
/// bridge; SSH-only callers can hand in any runtime (e.g. a tiny
/// throwaway one).
pub(crate) fn build_lock_client(
    cwd: &Path,
    remote: Option<&str>,
    runtime: Arc<Runtime>,
) -> Result<LockClient, String> {
    let api = crate::fetcher::build_api_client(cwd, remote)?;
    build_lock_client_with_api(cwd, remote, runtime, api)
}

/// Like [`build_lock_client`] but reuses an existing [`ApiClient`].
/// Used by [`LfsFetcher`](crate::fetcher::LfsFetcher) so the HTTP
/// fallback path shares its credential / SSH-auth cache with the
/// transfer client — without this, preflight verify ends up making
/// a second `git-lfs-authenticate` call that the test suite counts
/// (`t-batch-transfer` test 5).
pub(crate) fn build_lock_client_with_api(
    cwd: &Path,
    remote: Option<&str>,
    runtime: Arc<Runtime>,
    api: ApiClient,
) -> Result<LockClient, String> {
    let info = git_lfs_git::endpoint::resolve_endpoint(cwd, remote)
        .map_err(|e| format!("resolving LFS endpoint: {e}"))?;
    let Some(ssh) = info.ssh else {
        return Ok(LockClient::http(api, runtime));
    };
    match crate::fetcher::read_sshtransfer_for(cwd, &info.url).as_deref() {
        Some("never") => Ok(LockClient::http(api, runtime)),
        Some("always") => {
            let spawner = ssh_lock_pool_spawner(ssh);
            Ok(LockClient::ssh(spawner))
        }
        _ => {
            let spawner = ssh_lock_pool_spawner(ssh);
            Ok(LockClient::negotiate(spawner, api, runtime))
        }
    }
}

/// Build a single-connection [`Pool`] spawner for SSH locks. The
/// pool size is 1 — lock commands are short and serialize on the
/// wire, no benefit to extra slots.
fn ssh_lock_pool_spawner(ssh: SshInfo) -> SshLockPoolSpawner {
    let program = crate::fetcher::resolve_ssh_program();
    let multiplex_enabled = !cfg!(target_os = "windows");
    let metadata = PoolMetadata {
        user_and_host: ssh.user_and_host,
        port: ssh.port,
        path: ssh.path,
    };
    Arc::new(move |operation: SshOperation| {
        let pool_config = PoolConfig {
            program: program.clone(),
            variant: PoolVariant::Default,
            metadata: metadata.clone(),
            operation,
            multiplex_enabled,
        };
        Pool::new(pool_config, 1).map(Arc::new)
    })
}
