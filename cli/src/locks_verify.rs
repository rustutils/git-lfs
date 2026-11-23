//! The lock check that push and pre-push run before uploading.
//!
//! It asks the server for the locks on the ref being pushed, split into
//! ours and theirs. The caller then refuses to push files someone else has
//! locked.
//!
//! `lfs.<endpoint>.locksverify`, falling back to `lfs.locksverify`, decides
//! what happens:
//!
//! - false: the check is skipped.
//! - true: any failure aborts the push, except that the server has no
//!   locking API.
//! - unset: failures print a message and the push goes ahead. A successful
//!   check suggests turning the setting on.
//!
//! Either way, a 404 or 501 means there's no locking API, which sets
//! `lfs.<endpoint>.locksverify` to false. This follows upstream's
//! `lockVerifier.Verify`. `t-pre-push.sh` checks each status with each
//! setting.

use std::path::Path;

use git_lfs_api::{ApiError, Lock, Ref, VerifyLocksRequest};
use git_lfs_git::ConfigScope;

use crate::lock_client::{LockClient, LockClientError};
use crate::push::PushCommandError;

/// The effective `locksverify` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Set to a true value.
    Enabled,
    /// Set to anything else.
    Disabled,
    /// Not set.
    Unset,
}

/// Result of the lock check.
pub enum Outcome {
    /// No lock data, because the check was turned off or failed without
    /// aborting. The push goes ahead.
    Skipped,
    /// The server's locks, for the caller to compare with the pushed paths.
    Verified { ours: Vec<Lock>, theirs: Vec<Lock> },
    /// The check failed and the push must stop.
    Aborted,
}

/// Run the lock check for `endpoint`.
///
/// With an SSH endpoint the lock pool's master connection starts here,
/// which `t-batch-transfer.sh` expects.
pub fn run(
    client: &LockClient,
    cwd: &Path,
    remote_label: &str,
    endpoint: &str,
    refspec: Option<&Ref>,
) -> Result<Outcome, PushCommandError> {
    let setting = read_setting(cwd, endpoint)?;
    if matches!(setting, Setting::Disabled) {
        return Ok(Outcome::Skipped);
    }

    let mut req = VerifyLocksRequest::default();
    if let Some(r) = refspec {
        req.r#ref = Some(r.clone());
    }

    let resp = match client.verify_locks(&req) {
        Ok(resp) => resp,
        Err(e) => return Ok(handle_error(&e, setting, cwd, remote_label, endpoint)),
    };
    if matches!(setting, Setting::Unset) {
        eprintln!(
            "Locking support detected on remote \"{remote_label}\". \
             Consider enabling it with: "
        );
        eprintln!("  $ git config lfs.{endpoint}.locksverify true");
    }
    Ok(Outcome::Verified {
        ours: resp.ours,
        theirs: resp.theirs,
    })
}

/// How a failed check failed.
enum Failure {
    /// A 404 or 501: the server has no locking API.
    NotImplemented,
    /// A 401 or 403.
    Auth,
    /// Anything else, including a 5xx, a connection error, or an SSH
    /// error.
    Other,
}

impl Failure {
    fn of(e: &LockClientError) -> Self {
        let status = match e {
            LockClientError::Api(api) => match api.as_ref() {
                ApiError::Status { status, .. } => Some(*status),
                _ => None,
            },
            LockClientError::SshStatus { status, .. } => Some(*status),
            _ => None,
        };
        match status {
            Some(404 | 501) => Self::NotImplemented,
            Some(401 | 403) => Self::Auth,
            _ => Self::Other,
        }
    }
}

/// Turn a failed check into an [`Outcome`], as upstream's
/// `lockVerifier.Verify` does.
///
/// - No locking API (404 or 501): set `lfs.<endpoint>.locksverify` to
///   false and skip.
/// - An authentication error: abort if the setting is on, else warn.
/// - Anything else: suggest turning the setting off, and abort if it's on.
fn handle_error(
    e: &LockClientError,
    setting: Setting,
    cwd: &Path,
    remote_label: &str,
    endpoint: &str,
) -> Outcome {
    let enabled = matches!(setting, Setting::Enabled);
    match Failure::of(e) {
        Failure::NotImplemented => {
            let key = format!("lfs.{endpoint}.locksverify");
            let _ = git_lfs_git::config::set(cwd, ConfigScope::Local, &key, "false");
            Outcome::Skipped
        }
        Failure::Auth if enabled => {
            eprintln!("error: Authentication error: {e}");
            Outcome::Aborted
        }
        Failure::Auth => {
            eprintln!("warning: Authentication error: {e}");
            Outcome::Skipped
        }
        Failure::Other => {
            eprintln!(
                "\"{remote_label}\" does not support the Git LFS locking API. \
                 Consider disabling it with:"
            );
            eprintln!("  $ git config lfs.{endpoint}.locksverify false");
            if enabled {
                eprintln!("error: {e}");
                Outcome::Aborted
            } else {
                Outcome::Skipped
            }
        }
    }
}

/// Read `lfs.<endpoint>.locksverify`, falling back to `lfs.locksverify`.
fn read_setting(cwd: &Path, endpoint: &str) -> Result<Setting, PushCommandError> {
    let endpoint_key = format!("lfs.{endpoint}.locksverify");
    if let Some(v) = git_lfs_git::config::get_effective(cwd, &endpoint_key)? {
        return Ok(parse_bool(&v));
    }
    if let Some(v) = git_lfs_git::config::get_effective(cwd, "lfs.locksverify")? {
        return Ok(parse_bool(&v));
    }
    Ok(Setting::Unset)
}

/// A set value enables verification only if it's true. Anything else
/// disables it, as in upstream.
fn parse_bool(s: &str) -> Setting {
    if git_lfs_git::config::parse_bool(Some(s), false) {
        Setting::Enabled
    } else {
        Setting::Disabled
    }
}
