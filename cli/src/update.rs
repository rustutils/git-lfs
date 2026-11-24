//! `git lfs update`: install or upgrade the four Git LFS hooks.
//!
//! A hook that is missing, empty, or one of our older templates is
//! written. If any hook has other content, nothing is written: the
//! conflict is printed and the exit code is 2. `--force` overwrites such
//! hooks, and `--manual` prints what to add to each hook instead of
//! writing anything. The two can't be combined.
//!
//! Upstream also rewrites old `lfs.<url>.access` values. This command
//! doesn't.

use std::path::Path;

use git_lfs_git::git_dir;

use crate::install;

/// The error for `--force` with `--manual`, which `install` also uses.
pub const FORCE_AND_MANUAL: &str = "You cannot use --force and --manual options together";

/// The hooks, in the order `--manual` prints them, which is upstream's.
const HOOKS: &[&str] = &["pre-push", "post-checkout", "post-commit", "post-merge"];

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Install(#[from] install::InstallError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The command was run outside a repository.
    #[error("Not in a Git repository.")]
    NotInRepo,
}

/// Run `git lfs update` and return the exit code.
///
/// That's 0 on success and 2 after printing a hook conflict to stderr.
/// Outside a repository it returns [`UpdateError::NotInRepo`], which the
/// caller prints and turns into exit code 128, as upstream does.
pub fn run(cwd: &Path, force: bool, manual: bool) -> Result<u8, UpdateError> {
    git_dir(cwd).map_err(|_| UpdateError::NotInRepo)?;
    if force && manual {
        eprintln!("{FORCE_AND_MANUAL}");
        return Ok(2);
    }
    if manual {
        print_manual(cwd)?;
        return Ok(0);
    }

    let opts = install::InstallOptions {
        scope: install::InstallScope::Local,
        force,
        skip_repo: false,
        skip_smudge: false,
    };
    match install::install_all_hooks(cwd, &opts) {
        Ok(()) => {
            println!("Updated Git hooks.");
            Ok(0)
        }
        Err(install::InstallError::HookConflict { hook, existing }) => {
            install::print_hook_conflict(&hook, &existing);
            Ok(2)
        }
        Err(e) => Err(e.into()),
    }
}

/// Show the hooks directory relative to the Git directory's parent (the
/// working tree root, normally) or to `cwd`, or absolute if it's under
/// neither.
fn display_hooks_dir(cwd: &Path, git_dir: &Path, hooks_dir: &Path) -> String {
    let work_root = git_dir.parent().unwrap_or(git_dir);
    if let Ok(rel) = hooks_dir.strip_prefix(work_root) {
        return rel.display().to_string();
    }
    if let Ok(rel) = hooks_dir.strip_prefix(cwd) {
        return rel.display().to_string();
    }
    hooks_dir.display().to_string()
}

/// Print what to add to each hook to install it by hand.
pub fn print_manual(cwd: &Path) -> Result<(), UpdateError> {
    let git_dir = git_dir(cwd).map_err(|_| UpdateError::NotInRepo)?;
    let hooks_dir = install::effective_hooks_dir(cwd)?;
    let display_dir = display_hooks_dir(cwd, &git_dir, &hooks_dir);
    let mut first = true;
    for hook in HOOKS {
        if !first {
            println!();
        }
        first = false;
        println!("Add the following to '{display_dir}/{hook}':");
        println!();
        for line in install::current_template(hook).lines() {
            println!("\t{line}");
        }
    }
    Ok(())
}
