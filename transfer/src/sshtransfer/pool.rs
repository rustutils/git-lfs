//! Pool of pure-SSH transfer connections sharing a control socket.
//!
//! One pool instance is bound to a single `(user@host, port, path,
//! operation)` tuple — i.e. one logical transfer session. It owns a
//! [`tempfile::TempDir`] that holds the OpenSSH control socket and a
//! `Vec<SlotState>` of up to `max_size` connections. Slot 0 is the
//! master (created eagerly in [`Pool::new`] so the control socket
//! exists before any client connection tries to use it); slots
//! 1..max_size are spawned lazily on first acquire.
//!
//! Acquire/release is via [`PoolGuard`], which checks out a
//! connection at acquire time and returns it to the pool on drop —
//! or discards it (next acquire on that slot will respawn a fresh
//! one) if the caller signals the connection is in a bad state via
//! [`PoolGuard::discard`].
//!
//! Concurrency model: synchronous (the pool uses `std::sync::Mutex`
//! and `std::process::Command`). The transfer queue is async, so
//! callers wrap pool operations in `tokio::task::spawn_blocking`.
//! The pool itself isn't async-aware — once the queue's semaphore
//! has gated to `max_size` parallel transfers, acquire always finds
//! a free slot without blocking.

use std::path::PathBuf;
use std::sync::Mutex;

use tempfile::TempDir;

use crate::sshtransfer::connection::{
    Config, Connection, ConnectionError, Metadata, Multiplex, Operation, Variant,
};

/// Per-pool configuration. Mirrors [`Config`] but without the
/// connection-sequence number (the pool assigns IDs as it spawns)
/// and the multiplex role (the pool decides master vs. client).
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// SSH executable string (split on whitespace, first token is
    /// the program, rest are pre-args).
    pub program: String,
    /// SSH client variant.
    pub variant: Variant,
    /// Endpoint addressing.
    pub metadata: Metadata,
    /// Transfer operation; same for every connection in the pool.
    pub operation: Operation,
    /// Whether to enable SSH multiplexing for this pool. When
    /// `false` every connection spawns a full independent SSH
    /// session; when `true` (and the variant is OpenSSH) slot 0
    /// creates the control socket and 1.. share it.
    pub multiplex_enabled: bool,
}

/// One slot in the pool. `Vacant` means "never spawned"; the slot
/// transitions to `InUse` while a [`PoolGuard`] holds the
/// connection, and back to `Available` on drop (or `Vacant` if
/// discarded).
enum SlotState {
    Vacant,
    Available(Connection),
    InUse,
}

/// A pool of pure-SSH transfer connections.
///
/// Spawns the master connection eagerly so the control socket
/// exists before any client connection tries to use it. Client
/// connections are spawned lazily on first acquire.
pub struct Pool {
    config: PoolConfig,
    /// Tempdir holding the SSH control socket. `None` when
    /// multiplexing is disabled (variant != OpenSSH, or the user
    /// turned `lfs.ssh.automultiplex` off). Held as long as the
    /// pool lives so the socket file isn't removed out from under
    /// active connections.
    _control_dir: Option<TempDir>,
    /// Path to the control socket inside `_control_dir`. `None`
    /// when multiplexing is disabled.
    control_path: Option<PathBuf>,
    /// Per-slot state. Indexed 0..max_size; len doesn't change
    /// after construction (lazy spawns mutate state, not length).
    slots: Mutex<Vec<SlotState>>,
}

impl Pool {
    /// Build a pool of up to `max_size` connections and spawn the
    /// master (slot 0). Errors propagate the master's spawn or
    /// version-handshake failure.
    pub fn new(config: PoolConfig, max_size: usize) -> Result<Self, ConnectionError> {
        assert!(max_size >= 1, "pool max_size must be at least 1");

        // Set up the control socket dir if multiplexing is enabled
        // for this variant. The dir survives as long as the pool
        // does so the socket file stays put for client connections
        // (the master holds it open, but on some platforms removing
        // the directory while listeners exist trips ENOTEMPTY).
        let (control_dir, control_path) =
            if config.multiplex_enabled && matches!(config.variant, Variant::Default) {
                let dir = TempDir::new()?;
                let path = dir.path().join("lfs.sock");
                (Some(dir), Some(path))
            } else {
                (None, None)
            };

        let master_multiplex = match &control_path {
            Some(path) => Multiplex::Master { path: path.clone() },
            None => Multiplex::Disabled,
        };
        let master = spawn(&config, 0, master_multiplex)?;

        let mut slots = Vec::with_capacity(max_size);
        slots.push(SlotState::Available(master));
        for _ in 1..max_size {
            slots.push(SlotState::Vacant);
        }

        Ok(Self {
            config,
            _control_dir: control_dir,
            control_path,
            slots: Mutex::new(slots),
        })
    }

