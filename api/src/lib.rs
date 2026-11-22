//! Async client for the Git LFS batch and locking APIs.
//!
//! A [`Client`] talks to one LFS endpoint, such as
//! `https://git-server.com/foo/bar.git/info/lfs`. [`Client::batch`] sends a
//! [`BatchRequest`] (an [`Operation`] and a list of [`ObjectSpec`]s) and
//! returns a [`BatchResponse`] with one [`ObjectResult`] per object. Each
//! result holds [`Actions`] that say where to send or fetch the bytes, or an
//! [`ObjectError`], or neither when the server already has an uploaded
//! object. Moving the bytes is the job of [`git-lfs-transfer`][transfer].
//!
//! The locking calls are [`Client::create_lock`], [`Client::list_locks`],
//! [`Client::verify_locks`], and [`Client::delete_lock`]. They exchange
//! [`Lock`] records. The [batch spec][batch] and [locking spec][locking]
//! define the wire format.
//!
//! The [`Auth`] given to [`Client::new`] is sent with every request. To
//! handle 401 responses, attach a credential
//! [`Helper`](git_lfs_creds::Helper) from [`git-lfs-creds`][creds] with
//! [`Client::with_credential_helper`]. On a 401 the client rejects the
//! credentials it sent, fills new ones from the helper, and retries, a few
//! times at most. Credentials that succeed are approved and reused for
//! later requests. For SSH remotes, an
//! [`SshResolver`] attached with [`Client::with_ssh_resolver`] supplies
//! headers, and optionally a replacement endpoint URL, for each request.
//!
//! Calls fail with [`ApiError`], except [`Client::create_lock`], whose
//! [`CreateLockError`] adds a case for a path that is already locked.
//! [`ApiError::is_retryable`] and [`ApiError::retry_after`] tell the caller
//! whether and when to try again.
//!
//! ```no_run
//! use git_lfs_api::{ApiError, Auth, BatchRequest, Client, ObjectSpec, Operation};
//!
//! # async fn run() -> Result<(), ApiError> {
//! let endpoint = url::Url::parse("https://git-server.com/foo/bar.git/info/lfs")?;
//! let client = Client::new(endpoint, Auth::None);
//!
//! let object = ObjectSpec {
//!     oid: "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393".into(),
//!     size: 12345,
//! };
//! let request = BatchRequest::new(Operation::Download, vec![object])
//!     .with_transfers(["basic".to_owned()]);
//!
//! for result in client.batch(&request).await?.objects {
//!     if let Some(download) = result.actions.and_then(|a| a.download) {
//!         println!("{} at {}", result.oid, download.href);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [batch]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/batch.md
//! [locking]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/locking.md
//! [transfer]: https://docs.rs/git-lfs-transfer
//! [creds]: https://docs.rs/git-lfs-creds

// ApiError::Status carries enough context (URL, body, LFS-Authenticate,
// Retry-After) to exceed clippy's 128-byte threshold for the Err arm.
// Boxing it would touch every match site, which isn't worth it for an error
// type used only on failure paths.
#![allow(clippy::result_large_err)]

mod auth;
mod batch;
mod client;
mod error;
mod locks;
mod models;
mod redirect;
mod ssh;

pub use auth::{Auth, verbose_header_line};
pub use batch::{
    Action, Actions, BatchRequest, BatchResponse, ObjectError, ObjectResult, ObjectSpec, Operation,
};
pub use client::{Client, HeaderSource};
pub use error::{ApiError, ServerError, parse_retry_after};
pub use locks::{
    CreateLockError, CreateLockRequest, DeleteLockRequest, ListLocksFilter, LockList,
    VerifyLocksRequest, VerifyLocksResponse,
};
pub use models::{Lock, Owner, Ref};
pub use redirect::{
    http_client, is_credential_header, redirect_keeps_credentials, send_with_redirects,
    trace_request, trace_response,
};
pub use ssh::{SharedSshResolver, SshAuth, SshOperation, SshResolver};
