//! Credential helper bridge for Git LFS (git credential fill/approve/reject).
//!
//! LFS endpoints are usually HTTPS, and HTTPS auth needs a username
//! and password. Rather than maintaining a separate credential store,
//! this crate defers to git's existing one: whatever the user has
//! already configured for their git remote (osxkeychain, libsecret,
//! manager, store, plain `cache`, …) is what LFS uses too.
//!
//! The [`Helper`] trait represents one credential source. A
//! [`HelperChain`] tries multiple sources in order, broadcasting
//! [`Helper::approve`] / [`Helper::reject`] to every helper so caches
//! stay in sync. The bundled implementations are:
//!
//! - [`CachingHelper`]: in-process cache keyed on the [`Query`]
//!   tuple (protocol, host, path).
//! - [`GitCredentialHelper`]: shells out to `git credential
//!   fill/approve/reject`, picking up whatever helper the user has
//!   configured.
//! - [`AskpassHelper`]: spawns the `GIT_ASKPASS` / `core.askpass` /
//!   `SSH_ASKPASS` program for interactive prompts.
//! - [`NetrcCredentialHelper`]: parses `~/.netrc` (or `_netrc` on
//!   Windows) for host-keyed login/password pairs.
//!
//! SSH remotes follow a different flow. [`SshAuthClient`] runs
//! `git-lfs-authenticate <path> <operation>` over SSH and parses an
//! [`SshAuth`] response containing a replacement HTTPS endpoint plus
//! short-lived authorization headers; no username/password is asked
//! of the user. Results are cached per request key with the
//! server-supplied expiry honored.

mod askpass;
mod chain;
mod git_helper;
mod helper;
mod memory;
mod netrc;
mod query;
mod ssh;
mod trace;

pub use askpass::AskpassHelper;
pub use chain::HelperChain;
pub use git_helper::GitCredentialHelper;
pub use helper::{Credentials, FillContext, Helper, HelperError, HelperOutcome};
pub use memory::CachingHelper;
pub use netrc::NetrcCredentialHelper;
pub use query::Query;
pub use ssh::{SshAuth, SshAuthClient, SshAuthError, SshOperation};
