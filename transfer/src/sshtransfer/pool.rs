//! A fixed number of pure-SSH connections to one endpoint.
//!
//! A [`Pool`] serves one endpoint and operation. Slot 0 is spawned in
//! [`Pool::new`] and, when multiplexing, is the master that creates the
//! control socket in a temporary directory. The other slots are spawned on
//! first use and connect through that socket.
//!
//! [`Pool::acquire`] checks out a connection as a [`PoolGuard`], which puts
//! it back when dropped. [`PoolGuard::discard`] drops the connection
//! instead, and the slot is spawned again on next use.
//!
//! The pool is synchronous. Async callers run it in
//! `tokio::task::spawn_blocking`. [`Pool::acquire`] does not wait for a
//! free slot, so callers must limit themselves to [`Pool::capacity`]
//! connections at a time.

use std::path::PathBuf;
use std::sync::Mutex;

use git_lfs_creds::{SshProgram, SshVariant};
use tempfile::TempDir;

use crate::sshtransfer::connection::{
    Config, Connection, ConnectionError, Metadata, Multiplex, Operation,
};

/// How to spawn the connections of a [`Pool`].
///
/// The pool fills in the rest of [`Config`]: the slot index as the
/// connection number, and the multiplexing role.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// SSH command, as in [`Config::program`].
    pub program: SshProgram,
    /// SSH client variant.
    pub variant: SshVariant,
    /// Where to connect.
    pub metadata: Metadata,
    /// Operation for every connection in the pool.
    pub operation: Operation,
    /// Whether to share one SSH connection through a control socket.
    ///
    /// Only has an effect with [`SshVariant::Ssh`].
    pub multiplex_enabled: bool,
}

/// One slot in the pool.
///
/// A slot is `Vacant` until first use or after a discard, and `InUse`
/// while a [`PoolGuard`] holds its connection.
enum SlotState {
    Vacant,
    Available(Connection),
    InUse,
}

/// A fixed number of pure-SSH connections to one endpoint.
pub struct Pool {
    config: PoolConfig,
    /// Directory of the control socket, removed when the pool drops.
    /// `None` when not multiplexing.
    _control_dir: Option<TempDir>,
    /// Control socket path. `None` when not multiplexing.
    control_path: Option<PathBuf>,
    /// One entry per slot. It only grows, with [`reserve`](Pool::reserve).
    slots: Mutex<Vec<SlotState>>,
}

impl Pool {
    /// Build a pool of `max_size` slots and spawn the connection in
    /// slot 0.
    ///
    /// Errors if that connection fails to spawn or to complete the
    /// handshake. Panics if `max_size` is 0.
    pub fn new(config: PoolConfig, max_size: usize) -> Result<Self, ConnectionError> {
        assert!(max_size >= 1, "pool max_size must be at least 1");

        let (control_dir, control_path) =
            if config.multiplex_enabled && config.variant.supports_multiplex() {
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

    /// Grow the pool to at least `max_size` slots.
    ///
    /// The new slots are vacant, so connections are spawned only when
    /// needed. A pool is never shrunk.
    pub fn reserve(&self, max_size: usize) {
        let mut slots = self.slots.lock().unwrap();
        while slots.len() < max_size {
            slots.push(SlotState::Vacant);
        }
    }

    /// Number of connection slots this pool manages.
    pub fn capacity(&self) -> usize {
        self.slots.lock().unwrap().len()
    }

    /// Check out a connection.
    ///
    /// Reuses a free live connection if there is one, and otherwise
    /// spawns one in a vacant slot. Errors if every slot is checked out,
    /// or if the spawn fails.
    pub fn acquire(&self) -> Result<PoolGuard<'_>, ConnectionError> {
        let mut slots = self.slots.lock().unwrap();
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
                // Spawning is slow, so don't hold the lock. The slot
                // is already marked `InUse`.
                drop(slots);
                let multiplex = match &self.control_path {
                    Some(path) => Multiplex::Client { path: path.clone() },
                    None => Multiplex::Disabled,
                };
                match spawn(&self.config, slot_idx as u32, multiplex) {
                    Ok(c) => c,
                    Err(e) => {
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
            released: false,
        })
    }

    /// End every connection that isn't checked out with
    /// [`Connection::end`], the master last.
    ///
    /// Tries all of them, then returns the first error.
    pub fn shutdown(self) -> Result<(), ConnectionError> {
        let mut slots = self
            .slots
            .into_inner()
            .expect("Pool::shutdown lock poisoned");
        let mut first_err: Option<ConnectionError> = None;
        for i in end_order(slots.len()) {
            let slot = std::mem::replace(&mut slots[i], SlotState::Vacant);
            if let SlotState::Available(conn) = slot
                && let Err(e) = conn.end()
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
        if let Some(e) = first_err {
            Err(e)
        } else {
            Ok(())
        }
    }

    /// End every connection that isn't checked out, ignoring errors, the
    /// master last.
    ///
    /// Unlike [`shutdown`](Self::shutdown), this works through a shared
    /// reference, for a pool behind an `Arc`. The slots become vacant.
    pub fn end_all_available(&self) {
        let mut slots = match self.slots.lock() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };
        for i in end_order(slots.len()) {
            // A checked-out connection stays with its guard.
            if !matches!(slots[i], SlotState::Available(_)) {
                continue;
            }
            if let SlotState::Available(conn) = std::mem::replace(&mut slots[i], SlotState::Vacant)
            {
                let _ = conn.end();
            }
        }
    }
}

/// The order to end `len` slots in: the clients, then the master in slot
/// 0, whose control socket they may be using, as upstream does.
fn end_order(len: usize) -> impl Iterator<Item = usize> {
    (1..len).chain((len > 0).then_some(0))
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

/// A connection checked out of a [`Pool`].
///
/// Call [`release`](Self::release) once an exchange has finished cleanly
/// to put the connection back. Dropping the guard without it, as an early
/// return on an error does, ends the connection instead, since it may be
/// partway through a message. The slot is spawned again on next use.
pub struct PoolGuard<'a> {
    pool: &'a Pool,
    slot: usize,
    connection: Option<Connection>,
    released: bool,
}

impl PoolGuard<'_> {
    /// Borrow the connection.
    pub fn connection(&mut self) -> &mut Connection {
        self.connection
            .as_mut()
            .expect("connection accessed after discard")
    }

