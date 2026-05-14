//! Concurrent transfer queue and basic adapter for Git LFS uploads and downloads.
//!
//! When Git LFS wants to transfer files between a client and a
//! server, it first asks the server's batch endpoint with a list
//! of OIDs and sizes for the files involved, and the server
//! returns one URL per object (typically a presigned link to S3
//! or a CDN, plus auth headers and an expiry window); the client
//! then PUTs or GETs the bytes against those URLs.
//!
//! This crate implements the client side of that dance. It sits
//! between [`git_lfs_api`] and [`git_lfs_store`]: given a list of
//! `(oid, size)` pairs, [`Transfer`] negotiates the batch, drives
//! the per-object byte movement concurrently, and streams
//! [`Event`]s back to the caller.
//!
//! [`Transfer`] runs at most [`TransferConfig::concurrency`]
//! transfers in flight at once. Each transfer uses its own retry
//! loop with exponential backoff per [`TransferConfig`]. Outcomes
//! land in a [`Report`] keyed by OID; per-object [`TransferError`]s
//! sit alongside successful OIDs so partial failures don't tear
//! down the queue.
//!
//! [`git_lfs_store::Store`] is a synchronous API while transfers
//! are async. Downloads pipe HTTP body bytes through
//! `tokio_util::io::SyncIoBridge` into a `spawn_blocking` task
//! that calls `store.insert_verified`, so the full object is
//! never buffered in memory and the async runtime isn't blocked
//! while a multi-gigabyte object lands.
//!
//! Only the `basic` HTTPS transfer is implemented at the moment
//! (see the [basic-transfers spec][spec]). The `tus`,
//! custom-transfer-agent, and pure-SSH adapters are not
//! implemented yet.
//!
//! [`git_lfs_api`]: https://docs.rs/git-lfs-api
//! [`git_lfs_store`]: https://docs.rs/git-lfs-store
//! [spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/basic-transfers.md

mod basic;
mod config;
mod error;
mod event;
pub mod sshtransfer;
mod transfer;

pub use config::{TransferConfig, UrlRewriter};
pub use error::{Report, TransferError};
pub use event::Event;
pub use transfer::Transfer;
