//! `git lfs update` — (re-)install the four LFS git hooks.
//!
//! Counterpart to `git lfs install` for the hooks-only side. With no
//! flags: writes any of `pre-push` / `post-checkout` / `post-commit` /
//! `post-merge` that are missing or empty, silently upgrades any of our
//! own previously-shipped templates to the current version, and on the
//! first user-edited hook prints the upstream-format conflict block
//! and exits non-zero without touching anything.
//!
//! `--force` overwrites a user-edited hook. `--manual` prints the
//! shell-step instructions for installing all four hooks by hand
//! (used by the conflict-resolution flow) and never touches the disk.
//!
//! The `lfs.<url>.access` config migration upstream performs on update
//! is tracked in NOTES.md.

use std::path::Path;

use git_lfs_git::git_dir;

use crate::install::{self, HookStatus};

/// Hook names installed by `git lfs update`, in the order shown to the
/// user (matches upstream and the order tests assert on).
const HOOKS: &[&str] = &["pre-push", "post-checkout", "post-commit", "post-merge"];

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Install(#[from] install::InstallError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Caller isn't inside any git repo. Upstream prints
    /// `"Not in a Git repository."` and exits 128.
    #[error("Not in a Git repository.")]
    NotInRepo,
}

/// Run `git lfs update`. Returns the process exit code: `0` on success,
/// `2` on a hook conflict (after writing the conflict block to stderr),
/// `Err(NotInRepo)` for the outside-repo case (the caller maps that to
/// `128` after printing the upstream message).
pub fn run(cwd: &Path, force: bool, manual: bool) -> Result<u8, UpdateError> {
    let git_dir = git_dir(cwd).map_err(|_| UpdateError::NotInRepo)?;
    let hooks_dir = install::effective_hooks_dir(cwd)?;
    let display_dir = display_hooks_dir(cwd, &git_dir, &hooks_dir);

    if manual {
        print_manual(&display_dir);
        return Ok(0);
    }

    std::fs::create_dir_all(&hooks_dir)?;

    if !force {
        for hook in HOOKS {
            let path = hooks_dir.join(hook);
            if let HookStatus::Conflict { existing } = install::classify_hook(&path, hook)? {
                print_conflict(hook, &existing);
                return Ok(2);
            }
        }
    }

    let opts = install::InstallOptions {
        scope: install::InstallScope::Local,
        force,
        skip_repo: false,
        skip_smudge: false,
    };
    install::install_all_hooks(cwd, &opts)?;
    println!("Updated Git hooks.");
    Ok(0)
}

/// Render the hooks directory the way the user types it: relative to
/// the working-tree root when possible (e.g. `.git/hooks`, or `hooks`
/// when `core.hookspath` is set or the repo is bare), absolute when
/// it lives outside.
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

fn print_conflict(hook: &str, existing: &str) {
    eprintln!("Hook already exists: {hook}");
    eprintln!();
    for line in existing.lines() {
        eprintln!("\t{line}");
    }
    eprintln!();
    eprintln!("To resolve this, either:");
    eprintln!("  1: run `git lfs update --manual` for instructions on how to merge hooks.");
    eprintln!("  2: run `git lfs update --force` to overwrite your hook.");
}

fn print_manual(display_dir: &str) {
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
}