    /// Number of connection slots this pool manages.
    pub fn capacity(&self) -> usize {
        self.slots.lock().unwrap().len()
    }

    /// Acquire a free connection. Spawns the slot's child SSH
    /// process on first use (slots 1..) sharing the master's
    /// control socket.
    ///
    /// Errors if all slots are currently checked out (the caller
    /// is expected to have rate-limited via a semaphore so this
    /// shouldn't happen in practice) or if a lazy spawn fails.
    pub fn acquire(&self) -> Result<PoolGuard<'_>, ConnectionError> {
        let mut slots = self.slots.lock().unwrap();
        // Prefer Available slots over Vacant ones — reusing an
        // existing connection skips the per-spawn handshake.
        let mut chosen: Option<usize> = None;
        let mut fallback_vacant: Option<usize> = None;
        for (i, slot) in slots.iter().enumerate() {
            match slot {
                SlotState::Available(_) => {
                    chosen = Some(i);
                    break;
                }
                SlotState::Vacant if fallback_vacant.is_none() => {
                    fallback_vacant = Some(i);
                }
                _ => {}
            }
        }
        let slot_idx = chosen
            .or(fallback_vacant)
            .ok_or_else(|| ConnectionError::Protocol("all SSH pool slots are in use".into()))?;

        let connection = match std::mem::replace(&mut slots[slot_idx], SlotState::InUse) {
            SlotState::Available(conn) => conn,
            SlotState::Vacant => {
                // Drop the lock for the spawn — it can take a
                // while and other workers shouldn't block on it.
                drop(slots);
                let multiplex = match &self.control_path {
                    Some(path) => Multiplex::Client { path: path.clone() },
                    None => Multiplex::Disabled,
                };
                match spawn(&self.config, slot_idx as u32, multiplex) {
                    Ok(c) => c,
                    Err(e) => {
                        // Revert the slot back to Vacant so a
                        // future acquire can retry.
                        self.slots.lock().unwrap()[slot_idx] = SlotState::Vacant;
                        return Err(e);
                    }
                }
            }
            SlotState::InUse => unreachable!("InUse slot was selected for acquire"),
        };

        Ok(PoolGuard {
            pool: self,
            slot: slot_idx,
            connection: Some(connection),
        })
    }

    /// Shut down every live connection in the pool by sending
    /// `quit` and waiting for the subprocess. Best-effort: each
    /// connection's End error is returned only if every other
    /// connection's End succeeded; otherwise the first error wins.
    pub fn shutdown(self) -> Result<(), ConnectionError> {
        let mut slots = self
            .slots
            .into_inner()
            .expect("Pool::shutdown lock poisoned");
        let mut first_err: Option<ConnectionError> = None;
        for slot in slots.drain(..) {
            if let SlotState::Available(conn) = slot
                && let Err(e) = conn.end()
                && first_err.is_none()
            {
                first_err = Some(e);
            }
            // InUse at shutdown shouldn't happen — drop a stray
            // PoolGuard before shutting down. Vacant slots have
            // nothing to clean up.
        }
        if let Some(e) = first_err {
            Err(e)
        } else {
            Ok(())
        }
    }
}

fn spawn(
    config: &PoolConfig,
    id: u32,
    multiplex: Multiplex,
) -> Result<Connection, ConnectionError> {
    let mut conn_config = Config::new(
        id,
        config.program.clone(),
        config.metadata.clone(),
        config.operation,
    );
    conn_config.variant = config.variant;
    conn_config.multiplex = multiplex;
    Connection::spawn(&conn_config)
}

/// Mutable borrow on one pool slot. Returns the connection to the
/// pool on drop (or marks the slot as vacant if [`discard`](Self::discard)
/// was called).
pub struct PoolGuard<'a> {
    pool: &'a Pool,
    slot: usize,
    connection: Option<Connection>,
}

impl PoolGuard<'_> {
    /// Borrow the held connection. Panics if the guard has already
    /// been discarded.
    pub fn connection(&mut self) -> &mut Connection {
        self.connection
            .as_mut()
            .expect("connection accessed after discard")
    }

    /// Signal that the connection is in a bad state. The pool drops
    /// it instead of returning it to the slot, and the slot reverts
    /// to vacant so the next acquire spawns a fresh one.
    pub fn discard(mut self) {
        // Move the connection out so Drop sees None and marks the
        // slot vacant; the moved Connection's destructor closes
        // pipes, which makes the remote process EOF and exit.
        let _ = self.connection.take();
    }
}

