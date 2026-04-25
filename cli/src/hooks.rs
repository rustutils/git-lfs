//! `post-checkout`, `post-commit`, `post-merge` — git hook entry points.
//!
//! These hooks exist (in upstream git-lfs) to manage the **lockable**
//! read-only feature: files matching a `lockable` pattern in
//! `.gitattributes` get their write bit cleared on disk so users can't
//! accidentally edit a file someone else holds the lock for. After a
//! checkout / commit / merge, that read-only flag may need adjusting.
//!
//! v0 ships these as exit-0 stubs:
//!
//! - We don't have `track --lockable` support yet (see NOTES.md), so no
//!   user has any lockable patterns configured.
//! - Upstream itself early-exits with code 0 when no lockable patterns
//!   are present, so for any non-lockable user, "do nothing" is exactly
//!   the right behavior.
//! - But our `install` already writes hook scripts that invoke
//!   `git lfs post-checkout` etc. Without these stubs, every `git
//!   checkout` after `git lfs install` would fail with
//!   "unrecognized subcommand."
//!
//! When lockable lands, this module gets actual logic; argument shapes
//! match upstream so the hook scripts won't need to change.

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("{0}")]
    Usage(String),
}

/// `post-checkout <prev-sha> <post-sha> <flag>`. The `flag` is "1" if
/// HEAD moved, "0" if a single file was checked out.
pub fn post_checkout(args: &[String]) -> Result<(), HookError> {
    if args.len() != 3 {
        return Err(HookError::Usage(
            "post-checkout: expected 3 args (prev-sha, post-sha, flag); \
             this should be run through Git's post-checkout hook"
                .into(),
        ));
    }
    Ok(())
}

/// `post-commit` (no arguments).
pub fn post_commit(args: &[String]) -> Result<(), HookError> {
    if !args.is_empty() {
        return Err(HookError::Usage(format!(
            "post-commit: expected 0 args, got {}",
            args.len()
        )));
    }
    Ok(())
}

/// `post-merge <squash-flag>`. Argument is "1" for squash merges, "0"
/// otherwise — irrelevant to lockable read-only management, so we
/// only validate count.
pub fn post_merge(args: &[String]) -> Result<(), HookError> {
    if args.len() != 1 {
        return Err(HookError::Usage(
            "post-merge: expected 1 arg (squash-flag); \
             this should be run through Git's post-merge hook"
                .into(),
        ));
    }
    Ok(())
}
