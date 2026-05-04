//! Credential resolution for git-lfs.
//!
//! Bridges to git's credential machinery (`git credential fill/approve/reject`)
//! and adds a small in-memory cache so repeated requests against the same
//! host don't re-shell-out for every batch / upload / download.
//!
//! # Scope (v0)
//!
//! - [`Query`] is the (protocol, host, path) tuple git's credential helpers
//!   key on.
//! - [`Credentials`] holds a username + password.
//! - [`Helper`] is the trait the API client calls into when it gets a 401.
//! - [`GitCredentialHelper`] shells out to `git credential` for the real
//!   resolution.
//! - [`AskpassHelper`] spawns `GIT_ASKPASS` / `core.askpass` /
//!   `SSH_ASKPASS` for username + password prompts.
//! - [`CachingHelper`] memoizes the answer in-process.
//! - [`HelperChain`] tries each helper in order and writes
//!   approve/reject decisions back to all of them.
//!
//! Deferred (see `NOTES.md`): netrc, NTLM/Kerberos, multi-stage
//! `wwwauth[]`/`state[]`, URL-pattern config (`credential.<url>.helper`).

mod askpass;
mod chain;
mod git_helper;
mod helper;
mod memory;
mod query;

pub use askpass::AskpassHelper;
pub use chain::HelperChain;
pub use git_helper::GitCredentialHelper;
pub use helper::{Credentials, Helper, HelperError};
pub use memory::CachingHelper;
pub use query::Query;
