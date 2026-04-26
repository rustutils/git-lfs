//! `post-checkout`, `post-commit`, `post-merge` — git hook entry points.
//!
//! These hooks exist to manage the **lockable** read-only feature: files
//! matching a `lockable` pattern in `.gitattributes` get their write bit
//! cleared on disk so users can't accidentally edit a file someone else
//! holds the lock for. After a checkout / commit / merge, the working
//! tree may have new lockable files (or files whose lockable status
//! flipped) that need re-chmodding.
//!
//! All three hooks do the same thing: full workdir walk, apply the
//! lockable invariant. Upstream optimizes by diffing changed files
//! only — we can do that later, but a full `git ls-files` scan is fine
//! for correctness and matches the strict "post-* always sees a clean
//! workdir" assumption.

use std::path::Path;

use crate::lockable;

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `post-checkout <prev-sha> <post-sha> <flag>`. The `flag` is "1" if
/// HEAD moved, "0" if a single file was checked out.
pub fn post_checkout(cwd: &Path, args: &[String]) -> Result<(), HookError> {
    if args.len() != 3 {
        return Err(HookError::Usage(
            "post-checkout: expected 3 args (prev-sha, post-sha, flag); \
             this should be run through Git's post-checkout hook"
                .into(),
        ));
    }
    lockable::enforce_workdir(cwd)?;
    Ok(())
}

/// `post-commit` (no arguments).
pub fn post_commit(cwd: &Path, args: &[String]) -> Result<(), HookError> {
    if !args.is_empty() {
        return Err(HookError::Usage(format!(
            "post-commit: expected 0 args, got {}",
            args.len()
        )));
    }
    lockable::enforce_workdir(cwd)?;
    Ok(())
}

/// `post-merge <squash-flag>`. Argument is "1" for squash merges, "0"
/// otherwise — irrelevant to lockable read-only management, so we
/// only validate count.
pub fn post_merge(cwd: &Path, args: &[String]) -> Result<(), HookError> {
    if args.len() != 1 {
        return Err(HookError::Usage(
            "post-merge: expected 1 arg (squash-flag); \
             this should be run through Git's post-merge hook"
                .into(),
        ));
    }
    lockable::enforce_workdir(cwd)?;
    Ok(())
}
