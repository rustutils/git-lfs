//! Builds the API client, credential helpers, and transfer backend.
//!
//! [`LfsFetcher`] owns a tokio runtime and a [`Transfer`], and gives the
//! sync commands blocking methods for downloads, uploads, and one-off batch
//! requests. The smudge filter, fetch, pull, push, prune, and checkout all
//! go through it. [`build_api_client`] builds the same API client for the
//! locking commands.
//!
//! Building an [`LfsFetcher`] only fails if the runtime can't be built. If
//! the endpoint can't be resolved, the error is kept and returned by the
//! first method that needs it, so smudging an object that's already in the
//! store works without an endpoint.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use git_lfs_api::{
    Auth, Client as ApiClient, ObjectSpec, Operation, Ref, SharedSshResolver,
    SshAuth as ApiSshAuth, SshOperation as ApiSshOperation, SshResolver,
};
use git_lfs_creds::{
    AskpassHelper, CachingHelper, GitCredentialHelper, Helper, HelperChain, NetrcCredentialHelper,
    SshAuthClient, SshAuthError, SshOperation as CredsSshOperation, SshProgram,
};
use git_lfs_filter::FetchError;
use git_lfs_git::ConfigScope;
use git_lfs_git::endpoint::{Operation as EndpointOperation, SshInfo};
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use git_lfs_transfer::backend::{HttpTransport, SshBackend};
use git_lfs_transfer::sshtransfer::connection::{
    ConnectionError, Metadata as PoolMetadata, Operation as SshOperation, Variant as PoolVariant,
};
use git_lfs_transfer::sshtransfer::pool::{Pool, PoolConfig};
use git_lfs_transfer::{CustomAdapter, Report, Transfer, TransferConfig};
use tokio::runtime::Runtime;

/// Owns the tokio runtime and the configured [`Transfer`].
///
/// Commands build one per run. The filter-process loop calls
/// [`fetch`](Self::fetch) on the same fetcher for every missing object.
pub struct LfsFetcher {
    runtime: Runtime,
    transfer: Result<Transfer, String>,
    /// The API client, kept for [`api_client`](Self::api_client), the lock
    /// check, and [`persist_access_mode`](Self::persist_access_mode).
    api: Result<ApiClient, String>,
    /// Ref sent with every batch request.
    ///
    /// Defaults to the current branch's `branch.<name>.merge`, else the
    /// branch itself. Servers that scope objects by branch reject requests
    /// without it.
    refspec: Option<Ref>,
    /// Where to write `lfs.<url>.access`.
    cwd: PathBuf,
    /// The pure-SSH pools, shared with the lock check. `None` for an HTTP
    /// endpoint.
    ssh_pools: Option<Arc<SshPools>>,
    /// Whether a standalone transfer agent moves the objects, so there's
    /// no LFS server to check locks with.
    standalone: bool,
    /// File names for objects, by OID, for the `GIT_LFS_PROGRESS` log.
    progress_names: Mutex<HashMap<String, String>>,
}

impl LfsFetcher {
    /// Build a fetcher for the default remote.
    ///
    /// Only fails if the runtime can't be built. An endpoint error is
    /// returned later, by the first method that needs the endpoint.
    pub fn from_repo(cwd: &Path, store: &Store) -> std::io::Result<Self> {
        Self::from_repo_with_remote(cwd, store, None, EndpointOperation::Download)
    }

    /// Build a fetcher for the `operation` endpoint of `remote`.
    ///
    /// push and pre-push pass the remote being pushed to and
    /// [`Upload`](EndpointOperation::Upload), so its push endpoint is used.
    pub fn from_repo_with_remote(
        cwd: &Path,
        store: &Store,
        remote: Option<&str>,
        operation: EndpointOperation,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let endpoint_info = git_lfs_git::endpoint::resolve_endpoint_for(cwd, remote, operation);
        let endpoint_url = endpoint_info.as_ref().ok().map(|i| i.url.clone());
        // The HTTP client takes its `http.*` settings from the endpoint URL
        // and uses them for every request, including action URLs on other
        // hosts. For an SSH endpoint that's the SSH URL, so only the global
        // settings match.
        let http = endpoint_url
            .as_deref()
            .map(|u| crate::http_client::build(cwd, u))
            .unwrap_or_default();
        let api = build_api_client_with(cwd, remote, operation, http.clone());
        let mut config = transfer_config_for(cwd, endpoint_url.as_deref());
        config.remote = remote.unwrap_or("origin").to_owned();
        config.standalone = standalone_agent(
            cwd,
            endpoint_url.as_deref(),
            operation,
            &config.custom_adapters,
        );
        let standalone = config.standalone.is_some();
        config.action_auth = endpoint_url
            .as_deref()
            .and_then(|url| storage_auth(cwd, url, remote, operation));

        // SSH endpoints pick a backend from `lfs.<url>.sshtransfer`.
        // `always` uses only pure SSH, `never` uses only HTTP with
        // git-lfs-authenticate, and anything else tries pure SSH and falls
        // back to HTTP, per direction, if the pool can't start (for example
        // when the server has no git-lfs-transfer). The SSH connection
        // starts on the first transfer, not here.
        let ssh_mode = endpoint_info
            .as_ref()
            .ok()
            .and_then(|info| info.ssh.as_ref().map(|ssh| (info.url.clone(), ssh.clone())))
            .map(|(url, ssh)| {
                let mode = match read_sshtransfer_for(cwd, &url).as_deref() {
                    Some("always") => SshMode::Always,
                    Some("never") => SshMode::Never,
                    _ => SshMode::Negotiate,
                };
                (ssh, mode)
            });
        let ssh_pools = ssh_mode
            .as_ref()
            .map(|(ssh, _)| Arc::new(SshPools::new(cwd, ssh.clone())));
        let transfer = match ssh_mode {
            Some((_, SshMode::Always)) => {
                let pools = ssh_pools.clone().expect("SSH endpoint has pools");
                let spawner = ssh_pool_spawner(pools, config.concurrency.max(1));
                Ok(Transfer::new_ssh(
                    SshBackend::new(spawner, None),
                    store.clone(),
                    config,
                ))
            }
            Some((_, SshMode::Negotiate)) => {
                let pools = ssh_pools.clone().expect("SSH endpoint has pools");
                let spawner = ssh_pool_spawner(pools, config.concurrency.max(1));
                match api.clone() {
                    Ok(api_client) => {
                        let fallback = HttpTransport {
                            api: Box::new(api_client),
                            http: http.clone(),
                        };
                        Ok(Transfer::new_ssh(
                            SshBackend::new(spawner, Some(fallback)),
                            store.clone(),
                            config,
                        ))
                    }
                    // Without an API client there's no HTTP fallback, so a
                    // pool that can't start is an error.
                    Err(_) => Ok(Transfer::new_ssh(
                        SshBackend::new(spawner, None),
                        store.clone(),
                        config,
                    )),
                }
            }
            Some((_, SshMode::Never)) | None => api
                .clone()
                .map(|c| Transfer::with_http_client(c, store.clone(), config, http)),
        };
        let refspec = git_lfs_git::refs::current_refspec(cwd).map(Ref::new);
        Ok(Self {
            runtime,
            transfer,
            api,
            refspec,
            cwd: cwd.to_path_buf(),
            ssh_pools,
            standalone,
            progress_names: Mutex::new(HashMap::new()),
        })
    }