impl Drop for PoolGuard<'_> {
    fn drop(&mut self) {
        let mut slots = self.pool.slots.lock().unwrap();
        match self.connection.take() {
            Some(conn) => slots[self.slot] = SlotState::Available(conn),
            None => slots[self.slot] = SlotState::Vacant,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_pool_config() -> PoolConfig {
        // We invoke `/bin/sh -c 'exit 0'` style scripts in tests
        // that need a real connection; the default config here is
        // just a placeholder for tests that don't exercise spawn.
        PoolConfig {
            program: "true".to_owned(),
            variant: Variant::Default,
            metadata: Metadata {
                user_and_host: "git@host".to_owned(),
                port: None,
                path: "/repo".to_owned(),
            },
            operation: Operation::Upload,
            multiplex_enabled: false,
        }
    }

    #[test]
    #[should_panic(expected = "pool max_size must be at least 1")]
    fn pool_rejects_zero_size() {
        let _ = Pool::new(fake_pool_config(), 0);
    }

    #[cfg(unix)]
    fn working_stub_script(tmp: &TempDir, name: &str) -> String {
        let script = tmp.path().join(name);
        // Capability + handshake + quit ack — same wire shape as
        // `connection::tests::handshake_against_stub_server`.
        std::fs::write(
            &script,
            "printf '000eversion=1\\n0000'\n\
             dd bs=1 count=18 of=/dev/null 2>/dev/null\n\
             printf '000fstatus 200\\n00010000'\n\
             dd bs=1 count=13 of=/dev/null 2>/dev/null\n\
             printf '000fstatus 200\\n0000'\n",
        )
        .unwrap();
        format!("sh {}", script.to_string_lossy())
    }

    #[test]
    #[cfg(unix)]
    fn acquire_then_release_returns_connection_to_slot() {
        let tmp = TempDir::new().unwrap();
        let mut config = fake_pool_config();
        config.program = working_stub_script(&tmp, "stub.sh");

        let pool = Pool::new(config, 2).expect("pool spawn");
        assert_eq!(pool.capacity(), 2);

        // First acquire grabs slot 0 (the master).
        let guard1 = pool.acquire().expect("first acquire");
        assert_eq!(guard1.slot, 0);
        drop(guard1);

        // Second acquire after release should grab slot 0 again
        // (Available preferred over Vacant).
        let guard2 = pool.acquire().expect("second acquire");
        assert_eq!(guard2.slot, 0);
        drop(guard2);

        pool.shutdown().expect("shutdown");
    }

    #[test]
    #[cfg(unix)]
    fn acquire_spawns_client_slot_when_master_busy() {
        let tmp = TempDir::new().unwrap();
        let mut config = fake_pool_config();
        config.program = working_stub_script(&tmp, "stub.sh");

        let pool = Pool::new(config, 2).expect("pool spawn");

        let guard1 = pool.acquire().expect("first acquire");
        assert_eq!(guard1.slot, 0);

        // Master is checked out; second acquire should hit Vacant
        // slot 1 and spawn.
        let guard2 = pool.acquire().expect("second acquire");
        assert_eq!(guard2.slot, 1);

        drop(guard1);
        drop(guard2);
        pool.shutdown().expect("shutdown");
    }

    #[test]
    #[cfg(unix)]
    fn acquire_errors_when_all_slots_in_use() {
        let tmp = TempDir::new().unwrap();
        let mut config = fake_pool_config();
        config.program = working_stub_script(&tmp, "stub.sh");

        let pool = Pool::new(config, 1).expect("pool spawn");
        let _guard = pool.acquire().expect("first acquire");
        match pool.acquire() {
            Err(ConnectionError::Protocol(msg)) => {
                assert!(msg.contains("slots are in use"), "got: {msg}");
            }
            Err(other) => panic!("expected Protocol error, got {other:?}"),
            Ok(_) => panic!("expected acquire to fail"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn discard_marks_slot_vacant() {
        let tmp = TempDir::new().unwrap();
        let mut config = fake_pool_config();
        config.program = working_stub_script(&tmp, "stub.sh");

        let pool = Pool::new(config, 1).expect("pool spawn");
        let guard = pool.acquire().expect("first acquire");
        guard.discard();

        // Slot 0 should be Vacant after discard; a re-acquire will
        // spawn a fresh connection. We confirm by acquire-and-drop
        // (the connection field on the new guard means spawn ran).
        let _guard = pool.acquire().expect("re-acquire after discard");
    }
}
