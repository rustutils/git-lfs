//! Concurrent transfer queue + transfer adapters for git-lfs.
//!
//! This is the orchestration layer: take a list of objects, call the batch
//! API to negotiate URLs, then drive the actual byte movement (downloads
//! into [`git_lfs_store::Store`], uploads from it). v0 only ships the
//! `basic` adapter (`docs/api/basic-transfers.md`); tus, custom, and ssh
//! adapters live in NOTES.md as deferred work.
//!
//! ## Concurrency
//!
//! [`Transfer`] runs at most [`TransferConfig::concurrency`] in-flight
//! transfers at once. Each transfer uses its own retry loop with
//! exponential backoff per [`TransferConfig`].
//!
//! ## Sync/async bridge
//!
//! [`git_lfs_store::Store`] is a sync API. Downloads pipe HTTP body bytes
//! through [`tokio_util::io::SyncIoBridge`] into a `spawn_blocking` task
//! that calls `store.insert_verified`, so we never buffer the full object
//! in memory.

mod basic;
mod config;
mod error;
mod event;
mod transfer;

pub use config::TransferConfig;
pub use error::{Report, TransferError};
pub use event::Event;
pub use transfer::Transfer;