    /// Name the objects in the `GIT_LFS_PROGRESS` log by these paths,
    /// keyed by OID. Objects without one are named by their OID.
    pub fn set_progress_names<P: AsRef<Path>>(&self, names: &HashMap<String, P>) {
        let mut map = self.progress_names.lock().unwrap();
        for (oid, path) in names {
            map.insert(oid.clone(), path.as_ref().to_string_lossy().into_owned());
        }
    }

    /// Run a transfer, writing its progress to the `GIT_LFS_PROGRESS` log
    /// as `event` lines, like upstream's meter.
    fn run_logged<F, Fut>(&self, event: &str, count: usize, run: F) -> Result<Report, FetchError>
    where
        F: FnOnce(Option<tokio::sync::mpsc::UnboundedSender<git_lfs_transfer::Event>>) -> Fut,
        Fut: std::future::Future<Output = Result<Report, git_lfs_transfer::TransferError>>,
    {
        let Some(log) = git_lfs_filter::progress::ProgressLog::from_env() else {
            return Ok(self.runtime.block_on(run(None))?);
        };
        let names = self.progress_names.lock().unwrap().clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let consume = async move {
            use git_lfs_transfer::Event;
            // Each object's number, size, and the bytes last logged.
            let mut started: HashMap<String, (usize, u64, u64)> = HashMap::new();
            while let Some(e) = rx.recv().await {
                let name = |oid: &str| names.get(oid).cloned().unwrap_or_else(|| oid.to_owned());
                match e {
                    Event::Started { oid, size } => {
                        let n = started.len() + 1;
                        started.insert(oid, (n, size, 0));
                    }
                    Event::Progress { oid, bytes_done } => {
                        if let Some((n, size, logged)) = started.get_mut(&oid) {
                            // A compressed body counts its compressed
                            // bytes, which can pass the size.
                            let done = bytes_done.min(*size);
                            log.line(event, *n, count, done, Some(*size), &name(&oid));
                            *logged = done;
                        }
                    }
                    Event::Completed { oid } => {
                        if let Some((n, size, logged)) = started.get(&oid)
                            && logged != size
                        {
                            log.line(event, *n, count, *size, Some(*size), &name(&oid));
                        }
                    }
                    Event::Failed { .. } => {}
                }
            }
        };
        let (report, ()) = self
            .runtime
            .block_on(async { tokio::join!(run(Some(tx)), consume) });
        Ok(report?)
    }

    /// Set `lfs.<url>.access = basic` in the local config if the API client
    /// used HTTP Basic auth.
    ///
    /// `git lfs env` shows the value. Does nothing if the endpoint didn't
    /// resolve, Basic auth wasn't used, or the value is already `basic`. A
    /// failed write is ignored.
    pub fn persist_access_mode(&self) {
        let Ok(api) = self.api.as_ref() else { return };
        if !api.used_basic_auth() {
            return;
        }
        let url = api.endpoint().as_str();
        let key = format!("lfs.{url}.access");
        if let Ok(Some(existing)) = git_lfs_git::config::get_effective(&self.cwd, &key)
            && existing.eq_ignore_ascii_case("basic")
        {
            return;
        }
        let _ = git_lfs_git::config::set(&self.cwd, ConfigScope::Local, &key, "basic");
    }

    /// Replace the ref sent with batch and lock requests.
    ///
    /// pre-push sends the remote ref being pushed, from the hook's input,
    /// rather than the current branch.
    #[must_use]
    pub fn with_refspec(mut self, refspec: Option<String>) -> Self {
        self.refspec = refspec.map(Ref::new);
        self
    }

    /// Download the object for `pointer` into the store.
    ///
    /// The smudge filter, checkout, and the merge driver call this for
    /// objects missing from the store.
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

    /// Download objects into the store.
    ///
    /// Fails if the endpoint didn't resolve or the transfer can't run.
    /// Per-object failures are in the returned [`Report`].
    pub fn download_many(&self, specs: Vec<ObjectSpec>) -> Result<Report, FetchError> {
        let transfer = self.transfer()?;
        let count = specs.len();
        self.run_logged("download", count, |events| {
            transfer.download(specs, self.refspec.clone(), events)
        })
    }

