//! `t-install-custom-hooks-path.sh`.
//!
//! The shell suite skips itself with Git older than 2.9, which lacks
//! `core.hooksPath`. These assume a newer Git.
//!
//! The shell tests check that `.git/pre-push` and the like don't exist,
//! which they never would. These check `.git/hooks` instead, and that each
//! hook is the one install writes.

use std::path::Path;

use crate::harness::{Repo, Test};
use crate::install::{HOOKS, hook};

/// Check that `dir` has the four hooks and `repo`'s `.git/hooks` has none.
#[track_caller]
fn assert_hooks(repo: &Repo, dir: &Path) {
    for name in HOOKS {
        let contents = std::fs::read_to_string(dir.join(name));
        assert_eq!(contents.ok(), Some(hook(name)), "{name}");
        let default = repo.path().join(".git/hooks").join(name);
        assert!(!default.exists(), "{name} in .git/hooks");
    }
}

/// Check that `dir` has none of the four hooks.
#[track_caller]
fn refute_hooks(dir: &Path) {
    for name in HOOKS {
        assert!(!dir.join(name).exists(), "{name}");
    }
}

/// Run `git lfs install` in `dir`, and check that it updated the hooks.
fn install(dir: &Repo) {
    dir.lfs_command()
        .args(["install"])
        .merged()
        .run()
        .ok()
        .has("Updated Git hooks");
}

#[test]
fn install_with_supported_core_hookspath() {
    let t = Test::new();
    let repo = t.init("supported-custom-hooks-path");
    std::fs::create_dir(repo.path().join("custom_hooks_dir")).unwrap();
    repo.git(["config", "--local", "core.hooksPath", "custom_hooks_dir"])
        .ok();

    install(&repo);

    assert_hooks(&repo, &repo.path().join("custom_hooks_dir"));
}

/// A relative `core.hooksPath` is relative to the top of the working
/// tree, not the current directory.
#[test]
fn install_with_supported_core_hookspath_in_subdirectory() {
    let t = Test::new();
    let repo = t.init("supported-custom-hooks-path-subdir");
    std::fs::create_dir(repo.path().join("subdir")).unwrap();
    repo.git(["config", "--local", "core.hooksPath", "custom_hooks_dir"])
        .ok();

    install(&repo.at("subdir"));

    assert_hooks(&repo, &repo.path().join("custom_hooks_dir"));
    refute_hooks(&repo.path().join("subdir/custom_hooks_dir"));
}

#[test]
fn install_with_supported_expandable_core_hookspath() {
    let t = Test::new();
    let repo = t.init("supported-custom-hooks-expandable-path");
    // As in the shell test, which quotes the `~` and so makes a directory
    // named `~` in the repository.
    std::fs::create_dir_all(repo.path().join("~/custom_hooks_dir")).unwrap();
    repo.git(["config", "--local", "core.hooksPath", "~/custom_hooks_dir"])
        .ok();

    install(&repo);

    assert_hooks(&repo, &t.home().join("custom_hooks_dir"));
    refute_hooks(&repo.path().join("~/custom_hooks_dir"));
}