    /// Put the connection back for reuse.
    pub fn release(mut self) {
        self.released = true;
    }

    /// End the connection instead of putting it back, which is what
    /// dropping the guard does too.
    pub fn discard(self) {}
}

impl Drop for PoolGuard<'_> {
    fn drop(&mut self) {
        let mut slots = self.pool.slots.lock().unwrap();
        match self.connection.take() {
            Some(conn) if self.released => slots[self.slot] = SlotState::Available(conn),
            // Dropping the connection kills its subprocess.
            _ => slots[self.slot] = SlotState::Vacant,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_pool_config() -> PoolConfig {
        // Tests that spawn replace `program` with a stub script.
        PoolConfig {
            program: SshProgram::Exe("true".to_owned()),
            variant: SshVariant::Ssh,
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
    fn working_stub_script(tmp: &TempDir, name: &str) -> SshProgram {
        let script = tmp.path().join(name);
        // Handshake and quit, as in
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
        SshProgram::Shell(format!("sh {}", script.to_string_lossy()))
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
        guard1.release();
        assert!(matches!(
            pool.slots.lock().unwrap()[0],
            SlotState::Available(_)
        ));

        // Second acquire after release should grab slot 0 again
        // (Available preferred over Vacant).
        let guard2 = pool.acquire().expect("second acquire");
        assert_eq!(guard2.slot, 0);
        drop(guard2);

        pool.shutdown().expect("shutdown");
    }

    #[test]
    #[cfg(unix)]
    fn reserve_adds_slots_for_more_connections() {
        let tmp = TempDir::new().unwrap();
        let mut config = fake_pool_config();
        config.program = working_stub_script(&tmp, "stub.sh");

        let pool = Pool::new(config, 1).expect("pool spawn");
        let master = pool.acquire().expect("first acquire");
        assert!(pool.acquire().is_err(), "a one-slot pool has no room");

        pool.reserve(2);
        assert_eq!(pool.capacity(), 2);
        let second = pool.acquire().expect("acquire after reserve");
        assert_eq!(second.slot, 1);

        // Reserving fewer slots doesn't shrink the pool.
        pool.reserve(1);
        assert_eq!(pool.capacity(), 2);
        drop((master, second));
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

        // Master is checked out, so the second acquire should hit Vacant
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

    /// A stub like [`working_stub_script`] that logs `start <pid>` and
    /// `quit <pid>` to `log`.
    #[cfg(unix)]
    fn logging_stub_script(tmp: &TempDir, log: &std::path::Path) -> SshProgram {
        let script = tmp.path().join("logging.sh");
        std::fs::write(
            &script,
            format!(
                "echo \"start $$\" >> {log}\n\
                 printf '000eversion=1\\n0000'\n\
                 dd bs=1 count=18 of=/dev/null 2>/dev/null\n\
                 printf '000fstatus 200\\n00010000'\n\
                 dd bs=1 count=13 of=/dev/null 2>/dev/null\n\
                 echo \"quit $$\" >> {log}\n\
                 printf '000fstatus 200\\n0000'\n",
                log = log.display()
            ),
        )
        .unwrap();
        SshProgram::Shell(format!("sh {}", script.to_string_lossy()))
    }

    /// The order the stub's processes quit in, as indexes into the order
    /// they started in. The master starts first, at 0.
    #[cfg(unix)]
    fn quit_order(log: &std::path::Path) -> Vec<usize> {
        let text = std::fs::read_to_string(log).unwrap();
        let started: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("start "))
            .collect();
        text.lines()
            .filter_map(|l| l.strip_prefix("quit "))
            .map(|pid| started.iter().position(|s| *s == pid).unwrap())
            .collect()
    }

    /// Clients are ended before the master, whose control socket they may
    /// use, as upstream does.
    #[test]
    #[cfg(unix)]
    fn the_master_is_ended_last() {
        for use_shutdown in [true, false] {
            let tmp = TempDir::new().unwrap();
            let log = tmp.path().join("log");
            let mut config = fake_pool_config();
            config.program = logging_stub_script(&tmp, &log);

            let pool = Pool::new(config, 2).expect("pool spawn");
            let master = pool.acquire().expect("master");
            let client = pool.acquire().expect("client");
            master.release();
            client.release();
            if use_shutdown {
                pool.shutdown().expect("shutdown");
            } else {
                pool.end_all_available();
            }
            assert_eq!(quit_order(&log), [1, 0], "shutdown: {use_shutdown}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn dropped_guard_ends_its_connection() {
        // An error return drops the guard mid-exchange, so its connection
        // isn't reused.
        let tmp = TempDir::new().unwrap();
        let mut config = fake_pool_config();
        config.program = working_stub_script(&tmp, "stub.sh");

        let pool = Pool::new(config, 1).expect("pool spawn");
        let guard = pool.acquire().expect("first acquire");
        drop(guard);
        assert!(matches!(pool.slots.lock().unwrap()[0], SlotState::Vacant));
        let _guard = pool.acquire().expect("re-acquire spawns again");
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

        // With capacity 1, this only succeeds if the discarded slot
        // was freed and spawned again.
        let _guard = pool.acquire().expect("re-acquire after discard");
    }
}