    /// Upload objects to the endpoint.
    ///
    /// Objects the server already has come back without actions and count
    /// as uploaded without sending anything. Per-object failures are in the
    /// returned [`Report`].
    pub fn upload_many(&self, specs: Vec<ObjectSpec>) -> Result<Report, FetchError> {
        let transfer = self.transfer()?;
        let count = specs.len();
        self.run_logged("upload", count, |events| {
            transfer.upload(specs, self.refspec.clone(), events)
        })
    }

    /// The API client, or the error from resolving the endpoint.
    pub fn api_client(&self) -> Result<&ApiClient, FetchError> {
        self.api
            .as_ref()
            .map_err(|m| -> FetchError { m.clone().into() })
    }

    /// Run a future on the fetcher's runtime.
    ///
    /// fetch uses it to send its own batch request for `--json` and
    /// `--dry-run`.
    pub fn runtime_block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.runtime.block_on(fut)
    }

    /// Run the lock check that push and pre-push do before uploading.
    ///
    /// The lock client follows `lfs.<url>.sshtransfer` like transfers do,
    /// so for an SSH endpoint the lock pool's master connection starts
    /// here, which `t-batch-transfer.sh` expects. The HTTP side reuses this
    /// fetcher's API client and its cached git-lfs-authenticate response,
    /// so ssh isn't run a second time (the same test counts those runs).
    pub fn preflight_verify_locks(
        &self,
        cwd: &Path,
        remote: Option<&str>,
        remote_label: &str,
        endpoint: &str,
    ) -> Result<crate::locks_verify::Outcome, crate::push::PushCommandError> {
        if self.standalone {
            return Ok(crate::locks_verify::Outcome::Skipped);
        }
        let rt = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    crate::push::PushCommandError::Fetch(
                        format!("build verify runtime: {e}").into(),
                    )
                })?,
        );
        let api = self
            .api
            .as_ref()
            .map_err(|m| -> FetchError { m.clone().into() })
            .map_err(crate::push::PushCommandError::Fetch)?
            .clone();
        let client = crate::lock_client::build_lock_client_with_api(
            cwd,
            remote,
            rt,
            api,
            self.ssh_pools.clone(),
        )
        .map_err(|e| crate::push::PushCommandError::Fetch(e.into()))?;
        crate::locks_verify::run(&client, cwd, remote_label, endpoint, self.refspec.as_ref())
    }

    /// Return the OIDs the server already has, from one upload batch.
    ///
    /// The server says it has an object by returning it with neither
    /// actions nor an error. push uses this to tell objects missing only
    /// locally from objects missing on both sides.
    pub fn check_server_has(&self, specs: Vec<ObjectSpec>) -> Result<HashSet<String>, FetchError> {
        // A standalone agent has no server to ask, so nothing counts.
        if specs.is_empty() || self.standalone {
            return Ok(HashSet::new());
        }
        let transfer = self.transfer()?;
        let resp = self
            .runtime
            .block_on(transfer.batch(Operation::Upload, specs, self.refspec.clone()))
            .map_err(|e| -> FetchError { e.to_string().into() })?;
        Ok(resp
            .objects
            .into_iter()
            .filter(|o| o.actions.is_none() && o.error.is_none())
            .map(|o| o.oid)
            .collect())
    }

    /// Return the OIDs the server can serve, from one download batch.
    ///
    /// An object counts if it comes back with actions and no error. The
    /// action URLs aren't used. `prune --verify-remote` uses this before
    /// deleting objects.
    pub fn check_server_can_download(
        &self,
        specs: Vec<ObjectSpec>,
    ) -> Result<HashSet<String>, FetchError> {
        // A standalone agent has no server to ask, so nothing counts.
        if specs.is_empty() || self.standalone {
            return Ok(HashSet::new());
        }
        let transfer = self.transfer()?;
        let resp = self
            .runtime
            .block_on(transfer.batch(Operation::Download, specs, self.refspec.clone()))
            .map_err(|e| -> FetchError { e.to_string().into() })?;
        Ok(resp
            .objects
            .into_iter()
            .filter(|o| o.actions.is_some() && o.error.is_none())
            .map(|o| o.oid)
            .collect())
    }

    fn transfer(&self) -> Result<&Transfer, FetchError> {
        self.transfer
            .as_ref()
            .map_err(|msg| -> FetchError { msg.clone().into() })
    }
}

/// Build an [`ApiClient`] for the lock API of `remote`, with credential
/// helpers and, for SSH endpoints, git-lfs-authenticate.
///
/// The lock API uses the upload endpoint, as in upstream. The locking
/// commands and the lockable hooks use this. [`LfsFetcher`] builds the same
/// client.
pub fn build_api_client(cwd: &Path, remote: Option<&str>) -> Result<ApiClient, String> {
    let endpoint =
        git_lfs_git::endpoint::resolve_endpoint_for(cwd, remote, EndpointOperation::Upload)
            .map_err(|e| format!("resolving LFS endpoint: {e}"))?;
    let http = crate::http_client::build(cwd, &endpoint.url);
    build_api_client_with(cwd, remote, EndpointOperation::Upload, http)
}

