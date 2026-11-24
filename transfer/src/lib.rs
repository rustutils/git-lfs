//! Concurrent transfer queue for Git LFS uploads and downloads.
//!
//! A [`Transfer`] sends a batch request for a list of objects, then moves
//! each object's bytes between the server and the local
//! [`Store`](git_lfs_store::Store), several at once. Downloads are hashed
//! and only committed to the store when they match their OID.
//!
//! Build a [`Transfer`] from an API client, a store, and a
//! [`TransferConfig`], then call [`Transfer::download`] or
//! [`Transfer::upload`] with the objects. The config sets how many objects
//! are in flight at once, the retry budget and backoff, the batch size, and
//! optional rewriters for action URLs. Network errors, local I/O errors,
//! and 408, 429, or 5xx responses are retried with exponential backoff, or
//! after the server's `Retry-After` delay. Each retry sends a new batch
//! request, so it gets fresh actions, and an action about to expire is
//! renewed the same way. Interrupted downloads resume from the partial file
//! with a `Range` request.
//!
//! The [`backend::Backend`] picks how bytes move. [`Transfer::new`] uses
//! HTTPS: batch requests go through the [`git_lfs_api`] client and objects
//! go to the action URLs the server returns. [`Transfer::new_ssh`] uses the
//! pure-SSH protocol in [`sshtransfer`] (see the [SSH transfer
//! spec][ssh-spec]), which runs batch requests and transfers over a pool of
//! SSH connections. Its [`SshBackend`](backend::SshBackend) can carry an
//! HTTPS fallback, used when the SSH connection can't be set up (for
//! example when the server has no `git-lfs-transfer`).
//!
//! With [`TransferConfig::tus`], the server can pick resumable tus.io
//! uploads. It can also pick a [`CustomAdapter`] from those in the config
//! instead of `basic`. Its objects then go through an external process
//! that speaks the [custom transfer protocol][custom-spec].
//!
//! Pass a channel to receive [`Event`]s as objects start, make progress,
//! and finish. The returned [`Report`] lists every object as succeeded or
//! failed with its [`TransferError`], so one bad object doesn't stop the
//! rest. The call itself returns an error only when the batch as a whole
//! fails, such as a failed batch request.
//!
//! [custom-spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/custom-transfers.md
//! [ssh-spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/proposals/ssh_adapter.md

mod action_auth;
pub mod backend;
mod basic;
mod config;
mod custom;
mod error;
mod event;
mod http_clients;
pub mod sshtransfer;
mod transfer;
mod tus;

pub use action_auth::ActionAuth;
pub use config::{HeaderSource, TransferConfig, UrlRewriter, UrlSetting};
pub use custom::CustomAdapter;
pub use error::{Report, TransferError};
pub use event::Event;
pub use http_clients::HttpClients;
pub use transfer::Transfer;
