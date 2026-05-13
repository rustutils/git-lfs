//! HTTP client for the Git LFS batch and locking APIs.
//!
//! Git LFS speaks to a server over HTTPS using two endpoints.
//! The batch endpoint (`POST /objects/batch`) takes a list of
//! OIDs and sizes and returns one transfer URL per object
//! (plus any auth headers and an expiry window); the locking
//! endpoint suite (`/locks`, `/locks/verify`,
//! `/locks/{id}/unlock`, …) lets clients claim files to
//! coordinate edits across users. See the [batch spec][batch]
//! and [locking spec][locking] for the wire-protocol contract.
//!
//! [`Client`] is the entry point. Construct one per endpoint
//! URL with an initial [`Auth`] (None, Basic, or Bearer),
//! optionally attach a credential helper via
//! [`Client::with_credential_helper`] and an [`SshResolver`]
//! via [`Client::with_ssh_resolver`], then invoke the
//! per-operation methods. `Client` is cheap to clone and shares
//! an underlying connection pool.
//!
//! Request and response types are split by operation. Batch
//! uses [`BatchRequest`] and [`BatchResponse`], with
//! [`ObjectResult`] carrying either [`Actions`] or an
//! [`ObjectError`] per object. Lock operations use
//! [`CreateLockRequest`], [`ListLocksFilter`],
//! [`VerifyLocksRequest`], and [`DeleteLockRequest`], with
//! [`Lock`], [`Owner`], and [`Ref`] as the shared model types.
//!
//! On a 401 the client queries the attached credential helper,
//! retries once, and reports `approve` or `reject` based on the
//! outcome. Successful fills are cached for the lifetime of the
//! `Client`. SSH-mediated endpoints (via [`SshResolver`]) can
//! swap in a replacement HTTPS URL and auth headers per request.
//!
//! Failures surface as [`ApiError`], with predicates
//! ([`is_unauthorized`](ApiError::is_unauthorized),
//! [`is_retryable`](ApiError::is_retryable), …) for dispatch
//! without matching on the variant. Server-supplied
//! `Retry-After` (on 429 or 5xx responses) reaches callers via
//! [`ApiError::retry_after`]; [`parse_retry_after`] is exported
//! for reuse on other response paths.
//!
//! Byte transfer against action URLs lives in
//! [`git-lfs-transfer`][transfer-crate]; credential resolution
//! lives in [`git-lfs-creds`][creds-crate]; server URL
//! discovery from a git remote lives in
//! [`git-lfs-git`][git-crate].
//!
//! [batch]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/batch.md
//! [locking]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/locking.md
//! [transfer-crate]: https://docs.rs/git-lfs-transfer
//! [creds-crate]: https://docs.rs/git-lfs-creds
//! [git-crate]: https://docs.rs/git-lfs-git

// ApiError::Status carries enough context (URL, body, LFS-Authenticate,
// Retry-After) that it crosses clippy's default 128-byte threshold for
// the Err arm. Boxing the variant would cascade through every match
// site; for an error type used only on failure paths the size is not
// worth chasing.
#![allow(clippy::result_large_err)]

mod auth;
mod batch;
mod client;
mod error;
mod locks;
mod models;
mod ssh;

pub use auth::Auth;
pub use batch::{
    Action, Actions, BatchRequest, BatchResponse, ObjectError, ObjectResult, ObjectSpec, Operation,
};
pub use client::Client;
pub use error::{ApiError, ServerError, parse_retry_after};
pub use locks::{
    CreateLockError, CreateLockRequest, DeleteLockRequest, ListLocksFilter, LockList,
    VerifyLocksRequest, VerifyLocksResponse,
};
pub use models::{Lock, Owner, Ref};
pub use ssh::{SharedSshResolver, SshAuth, SshOperation, SshResolver};