fn build_api_client_with(
    cwd: &Path,
    remote: Option<&str>,
    operation: EndpointOperation,
    http: reqwest::Client,
) -> Result<ApiClient, String> {
    let info = git_lfs_git::endpoint::resolve_endpoint_for(cwd, remote, operation)
        .map_err(|e| format!("resolving LFS endpoint: {e}"))?;
    // An SSH endpoint gets its HTTPS form as the base URL. The href from
    // git-lfs-authenticate replaces it on each request.
    let endpoint =
        http_compatible_endpoint(&info.url).map_err(|e| format!("invalid LFS endpoint: {e}"))?;
    let mut url = url::Url::parse(&endpoint).map_err(|e| format!("invalid LFS endpoint: {e}"))?;
    // Credentials in the endpoint URL become the initial auth, so the first
    // request doesn't have to get a 401 first. Upstream's
    // `setRequestAuthFromURL` only does this when there's a password. Here
    // a username alone also gives Basic auth, with an empty password.
    let initial_auth = take_url_basic_auth(&mut url);
    let use_http_path = read_bool_default(cwd, "credential.useHttpPath", false);
    // Credential queries use the Git remote's URL when it has the same
    // scheme, host, and port as the endpoint, else the endpoint. Only a
    // remote the caller names is checked, so with `None` it's always the
    // endpoint. The helper chain uses this URL to find
    // `credential.<url>.helper`, and the API client uses it in prompts and
    // the credentials-not-found error.
    let resolved_cred_url = remote
        .and_then(|r| git_lfs_git::endpoint::remote_url(cwd, r).ok().flatten())
        .and_then(|raw| url::Url::parse(&raw).ok())
        .filter(|gu| {
            gu.scheme() == url.scheme()
                && gu.host_str() == url.host_str()
                && gu.port() == url.port()
        });
    let cred_url_for_helper = resolved_cred_url.clone().unwrap_or_else(|| url.clone());
    let skip_wwwauth = url_scoped_credential_bool(cwd, &cred_url_for_helper, "skipwwwauth", false);
    let mut client = ApiClient::with_http_client(url.clone(), initial_auth, http)
        .with_credential_helper(default_helper_chain(cwd, &cred_url_for_helper))
        .with_use_http_path(use_http_path)
        .with_skip_wwwauth(skip_wwwauth)
        .with_batch_retries(positive_setting(cwd, "lfs.transfer.maxretries").unwrap_or(8))
        .with_extra_headers(extra_header_source(cwd));
    if let Some(git_url) = resolved_cred_url {
        client = client.with_cred_url(git_url);
    }
    if let Some(ssh_info) = info.ssh {
        // With `sshtransfer = always`, only pure SSH may be used, so every
        // API call fails instead of running git-lfs-authenticate. Upstream
        // does this for any value other than `negotiate` and `never`. The
        // trace lines are upstream's.
        let sshtransfer = read_sshtransfer_for(cwd, &info.url);
        match sshtransfer.as_deref() {
            Some("always") => {
                ssh_trace(format_args!(
                    "git-lfs-authenticate has been disabled by request"
                ));
                client = client.with_ssh_resolver(Arc::new(DisabledSshResolver));
            }
            Some("never") => {
                ssh_trace(format_args!("skipping pure SSH protocol"));
                client = client.with_ssh_resolver(build_ssh_resolver(cwd, ssh_info));
            }
            _ => {
                client = client.with_ssh_resolver(build_ssh_resolver(cwd, ssh_info));
            }
        }
    }
    Ok(client)
}

/// Credentials for storage requests that answer 401. See
/// [`crate::action_auth`].
fn storage_auth(
    cwd: &Path,
    endpoint_url: &str,
    remote: Option<&str>,
    operation: EndpointOperation,
) -> Option<Arc<dyn git_lfs_transfer::ActionAuth>> {
    let endpoint = url::Url::parse(&http_compatible_endpoint(endpoint_url).ok()?).ok()?;
    let remote_name = remote.unwrap_or("origin");
    let remote_url = match operation {
        EndpointOperation::Upload => {
            git_lfs_git::config::get_effective(cwd, &format!("remote.{remote_name}.pushurl"))
                .ok()
                .flatten()
        }
        EndpointOperation::Download => None,
    }
    .or_else(|| {
        git_lfs_git::endpoint::remote_url(cwd, remote_name)
            .ok()
            .flatten()
    })
    .and_then(|u| url::Url::parse(&u).ok());
    let mut helper_url = remote_url
        .clone()
        .filter(|r| r.scheme() == endpoint.scheme() && r.host_str() == endpoint.host_str())
        .unwrap_or_else(|| endpoint.clone());
    let _ = helper_url.set_username("");
    let _ = helper_url.set_password(None);
    Some(Arc::new(crate::action_auth::StorageAuth::new(
        cwd,
        endpoint,
        remote_url,
        default_helper_chain(cwd, &helper_url),
        read_bool_default(cwd, "credential.useHttpPath", false),
    )))
}

/// Read `lfs.<endpoint>.sshtransfer`, falling back to `lfs.sshtransfer`.
///
/// The value is trimmed and lowercased.
pub(crate) fn read_sshtransfer_for(cwd: &Path, endpoint: &str) -> Option<String> {
    let endpoint_key = format!("lfs.{endpoint}.sshtransfer");
    if let Ok(Some(v)) = git_lfs_git::config::get_effective(cwd, &endpoint_key) {
        return Some(v.trim().to_ascii_lowercase());
    }
    if let Ok(Some(v)) = git_lfs_git::config::get_effective(cwd, "lfs.sshtransfer") {
        return Some(v.trim().to_ascii_lowercase());
    }
    None
}

/// Print a line to stderr if `GIT_TRACE` is set and not false.
///
/// Prints the `sshtransfer` trace lines the upstream tests grep for.
fn ssh_trace(args: std::fmt::Arguments) {
    if !ssh_trace_enabled() {
        return;
    }
    use std::io::Write as _;
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "{args}");
}

