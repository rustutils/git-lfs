//! Pure-SSH transfer protocol client (`git-lfs-transfer`).
//!
//! Transfers LFS objects over a long-lived SSH connection to a
//! `git-lfs-transfer` server, framed with Git's pkt-line scheme. The
//! protocol is specified in
//! [`docs/proposals/ssh_adapter.md`](https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/proposals/ssh_adapter.md).
//!
//! Wire layout: pkt-line framed messages, version handshake on
//! connect (`version=1` capability advertisement, client sends
//! `version 1`, server responds with `status 200`), then a sequence
//! of request-response commands (`batch`, `get-object`,
//! `put-object`, `verify-object`, `lock`, `unlock`, `list-lock`,
//! `quit`).
//!
//! Connections are persistent — one SSH subprocess per logical
//! transfer session, with SSH multiplexing (`-oControlMaster=yes` on
//! the first connection, `-oControlMaster=no` on follow-ups
//! sharing the control socket) so the upfront handshake cost
//! amortizes across multiple commands.

pub mod adapter;
pub mod connection;
pub mod pktline;
pub mod pool;
