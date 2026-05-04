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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use git_lfs_api::{
    Auth, BatchRequest, Client as ApiClient, ObjectSpec, Operation, Ref, SharedSshResolver,
    SshAuth as ApiSshAuth, SshOperation as ApiSshOperation, SshResolver,
};
use git_lfs_creds::{
    AskpassHelper, CachingHelper, GitCredentialHelper, Helper, HelperChain, SshAuthClient,
    SshOperation as CredsSshOperation,
};
use git_lfs_filter::FetchError;
use git_lfs_git::ConfigScope;
use git_lfs_git::SshInfo;
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
    /// API client used directly for `check_server_has` (so push can ask
    /// the server which "missing locally" objects the server already
    /// holds before deciding whether to fail). Same client the
    /// `Transfer` uses for its batch calls.
    api: Result<ApiClient, String>,
    /// Refspec sent on every batch request. Resolved once at
    /// construction (current branch's tracked upstream, falling back
    /// to the current branch's full ref). Required by servers that
    /// scope LFS operations per-branch — without it, branch-required
    /// repos return `403 Expected ref ..., got ""` to push/pull.
    refspec: Option<Ref>,
    /// Repo cwd captured at construction. Used to persist
    /// `lfs.<url>.access` after a successful authenticated request,
    /// so future `git lfs env` runs (and the cred-prompting flow)
    /// know basic auth is in play.
    cwd: PathBuf,
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
        let endpoint_url = git_lfs_git::endpoint_for_remote(cwd, remote);
        // Even for SSH endpoints we want the http client configured for
        // the *eventual* HTTPS host — proxy/CA settings index by URL.
        // For SSH-shaped endpoints, the resolver will replace the URL
        // per request via `href`; the endpoint string we pass here is
        // just the bootstrap. Pass the raw value — `http_client::build`
        // tolerates non-HTTP schemes.
        let http = endpoint_url
            .as_ref()
            .map(|u| crate::http_client::build(cwd, u))
            .unwrap_or_default();
        let api = build_api_client_with(cwd, remote, http.clone());
        let config = transfer_config_for(cwd);
        let transfer = api
            .clone()
            .map(|c| Transfer::with_http_client(c, store.clone(), config, http));
        let refspec = git_lfs_git::refs::current_refspec(cwd).map(Ref::new);
        Ok(Self {
            runtime,
            transfer,
            api,
            refspec,
            cwd: cwd.to_path_buf(),
        })
    }

    /// If our most recent operation authenticated via HTTP Basic,
    /// persist `lfs.<url>.access = basic` to local git config so
    /// future `git lfs env` runs render `(auth=basic)` and the
    /// cred-prompting flow knows to fill upfront. No-op when the
    /// endpoint isn't resolvable, no auth was used, or the value is
    /// already set. Best-effort — config writes that fail are
    /// swallowed (we'd rather complete the user's operation than
    /// abort it on a bookkeeping write).
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

    /// Borrow the inner API client (e.g. for callers that want to
    /// run a one-off batch without going through `download_many`).
    /// Errors lazily — same `lfs.url` resolution as the transfer
    /// path.
    pub fn api_client(&self) -> Result<&ApiClient, FetchError> {
        self.api
            .as_ref()
            .map_err(|m| -> FetchError { m.clone().into() })
    }

    /// Drive an arbitrary future on the fetcher's tokio runtime. Used
    /// by callers (like fetch's --json --dry-run path) that need to
    /// hit the API directly while keeping the fetcher's runtime ownership.
    pub fn runtime_block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.runtime.block_on(fut)
    }

    /// Run the pre-flight `/locks/verify` check before push, returning
    /// an [`Outcome`](crate::locks_verify::Outcome) the caller acts on.
    /// Threads the fetcher's runtime + api client through to
    /// `locks_verify`.
    pub fn preflight_verify_locks(
        &self,
        cwd: &Path,
        remote_label: &str,
        endpoint: &str,
    ) -> Result<crate::locks_verify::Outcome, crate::push::PushCommandError> {
        let api = self
            .api
            .as_ref()
            .map_err(|m| -> FetchError { m.clone().into() })
            .map_err(crate::push::PushCommandError::Fetch)?;
        crate::locks_verify::run(
            &self.runtime,
            api,
            cwd,
            remote_label,
            endpoint,
            self.refspec.as_ref(),
        )
    }

    /// Ask the server which of the given OIDs it already holds. Used by
    /// `push` / `pre-push` to distinguish "missing locally but server
    /// has it" (silent skip — see `lfs.allowincompletepush`) from
    /// "missing both places" (the failure case).
    ///
    /// Sends one upload-direction batch and returns the OIDs that came
    /// back with neither `actions` nor `error` (the spec's no-op
    /// signal — server already has the object).
    pub fn check_server_has(&self, specs: Vec<ObjectSpec>) -> Result<HashSet<String>, FetchError> {
        if specs.is_empty() {
            return Ok(HashSet::new());
        }
        let api = self
            .api
            .as_ref()
            .map_err(|m| -> FetchError { m.clone().into() })?;
        let mut req = BatchRequest::new(Operation::Upload, specs);
        if let Some(r) = self.refspec.clone() {
            req = req.with_ref(r);
        }
        let resp = self
            .runtime
            .block_on(api.batch(&req))
            .map_err(|e| -> FetchError { e.to_string().into() })?;
        Ok(resp
            .objects
            .into_iter()
            .filter(|o| o.actions.is_none() && o.error.is_none())
            .map(|o| o.oid)
            .collect())
    }

    fn transfer(&self) -> Result<&Transfer, FetchError> {
        self.transfer
            .as_ref()
            .map_err(|msg| -> FetchError { msg.clone().into() })
    }
}