fn ssh_trace_enabled() -> bool {
    match std::env::var_os("GIT_TRACE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy().trim().to_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
}

/// SSH resolver for `sshtransfer = always`, which refuses to run
/// git-lfs-authenticate.
///
/// Every API call fails with upstream's message.
struct DisabledSshResolver;

impl SshResolver for DisabledSshResolver {
    fn resolve(&self, _op: ApiSshOperation) -> Result<ApiSshAuth, git_lfs_api::ApiError> {
        Err(git_lfs_api::ApiError::Decode(
            "git-lfs-authenticate has been disabled by request".into(),
        ))
    }
}

/// Turn an endpoint into an HTTP or HTTPS URL for the API client.
///
/// HTTP and HTTPS URLs are kept. An SSH URL gets the HTTPS form from
/// [`guessed_https_url`](git_lfs_git::endpoint::guessed_https_url), which
/// requests use unless `git-lfs-authenticate` gives another. Anything else
/// goes through [`derive_lfs_url`](git_lfs_git::endpoint::derive_lfs_url).
fn http_compatible_endpoint(url_str: &str) -> Result<String, git_lfs_git::endpoint::EndpointError> {
    if url_str.starts_with("http://") || url_str.starts_with("https://") {
        return Ok(url_str.to_owned());
    }
    // A clone URL has already become an HTTPS URL, so an SSH URL here was
    // configured as the LFS URL itself.
    if let Some(url) = git_lfs_git::endpoint::guessed_https_url(url_str) {
        return Ok(url);
    }
    git_lfs_git::endpoint::derive_lfs_url(url_str)
}

/// Build the [`SshResolver`] that runs git-lfs-authenticate for an SSH
/// endpoint, with the program from [`resolve_ssh_program`].
fn build_ssh_resolver(cwd: &Path, info: SshInfo) -> SharedSshResolver {
    let program = resolve_ssh_program(cwd);
    Arc::new(SshAuthAdapter {
        client: Arc::new(SshAuthClient::new(program)),
        ssh: info,
    })
}

/// The pure-SSH connection pools for one endpoint, one per operation.
///
/// The transfer queue and the lock client share them, so a push opens one
/// SSH session for both, as upstream does.
pub(crate) struct SshPools {
    program: SshProgram,
    metadata: PoolMetadata,
    pools: Mutex<HashMap<SshOperation, Arc<Pool>>>,
}

impl SshPools {
    pub(crate) fn new(cwd: &Path, ssh: SshInfo) -> Self {
        Self {
            program: resolve_ssh_program(cwd),
            metadata: PoolMetadata {
                user_and_host: ssh.user_and_host,
                port: ssh.port,
                path: ssh.path,
            },
            pools: Mutex::new(HashMap::new()),
        }
    }

    /// The pool for `operation`, with at least `size` slots.
    ///
    /// Starts the pool on first use, spawning its first connection. Later
    /// calls grow it if they need more slots.
    pub(crate) fn get(
        &self,
        operation: SshOperation,
        size: usize,
    ) -> Result<Arc<Pool>, ConnectionError> {
        let mut pools = self.pools.lock().unwrap();
        if let Some(pool) = pools.get(&operation) {
            pool.reserve(size);
            return Ok(pool.clone());
        }
        let config = PoolConfig {
            program: self.program.clone(),
            // Always OpenSSH: `GIT_SSH_VARIANT` and `ssh.variant` aren't
            // read, and the variant isn't guessed from the program name.
            variant: PoolVariant::Default,
            metadata: self.metadata.clone(),
            operation,
            // `lfs.ssh.automultiplex` isn't read. This is its default, on
            // everywhere but Windows.
            multiplex_enabled: !cfg!(target_os = "windows"),
        };
        let pool = Arc::new(Pool::new(config, size)?);
        pools.insert(operation, pool.clone());
        Ok(pool)
    }
}

/// Build the closure that starts a transfer's pure-SSH pool.
///
/// [`SshBackend`] calls it at most once per direction, on first use.
fn ssh_pool_spawner(
    pools: Arc<SshPools>,
    pool_size: usize,
) -> git_lfs_transfer::backend::PoolSpawner {
    Arc::new(move |operation| pools.get(operation.into(), pool_size))
}

/// How an SSH endpoint transfers, from `lfs.<url>.sshtransfer`.
///
/// `always` uses only pure SSH and `never` only HTTP. Any other value,
/// including none, is `Negotiate`: pure SSH with HTTP as the fallback.
enum SshMode {
    Always,
    Negotiate,
    Never,
}

/// Pick the SSH program like upstream: `GIT_SSH_COMMAND`, then `GIT_SSH`,
/// then `core.sshCommand`, then `ssh`.
///
/// A command made only of whitespace counts as unset.
fn resolve_ssh_program(cwd: &Path) -> SshProgram {
    let env = |name: &str| std::env::var(name).ok();
    if let Some(command) = env("GIT_SSH_COMMAND").filter(|c| !c.trim().is_empty()) {
        return SshProgram::Shell(command);
    }
    if let Some(program) = env("GIT_SSH").filter(|p| !p.is_empty()) {
        return SshProgram::Exe(program);
    }
    if let Ok(Some(command)) = git_lfs_git::config::get_effective(cwd, "core.sshCommand")
        && !command.trim().is_empty()
    {
        return SshProgram::Shell(command);
    }
    SshProgram::default()
}

/// Adapts [`SshAuthClient`] to the API client's [`SshResolver`] trait for
/// one SSH endpoint.
struct SshAuthAdapter {
    client: Arc<SshAuthClient>,
    ssh: SshInfo,
}

impl SshResolver for SshAuthAdapter {
    fn resolve(&self, op: ApiSshOperation) -> Result<ApiSshAuth, git_lfs_api::ApiError> {
        let creds_op = match op {
            ApiSshOperation::Upload => CredsSshOperation::Upload,
            ApiSshOperation::Download => CredsSshOperation::Download,
        };
        let resolved = match self.client.resolve(
            &self.ssh.user_and_host,
            self.ssh.port.as_deref(),
            &self.ssh.path,
            creds_op,
        ) {
            Ok(resolved) => resolved,
            // The server discovery spec says to use the guessed endpoint,
            // which an empty href keeps.
            Err(SshAuthError::Unavailable(_)) => {
                ssh_trace(format_args!(
                    "ssh: {} does not provide git-lfs-authenticate, falling back to guessed LFS endpoint",
                    self.ssh.user_and_host
                ));
                return Ok(ApiSshAuth {
                    href: String::new(),
                    headers: HashMap::new(),
                });
            }
            Err(e) => {
                return Err(git_lfs_api::ApiError::Decode(format!(
                    "ssh git-lfs-authenticate: {e}"
                )));
            }
        };
        Ok(ApiSshAuth {
            href: resolved.href,
            headers: resolved.header,
        })
    }
}

/// Remove the user info from `url` and return it as [`Auth::Basic`].
///
/// A username without a password gives an empty password. Returns
/// [`Auth::None`] if the URL has no user info.
fn take_url_basic_auth(url: &mut url::Url) -> Auth {
    let user = url.username().to_owned();
    let pass = url.password().map(str::to_owned).unwrap_or_default();
    if user.is_empty() && pass.is_empty() {
        return Auth::None;
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Auth::Basic {
        username: user,
        password: pass,
    }
}

/// Build the [`TransferConfig`] from git config.
///
/// With `lfs.transfer.enablehrefrewrite`, action URLs are rewritten by
/// `url.<base>.insteadOf`, and uploads first by `pushInsteadOf`. The rules
/// are loaded once here, not per URL. Also reads `lfs.transfer.batchSize`,
/// `lfs.concurrenttransfers`, `lfs.transfer.maxverifies`, and
/// `lfs.<endpoint_url>.contenttype`.
/// A setting that must be a positive number. As in upstream, any other
/// value counts as unset, so the default applies.
fn positive_setting(cwd: &Path, key: &str) -> Option<u32> {
    let raw = git_lfs_git::config::get_effective(cwd, key).ok()??;
    raw.trim().parse::<u32>().ok().filter(|n| *n > 0)
}

/// `lfs.transfer.<url>.httpDownloadEncoding`, looked up for each download
/// URL.
///
/// `None` when no such setting exists, so downloads don't each run
/// `git config` just to find nothing.
fn download_encoding_source(cwd: &Path) -> Option<git_lfs_transfer::UrlSetting> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "config",
            "--includes",
            "--name-only",
            "--get-regexp",
            r"^lfs\.transfer\.(.*\.)?httpdownloadencoding$",
        ])
        .output()
        .ok()?;
    if out.stdout.is_empty() {
        return None;
    }
    let cwd = cwd.to_path_buf();
    Some(Arc::new(move |url: &str| {
        git_lfs_git::url_value(&cwd, "lfs.transfer", url, "httpdownloadencoding")
    }))
}

