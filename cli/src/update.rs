//! `git lfs update`: install or upgrade the four Git LFS hooks.
//!
//! A hook that is missing, empty, or one of our older templates is
//! written. If any hook has other content, nothing is written: the
//! conflict is printed and the exit code is 2. `--force` overwrites such
//! hooks, and `--manual` prints what to add to each hook instead of
//! writing anything. The two can't be combined.
//!
//! First, old `lfs.<url>.access` values are rewritten in the local config:
//! `private` becomes `basic`, and anything other than `basic` is removed.

use std::path::Path;

use git_lfs_git::config::{self, ConfigScope};
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
    migrate_access(cwd);
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

/// Rewrite the `lfs.<url>.access` values older versions wrote, as
/// upstream does in `update`, and in `install` in a repository.
///
/// Changes are made in the local config, and failures are ignored, so a
/// value set in another file is reported but stays.
pub fn migrate_access(cwd: &Path) {
    for (key, value) in access_settings(cwd) {
        let url = &key["lfs.".len()..key.len() - ".access".len()];
        match value.as_str() {
            "basic" => {}
            "private" => {
                let _ = config::set(cwd, ConfigScope::Local, &key, "basic");
                println!("Updated {url} access from private to basic.");
            }
            _ => {
                let _ = config::unset(cwd, ConfigScope::Local, &key);
                println!("Removed invalid {url} access of {value}.");
            }
        }
    }
}

/// Every `lfs.<url>.access` key with its effective value, in config order.
fn access_settings(cwd: &Path) -> Vec<(String, String)> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "-z", "--get-regexp", r"^lfs\..*\.access$"])
        .output()
    else {
        return Vec::new();
    };
    let mut settings: Vec<(String, String)> = Vec::new();
    for entry in out.stdout.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let entry = String::from_utf8_lossy(entry);
        let (key, value) = entry.split_once('\n').unwrap_or((&entry, ""));
        // A later value overrides an earlier one.
        match settings.iter_mut().find(|(k, _)| k == key) {
            Some(existing) => existing.1 = value.to_owned(),
            None => settings.push((key.to_owned(), value.to_owned())),
        }
    }
    settings
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
