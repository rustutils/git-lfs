//! Transport backend the transfer queue dispatches through.
//!
//! Two variants today:
//!
//! - [`Backend::Http`] — classic HTTP path: batch over the LFS API
//!   client, per-object via `reqwest::Client` action URLs. The
//!   only choice when the endpoint isn't SSH-shaped.
//! - [`Backend::Ssh`] — pure-SSH transfer over a connection
//!   [`Pool`]. Lazy per-direction: the pool for each operation
//!   (download/upload) spawns on first use, so an `LfsFetcher`
//!   that ends up only doing downloads never pays for an
//!   upload-session SSH handshake. Carries an optional HTTP
//!   fallback for negotiate mode — when pool spawn fails (e.g.
//!   the remote doesn't have `git-lfs-transfer` installed), the
//!   operation transparently reroutes through HTTP.

use std::sync::{Arc, Mutex};

use git_lfs_api::{Client as ApiClient, Operation};

use crate::sshtransfer::connection::ConnectionError;
use crate::sshtransfer::pool::Pool;

/// Transport backend the queue uses for batch + per-object transfers.
///
/// Cloning is cheap — `reqwest::Client` is `Arc`-backed internally
/// and `Arc<SshBackend>` is a pointer copy. The HTTP variant boxes
/// `ApiClient` so the enum doesn't bloat to the largest variant's
/// size (clippy's `large_enum_variant` gate).
#[derive(Clone)]
pub enum Backend {
    /// HTTP backend: batch over `api`, per-object via `http`.
    Http {
        /// LFS batch API client.
        api: Box<ApiClient>,
        /// `reqwest::Client` for action-URL transfers.
        http: reqwest::Client,
    },
    /// Pure-SSH backend (with optional HTTP fallback). See [`SshBackend`].
    Ssh(Arc<SshBackend>),
}

/// HTTP transport used either as the primary `Backend::Http` or as
/// the negotiate-mode fallback inside `Backend::Ssh`.
#[derive(Clone)]
pub struct HttpTransport {
    /// LFS batch API client.
    pub api: Box<ApiClient>,
    /// `reqwest::Client` for action-URL transfers.
    pub http: reqwest::Client,
}

/// Closure that spawns a fresh [`Pool`] for one operation
/// direction. Stored in `Arc<dyn Fn>` so the resolved program /
/// variant / metadata only live once even when the backend is
/// cloned across many concurrent tasks.
pub type PoolSpawner = Arc<dyn Fn(Operation) -> Result<Arc<Pool>, ConnectionError> + Send + Sync>;

/// Pure-SSH backend with lazy per-direction pool and optional
/// HTTP fallback for negotiate mode.
pub struct SshBackend {
    /// Closure called at most once per direction to bring up an
    /// SSH connection pool. Errors propagate through
    /// [`dispatch`](Self::dispatch) — if `fallback` is `Some`, the
    /// caller transparently falls back; if `None`, the error
    /// surfaces as `Transport::NoFallback`.
    spawner: PoolSpawner,
    /// HTTP fallback for negotiate mode. `None` means the user
    /// opted into `sshtransfer=always` — pure-SSH only, no
    /// fallback (failure surfaces as an error to the user).
    fallback: Option<HttpTransport>,
    /// Lazy transport for download direction.
    download: Mutex<TransportState>,
    /// Lazy transport for upload direction.
    upload: Mutex<TransportState>,
}

/// Per-direction lazy state.
///
/// First [`SshBackend::dispatch`] call for a direction transitions
/// `Untried` → `Pool` on successful spawn, or `Untried` → `Fallback`
/// when the spawn failed and `fallback` was configured. Subsequent
/// calls reuse the cached choice without re-attempting the spawn.
enum TransportState {
    Untried,
    Pool(Arc<Pool>),
    Fallback,
}

impl SshBackend {
    /// Build an SSH backend with the given spawner and optional
    /// HTTP fallback.
    ///
    /// Pass `fallback = Some(_)` for negotiate mode (try pure-SSH
    /// first, fall back to HTTP on spawn failure); pass `None` for
    /// `sshtransfer=always` mode (no fallback, pool spawn errors
    /// surface to the caller).
    pub fn new(spawner: PoolSpawner, fallback: Option<HttpTransport>) -> Self {
        Self {
            spawner,
            fallback,
            download: Mutex::new(TransportState::Untried),
            upload: Mutex::new(TransportState::Untried),
        }
    }

    /// Pick the transport for `op`, spawning the pool on first use
    /// and caching the choice for subsequent calls. Holds the
    /// per-direction lock for the duration of the spawn so two
    /// concurrent first-callers don't race.
    pub fn dispatch(&self, op: Operation) -> Transport {
        let slot = match op {
            Operation::Download => &self.download,
            Operation::Upload => &self.upload,
        };
        let mut state = slot.lock().unwrap();
        match &*state {
            TransportState::Pool(p) => return Transport::Pool(p.clone()),
            TransportState::Fallback => {
                let http = self
                    .fallback
                    .as_ref()
                    .expect("Fallback state requires fallback to be Some");
                return Transport::Http(http.clone());
            }
            TransportState::Untried => {}
        }

        // First call for this direction — try to spawn.
        match (self.spawner)(op) {
            Ok(pool) => {
                *state = TransportState::Pool(pool.clone());
                Transport::Pool(pool)
            }
            Err(e) => match &self.fallback {
                Some(http) => {
                    *state = TransportState::Fallback;
                    Transport::Http(http.clone())
                }
                None => Transport::NoFallback(e),
            },
        }
    }
}

/// One concrete transport choice made by [`SshBackend::dispatch`].
pub enum Transport {
    /// Use the SSH connection pool.
    Pool(Arc<Pool>),
    /// Use the HTTP fallback (only emitted in negotiate mode).
    Http(HttpTransport),
    /// Pool spawn failed and no fallback was configured.
    NoFallback(ConnectionError),
}