/// Whether the boolean setting `key` is true, or `default` if it's unset.
fn config_true(cwd: &Path, key: &str, default: bool) -> bool {
    match git_lfs_git::config::get_effective(cwd, key) {
        Ok(Some(v)) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off" | ""
        ),
        _ => default,
    }
}

/// The name of the built-in standalone adapter for `file://` URLs.
const STANDALONE_FILE: &str = "lfs-standalone-file";

/// The standalone transfer agent for `endpoint_url`, if any.
///
/// It's `lfs.<url>.standaloneTransferAgent`, or `lfs-standalone-file` for
/// a `file://` URL. As in upstream, an agent that isn't a custom adapter
/// for `operation` is ignored, with a trace line.
fn standalone_agent(
    cwd: &Path,
    endpoint_url: Option<&str>,
    operation: EndpointOperation,
    customs: &[CustomAdapter],
) -> Option<CustomAdapter> {
    let name = match endpoint_url {
        Some(url) => {
            git_lfs_git::lfs_url_value(cwd, url, "standalonetransferagent").or_else(|| {
                url.starts_with("file://")
                    .then(|| STANDALONE_FILE.to_owned())
            })
        }
        None => git_lfs_git::config::get_effective(cwd, "lfs.standalonetransferagent")
            .ok()
            .flatten(),
    }
    .filter(|n| !n.is_empty())?;
    let upload = operation == EndpointOperation::Upload;
    let builtin = (name == STANDALONE_FILE).then(standalone_file_adapter);
    let agent = customs
        .iter()
        .find(|a| a.name == name)
        .cloned()
        .or(builtin)
        .filter(|a| a.handles(upload));
    if agent.is_none() && trace_enabled() {
        eprintln!(
            "standalone agent {name:?} is not a registered custom transfer adapter; ignoring"
        );
    }
    agent
}

/// The built-in `lfs-standalone-file` adapter, which runs
/// `git lfs standalone-file` to copy objects to and from a local
/// repository.
fn standalone_file_adapter() -> CustomAdapter {
    // This binary, so the adapter matches the running version. Upstream
    // runs `git-lfs` from `PATH`.
    let path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "git-lfs".to_owned());
    CustomAdapter {
        name: STANDALONE_FILE.to_owned(),
        path,
        args: "standalone-file".to_owned(),
        concurrent: false,
        download: true,
        upload: true,
    }
}

fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Names of the built-in adapters, which a custom adapter can't replace.
const STANDARD_ADAPTERS: &[&str] = &["basic", "ssh"];

/// The custom transfer adapters with a `lfs.customtransfer.<name>.path`.
///
/// They come from Git's config only. `.lfsconfig` can't set them, since
/// they run commands. One named like a built-in adapter is dropped with
/// upstream's warning, once for each direction it handles.
fn custom_adapters(cwd: &Path) -> Vec<CustomAdapter> {
    // `tus` is built in only when it's enabled, as in upstream.
    let tus = config_true(cwd, "lfs.tustransfers", false);
    let (adapters, conflicts): (Vec<_>, Vec<_>) = configured_adapters(cwd)
        .into_iter()
        .partition(|a| !STANDARD_ADAPTERS.contains(&a.name.as_str()) && !(tus && a.name == "tus"));
    for a in conflicts {
        for (dir, handled) in [("download", a.download), ("upload", a.upload)] {
            if handled {
                eprintln!(
                    "warning: custom {dir} transfer adapter {:?} ignored due to conflict with standard adapter",
                    a.name
                );
            }
        }
    }
    adapters
}