/// Build an [`ApiClient`] for `cwd`+`remote` with our standard credential
/// helper chain attached. Used both by [`LfsFetcher`] (for transfers) and
/// by the locking commands (no transfers, just JSON requests).
pub fn build_api_client(cwd: &Path, remote: Option<&str>) -> Result<ApiClient, String> {
    let endpoint = git_lfs_git::endpoint_for_remote(cwd, remote)
        .map_err(|e| format!("resolving LFS endpoint: {e}"))?;
    let http = crate::http_client::build(cwd, &endpoint);
    build_api_client_with(cwd, remote, http)
}

fn build_api_client_with(
    cwd: &Path,
    remote: Option<&str>,
    http: reqwest::Client,
) -> Result<ApiClient, String> {
    let info = git_lfs_git::resolve_endpoint(cwd, remote)
        .map_err(|e| format!("resolving LFS endpoint: {e}"))?;
    // For SSH-shaped endpoints, the configured URL might be `ssh://...`
    // (e.g. `lfs.url = ssh://git@host/repo`). reqwest needs an http(s)
    // base URL; the SSH resolver's `href` will replace it on each
    // request, but we still need a parseable bootstrap value here.
    let endpoint =
        http_compatible_endpoint(&info.url).map_err(|e| format!("invalid LFS endpoint: {e}"))?;
    let mut url = url::Url::parse(&endpoint).map_err(|e| format!("invalid LFS endpoint: {e}"))?;
    // Extract `user:pass@` from the endpoint and use it as the initial
    // auth so we don't have to round-trip through 401 → fill on every
    // first request. Mirrors upstream's `setRequestAuthWithCreds` for
    // URL-embedded credentials. Strip the user info from the URL we
    // pass to reqwest so it doesn't double-apply.
    let initial_auth = take_url_basic_auth(&mut url);
    let use_http_path = read_bool_default(cwd, "credential.useHttpPath", false);
    // Resolve the credential URL up front (git remote URL when it
    // shares scheme+host with the LFS endpoint, else the LFS endpoint
    // itself). The helper chain consults it for URL-specific
    // `credential.<url>.helper` lookup, and the API client uses it for
    // prompts and "Git credentials for X not found" wording.
    let resolved_cred_url = remote
        .and_then(|r| git_lfs_git::remote_url(cwd, r).ok().flatten())
        .and_then(|raw| url::Url::parse(&raw).ok())
        .filter(|gu| {
            gu.scheme() == url.scheme()
                && gu.host_str() == url.host_str()
                && gu.port() == url.port()
        });
    let cred_url_for_helper = resolved_cred_url.clone().unwrap_or_else(|| url.clone());
    let mut client = ApiClient::with_http_client(url.clone(), initial_auth, http)
        .with_credential_helper(default_helper_chain(cwd, &cred_url_for_helper))
        .with_use_http_path(use_http_path);
    if let Some(git_url) = resolved_cred_url {
        client = client.with_cred_url(git_url);
    }
    if let Some(ssh_info) = info.ssh {
        // `lfs.<url>.sshtransfer` (with `lfs.sshtransfer` fallback) gates
        // the pure-SSH transfer protocol (`git-lfs-transfer`). We don't
        // implement that yet, so:
        //   - `always`: the user explicitly asked for the unimplemented
        //     path → fail loudly with upstream's exact wording so
        //     scripts that opted in get a clear signal (and the shell
        //     test grep matches).
        //   - `never`: the user wants pure-SSH off; we honor that
        //     trivially (we already only do `git-lfs-authenticate`),
        //     and emit the trace line upstream prints for parity.
        //   - unset / other: default behavior — just use
        //     `git-lfs-authenticate`.
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
                client = client.with_ssh_resolver(build_ssh_resolver(ssh_info));
            }
            _ => {
                client = client.with_ssh_resolver(build_ssh_resolver(ssh_info));
            }
        }
    }
    Ok(client)
}

