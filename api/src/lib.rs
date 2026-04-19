//! HTTP client for the git-lfs batch and locking APIs.
//!
//! See `docs/api/` for the wire-protocol specification.
//!
//! Scope is deliberately narrow: this crate handles JSON request/response
//! against the LFS server. Concurrency, retries, and the actual byte
//! transfer against action URLs live in `git-lfs-transfer`. Credential
//! resolution lives in `git-lfs-creds`. Server URL discovery from a git
//! remote lives in `git-lfs-git`.

mod auth;
mod batch;
mod client;
mod error;
mod locks;
mod models;

pub use auth::Auth;
pub use batch::{
    Action, Actions, BatchRequest, BatchResponse, ObjectError, ObjectResult, ObjectSpec, Operation,
};
pub use client::Client;
pub use error::{ApiError, ServerError};
pub use locks::{
    CreateLockError, CreateLockRequest, DeleteLockRequest, ListLocksFilter, LockList,
    VerifyLocksRequest, VerifyLocksResponse,
};
pub use models::{Lock, Owner, Ref};