/// Every adapter in `lfs.customtransfer.<name>.*`, including ones named
/// like a built-in adapter.
pub(crate) fn configured_adapters(cwd: &Path) -> Vec<CustomAdapter> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "config",
            "--includes",
            "--null",
            "--get-regexp",
            r"^lfs\.[cC][uU][sS][tT][oO][mM][tT][rR][aA][nN][sS][fF][eE][rR]\..*\.path$",
        ])
        .output()
    else {
        return Vec::new();
    };
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut adapters: Vec<CustomAdapter> = Vec::new();
    for entry in listing.split('\0') {
        let Some((key, path)) = entry.split_once('\n') else {
            continue;
        };
        // `customtransfer` is part of the subsection, so Git keeps its
        // case, and any case matches, as in upstream. The name can't
        // contain a dot.
        let Some((section, name)) = key
            .strip_prefix("lfs.")
            .and_then(|k| k.strip_suffix(".path"))
            .and_then(|k| k.split_once('.'))
            .filter(|(s, n)| {
                s.eq_ignore_ascii_case("customtransfer") && !n.is_empty() && !n.contains('.')
            })
        else {
            continue;
        };
        let get = |prop: &str| {
            git_lfs_git::config::get_effective(cwd, &format!("lfs.{section}.{name}.{prop}"))
                .ok()
                .flatten()
        };
        let direction = get("direction")
            .map(|d| d.to_ascii_lowercase())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "both".to_owned());
        let adapter = CustomAdapter {
            name: name.to_owned(),
            path: path.to_owned(),
            args: get("args").unwrap_or_default(),
            concurrent: config_true(cwd, &format!("lfs.{section}.{name}.concurrent"), true),
            download: matches!(direction.as_str(), "download" | "both"),
            upload: matches!(direction.as_str(), "upload" | "both"),
        };
        // A later setting for the same name wins.
        adapters.retain(|a| a.name != adapter.name);
        adapters.push(adapter);
    }
    adapters
}

/// The `http.extraHeader` settings, looked up for each request's URL.
fn extra_header_source(cwd: &Path) -> git_lfs_api::HeaderSource {
    let headers = git_lfs_git::ExtraHeaders::load(cwd);
    Arc::new(move |url: &str| headers.for_url(url))
}

fn transfer_config_for(cwd: &Path, endpoint_url: Option<&str>) -> TransferConfig {
    let mut config = TransferConfig {
        extra_headers: Some(extra_header_source(cwd)),
        download_encoding: download_encoding_source(cwd),
        custom_adapters: custom_adapters(cwd),
        basic_transfers_only: config_true(cwd, "lfs.basictransfersonly", false),
        tus: config_true(cwd, "lfs.tustransfers", false),
        ..TransferConfig::default()
    };
    if href_rewrite_enabled(cwd) {
        let aliases = git_lfs_git::aliases::load_aliases(cwd).unwrap_or_default();
        let push_aliases = git_lfs_git::aliases::load_push_aliases(cwd).unwrap_or_default();
        if !aliases.is_empty() {
            let download_aliases = aliases.clone();
            config.url_rewriter = Some(Arc::new(move |url: &str| {
                git_lfs_git::aliases::apply(&download_aliases, url)
            }));
        }
        if !push_aliases.is_empty() {
            // Upload and verify URLs try pushInsteadOf first, then
            // insteadOf.
            let upload_push = push_aliases;
            let upload_fallback = aliases;
            config.upload_url_rewriter = Some(Arc::new(move |url: &str| {
                let rewritten = git_lfs_git::aliases::apply(&upload_push, url);
                if rewritten != url {
                    rewritten
                } else {
                    git_lfs_git::aliases::apply(&upload_fallback, url)
                }
            }));
        }
    }
    if let Ok(Some(raw)) = git_lfs_git::config::get_effective(cwd, "lfs.transfer.batchSize")
        && let Ok(n) = raw.trim().parse::<usize>()
        && n > 0
    {
        config.batch_size = n;
    }
    // Without this setting the default of 8 applies. Upstream scales its
    // default with the CPU count.
    if let Ok(Some(raw)) = git_lfs_git::config::get_effective(cwd, "lfs.concurrenttransfers")
        && let Ok(n) = raw.trim().parse::<usize>()
        && n > 0
    {
        config.concurrency = n;
    }
    // Values below the default of 3 are ignored, like upstream's
    // `max(defaultMaxVerifyAttempts, mv)` in `tq/verify.go`.
    if let Ok(Some(raw)) = git_lfs_git::config::get_effective(cwd, "lfs.transfer.maxverifies")
        && let Ok(n) = raw.trim().parse::<u32>()
        && n >= 3
    {
        config.max_verifies = n;
    }
    if let Some(n) = positive_setting(cwd, "lfs.transfer.maxretries") {
        config.max_attempts = n.saturating_add(1);
    }
    if let Some(n) = positive_setting(cwd, "lfs.transfer.maxretrydelay") {
        config.backoff_max = Duration::from_secs(n.into());
    }
    if let Some(n) = positive_setting(cwd, "lfs.transfer.maxretrytime") {
        config.max_retry_time = Duration::from_secs(n.into());
    }
    // Defaults to true. Without an endpoint only `lfs.contenttype` is read.
    config.detect_content_type = match endpoint_url {
        Some(u) => git_lfs_git::lfs_url_bool(cwd, u, "contenttype", true),
        None => match git_lfs_git::config::get_effective(cwd, "lfs.contenttype") {
            Ok(Some(v)) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            ),
            _ => true,
        },
    };
    config
}