/// Read `lfs.<endpoint>.sshtransfer` falling back to `lfs.sshtransfer`.
/// Lowercased so callers can pattern-match on `"always" / "never"`.
fn read_sshtransfer_for(cwd: &Path, endpoint: &str) -> Option<String> {
    let endpoint_key = format!("lfs.{endpoint}.sshtransfer");
    if let Ok(Some(v)) = git_lfs_git::config::get_effective(cwd, &endpoint_key) {
        return Some(v.trim().to_ascii_lowercase());
    }
    if let Ok(Some(v)) = git_lfs_git::config::get_effective(cwd, "lfs.sshtransfer") {
        return Some(v.trim().to_ascii_lowercase());
    }
    None
}

/// Emit one stderr trace line, gated on `GIT_TRACE` (matches the
/// `creds/src/ssh.rs` trace gate). Used for the `sshtransfer=always` /
/// `sshtransfer=never` notes the upstream test suite greps for.
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

/// Resolver installed when `lfs.<url>.sshtransfer=always` and we don't
/// implement pure-SSH transfer: refuses every request with upstream's
/// "git-lfs-authenticate has been disabled by request" wording so the
/// failure surfaces consistently regardless of which API method the
/// caller hit.
struct DisabledSshResolver;

impl SshResolver for DisabledSshResolver {
    fn resolve(&self, _op: ApiSshOperation) -> Result<ApiSshAuth, git_lfs_api::ApiError> {
        Err(git_lfs_api::ApiError::Decode(
            "git-lfs-authenticate has been disabled by request".into(),
        ))
    }
}

/// Convert an LFS endpoint string into something reqwest can use as a
/// base URL. SSH-style schemes (`ssh://...`, bare `git@host:repo`, …)
/// are run through [`derive_lfs_url`] to get the matching HTTPS form;
/// HTTP/HTTPS pass through verbatim. The SSH resolver overrides this
/// per-request when it returns a non-empty `href`.
fn http_compatible_endpoint(url_str: &str) -> Result<String, git_lfs_git::EndpointError> {
    if url_str.starts_with("http://") || url_str.starts_with("https://") {
        return Ok(url_str.to_owned());
    }
    git_lfs_git::derive_lfs_url(url_str)
}

/// Construct an [`SshResolver`] for the given SSH endpoint metadata,
/// using `GIT_SSH_COMMAND` / `GIT_SSH` / `ssh` for the executable in
/// upstream's selection order.
fn build_ssh_resolver(info: SshInfo) -> SharedSshResolver {
    let program = resolve_ssh_program();
    Arc::new(SshAuthAdapter {
        client: Arc::new(SshAuthClient::new(program)),
        ssh: info,
    })
}

/// Pick the SSH executable per upstream's order:
/// `GIT_SSH_COMMAND` (full command line, parsed by SshAuthClient) >
/// `GIT_SSH` (single program path) > literal `ssh` on `$PATH`.
/// `core.sshCommand` is upstream's fallback before `GIT_SSH`; we don't
/// honor it yet — see NOTES.md.
fn resolve_ssh_program() -> String {
    if let Some(v) = std::env::var_os("GIT_SSH_COMMAND")
        && !v.is_empty()
    {
        return v.to_string_lossy().into_owned();
    }
    if let Some(v) = std::env::var_os("GIT_SSH")
        && !v.is_empty()
    {
        return v.to_string_lossy().into_owned();
    }
    "ssh".to_owned()
}

