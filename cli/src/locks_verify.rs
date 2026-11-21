//! Pre-flight `/locks/verify` for `push` / `pre-push`.
//!
//! Before uploading, ask the server which locks are held against the
//! refs being pushed. Three setting states drive behavior:
//!
//! - `lfs.<endpoint>.locksverify=true` (or `lfs.locksverify=true`):
//!   verify is required; any non-success status aborts the push.
//! - `…=false`: skip the verify call entirely.
//! - unset: best-effort. On 200 we suggest enabling the config; on 5xx
//!   /501/403 we warn and proceed; on 501 we additionally disable the
//!   config (matches upstream's "this server doesn't implement
//!   locking" auto-disable).
//!
//! When verify *succeeds*, returned ours/theirs lists let the caller
//! gate the push on whether any pushed path is held by another user.
//! That second-stage check (path intersection) lives in the caller.
//!
//! See `t-pre-push.sh` "pre-push locks verify {200, 5xx, 501, 403}
//! with verification {enabled, unset, disabled}" for the matrix.
//!
//! Configuration keys mirror upstream:
//! - `lfs.<endpoint>.locksverify` — per-endpoint
//! - `lfs.locksverify` — global default
//!
//! `<endpoint>` is the LFS URL (e.g. `http://host/repo.git/info/lfs`)
//! used as a git config subsection — git handles the dot-laden URL
//! parsing for us when the same string is passed to `--get`.

use std::path::Path;

use git_lfs_api::{ApiError, Lock, Ref, VerifyLocksRequest};
use git_lfs_git::ConfigScope;

use crate::lock_client::{LockClient, LockClientError};
use crate::push::PushCommandError;

/// Effective `locksverify` setting for `endpoint`. Falls back from
/// per-endpoint to the global default; defaults to [`Setting::Unset`]
/// when neither is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Explicit `true`.
    Enabled,
    /// Explicit `false`.
    Disabled,
    /// Neither key is set.
    Unset,
}

/// Outcome of the pre-flight verify.
pub enum Outcome {
    /// Verify wasn't run (config disabled, or transport-level skip).
    /// Caller proceeds without lock data.
    Skipped,
    /// Verify succeeded. Caller may compare `theirs` against the paths
    /// being pushed and abort if any match.
    Verified { ours: Vec<Lock>, theirs: Vec<Lock> },
    /// Verify failed and the setting requires we abort the push.
    Aborted,
}

/// Pre-flight check called by `push` / `pre-push` before any byte
/// transfer. `client` carries the transport choice (HTTP, SSH, or
/// negotiate); when SSH is in play the lock pool master spawns here,
/// which is the connection upstream's test suite expects under the
/// `-oControlMaster=yes` count on upload.
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
    // Successful verify. If the user hasn't opted in yet, give them a
    // one-line nudge — they need to know locking is available so they
    // can decide whether to enforce it.
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

/// Translate an HTTP-side verify failure into an [`Outcome`], applying
/// upstream's quirks per status code:
///
/// - 501 ⇒ auto-disable `lfs.<endpoint>.locksverify` and skip.
/// - 5xx other than 501 ⇒ "no locking API" message; abort if verify
///   is explicitly enabled, else skip with a warning.
/// - 401 / 403 ⇒ "Authentication error" with the same enabled-vs-not
///   gating, mirroring `lockverifier.go`.
/// - 404 ⇒ silently skip (no locking endpoint at all).
/// - anything else ⇒ warn and skip.
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

/// Resolve `lfs.<endpoint>.locksverify` then `lfs.locksverify`,
/// defaulting to [`Setting::Unset`].
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
