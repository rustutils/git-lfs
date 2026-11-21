//! Find credentials for Git LFS requests.
//!
//! A [`Helper`] is one source of credentials. Given a [`Query`] (protocol,
//! host, and optional path, usually built with [`Query::from_url`]), its
//! [`fill`](Helper::fill) returns [`Credentials`] or nothing. After the
//! request, the caller reports the outcome with [`approve`](Helper::approve)
//! or [`reject`](Helper::reject) so the source can store or forget them.
//!
//! A [`HelperChain`] is itself a helper that asks several in order. Fill
//! returns the first credentials found. Approve and reject stop at the first
//! helper that returns [`HelperOutcome::Handled`]. The Git LFS CLI chains
//! [`NetrcCredentialHelper`], [`CachingHelper`], [`AskpassHelper`] (only when
//! no `credential.helper` is configured), and [`GitCredentialHelper`], which
//! runs `git credential` so LFS uses whatever store the user set up for Git.
//!
//! SSH remotes authenticate differently. [`SshAuthClient`] runs
//! `git-lfs-authenticate` on the server and returns an [`SshAuth`]: headers
//! to send to the LFS API, and optionally an HTTPS endpoint to use instead.
//! Responses are cached until they expire.

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
pub use ssh::{SshAuth, SshAuthClient, SshAuthError, SshOperation, shell_quote};
