//! HTTP client for the git-lfs batch and locking APIs.
//!
//! See `docs/api/` for the wire-protocol specification.
//!
//! Scope is deliberately narrow: this crate handles JSON request/response
//! against the LFS server. Concurrency, retries, and the actual byte
//! transfer against action URLs live in `git-lfs-transfer`. Credential
//! resolution lives in `git-lfs-creds`. Server URL discovery from a git
//! remote lives in `git-lfs-git`.

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
