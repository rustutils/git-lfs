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
//! - true: a 5xx other than 501, a 401, or a 403 aborts the push.
//! - unset: those errors print a message and the push goes ahead. A
//!   successful check suggests turning the setting on.
//!
//! Either way, a 501 sets `lfs.<endpoint>.locksverify` to false, and any
//! other error, including a 404 or an SSH failure, lets the push go ahead
//! without lock data. `t-pre-push.sh` checks each status with each setting.

use std::path::Path;

use git_lfs_api::{ApiError, Lock, Ref, VerifyLocksRequest};
use git_lfs_git::ConfigScope;

use crate::lock_client::{LockClient, LockClientError};
use crate::push::PushCommandError;

/// The effective `locksverify` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// True, 1, yes, or on.
    Enabled,
    /// False, 0, no, off, or empty.
    Disabled,
    /// Not set, or set to anything else.
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
        Err(LockClientError::Api(boxed)) => {
            return Ok(handle_api_error(
                *boxed,
                setting,
                cwd,
                remote_label,
                endpoint,
            ));
        }
        Err(e) => {
            eprintln!("warning: lock verify failed: {e}");
            return Ok(Outcome::Skipped);
        }
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

/// Turn an HTTP error from the check into an [`Outcome`].
///
/// - 501: set `lfs.<endpoint>.locksverify` to false and skip.
/// - Other 5xx: say the server has no locking API, and abort if the
///   setting is on.
/// - 401 or 403: an authentication error, which aborts if the setting is
///   on.
/// - 404: skip without a message.
/// - Anything else: warn and skip.
///
/// Upstream's `lockVerifier.Verify` aborts on every error except a 501
/// when the setting is on.
fn handle_api_error(
    e: ApiError,
    setting: Setting,
    cwd: &Path,
    remote_label: &str,
    endpoint: &str,
) -> Outcome {
    match &e {
        ApiError::Status { status: 501, .. } => {
            let key = format!("lfs.{endpoint}.locksverify");
            let _ = git_lfs_git::config::set(cwd, ConfigScope::Local, &key, "false");
            Outcome::Skipped
        }
        ApiError::Status { status, .. } if (500..600).contains(status) => {
            if matches!(setting, Setting::Enabled) {
                eprintln!(
                    "\"{remote_label}\" does not support the Git LFS locking API. \
                     Consider disabling it with:"
                );
                eprintln!("  $ git config lfs.{endpoint}.locksverify false");
                Outcome::Aborted
            } else {
                eprintln!("\"{remote_label}\" does not support the Git LFS locking API.");
                Outcome::Skipped
            }
        }
        ApiError::Status {
            status: 401 | 403, ..
        } => {
            if matches!(setting, Setting::Enabled) {
                eprintln!("error: Authentication error: {e}");
                Outcome::Aborted
            } else {
                eprintln!("warning: Authentication error: {e}");
                Outcome::Skipped
            }
        }
        ApiError::Status { status: 404, .. } => Outcome::Skipped,
        _ => {
            eprintln!("warning: lock verify failed: {e}");
            Outcome::Skipped
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

fn parse_bool(s: &str) -> Setting {
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Setting::Enabled,
        "false" | "0" | "no" | "off" | "" => Setting::Disabled,
        _ => Setting::Unset,
    }
}