/// Read `lfs.transfer.enablehrefrewrite`, default false.
fn href_rewrite_enabled(cwd: &Path) -> bool {
    let raw = git_lfs_git::config::get_effective(cwd, "lfs.transfer.enablehrefrewrite")
        .ok()
        .flatten()
        .unwrap_or_default();
    matches!(
        raw.to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// Build the credential helper chain: netrc, the in-process cache, askpass,
/// then `git credential`.
///
/// This is upstream's order. netrc is only added if the user's netrc file
/// exists. It comes before the cache, so its trace line prints on every
/// request. Askpass is only added if a program is configured and no
/// `credential.helper` matches `cred_url`. `credential.protectProtocol`
/// (default true) is passed on to `git credential`.
pub(crate) fn default_helper_chain(cwd: &Path, cred_url: &url::Url) -> Arc<dyn Helper> {
    let protect_protocol = read_bool_default(cwd, "credential.protectProtocol", true);
    let mut helpers: Vec<Box<dyn Helper>> = Vec::new();
    if let Some(netrc) = NetrcCredentialHelper::from_default_location() {
        helpers.push(Box::new(netrc));
    }
    helpers.push(Box::new(CachingHelper::new()));
    if let Some(askpass) = resolve_askpass_program(cwd)
        && !has_credential_helper(cwd, cred_url)
    {
        helpers.push(Box::new(AskpassHelper::new(askpass)));
    }
    helpers.push(Box::new(
        GitCredentialHelper::new().with_protect_protocol(protect_protocol),
    ));
    Arc::new(HelperChain::new(helpers))
}

/// Pick the askpass program: `GIT_ASKPASS`, then `core.askpass`, then
/// `SSH_ASKPASS`, as upstream does.
///
/// The first non-empty value wins.
fn resolve_askpass_program(cwd: &Path) -> Option<String> {
    if let Some(v) = std::env::var_os("GIT_ASKPASS")
        && !v.is_empty()
    {
        return Some(v.to_string_lossy().into_owned());
    }
    if let Ok(Some(v)) = git_lfs_git::config::get_effective(cwd, "core.askpass")
        && !v.trim().is_empty()
    {
        return Some(v.trim().to_owned());
    }
    if let Some(v) = std::env::var_os("SSH_ASKPASS")
        && !v.is_empty()
    {
        return Some(v.to_string_lossy().into_owned());
    }
    None
}

/// Read a bool from `credential.<url>.<suffix>`, falling back to
/// `credential.<suffix>`.
///
/// Tries the URL with its path, then without, then the bare key. The first
/// valid value wins. Upstream's `urlConfig` also tries shorter paths,
/// wildcard hosts, and URLs with a username. This doesn't.
fn url_scoped_credential_bool(
    cwd: &Path,
    cred_url: &url::Url,
    suffix: &str,
    default: bool,
) -> bool {
    let mut keys = Vec::with_capacity(3);
    let host_authority = match (cred_url.host_str(), cred_url.port()) {
        (Some(h), Some(p)) => Some(format!("{}://{h}:{p}", cred_url.scheme())),
        (Some(h), None) => Some(format!("{}://{h}", cred_url.scheme())),
        _ => None,
    };
    if let Some(h) = &host_authority {
        if !cred_url.path().is_empty() && cred_url.path() != "/" {
            keys.push(format!(
                "credential.{}{}.{suffix}",
                h,
                cred_url.path().trim_end_matches('/'),
            ));
        }
        keys.push(format!("credential.{h}.{suffix}"));
    }
    keys.push(format!("credential.{suffix}"));
    for key in &keys {
        let Ok(Some(raw)) = git_lfs_git::config::get_effective(cwd, key) else {
            continue;
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => return true,
            "false" | "0" | "no" | "off" => return false,
            _ => continue,
        }
    }
    default
}

/// Whether a non-empty `credential.helper` is set for `cred_url`.
///
/// Tries the same keys as [`url_scoped_credential_bool`], with the same
/// limits compared to upstream.
fn has_credential_helper(cwd: &Path, cred_url: &url::Url) -> bool {
    let mut keys = Vec::with_capacity(3);
    let host_authority = match (cred_url.host_str(), cred_url.port()) {
        (Some(h), Some(p)) => Some(format!("{}://{h}:{p}", cred_url.scheme())),
        (Some(h), None) => Some(format!("{}://{h}", cred_url.scheme())),
        _ => None,
    };
    if let Some(h) = &host_authority {
        if !cred_url.path().is_empty() && cred_url.path() != "/" {
            keys.push(format!(
                "credential.{}{}.helper",
                h,
                cred_url.path().trim_end_matches('/')
            ));
        }
        keys.push(format!("credential.{h}.helper"));
    }
    keys.push("credential.helper".to_string());
    for key in &keys {
        if let Ok(Some(v)) = git_lfs_git::config::get_effective(cwd, key)
            && !v.trim().is_empty()
        {
            return true;
        }
    }
    false
}

/// Read a boolean config value, or `default` if it's unset or not one of
/// true, false, yes, no, on, off, 1, or 0.
fn read_bool_default(cwd: &Path, key: &str, default: bool) -> bool {
    let Ok(Some(raw)) = git_lfs_git::config::get_effective(cwd, key) else {
        return default;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with(settings: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        for (key, value) in settings {
            git(&["config", key, value]);
        }
        dir
    }

    #[test]
    fn retry_settings_are_read() {
        let repo = repo_with(&[
            ("lfs.transfer.maxRetries", "3"),
            ("lfs.transfer.maxRetryDelay", "2"),
            ("lfs.transfer.maxRetryTime", "5"),
        ]);
        let config = transfer_config_for(repo.path(), None);
        assert_eq!(config.max_attempts, 4);
        assert_eq!(config.backoff_max, Duration::from_secs(2));
        assert_eq!(config.max_retry_time, Duration::from_secs(5));
    }

    #[test]
    fn retry_settings_below_one_keep_the_defaults() {
        let repo = repo_with(&[
            ("lfs.transfer.maxRetries", "0"),
            ("lfs.transfer.maxRetryDelay", "-1"),
            ("lfs.transfer.maxRetryTime", "0"),
        ]);
        let config = transfer_config_for(repo.path(), None);
        let default = TransferConfig::default();
        assert_eq!(config.max_attempts, default.max_attempts);
        assert_eq!(config.backoff_max, default.backoff_max);
        assert_eq!(config.max_retry_time, default.max_retry_time);
    }
}
