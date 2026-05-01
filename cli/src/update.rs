//! `git lfs update` — (re-)install the four LFS git hooks.
//!
//! Counterpart to `git lfs install` for the hooks-only side: writes any
//! of `pre-push` / `post-checkout` / `post-commit` / `post-merge` that
//! are missing or empty, replaces older versions of our own template
//! when found, and refuses to overwrite a user-edited hook unless
//! `--force` is passed.
//!
//! `--manual` (print install instructions instead of writing) and the
//! `lfs.<url>.access` config migration upstream performs on update are
//! tracked in NOTES.md.

use std::path::Path;

use git_lfs_git::ConfigScope;

use crate::install;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Install(#[from] install::InstallError),
    /// Caller isn't inside any git repo. Upstream prints
    /// `"Not in a Git repository."` and exits 128.
    #[error("Not in a Git repository.")]
    NotInRepo,
}

pub fn run(cwd: &Path, force: bool, _manual: bool) -> Result<(), UpdateError> {
    if git_lfs_git::git_dir(cwd).is_err() {
        return Err(UpdateError::NotInRepo);
    }

    let opts = install::InstallOptions {
        scope: ConfigScope::Local,
        force,
        skip_repo: false,
        skip_smudge: false,
    };
    install::install_all_hooks(cwd, &opts)?;
    println!("Updated Git hooks.");
    Ok(())
}