/// Bridge between `git_lfs_creds::SshAuthClient` (which knows how to
/// spawn ssh and parse responses) and `git_lfs_api::SshResolver` (the
/// trait the API client calls into). Holds the per-endpoint metadata
/// the SSH command needs (`user@host`, port, path) and the operation
/// translation between the two crates' enums.
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
        let resolved = self
            .client
            .resolve(
                &self.ssh.user_and_host,
                self.ssh.port.as_deref(),
                &self.ssh.path,
                creds_op,
            )
            .map_err(|e| git_lfs_api::ApiError::Decode(format!("ssh git-lfs-authenticate: {e}")))?;
        Ok(ApiSshAuth {
            href: resolved.href,
            headers: resolved.header,
        })
    }
}

/// Pull `user:pass@` (or `user@`, with empty password) out of `url` and
/// turn it into an [`Auth::Basic`]. Strips both fields from the URL so
/// downstream callers see the bare endpoint. Returns [`Auth::None`]
/// when there's no user info to extract.
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

/// Build a [`TransferConfig`] for `cwd`, plumbing
/// `lfs.transfer.enablehrefrewrite` + `url.<base>.insteadOf` into
/// `url_rewriter` when the flag is enabled. The rewrite map is captured
/// at construction so the queue's hot path doesn't shell out to git for
/// every action URL.
fn transfer_config_for(cwd: &Path) -> TransferConfig {
    let mut config = TransferConfig::default();
    if href_rewrite_enabled(cwd)
        && let Ok(aliases) = git_lfs_git::aliases::load_aliases(cwd)
        && !aliases.is_empty()
    {
        config.url_rewriter = Some(Arc::new(move |url: &str| {
            git_lfs_git::aliases::apply(&aliases, url)
        }));
    }
    if let Ok(Some(raw)) = git_lfs_git::config::get_effective(cwd, "lfs.transfer.batchSize")
        && let Ok(n) = raw.trim().parse::<usize>()
        && n > 0
    {
        config.batch_size = n;
    }
    config
}

/// Read `lfs.transfer.enablehrefrewrite` from the effective config
/// (default false). Accepts the standard git-bool spellings.
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

/// Default credential resolution chain: in-process cache → optional
/// askpass → `git credential`. Cache is consulted first so a single CLI
/// invocation only shells out once per host. Reads
/// `credential.protectProtocol` (default `true`) and threads it into
/// the git-credential helper so CR-bearing URLs can be opted into when
/// the user explicitly does so.
///
/// Askpass is only inserted when:
/// - `GIT_ASKPASS` / `core.askpass` / `SSH_ASKPASS` resolves to a
///   non-empty program (priority in that order, matching upstream's
///   `creds.NewCredentialHelperContext`), AND
/// - `credential.helper` is unset — a configured helper takes
///   precedence over interactive prompting.
fn default_helper_chain(cwd: &Path, cred_url: &url::Url) -> Arc<dyn Helper> {
    let protect_protocol = read_bool_default(cwd, "credential.protectProtocol", true);
    let mut helpers: Vec<Box<dyn Helper>> = vec![Box::new(CachingHelper::new())];
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

/// Resolve the askpass program per upstream's selection order:
/// `GIT_ASKPASS` env > `core.askpass` config > `SSH_ASKPASS` env. First
/// non-empty value wins. Returns `None` when none is configured (which
/// is the typical headless / CI case).
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

/// True if a non-empty `credential.helper` is configured for `cred_url`.
/// Checks the URL-prefix variants in upstream's precedence order
/// (`credential.<scheme>://<host>[:port]/<path>.helper` →
/// `credential.<scheme>://<host>[:port].helper` →
/// `credential.helper`) and returns true on the first non-empty match.
/// When `true`, we skip askpass so a configured helper isn't shadowed
/// by a pop-up prompt. Mirrors upstream's
/// `urlConfig.Get("credential", rawurl, "helper")` lookup.
fn has_credential_helper(cwd: &Path, cred_url: &url::Url) -> bool {
    let mut keys = Vec::with_capacity(3);
    let host_authority = match (cred_url.host_str(), cred_url.port()) {
        (Some(h), Some(p)) => Some(format!("{}://{h}:{p}", cred_url.scheme())),
        (Some(h), None) => Some(format!("{}://{h}", cred_url.scheme())),
        _ => None,
    };
    if let Some(h) = &host_authority {
        // Most-specific: include path component if present.
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

/// Read a git-bool config value, returning `default` when the key is
/// unset or unparseable. Matches git's accepted spellings (true/false,
/// yes/no, on/off, 1/0).
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
