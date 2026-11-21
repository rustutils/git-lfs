//! Lock client for `git lfs lock`, `unlock`, and `locks`, and for the
//! pre-push lock check.
//!
//! [`LockClient`] sends lock requests over HTTP or pure SSH, chosen by
//! `lfs.<url>.sshtransfer` (see [`build_lock_client_with_api`]). Over SSH,
//! it uses a one-connection pool of git-lfs-transfer sessions. The
//! negotiate variant tries pure SSH and falls back to HTTP if the pool
//! can't start, then keeps that choice.
//!
//! The SSH pool starts on the first request and is never shut down. Unlike
//! upstream, which keeps one SSH connection per operation, the first
//! request's operation is used for every later request.

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

/// Closure that starts the SSH lock pool for an operation.
///
/// The SSH protocol only allows `lock` and `unlock` on an upload
/// connection. `list-lock` works on either.
pub type SshLockPoolSpawner =
    Arc<dyn Fn(SshOperation) -> Result<Arc<Pool>, ConnectionError> + Send + Sync>;

/// Error from a [`LockClient`] call.
///
/// A conflict is [`Conflict`](Self::Conflict) over either transport, so
/// callers don't need to know which one was used.
///
/// `Api` is boxed because [`ApiError`] is large enough to trip clippy's
/// `result_large_err`.
#[derive(Debug, thiserror::Error)]
pub enum LockClientError {
    /// The path is already locked.
    #[error("lock conflict: {message}")]
    Conflict {
        existing: Option<Lock>,
        message: String,
    },
    /// An HTTP API error.
    #[error(transparent)]
    Api(Box<ApiError>),
    /// The SSH pool couldn't start, or the connection failed.
    #[error("ssh: {0}")]
    Ssh(String),
    /// The SSH server returned an error status other than a conflict.
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

/// One page of locks, from either transport.
#[derive(Debug, Clone, Default)]
pub struct LockPage {
    pub locks: Vec<Lock>,
    /// Cursor for the next page, if there is one.
    pub next_cursor: Option<String>,
}

/// One page of locks split into ours and theirs, from either transport.
///
/// Over SSH the split comes from the `owner` lines that `list-lock` returns
/// on an upload connection.
#[derive(Debug, Clone, Default)]
pub struct VerifyPage {
    pub ours: Vec<Lock>,
    pub theirs: Vec<Lock>,
    pub next_cursor: Option<String>,
}

/// Lock client over HTTP, pure SSH, or pure SSH with HTTP fallback.
pub enum LockClient {
    Http(HttpLockClient),
    Ssh(SshLockClient),
    /// Pure SSH, or HTTP if the SSH pool can't start.
    Negotiate(NegotiateLockClient),
}

impl LockClient {
    /// Build an HTTP client that runs `api`'s requests on `runtime`.
    pub fn http(api: ApiClient, runtime: Arc<Runtime>) -> Self {
        Self::Http(HttpLockClient { api, runtime })
    }

    /// Build a pure-SSH client. The pool starts on the first request.
    pub fn ssh(spawner: SshLockPoolSpawner) -> Self {
        Self::Ssh(SshLockClient::new(spawner))
    }

    /// Build a client that tries pure SSH on the first request and uses
    /// HTTP from then on if the pool can't start.
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

    /// Delete the lock with the given id.
    ///
    /// `force` asks an HTTP server to delete another user's lock. It isn't
    /// sent over SSH.
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

    /// List one page of locks. The caller follows `next_cursor`.
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

    /// List one page of locks split into ours and theirs.
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

/// HTTP lock client that runs the async [`ApiClient`] calls on a runtime.
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

/// Pure-SSH lock client.
///
/// Starts its pool on the first request and keeps it for the client's
/// lifetime.
pub struct SshLockClient {
    spawner: SshLockPoolSpawner,
    state: Mutex<SshState>,
}

/// Whether the SSH pool has started. A failed start isn't remembered, so
/// the next request tries again.
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

    /// Return the pool, starting it for `op` if it hasn't started.
    ///
    /// Once started, the pool is returned whatever `op` is, so a
    /// `list_locks` followed by a `delete_lock` (as path-based unlock does)
    /// sends `unlock` on a download connection.
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
        // The protocol has a `force` argument, but upstream's
        // `sshLockClient.Unlock` doesn't send it either.
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
        // SSH has no verify command. On an upload connection, `list-lock`
        // adds an `owner` line to each lock saying ours or theirs.
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

/// Lock client that tries pure SSH first and falls back to HTTP if the
/// pool can't start.
///
/// The first request decides, and the choice is kept for the client's
/// lifetime.
pub struct NegotiateLockClient {
    spawner: SshLockPoolSpawner,
    http: HttpLockClient,
    state: Mutex<NegotiateState>,
}

enum NegotiateState {
    Untried,
    Ssh(Arc<SshLockClient>),
    Fallback,
}

/// The transport [`NegotiateLockClient::resolve`] picked.
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

/// Turn an SSH `lock` or `unlock` response into a result.
///
/// A 409 becomes [`LockClientError::Conflict`], as it is over HTTP.
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

/// Turn an SSH `list-lock` response into a [`LockPage`].
///
/// With a path filter, a 404 means no lock matches (scutiger answers that
/// way), so it becomes an empty page.
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

/// Convert an SSH lock into the API crate's [`Lock`].
fn lock_from_spec(spec: ssh_lock::LockSpec) -> Lock {
    Lock {
        id: spec.id,
        path: spec.path,
        owner: spec.ownername.map(|name| Owner { name }),
        locked_at: spec.locked_at,
    }
}

/// Build a [`LockClient`] for the endpoint of `remote`.
///
/// See [`build_lock_client_with_api`]. `runtime` runs the HTTP requests.
pub(crate) fn build_lock_client(
    cwd: &Path,
    remote: Option<&str>,
    runtime: Arc<Runtime>,
) -> Result<LockClient, String> {
    let api = crate::fetcher::build_api_client(cwd, remote)?;
    build_lock_client_with_api(cwd, remote, runtime, api)
}

/// Build a [`LockClient`] that uses `api` for HTTP.
///
/// The transport depends on the endpoint and `lfs.<url>.sshtransfer`:
///
/// - An HTTP endpoint, or `never`: [`LockClient::Http`].
/// - An SSH endpoint with `always`: [`LockClient::Ssh`].
/// - An SSH endpoint with any other value, or none:
///   [`LockClient::Negotiate`].
///
/// [`LfsFetcher`](crate::fetcher::LfsFetcher) passes its own API client, so
/// its cached credentials and git-lfs-authenticate response are reused.
/// `t-batch-transfer.sh` counts git-lfs-authenticate runs.
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

/// Build the spawner for a one-connection SSH lock pool.
///
/// Lock requests are sent one at a time, so more connections wouldn't help.
/// Like transfers, it always uses the OpenSSH variant.
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
