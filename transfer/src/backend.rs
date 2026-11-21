//! Choice between HTTPS and pure-SSH transport for the transfer queue.
//!
//! The SSH backend spawns a connection [`Pool`] for each direction
//! (download or upload) on first use, so a run that only downloads never
//! opens an upload session. With an HTTP fallback configured, a direction
//! whose pool fails to spawn (for example because the remote has no
//! `git-lfs-transfer`) uses HTTPS instead.

use std::sync::{Arc, Mutex};

use git_lfs_api::{Client as ApiClient, Operation};

use crate::sshtransfer::connection::ConnectionError;
use crate::sshtransfer::pool::Pool;

/// Transport the queue uses for batch requests and object transfers.
///
/// The HTTP variant boxes the API client to keep the enum small.
#[derive(Clone)]
pub enum Backend {
    /// Batch requests through `api`, object transfers through `http`.
    Http {
        /// LFS batch API client.
        api: Box<ApiClient>,
        /// `reqwest::Client` for action-URL transfers.
        http: reqwest::Client,
    },
    /// Pure SSH, with an optional HTTP fallback. See [`SshBackend`].
    Ssh(Arc<SshBackend>),
}

/// HTTP clients for one endpoint, used as the fallback of an
/// [`SshBackend`].
#[derive(Clone)]
pub struct HttpTransport {
    /// LFS batch API client.
    pub api: Box<ApiClient>,
    /// `reqwest::Client` for action-URL transfers.
    pub http: reqwest::Client,
}

/// Closure that spawns a fresh [`Pool`] for one direction.
pub type PoolSpawner = Arc<dyn Fn(Operation) -> Result<Arc<Pool>, ConnectionError> + Send + Sync>;

/// Pure-SSH backend with a lazily spawned pool per direction and an
/// optional HTTP fallback.
pub struct SshBackend {
    spawner: PoolSpawner,
    /// `None` when pure SSH is required (`sshtransfer=always`).
    fallback: Option<HttpTransport>,
    download: Mutex<TransportState>,
    upload: Mutex<TransportState>,
}

/// Transport chosen for one direction.
///
/// A successful spawn moves `Untried` to `Pool`. A failed spawn moves it
/// to `Fallback` when there is a fallback, and otherwise leaves it
/// `Untried`, so the next dispatch tries to spawn again.
enum TransportState {
    Untried,
    Pool(Arc<Pool>),
    Fallback,
}

impl SshBackend {
    /// Build an SSH backend from a pool spawner and an optional HTTP
    /// fallback.
    ///
    /// Pass a fallback to try pure SSH first and use HTTP when the pool
    /// fails to spawn (`sshtransfer=negotiate`). Pass `None` to require
    /// pure SSH (`sshtransfer=always`), so spawn errors reach the caller.
    pub fn new(spawner: PoolSpawner, fallback: Option<HttpTransport>) -> Self {
        Self {
            spawner,
            fallback,
            download: Mutex::new(TransportState::Untried),
            upload: Mutex::new(TransportState::Untried),
        }
    }

    /// Pick the transport for `op`, spawning its pool on first use.
    ///
    /// The choice is cached for later calls. The direction's lock is held
    /// during the spawn, so concurrent first calls spawn only one pool.
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

impl Drop for SshBackend {
    /// Quit each direction's pool, best effort.
    ///
    /// This emits the `terminating pure SSH connection` trace lines that
    /// the upstream tests count.
    fn drop(&mut self) {
        for slot in [&self.download, &self.upload] {
            let mut state = match slot.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            if let TransportState::Pool(p) = &*state {
                p.end_all_available();
            }
            *state = TransportState::Untried;
        }
    }
}

/// One concrete transport choice made by [`SshBackend::dispatch`].
pub enum Transport {
    /// Use the SSH connection pool.
    Pool(Arc<Pool>),
    /// Use HTTP: the backend is HTTP, or the SSH pool failed to spawn and
    /// there is a fallback.
    Http(HttpTransport),
    /// Pool spawn failed and no fallback was configured.
    NoFallback(ConnectionError),
}
