//! Client for the pure-SSH transfer protocol.
//!
//! Instead of the HTTP API, objects and locks go over a long-lived SSH
//! connection to a `git-lfs-transfer` command on the server, framed with
//! Git's pkt-line scheme. The protocol is specified in
//! [`docs/proposals/ssh_adapter.md`](https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/proposals/ssh_adapter.md).
//!
//! The server opens with a capability advertisement that must include
//! `version=1`. The client answers `version 1`, the server replies
//! `status 200`, and then the client sends commands (`batch`,
//! `get-object`, `put-object`, `verify-object`, `lock`, `unlock`,
//! `list-lock`) one at a time, ending with `quit`.
//!
//! A [`connection::Connection`] is one SSH subprocess. A [`pool::Pool`]
//! holds several for one endpoint and operation. With OpenSSH they share
//! one TCP connection through a control socket, so only the first pays
//! for the SSH handshake. [`adapter`] runs transfers over a pool, and
//! [`lock`] runs the lock commands over a single connection.

pub mod adapter;
pub mod connection;
pub mod lock;
pub mod pktline;
pub mod pool;
