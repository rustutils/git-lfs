//! The `post-checkout`, `post-commit`, and `post-merge` hook entry points.
//!
//! All three keep lockable files read-only: files matching a `lockable`
//! pattern in `.gitattributes` lose their write bit unless the user holds
//! the lock, so nobody edits a file without locking it first. A checkout,
//! commit, or merge can add such files or change which files are lockable.
//!
//! Each hook walks every file in the index. Upstream's `post-checkout` and
//! `post-commit` only check the paths that changed.

use std::path::Path;

use crate::lockable;

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Run `post-checkout <prev-sha> <post-sha> <flag>`.
///
/// The flag is 1 for a branch checkout and 0 for a file checkout. Only the
/// argument count is checked.
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

/// Run `post-commit`, which takes no arguments.
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

/// Run `post-merge <squash-flag>`.
///
/// Only the argument count is checked.
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
